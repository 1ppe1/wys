//! エンドツーエンドテスト: ソース → Native 実行ファイル → 実行結果。
//!
//! 実際に clang でビルドし、生成した実行ファイルを起動して stdout と終了コードを確かめる。

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::OnceLock;

const CLI: &str = env!("CARGO_BIN_EXE_wys");

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
        .expect("run wys")
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

    // 外部インタプリタやランタイムの共有ライブラリに依存しない（OS 標準のライブラリだけ）
    for lib in dynamic_dependencies(&exe) {
        let system = if cfg!(target_os = "macos") {
            lib.starts_with("/usr/lib/") || lib.starts_with("/System/")
        } else {
            lib.starts_with("/lib") || lib.starts_with("/usr/lib") || lib.starts_with("linux-vdso")
        };
        assert!(system, "unexpected dynamic dependency: {lib}");
    }
}

/// 実行ファイルが動的にリンクしているライブラリのパス（macOS は `otool -L`、Linux は `ldd`）。
fn dynamic_dependencies(exe: &std::path::Path) -> Vec<String> {
    if cfg!(target_os = "macos") {
        let out = Command::new("otool").arg("-L").arg(exe).output().unwrap();
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .skip(1)
            .map(|l| l.trim().to_string())
            .collect()
    } else {
        let out = Command::new("ldd").arg(exe).output().unwrap();
        // 例: "libc.so.6 => /lib/x86_64-linux-gnu/libc.so.6 (0x...)" / "/lib64/ld-linux-x86-64.so.2 (0x...)"
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(|l| {
                let l = l.trim();
                match l.split_once("=>") {
                    Some((_, rhs)) => rhs.trim().to_string(),
                    None => l.to_string(),
                }
            })
            .collect()
    }
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

#[test]
fn warnings_are_reported_but_do_not_fail() {
    let path = scratch(
        "warn",
        "def main() -> None:\n    x: int | None = 1\n    match x:\n        case _:\n            print(\"ok\")\n        case None:\n            pass\n",
    );
    let check = cli(&["check", "--format", "json", path.to_str().unwrap()]);
    assert_eq!(check.status.code(), Some(0));
    let v: serde_json::Value = serde_json::from_slice(&check.stdout).unwrap();
    assert_eq!(v["diagnostics"][0]["severity"], "warning");
    assert_eq!(v["diagnostics"][0]["code"], "W0301");
    let run = cli(&["run", path.to_str().unwrap()]);
    assert_eq!(run.status.code(), Some(0));
    assert_eq!(String::from_utf8(run.stdout).unwrap(), "ok\n");
    assert!(
        String::from_utf8(run.stderr)
            .unwrap()
            .contains("warning[W0301]")
    );
}

#[test]
fn run_passes_arguments_after_double_dash() {
    let path = scratch(
        "args",
        "def main() -> None:\n    print(arg_count())\n    a = arg(1)\n    if a is not None:\n        print(a)\n",
    );
    let out = cli(&["run", path.to_str().unwrap(), "--", "first", "second"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "2\nsecond\n");
}

#[test]
fn wys_test_runs_each_test_in_isolation() {
    let path = scratch(
        "tests_mixed",
        "def add(a: int, b: int) -> int:\n    return a + b\n\ndef test_add() -> None:\n    assert(add(1, 2) == 3)\n\ndef test_wrong() -> None:\n    print(\"before\")\n    assert(add(1, 1) == 3)\n\ndef test_optional() -> None:\n    x: int | None = 2\n    assert(x is not None)\n    assert(x + 1 == 3)\n",
    );
    let out = cli(&["test", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("test test_add ... ok"), "{stdout}");
    assert!(stdout.contains("test test_wrong ... FAILED"), "{stdout}");
    assert!(stdout.contains("test test_optional ... ok"), "{stdout}");
    assert!(stdout.contains("assertion failed at"), "{stdout}");
    assert!(stdout.contains(":9:5"), "{stdout}");
    assert!(stdout.contains("before"), "{stdout}");
    assert!(
        stdout.contains("test result: FAILED. 2 passed; 1 failed"),
        "{stdout}"
    );
}

#[test]
fn wys_test_passes_and_checks_signatures() {
    let ok = scratch(
        "tests_ok",
        "def test_one() -> None:\n    assert(1 + 1 == 2)\n",
    );
    let out = cli(&["test", ok.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0));
    assert!(
        String::from_utf8(out.stdout)
            .unwrap()
            .contains("test result: ok. 1 passed; 0 failed")
    );
    let bad = scratch("tests_bad", "def test_args(n: int) -> None:\n    pass\n");
    let out = cli(&["test", bad.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        String::from_utf8(out.stderr)
            .unwrap()
            .contains("error[E0801]")
    );
}

#[test]
fn wys_fmt_rewrites_and_checks() {
    let path = scratch("fmt_me", "def main()->None:\n  print( 1+2 )\n");
    let check = cli(&["fmt", "--check", path.to_str().unwrap()]);
    assert_eq!(check.status.code(), Some(1));
    assert!(
        String::from_utf8(check.stdout)
            .unwrap()
            .contains("would reformat")
    );
    let fmt = cli(&["fmt", path.to_str().unwrap()]);
    assert_eq!(fmt.status.code(), Some(0));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "def main() -> None:\n    print(1 + 2)\n"
    );
    let again = cli(&["fmt", "--check", path.to_str().unwrap()]);
    assert_eq!(again.status.code(), Some(0));
}

/// 生成した LLVM の release を1か所ずつ取り除く／二重にして、ARC の検査が誤りを検出できることを確かめる（ADR-0012）。
/// - 取り除くと: 終了時の生存オブジェクト数の検査が、リークとして終了コード 102 で知らせる
/// - 二重にすると: AddressSanitizer が解放済みメモリの使用を検出する
#[test]
fn arc_checks_detect_injected_errors() {
    let src = scratch(
        "arc_inject",
        "def repeat(s: str, n: int) -> str:\n    out = \"\"\n    i = 0\n    while i < n:\n        out = out + s\n        i = i + 1\n    return out\n\ndef main() -> None:\n    print(len(repeat(\"ab\", 10)))\n",
    );
    let ll = String::from_utf8(cli(&["dump-llvm", src.to_str().unwrap()]).stdout).unwrap();
    let lines: Vec<&str> = ll.lines().collect();
    // ASan 用: 関数に sanitize_address 属性が付いた LLVM（同じ行の並び）
    let asan_out = Command::new(CLI)
        .args(["dump-llvm", src.to_str().unwrap()])
        .env("WYS_SANITIZE", "address")
        .env("WYS_RUNTIME_LIB", runtime_lib())
        .output()
        .unwrap();
    let asan_ll = String::from_utf8(asan_out.stdout).unwrap();
    let asan_lines: Vec<&str> = asan_ll.lines().collect();
    assert_eq!(asan_lines.len(), lines.len());
    let releases: Vec<usize> = (0..lines.len())
        .filter(|i| lines[*i].contains("call void @wys_rc_release(ptr"))
        .collect();
    assert!(releases.len() >= 2, "{ll}");
    let dir = src.parent().unwrap().to_path_buf();
    let build_and_run = |name: &str, text: String, flags: &[&str]| {
        let ll_path = dir.join(format!("{name}.ll"));
        let exe = dir.join(name);
        std::fs::write(&ll_path, text).unwrap();
        let mut cc = Command::new("clang");
        cc.arg("-Wno-override-module")
            .args(flags)
            .arg(&ll_path)
            .arg(runtime_lib());
        if cfg!(target_os = "linux") {
            cc.args([
                "-lgcc_s",
                "-lutil",
                "-lrt",
                "-lpthread",
                "-lm",
                "-ldl",
                "-lc",
            ]);
        }
        let status = cc.arg("-o").arg(&exe).status().unwrap();
        assert!(status.success(), "clang failed for {name}");
        Command::new(&exe).output().unwrap()
    };
    let mut leak_detected = false;
    let mut asan_detected = false;
    for (k, &i) in releases.iter().enumerate() {
        let without: Vec<&str> = lines
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, l)| *l)
            .collect();
        let out = build_and_run(&format!("leak{k}"), without.join("\n"), &[]);
        leak_detected |= out.status.code() == Some(102)
            && String::from_utf8_lossy(&out.stderr).contains("were not freed");
        let mut doubled: Vec<&str> = asan_lines.clone();
        doubled.insert(i, asan_lines[i]);
        let out = build_and_run(
            &format!("double{k}"),
            doubled.join("\n"),
            &["-fsanitize=address", "-g"],
        );
        asan_detected |= !out.status.success()
            && String::from_utf8_lossy(&out.stderr).contains("AddressSanitizer");
    }
    assert!(
        leak_detected,
        "removing a release was never reported as a leak"
    );
    assert!(
        asan_detected,
        "doubling a release was never reported by AddressSanitizer"
    );
}
