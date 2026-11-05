//! Python のランタイムの C ABI（ADR-0018。PoC3 の M4 T1 から）。`embed_poc.c` を `libwys_runtime_python.a` と
//! `libpython3.14` にリンクして実行する。生成コードからの利用は `wys_driver/tests/python_link.rs`。
//!
//! Python 3.14 は `WYS_PYTHON`（既定は `python3.14`）で探す。見つからなければ理由を出して飛ばす。
//! `WYS_REQUIRE_PYTHON=1`（CI）なら、見つからないときに失敗させる。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const EXPECTED: &str = "\
3628800
\"hi\"
True
True
False
ValueError
invalid literal for int() with base 10: 'x'
ValueError
factorial() not defined for negative values
ModuleNotFoundError
No module named 'wys_no_such_module'
AttributeError
module 'math' has no attribute 'nope'
ZeroDivisionError
division by zero
TypeError
expected an int
TypeError
expected a bool
E
<str() of the exception failed>
ValueError
embedded null character in a Python name
ValueError
embedded null character in a Python name
ok
traceback mentions boom
TypeError
expected an int
ValueError
v
ok
";

/// リンクのフラグ（driver と同じもの）。
fn python_ldflags() -> Option<Vec<String>> {
    static FLAGS: OnceLock<Option<Vec<String>>> = OnceLock::new();
    FLAGS
        .get_or_init(|| wys_driver::python_link_flags().ok())
        .clone()
}

fn require() -> Option<Vec<String>> {
    let flags = python_ldflags();
    if flags.is_none() {
        assert!(
            std::env::var_os("WYS_REQUIRE_PYTHON").is_none(),
            "WYS_REQUIRE_PYTHON is set but Python 3.14 (WYS_PYTHON) was not found"
        );
        eprintln!("skipping: Python 3.14 was not found (set WYS_PYTHON)");
    }
    flags
}

/// `cargo build -p wys_runtime_python`（テストと同じプロファイル）で作った staticlib。
fn runtime_lib() -> &'static PathBuf {
    static LIB: OnceLock<PathBuf> = OnceLock::new();
    LIB.get_or_init(|| {
        let exe = std::env::current_exe().unwrap();
        let profile_dir = exe.parent().unwrap().parent().unwrap().to_path_buf();
        let mut cmd = Command::new(env!("CARGO"));
        cmd.args(["build", "-q", "-p", "wys_runtime_python"]);
        if profile_dir.ends_with("release") {
            cmd.arg("--release");
        }
        assert!(
            cmd.status().unwrap().success(),
            "failed to build wys_runtime_python"
        );
        profile_dir.join("libwys_runtime_python.a")
    })
}

fn build(name: &str, src: &Path, extra: &[&str], flags: &[String]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wys-poc3-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join(name);
    let out = Command::new(std::env::var("WYS_CC").unwrap_or_else(|_| "clang".into()))
        .arg(src)
        .arg(runtime_lib())
        .args(flags)
        .args(extra)
        .arg("-o")
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "link failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    exe
}

fn source() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/embed_poc.c")
}

fn run(exe: &Path) -> (i32, String, String) {
    let o = Command::new(exe).output().unwrap();
    (
        o.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&o.stdout).into_owned(),
        String::from_utf8_lossy(&o.stderr).into_owned(),
    )
}

#[test]
fn python_calls_round_trip_and_exceptions_become_values() {
    let Some(flags) = require() else { return };
    let exe = build("poc3", &source(), &["-O2"], &flags);
    let (code, stdout, stderr) = run(&exe);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, EXPECTED);
    // 送出中の例外を残していれば、終了処理で「Exception ignored」が出る
    assert!(!stderr.contains("Exception ignored"), "{stderr}");
}

#[test]
fn python_embedding_works_under_address_sanitizer() {
    let Some(flags) = require() else { return };
    let exe = build(
        "poc3-asan",
        &source(),
        &["-fsanitize=address", "-g"],
        &flags,
    );
    let (code, stdout, stderr) = run(&exe);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, EXPECTED);
}

#[test]
fn a_leaked_python_handle_is_detected() {
    let Some(flags) = require() else { return };
    // 最初のハンドル（math）を解放しない
    let src = std::fs::read_to_string(source()).unwrap().replace(
        "wys_py_release(handles[i]);",
        "if (i) wys_py_release(handles[i]);",
    );
    let dir = std::env::temp_dir().join(format!("wys-poc3-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let leak = dir.join("leak.c");
    std::fs::write(&leak, src).unwrap();
    let exe = build("poc3-leak", &leak, &[], &flags);
    let (code, _, stderr) = run(&exe);
    assert_eq!(code, 102, "{stderr}");
    assert!(stderr.contains("1 object(s) were not freed"), "{stderr}");
}
