//! Python を使う IR のリンクと実行（ADR-0018、M4 T5）。言語側の表面（`py_import` など）は T6 で入れるので、
//! ここでは IR を直接組み立てる。
//!
//! Python 3.14 は `WYS_PYTHON`（既定は `python3.14`）で探す。見つからなければ理由を出して飛ばす。
//! `WYS_REQUIRE_PYTHON=1`（CI）なら、見つからないときに失敗させる。

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use wys_driver::BuildOptions;
use wys_ir::*;

fn python_available() -> bool {
    let ok = wys_driver::python_link_flags().is_ok();
    if !ok {
        assert!(
            std::env::var_os("WYS_REQUIRE_PYTHON").is_none(),
            "WYS_REQUIRE_PYTHON is set but Python 3.14 (WYS_PYTHON) was not found"
        );
        eprintln!("skipping: Python 3.14 was not found (set WYS_PYTHON)");
    }
    ok
}

/// テストと同じプロファイルで作ったランタイム（Python を含むものと含まないもの）。
fn runtimes() -> &'static (PathBuf, PathBuf) {
    static LIBS: OnceLock<(PathBuf, PathBuf)> = OnceLock::new();
    LIBS.get_or_init(|| {
        let exe = std::env::current_exe().unwrap();
        let profile_dir = exe.parent().unwrap().parent().unwrap().to_path_buf();
        let mut cmd = Command::new(env!("CARGO"));
        cmd.args([
            "build",
            "-q",
            "-p",
            "wys_runtime",
            "-p",
            "wys_runtime_python",
        ]);
        if profile_dir.ends_with("release") {
            cmd.arg("--release");
        }
        assert!(
            cmd.status().unwrap().success(),
            "failed to build the runtimes"
        );
        (
            profile_dir.join("libwys_runtime.a"),
            profile_dir.join("libwys_runtime_python.a"),
        )
    })
}

fn opts(sanitize: Option<&str>) -> BuildOptions {
    let (rt, py) = runtimes();
    BuildOptions {
        runtime_lib: Some(rt.clone()),
        python_runtime_lib: Some(py.clone()),
        sanitize: sanitize.map(String::from),
        ..Default::default()
    }
}

/// 関数の本体を組み立てる小さな補助。
struct B {
    strings: Vec<String>,
    temps: Vec<Ty>,
    insts: Vec<Inst>,
}

impl B {
    fn temp(&mut self, t: Ty) -> TempId {
        self.temps.push(t);
        TempId(self.temps.len() as u32 - 1)
    }
    fn str(&mut self, s: &str) -> TempId {
        self.strings.push(s.into());
        let dst = self.temp(Ty::Str);
        let value = Const::Str(StrId(self.strings.len() as u32 - 1));
        self.insts.push(Inst::Const { dst, value });
        dst
    }
    fn int(&mut self, v: i64) -> TempId {
        let dst = self.temp(Ty::Int);
        self.insts.push(Inst::Const {
            dst,
            value: Const::Int(v),
        });
        dst
    }
    fn call(&mut self, b: Builtin, args: Vec<TempId>) -> Option<TempId> {
        let dst = b.ret().map(|t| self.temp(t));
        self.insts.push(Inst::Call {
            dst,
            callee: Callee::Builtin(b),
            args,
        });
        dst
    }
}

