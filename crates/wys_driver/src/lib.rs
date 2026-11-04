//! コンパイルパイプラインの統合
//!
//! ソース → 字句/構文解析 → 型検査 → IR 変換・検証 → バックエンド → リンク。

use std::path::{Path, PathBuf};
use std::process::Command;
use wys_backend::{Backend, LlvmTextBackend};
use wys_diagnostics::Diagnostic;
pub use wys_ir::Program;

pub use wys_diagnostics::{to_json, to_text};
pub use wys_span::{SourceFile, SourceMap};

/// フロントエンドの結果。`program` はエラーがなく、コード生成もできるときだけ `Some`。
pub struct Frontend {
    /// 読み込んだすべてのソース（エントリファイルと import したモジュール）。
    pub file: SourceMap,
    pub diagnostics: Vec<Diagnostic>,
    pub program: Option<Program>,
    /// 型検査は通ったが、このコンパイラのコード生成がまだ対応していない機能を使っている場合の説明。
    /// `check` はこのプログラムを受け付ける。`build` / `run` はこのメッセージで止まる。
    pub unsupported: Option<String>,
}

impl Frontend {
    /// エラー（警告ではない診断）があるか。警告だけならビルドできる。
    pub fn has_errors(&self) -> bool {
        self.diagnostics.iter().any(Diagnostic::is_error)
    }

    pub fn warnings(&self) -> Vec<Diagnostic> {
        self.diagnostics
            .iter()
            .filter(|d| !d.is_error())
            .cloned()
            .collect()
    }
}

/// 字句解析から IR 生成までを行う。構文エラーがあれば型検査はしない（エラーの連鎖を防ぐ）。
pub fn frontend(name: &str, text: &str) -> Frontend {
    frontend_with(name, text, &|p| std::fs::read_to_string(p).ok())
}

/// `read` で import 先のファイルを読む（テストなどでファイルシステムを使わない場合）。
pub fn frontend_with(
    name: &str,
    text: &str,
    read: &dyn Fn(&std::path::Path) -> Option<String>,
) -> Frontend {
    run_frontend(name, text, read, false).0
}

/// `wys test` 用: `main` は不要。エントリファイルの `test_` で始まる関数を集め、それらを呼び分けるエントリで IR を作る。
/// 戻り値の2つ目はテスト関数の名前（IR のエントリが使う番号の順）。
pub fn frontend_tests(name: &str, text: &str) -> (Frontend, Vec<String>) {
    run_frontend(name, text, &|p| std::fs::read_to_string(p).ok(), true)
}

/// `frontend_tests` の、import 先の読み方を指定する版。
pub fn frontend_tests_with(
    name: &str,
    text: &str,
    read: &dyn Fn(&std::path::Path) -> Option<String>,
) -> (Frontend, Vec<String>) {
    run_frontend(name, text, read, true)
}

fn run_frontend(
    name: &str,
    text: &str,
    read: &dyn Fn(&std::path::Path) -> Option<String>,
    test_mode: bool,
) -> (Frontend, Vec<String>) {
    let loaded = wys_resolve::load_program(name, text, read);
    let file = loaded.sources;
    let module = loaded.module;
    let failed = |file, diagnostics| {
        (
            Frontend {
                file,
                diagnostics,
                program: None,
                unsupported: None,
            },
            Vec::new(),
        )
    };
    if loaded.diagnostics.iter().any(Diagnostic::is_error) {
        return failed(file, loaded.diagnostics);
    }
    let (tc, mut diagnostics) = wys_typeck::check_program(&module, !test_mode);
    diagnostics.splice(0..0, loaded.diagnostics);
    // テスト関数（エントリファイルの `test_*`）。引数なし・戻り値 None でなければ E0801
    let mut tests = Vec::new();
    if test_mode {
        for (i, f) in module.funcs.iter().enumerate() {
            if f.name.name.contains('.') || !f.name.name.starts_with("test_") {
                continue;
            }
            if !f.params.is_empty()
                || !f.type_params.is_empty()
                || tc.sigs[i].ret != wys_typeck::Ty::None
            {
                diagnostics.push(
                    Diagnostic::error(
                        "E0801",
                        format!(
                            "test function `{}` must take no parameters and return `None`",
                            f.name.name
                        ),
                        f.name.span,
                    )
                    .with_hint(format!("declare it as `def {}() -> None:`", f.name.name)),
                );
                continue;
            }
            tests.push(i);
        }
    }
    if diagnostics.iter().any(Diagnostic::is_error) {
        return failed(file, diagnostics);
    }
    let names = tests
        .iter()
        .map(|i| module.funcs[*i].name.name.clone())
        .collect();
    if test_mode && tests.is_empty() {
        return (
            Frontend {
                file,
                diagnostics,
                program: None,
                unsupported: None,
            },
            names,
        );
    }
    let locate = |b: u32| {
        let p = file.position(b);
        format!("{}:{}:{}", file.file_name(b), p.line, p.column)
    };
    let lowered = wys_lower::lower_with(
        &module,
        &tc,
        &wys_lower::LowerOptions {
            tests,
            locate: &locate,
        },
    );
    let program = match lowered {
        Ok(p) => p,
        Err(d) => {
            diagnostics.push(*d);
            return failed(file, diagnostics);
        }
    };
    let mut program = program;
    // ARC: retain / release を挿入し（ADR-0012）、IR と参照カウントの釣り合いを検証する
    wys_ir::arc::insert(&mut program);
    if let Err(e) = wys_ir::verify(&program).and_then(|()| wys_ir::arc::verify(&program)) {
        panic!("internal compiler error: IR verification failed: {e}\n{program}");
    }
    (
        Frontend {
            file,
            diagnostics,
            program: Some(program),
            unsupported: None,
        },
        names,
    )
}

