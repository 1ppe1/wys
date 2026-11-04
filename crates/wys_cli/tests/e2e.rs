//! エンドツーエンドテスト: ソース → Native 実行ファイル → 実行結果。
//!
//! 実際に clang でビルドし、生成した実行ファイルを起動して stdout と終了コードを確かめる。

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::OnceLock;

const CLI: &str = env!("CARGO_BIN_EXE_wys_cli");

/// ランタイムの staticlib をビルドしてパスを返す。
///
/// `cargo test` は依存クレートの staticlib を出力しないため、テストの最初に1回だけビルドする。
fn runtime_lib() -> &'static PathBuf {
    static LIB: OnceLock<PathBuf> = OnceLock::new();
    LIB.get_or_init(|| {
        let profile_dir = PathBuf::from(CLI).parent().unwrap().to_path_buf();
        let mut cmd = Command::new(env!("CARGO"));
        cmd.args(["build", "-q", "-p", "wys_runtime"]);
        if profile_dir.ends_with("release") {
            cmd.arg("--release");
        }
        let status = cmd.status().expect("run cargo build -p wys_runtime");
        assert!(status.success(), "failed to build wys_runtime");
        let lib = profile_dir.join("libwys_runtime.a");
        assert!(lib.exists(), "{} was not produced", lib.display());
        lib
    })
}

fn scratch(name: &str, src: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wys-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.wys"));
    std::fs::write(&path, src).unwrap();
    path
}

fn cli(args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
        .env("WYS_RUNTIME_LIB", runtime_lib())
        .output()
        .expect("run wys_cli")
}

fn run(name: &str, src: &str) -> (i32, String, String) {
    let path = scratch(name, src);
    let out = cli(&["run", path.to_str().unwrap()]);
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8(out.stdout).unwrap(),
        String::from_utf8(out.stderr).unwrap(),
    )
}

fn main_body(body: &str) -> String {
    format!("def main() -> None:\n{body}")
}

#[test]
fn ac004_hello_world_builds_a_standalone_executable() {
    let path = scratch(
        "hello",
        "def main() -> None:\n    print(\"Hello, world!\")\n",
    );
    let exe = path.with_extension("bin");
    let out = cli(&["build", path.to_str().unwrap(), "-o", exe.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&exe).output().unwrap();
    assert_eq!(String::from_utf8(run.stdout).unwrap(), "Hello, world!\n");
    assert_eq!(run.status.code(), Some(0));
}

#[test]
fn recursion_loops_and_conditionals() {
    let src = "def fib(n: int) -> int:\n    if n < 2:\n        return n\n    return fib(n - 1) + fib(n - 2)\n\ndef main() -> None:\n    i = 0\n    while i <= 10:\n        print(fib(i))\n        i = i + 1\n";
    let (code, out, _) = run("fib", src);
    assert_eq!(code, 0);
    assert_eq!(out, "0\n1\n1\n2\n3\n5\n8\n13\n21\n34\n55\n");
}

#[test]
fn integer_semantics_match_python() {
    let (code, out, _) = run(
        "arith",
        &main_body(
            "    print(-7 // 2)\n    print(-7 % 3)\n    print(7 % -3)\n    print(1 + 2 * -3)\n    print((1 + 2) * 3)\n",
        ),
    );
    assert_eq!(code, 0);
    assert_eq!(out, "-4\n2\n-2\n-5\n9\n");
}

#[test]
fn booleans_and_short_circuit() {
    let src = "def loud() -> bool:\n    print(\"evaluated\")\n    return True\n\ndef main() -> None:\n    print(False and loud())\n    print(True or loud())\n    print(not (1 == 2))\n    if 1 < 2 and 2 >= 2:\n        print(\"yes\")\n    elif True:\n        print(\"no\")\n";
    let (code, out, _) = run("bools", src);
    assert_eq!(code, 0);
    assert_eq!(out, "False\nTrue\nTrue\nyes\n");
}

#[test]
fn strings_keep_unicode_and_escapes() {
    let (code, out, _) = run(
        "strs",
        &main_body("    s = \"こんにちは\\t\\\"wys\\\"\"\n    print(s)\n    print(\"\")\n"),
    );
    assert_eq!(code, 0);
    assert_eq!(out, "こんにちは\t\"wys\"\n\n");
}

#[test]
fn exit_sets_the_status_code() {
    let (code, out, _) = run(
        "exit",
        &main_body("    print(1)\n    exit(3)\n    print(2)\n"),
    );
    assert_eq!(code, 3);
    assert_eq!(out, "1\n");
}

#[test]
fn integer_overflow_is_a_runtime_error() {
    let (code, out, err) = run(
        "overflow",
        &main_body("    x = 9223372036854775807\n    print(1)\n    print(x + 1)\n"),
    );
    assert_eq!(code, 101);
    assert_eq!(out, "1\n");
    assert!(err.contains("integer overflow"), "{err}");
}

#[test]
fn division_by_zero_is_a_runtime_error() {
    let (code, _, err) = run("divzero", &main_body("    z = 0\n    print(10 // z)\n"));
    assert_eq!(code, 101);
    assert!(err.contains("division by zero"), "{err}");
}

#[test]
fn ac001_ac005_type_error_is_reported_as_json() {
    let path = scratch(
        "type_error",
        "def double(x: int) -> int:\n    return x * 2\n\ndef main() -> None:\n    print(double(\"ten\"))\n",
    );
    let out = cli(&["check", "--format", "json", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["schema_version"], "0");
    let d = &v["diagnostics"][0];
    assert_eq!(d["code"], "E0201");
    assert_eq!(d["span"]["start"]["line"], 5);
    assert_eq!(d["span"]["start"]["column"], 18);
    assert_eq!(d["related"][0]["name"], "double");
}

#[test]
fn build_refuses_programs_with_errors() {
    let path = scratch("bad", "def main() -> None:\n    x = 1 + True\n");
    let out = cli(&["build", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("error[E0207]"), "{err}");
}

#[test]
fn check_succeeds_with_empty_report() {
    let path = scratch("ok", "def main() -> None:\n    pass\n");
    let out = cli(&["check", "--format", "json", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["diagnostics"].as_array().unwrap().len(), 0);
}