/// `math.factorial(10)` を表示し、存在しないモジュールの import の失敗と、その例外の型名を表示する。
fn program() -> Program {
    let mut b = B {
        strings: Vec::new(),
        temps: Vec::new(),
        insts: Vec::new(),
    };
    let name = b.str("math");
    let math = b.call(Builtin::PyImport, vec![name]).unwrap();
    let attr = b.str("factorial");
    let fact = b.call(Builtin::PyGetAttr, vec![math, attr]).unwrap();
    let ten = b.int(10);
    let arg = b.call(Builtin::PyFromInt, vec![ten]).unwrap();
    let args = b.temp(Ty::List(ListId(0)));
    b.insts.push(Inst::ListNew {
        dst: args,
        list: ListId(0),
        items: vec![arg],
    });
    let r = b.call(Builtin::PyCall, vec![fact, args]).unwrap();
    let v = b.call(Builtin::PyToInt, vec![r]).unwrap();
    b.call(Builtin::PrintInt, vec![v]);
    let missing = b.str("wys_no_such_module");
    b.call(Builtin::PyImport, vec![missing]);
    let failed = b.call(Builtin::PyFailed, vec![]).unwrap();
    b.call(Builtin::PrintBool, vec![failed]);
    let kind = b.call(Builtin::PyErrKind, vec![]).unwrap();
    b.call(Builtin::PrintStr, vec![kind]);
    let mut p = Program {
        strings: b.strings,
        unions: vec![],
        records: vec![],
        lists: vec![Ty::Py],
        dicts: vec![],
        tasks: vec![],
        unknown: None,
        display: DisplayInfo::default(),
        entry: FuncId(0),
        funcs: vec![Function {
            name: "main".into(),
            param_count: 0,
            locals: vec![],
            ret: None,
            temps: b.temps,
            blocks: vec![Block {
                insts: b.insts,
                term: Terminator::Return(None),
            }],
            elided_rc: Vec::new(),
            is_async: false,
            effects: None,
            tool: None,
            agent: None,
        }],
    };
    arc::insert(&mut p);
    verify(&p).and_then(|()| arc::verify(&p)).unwrap();
    p
}

fn build_and_run(name: &str, sanitize: Option<&str>) -> (i32, String, String) {
    let dir = std::env::temp_dir().join(format!("wys-python-link-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join(name);
    wys_driver::link_program(&program(), &exe, &opts(sanitize)).unwrap();
    let o = Command::new(&exe).output().unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

#[test]
fn a_python_program_links_libpython_and_runs() {
    if !python_available() {
        return;
    }
    assert!(uses_python(&program()));
    let (code, stdout, stderr) = build_and_run("py", None);
    // 終了時の生存数が 0（ハンドルの解放で Py_DecRef が呼ばれる）で、終了コード 0
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, "3628800\nTrue\nModuleNotFoundError\n");
}

#[test]
fn a_python_program_runs_under_address_sanitizer() {
    if !python_available() {
        return;
    }
    let (code, stdout, stderr) = build_and_run("py-asan", Some("address"));
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, "3628800\nTrue\nModuleNotFoundError\n");
}

#[test]
fn programs_without_python_do_not_need_python() {
    // Python の組み込みも `py` 型も使わないプログラムは、Python がなくてもリンクできる（AC-004 を保つ）
    let p = Program {
        strings: vec![],
        unions: vec![],
        records: vec![],
        lists: vec![],
        dicts: vec![],
        tasks: vec![],
        unknown: None,
        display: DisplayInfo::default(),
        entry: FuncId(0),
        funcs: vec![Function {
            name: "main".into(),
            param_count: 0,
            locals: vec![],
            ret: None,
            temps: vec![],
            blocks: vec![Block {
                insts: vec![],
                term: Terminator::Return(None),
            }],
            elided_rc: Vec::new(),
            is_async: false,
            effects: None,
            tool: None,
            agent: None,
        }],
    };
    assert!(!uses_python(&p));
    let dir = std::env::temp_dir().join(format!("wys-python-link-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("no-py");
    let (rt, _) = runtimes();
    let opts = BuildOptions {
        runtime_lib: Some(rt.clone()),
        // 見つからない Python のランタイムを指定しても、使わなければ問題にならない
        python_runtime_lib: Some(dir.join("does-not-exist.a")),
        ..Default::default()
    };
    wys_driver::link_program(&p, &exe, &opts).unwrap();
    assert!(Command::new(&exe).status().unwrap().success());
}