#[derive(Debug, Default, Clone)]
pub struct BuildOptions {
    /// ランタイムの静的ライブラリ。未指定なら `WYS_RUNTIME_LIB`、次に実行ファイルの隣を探す。
    pub runtime_lib: Option<PathBuf>,
    /// C コンパイラ／リンカ。未指定なら `WYS_CC`、なければ `clang`。
    pub cc: Option<String>,
    /// 最適化レベル（0 = Debug、2 = Release）。未指定なら `WYS_OPT`、なければ 0。
    pub opt_level: Option<u8>,
    /// サニタイザ（`address`）。未指定なら `WYS_SANITIZE`、なければ付けない（ADR-0005 / ADR-0012 の検査用）。
    pub sanitize: Option<String>,
}

impl BuildOptions {
    fn effective_opt_level(&self) -> u8 {
        self.opt_level
            .or_else(|| std::env::var("WYS_OPT").ok().and_then(|v| v.parse().ok()))
            .unwrap_or(0)
    }

    fn sanitizer_flags(&self) -> Vec<String> {
        self.sanitize
            .clone()
            .or_else(|| std::env::var("WYS_SANITIZE").ok())
            .filter(|s| !s.is_empty())
            .map(|s| vec![format!("-fsanitize={s}"), "-g".to_string()])
            .unwrap_or_default()
    }
}

#[derive(Debug)]
pub enum BuildError {
    Diagnostics(Box<Frontend>),
    Io(String),
    Tool(String),
}

impl std::fmt::Debug for Frontend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frontend")
            .field("file", &self.file.entry().name)
            .field("diagnostics", &self.diagnostics.len())
            .finish()
    }
}

const RUNTIME_LIB_NAME: &str = "libwys_runtime.a";

pub fn find_runtime_lib(opts: &BuildOptions) -> Result<PathBuf, BuildError> {
    if let Some(p) = &opts.runtime_lib {
        return Ok(p.clone());
    }
    if let Some(p) = std::env::var_os("WYS_RUNTIME_LIB") {
        return Ok(PathBuf::from(p));
    }
    let exe = std::env::current_exe().map_err(|e| BuildError::Io(e.to_string()))?;
    let mut candidates = Vec::new();
    if let Some(dir) = exe.parent() {
        candidates.push(dir.join(RUNTIME_LIB_NAME));
        if let Some(parent) = dir.parent() {
            // `target/debug/deps/` から実行されるテストバイナリ用
            candidates.push(parent.join(RUNTIME_LIB_NAME));
        }
    }
    candidates.into_iter().find(|p| p.exists()).ok_or_else(|| {
        BuildError::Tool(format!(
            "cannot find the runtime library `{RUNTIME_LIB_NAME}`; build it with `cargo build -p wys_runtime` or set WYS_RUNTIME_LIB"
        ))
    })
}

fn cc(opts: &BuildOptions) -> String {
    opts.cc
        .clone()
        .or_else(|| std::env::var("WYS_CC").ok())
        .unwrap_or_else(|| "clang".to_string())
}

/// 一時ディレクトリ（終了時に削除）。
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Result<Self, BuildError> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("wys-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|e| BuildError::Io(e.to_string()))?;
        Ok(Self(dir))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Rust の staticlib（ランタイム）をリンクするのに必要なシステムライブラリ。
/// `cargo rustc -p wys_runtime --lib --crate-type staticlib -- --print native-static-libs` の出力に合わせる。
/// macOS では clang が既定で `libSystem` をリンクするので追加は要らない。
fn native_link_args() -> &'static [&'static str] {
    if cfg!(target_os = "linux") {
        &[
            "-lgcc_s",
            "-lutil",
            "-lrt",
            "-lpthread",
            "-lm",
            "-ldl",
            "-lc",
        ]
    } else {
        &[]
    }
}

/// IR から実行ファイルを作る。
pub fn link_program(
    program: &Program,
    output: &Path,
    opts: &BuildOptions,
) -> Result<(), BuildError> {
    let runtime = find_runtime_lib(opts)?;
    let cc = cc(opts);
    let tmp = TempDir::new()?;
    let obj = tmp.0.join("module.o");
    let sanitize = opts.sanitizer_flags();
    let backend = LlvmTextBackend {
        cc: cc.clone(),
        opt_level: opts.effective_opt_level(),
        extra_flags: sanitize.clone(),
    };
    backend
        .emit_object(program, &obj)
        .map_err(|e| BuildError::Tool(e.to_string()))?;
    let out = Command::new(&cc)
        .arg(&obj)
        .arg(&runtime)
        .args(native_link_args())
        .args(&sanitize)
        .arg("-o")
        .arg(output)
        .output()
        .map_err(|e| BuildError::Tool(format!("cannot run `{cc}`: {e}")))?;
    if !out.status.success() {
        return Err(BuildError::Tool(format!(
            "linking failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(())
}

/// ソースファイルから実行ファイルを作る。
pub fn build_file(source: &Path, output: &Path, opts: &BuildOptions) -> Result<(), BuildError> {
    let text = std::fs::read_to_string(source)
        .map_err(|e| BuildError::Io(format!("cannot read {}: {e}", source.display())))?;
    let fe = frontend(&source.display().to_string(), &text);
    if let Some(msg) = &fe.unsupported {
        return Err(BuildError::Tool(msg.clone()));
    }
    let Some(program) = &fe.program else {
        return Err(BuildError::Diagnostics(Box::new(fe)));
    };
    link_program(program, output, opts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontend_reports_parse_errors_before_type_errors() {
        let fe = frontend("t.wys", "def main() -> None:\n    x = \n    y = 1 + True\n");
        let codes: Vec<_> = fe.diagnostics.iter().map(|d| d.code).collect();
        assert_eq!(codes, vec!["E0101"]);
        assert!(fe.program.is_none());
    }

    #[test]
    fn unions_produce_verified_ir() {
        let fe = frontend(
            "t.wys",
            "def f(x: int | None) -> int:\n    if x is None:\n        return 0\n    return x\n\ndef main() -> None:\n    print(f(None))\n",
        );
        assert!(!fe.has_errors());
        assert!(fe.unsupported.is_none());
        assert!(!fe.program.unwrap().unions.is_empty());
    }

    #[test]
    fn warnings_do_not_block_the_build() {
        let fe = frontend(
            "t.wys",
            "def main() -> None:\n    x: int | None = 1\n    match x:\n        case _:\n            pass\n        case None:\n            pass\n",
        );
        assert!(!fe.has_errors());
        assert_eq!(fe.warnings().len(), 1);
        assert!(fe.program.is_some());
    }

    #[test]
    fn modules_are_resolved_and_built() {
        let read = |p: &std::path::Path| {
            (p == std::path::Path::new("dir/geo.wys"))
                .then(|| "pub def area(w: int, h: int) -> int:\n    return w * h\n".to_string())
        };
        let fe = frontend_with(
            "dir/main.wys",
            "from geo import area\n\ndef main() -> None:\n    print(area(2, 3))\n",
            &read,
        );
        assert!(!fe.has_errors(), "{:?}", fe.diagnostics);
        assert!(
            fe.program
                .unwrap()
                .funcs
                .iter()
                .any(|f| f.name == "geo.area")
        );
    }

    #[test]
    fn frontend_produces_verified_ir() {
        let fe = frontend("t.wys", "def main() -> None:\n    print(\"hi\")\n");
        assert!(!fe.has_errors());
        assert!(fe.program.is_some());
    }
}
