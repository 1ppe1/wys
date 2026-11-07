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
        // Python を使うプログラム用のランタイム（ADR-0018）も一緒に作る
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

/// `wys` のコマンド。開発者のシェルの AI Runtime の設定（ADR-0023）は外す（テストの結果を変えないように）
fn wys() -> Command {
    let mut cmd = Command::new(CLI);
    for (k, _) in std::env::vars() {
        if k.starts_with("WYS_LLM_")
            || matches!(
                k.as_str(),
                "WYS_CAPABILITIES"
                    | "WYS_APPROVAL"
                    | "WYS_AUDIT"
                    | "WYS_TRACE"
                    | "WYS_TRACE_CONTENT"
            )
        {
            cmd.env_remove(k);
        }
    }
    cmd
}

fn cli(args: &[&str]) -> Output {
    wys()
        .args(args)
        .env("WYS_RUNTIME_LIB", runtime_lib())
        .env("WYS_RUNTIME_PYTHON_LIB", python_runtime_lib())
        .output()
        .expect("run wys")
}

fn python_runtime_lib() -> PathBuf {
    runtime_lib().with_file_name("libwys_runtime_python.a")
}

fn cli_env(args: &[&str], envs: &[(&str, &str)]) -> Output {
    wys()
        .args(args)
        .env("WYS_RUNTIME_LIB", runtime_lib())
        .env("WYS_RUNTIME_PYTHON_LIB", python_runtime_lib())
        .envs(envs.iter().copied())
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

    assert_only_system_libraries(&exe);
}

/// 外部インタプリタやランタイムの共有ライブラリに依存しない（OS 標準のライブラリだけ。AC-004）
fn assert_only_system_libraries(exe: &std::path::Path) {
    for lib in dynamic_dependencies(exe) {
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
fn unused_names_are_warned_but_do_not_fail() {
    scratch("unused_lib", "pub def f() -> int:\n    return 1\n");
    let path = scratch(
        "unused_main",
        "import unused_lib\n\ndef main() -> None:\n    x = 1\n    _y = 2\n    print(\"ok\")\n",
    );
    let check = cli(&["check", "--format", "json", path.to_str().unwrap()]);
    assert_eq!(check.status.code(), Some(0));
    let v: serde_json::Value = serde_json::from_slice(&check.stdout).unwrap();
    let codes: Vec<&str> = v["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, ["W0202", "W0201"]);
    let run = cli(&["run", path.to_str().unwrap()]);
    assert_eq!(run.status.code(), Some(0));
    assert_eq!(String::from_utf8(run.stdout).unwrap(), "ok\n");
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

#[test]
fn result_values_run_and_missing_types_are_hinted() {
    let ok = scratch(
        "result_ok",
        "def half(n: int) -> Result[int, str]:\n    if n % 2 == 1:\n        return Err(\"odd\")\n    return Ok(n // 2)\n\ndef main() -> None:\n    for n in [4, 3]:\n        match half(n):\n            case Ok(v):\n                print(v)\n            case Err(e):\n                print(e)\n",
    );
    let run = cli(&["run", ok.to_str().unwrap()]);
    assert_eq!(run.status.code(), Some(0));
    assert_eq!(String::from_utf8(run.stdout).unwrap(), "2\nodd\n");

    let bad = scratch("result_bad", "def main() -> None:\n    r = Ok(1)\n");
    let check = cli(&["check", "--format", "json", bad.to_str().unwrap()]);
    assert_eq!(check.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&check.stdout).unwrap();
    let d = &v["diagnostics"][0];
    assert_eq!(d["code"], "E1001");
    assert!(
        d["hints"][0]["message"]
            .as_str()
            .unwrap()
            .contains("Result[int, str]")
    );
}

#[test]
fn try_propagates_and_reports_uncovered_errors() {
    let ok = scratch(
        "try_ok",
        "def parse(s: str) -> Result[int, str]:\n    if s == \"1\":\n        return Ok(1)\n    return Err(\"bad \" + s)\n\ndef twice(s: str) -> Result[int, str]:\n    return Ok(parse(s)? * 2)\n\ndef main() -> None:\n    for s in [\"1\", \"x\"]:\n        match twice(s):\n            case Ok(v):\n                print(v)\n            case Err(e):\n                print(e)\n",
    );
    let run = cli(&["run", ok.to_str().unwrap()]);
    assert_eq!(run.status.code(), Some(0));
    assert_eq!(String::from_utf8(run.stdout).unwrap(), "2\nbad x\n");

    let bad = scratch(
        "try_bad",
        "def f() -> Result[int, str]:\n    return Ok(1)\n\ndef g() -> Result[int, int]:\n    return Ok(f()?)\n\ndef main() -> None:\n    g()\n",
    );
    let check = cli(&["check", "--format", "json", bad.to_str().unwrap()]);
    assert_eq!(check.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&check.stdout).unwrap();
    let d = &v["diagnostics"][0];
    assert_eq!(d["code"], "E1012");
    assert!(
        d["hints"][0]["message"]
            .as_str()
            .unwrap()
            .contains("int | str")
    );
}

#[test]
fn panic_flushes_stdout_and_exits_with_101() {
    let path = scratch(
        "panic",
        "def main() -> None:\n    print(\"partial\")\n    panic(\"stop \" + \"here\")\n",
    );
    let run = cli(&["run", path.to_str().unwrap()]);
    assert_eq!(run.status.code(), Some(101));
    assert_eq!(String::from_utf8(run.stdout).unwrap(), "partial\n");
    let stderr = String::from_utf8(run.stderr).unwrap();
    assert!(
        stderr.starts_with("runtime error: stop here at "),
        "{stderr}"
    );
    assert!(stderr.trim_end().ends_with(":3:5"), "{stderr}");
}

#[test]
fn dict_counts_words_in_insertion_order() {
    let path = scratch(
        "dict_count",
        "def main() -> None:\n    counts: dict[str, int] = {}\n    for w in [\"b\", \"a\", \"b\", \"c\", \"b\"]:\n        n = counts.get(w)\n        if n is None:\n            counts[w] = 1\n        else:\n            counts[w] = n + 1\n    for w in counts:\n        print(w)\n        print(counts[w])\n    print(\"z\" in counts)\n",
    );
    let run = cli(&["run", path.to_str().unwrap()]);
    assert_eq!(run.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(run.stdout).unwrap(),
        "b\n3\na\n1\nc\n1\nFalse\n"
    );
}

#[test]
fn rc_stats_count_reference_count_operations() {
    let path = scratch(
        "rc_stats",
        "def main() -> None:\n    s = \"\"\n    for i in range(3):\n        s = s + \"x\"\n    print(s)\n",
    );
    let exe = path.with_extension("stats");
    let build = cli_env(
        &["build", path.to_str().unwrap(), "-o", exe.to_str().unwrap()],
        &[("WYS_RC_STATS", "1")],
    );
    assert_eq!(build.status.code(), Some(0));
    let run = std::process::Command::new(&exe)
        .env("WYS_RC_STATS", "1")
        .output()
        .unwrap();
    assert_eq!(String::from_utf8(run.stdout).unwrap(), "xxx\n");
    let stderr = String::from_utf8(run.stderr).unwrap();
    // 3回の連結で作った文字列はすべて解放される（最初の空文字列はリテラルで不死）
    assert!(stderr.contains("rc stats: retain "), "{stderr}");
    assert!(stderr.contains("release 3"), "{stderr}");
}

#[test]
fn explain_reports_types_narrowing_and_arc_reasons_as_json() {
    let path = scratch(
        "explain",
        "def first(xs: list[str]) -> str:\n    return xs[0]\n\ndef main() -> None:\n    s: str | None = first([\"a\"])\n    if s is not None:\n        print(s)\n",
    );
    let out = cli(&["explain", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["schema_version"], "0");
    assert_eq!(v["kind"], "file");
    let first = &v["functions"][0];
    assert_eq!(first["name"], "first");
    assert_eq!(first["signature"]["params"][0]["type"], "list[str]");
    let ops = first["arc"][0]["ops"].as_array().unwrap();
    assert!(
        ops.iter()
            .any(|o| o["op"] == "retain" && o["reason"] == "borrow_from_mutable_place"),
        "{ops:?}"
    );
    let main = &v["functions"][1];
    assert_eq!(main["narrowing"][0]["variable"], "s");
    assert_eq!(main["narrowing"][0]["narrowed"], "str");
    // `--format json` を明示してもよい。text はまだない
    let out = cli(&["explain", "--format", "json", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0));
    let out = cli(&["explain", "--format", "text", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn explain_of_a_file_with_errors_prints_diagnostics() {
    let path = scratch("explain_bad", "def main() -> None:\n    x = 1 + True\n");
    let out = cli(&["explain", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["diagnostics"][0]["code"], "E0207");
}

#[test]
fn explain_looks_up_error_codes() {
    let out = cli(&["explain", "E0201"]);
    assert_eq!(out.status.code(), Some(0));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["kind"], "code");
    assert_eq!(v["code"], "E0201");
    assert_eq!(v["stage"], "型");
    let out = cli(&["explain", "E9999"]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains("unknown error code `E9999`"), "{err}");
}

#[test]
fn effects_are_checked_and_do_not_change_the_program() {
    // 注釈のある関数が宣言にない Effect を使うと E1102。修正案は `uses` 節を書き換える
    let bad = scratch(
        "effects_bad",
        "def log(x: int) -> None uses pure:\n    print(x)\n\ndef main() -> None:\n    log(1)\n",
    );
    let out = cli(&["check", "--format", "json", bad.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let d = &v["diagnostics"][0];
    assert_eq!(d["code"], "E1102");
    assert_eq!(d["hints"][0]["replacement"]["text"], "io");
    // 注釈を正しく書けば、注釈のないプログラムと同じように動く
    let (code, stdout, _) = run(
        "effects_ok",
        "def log(x: int) -> None uses io:\n    print(x)\n\ndef main() -> None uses io:\n    log(1)\n",
    );
    assert_eq!(code, 0);
    assert_eq!(stdout, "1\n");
    // IR の属性と `wys explain` に出る（T9b）
    let ok = scratch(
        "effects_explain",
        "def log(x: int) -> None uses io:\n    print(x)\n\ndef main() -> None:\n    log(1)\n",
    );
    let out = cli(&["dump-ir", ok.to_str().unwrap()]);
    let ir = String::from_utf8(out.stdout).unwrap();
    assert!(ir.contains("fn f0 log(l0: int) uses io {"), "{ir}");
    let out = cli(&["explain", ok.to_str().unwrap()]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["functions"][0]["effects"]["declared"][0], "io");
    assert!(v["functions"][1]["effects"]["declared"].is_null());
    assert_eq!(v["functions"][1]["effects"]["sites"][0]["callee"], "log");
}

#[test]
fn record_fields_are_narrowed_until_the_root_is_reassigned() {
    let (code, stdout, stderr) = run(
        "field_narrowing",
        "type P = {x: int | None}\n\ndef main() -> None:\n    p = P(x=41)\n    if p.x is not None:\n        print(p.x + 1)\n",
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, "42\n");
    let path = scratch(
        "field_narrowing_reset",
        "type P = {x: int | None}\n\ndef main() -> None:\n    p = P(x=1)\n    if p.x is not None:\n        p = P(x=None)\n        print(p.x + 1)\n",
    );
    let out = cli(&["check", "--format", "json", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["diagnostics"][0]["code"], "E0301");
}

#[test]
fn structural_values_compare_by_content() {
    let (code, stdout, stderr) = run(
        "structural_eq",
        "type P = {x: int, tags: list[str]}\n\ndef main() -> None:\n    a = P(x=1, tags=[\"t\"])\n    print(a == P(x=1, tags=[\"t\"]))\n    print({\"a\": 1, \"b\": 2} == {\"b\": 2, \"a\": 1})\n    o: int | None = 1\n    print(o != 1)\n",
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, "True\nTrue\nFalse\n");
}

#[test]
fn modules_and_names_can_be_imported_under_aliases() {
    scratch(
        "alias_lib",
        "pub def twice(n: int) -> int:\n    return n * 2\n\npub def name() -> str:\n    return \"lib\"\n",
    );
    let (code, stdout, stderr) = run(
        "alias_main",
        "import alias_lib as l\nfrom alias_lib import name as lib_name\n\ndef name() -> str:\n    return \"main\"\n\ndef main() -> None:\n    print(l.twice(21))\n    print(lib_name())\n    print(name())\n",
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, "42\nlib\nmain\n");
}

#[test]
fn python_libraries_can_be_called_and_exceptions_are_values() {
    // ADR-0018: Python 3.14 がなければ飛ばす（CI では必須）
    if wys_driver::python_link_flags().is_err() {
        assert!(
            std::env::var_os("WYS_REQUIRE_PYTHON").is_none(),
            "WYS_REQUIRE_PYTHON is set but Python 3.14 was not found"
        );
        eprintln!("skipping: Python 3.14 was not found");
        return;
    }
    let (code, stdout, stderr) = run(
        "python",
        "def sqrt(x: float) -> Result[str, PyError] uses python:\n    m = py_import(\"math\")?\n    r = (m.attr(\"sqrt\")?).call([py_value(x)])?\n    return ((r.attr(\"__repr__\")?).call([])?).to_str()\n\ndef main() -> None:\n    match sqrt(2.0):\n        case Ok(s):\n            print(s)\n        case Err(e):\n            print(e.kind)\n    match sqrt(-1.0):\n        case Ok(s):\n            print(s)\n        case Err(e):\n            print(e.kind + \": \" + e.message)\n",
    );
    assert_eq!(code, 0, "{stderr}");
    // メッセージの文言は Python の版で変わるので、例外の種類だけを確かめる
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines[0], "1.4142135623730951");
    assert!(lines[1].starts_with("ValueError: "), "{stdout}");
}

#[test]
fn python_values_are_copied_across_the_boundary() {
    if wys_driver::python_link_flags().is_err() {
        assert!(
            std::env::var_os("WYS_REQUIRE_PYTHON").is_none(),
            "WYS_REQUIRE_PYTHON is set but Python 3.14 was not found"
        );
        return;
    }
    let (code, stdout, stderr) = run(
        "python_conv",
        "def round_trip(d: dict[str, list[float]]) -> Result[int, PyError] uses python:\n    json = py_import(\"json\")?\n    text = (json.attr(\"dumps\")?).call([py_value(d)])?\n    back = ((json.attr(\"loads\")?).call([text])?).to_dict()?\n    return Ok(len(back[\"xs\"].to_list()?))\n\ndef main() -> None:\n    match round_trip({\"xs\": [1.5, 2.0, -0.0]}):\n        case Ok(n):\n            print(n)\n        case Err(e):\n            print(e.kind)\n",
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, "3\n");
}

#[test]
fn move_optimization_reduces_rc_ops() {
    // M4 T3: 同じプログラムを最適化あり／なし（WYS_ARC_OPT=0）で動かし、出力が同じで回数が減ることを確かめる
    let path = scratch(
        "move_opt",
        "def same(s: str) -> str:\n    return s\n\ndef main() -> None:\n    xs = [\"a\" + \"b\", \"c\" + \"d\"]\n    n = 0\n    for i in range(1000):\n        n = n + len(xs[i % 2])\n        n = n + len(same(xs[0]))\n    print(n)\n",
    );
    let stats = |opt: &str| {
        let out = cli_env(
            &["run", path.to_str().unwrap()],
            &[("WYS_RC_STATS", "1"), ("WYS_ARC_OPT", opt)],
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let err = String::from_utf8(out.stderr).unwrap();
        let nums: Vec<u64> = err
            .split(|c: char| !c.is_ascii_digit())
            .filter_map(|w| w.parse().ok())
            .collect();
        (String::from_utf8(out.stdout).unwrap(), nums[0], nums[1])
    };
    let (out_on, retains_on, releases_on) = stats("1");
    let (out_off, retains_off, releases_off) = stats("0");
    assert_eq!(out_on, out_off);
    assert_eq!(out_on, "4000\n");
    assert!(retains_on < retains_off, "{retains_on} vs {retains_off}");
    assert!(
        releases_on < releases_off,
        "{releases_on} vs {releases_off}"
    );
}

#[test]
fn floats_follow_ieee_754_and_print_like_python() {
    let (code, stdout, stderr) = run(
        "floats",
        "def main() -> None:\n    x = 1.0 / 3.0\n    print(x)\n    print(0.1 + 0.2 == 0.3)\n    print(float(2) * 1e16)\n    print(int(-2.7))\n    print([1.5, -0.0])\n",
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stdout,
        "0.3333333333333333\nFalse\n2e+16\n-2\n[1.5, -0.0]\n"
    );
}

#[test]
fn structural_values_are_printed() {
    let (code, stdout, stderr) = run(
        "structural_print",
        "type P = {y: int, x: str}\n\ndef main() -> None:\n    print([P(x=\"a\\n\", y=1)])\n    print({1: None, 2: [True]})\n    r: Result[int, str] = Err(\"e\")\n    print(r)\n",
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(
        stdout,
        "[P(x=\"a\\n\", y=1)]\n{1: None, 2: [True]}\nErr(\"e\")\n"
    );
}

/// `async`（ADR-0020、M4 T10）: 実行できる。`sleep_ms` は、`WYS_VIRTUAL_TIME` がなければ実時間を待つ。
#[test]
fn async_programs_run_and_sleep_waits_real_time() {
    let src = "async def double(n: int) -> int:\n    await sleep_ms(50)\n    return n * 2\n\nasync def main() -> None:\n    t = spawn(double(2))\n    print(await double(1) + await t)\n";
    let path = scratch("async_run", src);
    let start = std::time::Instant::now();
    let out = cli(&["run", path.to_str().unwrap()]);
    let elapsed = start.elapsed();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "6\n");
    // 2 つのタスクが並行に 50 ms ずつ眠る（実時間で 50 ms 以上）
    assert!(
        elapsed >= std::time::Duration::from_millis(50),
        "{elapsed:?}"
    );
    // 仮想時計なら待たないが、結果は同じ
    let out = cli_env(
        &["run", path.to_str().unwrap()],
        &[("WYS_VIRTUAL_TIME", "1")],
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "6\n");
    // `async def` の呼び出しには `await` か `spawn` が要る（E1303）
    let path = scratch(
        "async_e1303",
        "async def f() -> int:\n    return 1\n\nasync def main() -> None:\n    print(f())\n",
    );
    let out = cli(&["check", "--format", "json", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("\"E1303\""));
}

/// 同期の `main` のプログラムが `Task` の型を注釈だけで使っても、ビルドして実行できる（スケジューラは動かない）。
#[test]
fn sync_programs_may_mention_task_types() {
    let (code, stdout, stderr) = run(
        "task_annotation_only",
        "def identity(t: Task[int]) -> Task[int]:\n    return t\n\ndef main() -> None:\n    x: Task[int] | None = None\n    print(x is None)\n",
    );
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, "True\n");
}

/// `wys test` の `async def test_*`（ADR-0020 論点 7）と、`dump-ir` の表記（論点 19）。
#[test]
fn async_tests_run_and_ir_shows_tasks() {
    let src = "async def inc(n: int) -> int:\n    await sleep_ms(1)\n    return n + 1\n\nasync def test_spawn() -> None:\n    assert(await spawn(inc(1)) == 2)\n\nasync def test_fails() -> None:\n    assert(await inc(1) == 3)\n\ndef test_sync() -> None:\n    assert(True)\n";
    let path = scratch("async_tests", src);
    let out = cli_env(
        &["test", path.to_str().unwrap()],
        &[("WYS_VIRTUAL_TIME", "1")],
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("test_spawn ... ok"), "{stdout}");
    assert!(stdout.contains("test_fails ... FAILED"), "{stdout}");
    assert!(stdout.contains("test_sync ... ok"), "{stdout}");
    let path = scratch(
        "async_ir",
        "async def inc(n: int) -> int:\n    return n + 1\n\nasync def main() -> None:\n    t = spawn(inc(1))\n    print(await t)\n",
    );
    let out = cli(&["dump-ir", path.to_str().unwrap()]);
    let ir = String::from_utf8_lossy(&out.stdout);
    assert!(ir.contains("task task0 = Task[int]"), "{ir}");
    assert!(ir.contains("async fn f0 inc("), "{ir}");
    assert!(ir.contains("= spawn task0 f0("), "{ir}");
    assert!(ir.contains("= await t"), "{ir}");
}

#[test]
fn explicit_type_arguments_select_the_instance() {
    // `f[T](x)` と `Box[T](...)`（ADR-0021 論点 9）: 引数から決まらない型パラメータを明示する。
    // `dump-ir` には明示した型で単相化した関数が出る（`size[int]` の引数は `list[int]`）
    let src = "type Box[T] = {value: T | None}\n\ndef empty[T]() -> Box[T]:\n    return Box[T](value=None)\n\ndef size[T](xs: list[T]) -> int:\n    return len(xs)\n\ndef main() -> None:\n    b = empty[str]()\n    if b.value is None:\n        print(size[int]([]))\n";
    let (code, out, err) = run("explicit_targs", src);
    assert_eq!((code, out.as_str()), (0, "0\n"), "{err}");
    let path = scratch("explicit_targs_ir.wys", src);
    let ir = cli(&["dump-ir", path.to_str().unwrap()]);
    let ir = String::from_utf8_lossy(&ir.stdout);
    assert!(ir.contains("fn f1 empty__"), "{ir}");
    assert!(ir.contains("size__") && ir.contains("(l0: list"), "{ir}");
}

#[test]
fn parse_json_builds_an_unknown_tree() {
    // `parse_json` と `case list():` / `case dict():`（ADR-0021）。`dump-ir` には `Unknown` の型と `make_unknown` が出る
    let src = "def size(u: Unknown) -> int:\n    match u:\n        case list():\n            return len(u)\n        case dict():\n            return len(u)\n        case _:\n            return -1\n\ndef main() -> None:\n    match parse_json(\"{\\\"a\\\": [1, 2, 3]}\"):\n        case Ok(v):\n            match v:\n                case dict():\n                    if \"a\" in v:\n                        print(size(v[\"a\"]))\n                case _:\n                    pass\n        case Err(e):\n            print(e.message)\n    print(size(parse_scalar(\"7\")))\n    match parse_json(\"[1,\"):\n        case Ok(_v):\n            pass\n        case Err(e):\n            print(e.offset)\n";
    let (code, out, err) = run("parse_json", src);
    assert_eq!((code, out.as_str()), (0, "3\n-1\n3\n"), "{err}");
    let path = scratch("parse_json_ir.wys", src);
    let ir = cli(&["dump-ir", path.to_str().unwrap()]);
    let ir = String::from_utf8_lossy(&ir.stdout);
    assert!(
        ir.contains("unknown = int | bool | str | float | None | list"),
        "{ir}"
    );
    assert!(ir.contains("call @json_parse("), "{ir}");
    assert!(ir.contains("make_unknown int("), "{ir}");
}

#[test]
fn validate_and_to_json_follow_the_declared_type() {
    // validate[T] と to_json（ADR-0022 論点 6・7）。dump-ir には validate / to_json の組み込みが出る
    let src = "type Step = {name: str, n: int}\n\ndef main() -> None:\n    match parse_json(\"{\\\"name\\\": \\\"a\\\", \\\"n\\\": \\\"x\\\"}\"):\n        case Ok(u):\n            match validate[Step](u):\n                case Ok(s):\n                    print(to_json(s))\n                case Err(e):\n                    print(e.path + \": expected \" + e.expected + \", found \" + e.found)\n        case Err(e):\n            print(e.message)\n    print(to_json(Step(name=\"b\", n=2)))\n";
    let (code, out, err) = run("validate", src);
    assert_eq!(
        (code, out.as_str()),
        (0, "n: expected int, found str\n{\"n\":2,\"name\":\"b\"}\n"),
        "{err}"
    );
    let path = scratch("validate_ir.wys", src);
    let ir = cli(&["dump-ir", path.to_str().unwrap()]);
    let ir = String::from_utf8_lossy(&ir.stdout);
    assert!(ir.contains("call @validate("), "{ir}");
    assert!(ir.contains("call @to_json("), "{ir}");
}

#[test]
fn decorators_appear_in_dump_ir() {
    // @tool / @agent はコンパイル時の属性（ADR-0022 論点 12・16）。実行の意味を変えず、dump-ir の署名に出る
    let src = "@tool(description=\"Add.\", approval=\"required\")\ndef add(a: int, b: int) -> int uses pure:\n    return a + b\n\n@agent(tools=[add], capabilities=[\"llm\"], max_tool_calls=2)\ndef plan() -> int uses llm:\n    return add(1, 2)\n\ndef main() -> None uses io, llm:\n    print(plan())\n";
    let (code, out, err) = run("decorators", src);
    assert_eq!((code, out.as_str()), (0, "3\n"), "{err}");
    let path = scratch("decorators_ir.wys", src);
    let ir = cli(&["dump-ir", path.to_str().unwrap()]);
    let ir = String::from_utf8_lossy(&ir.stdout);
    assert!(
        ir.contains("add(l0: int, l1: int) -> int uses pure @tool(approval=required) {"),
        "{ir}"
    );
    assert!(
        ir.contains(
            "plan() -> int uses llm @agent(tools=[add], capabilities=[llm], max_tool_calls=2) {"
        ),
        "{ir}"
    );
}

#[test]
fn llm_output_is_validated_against_the_declared_type() {
    // llm_generate[T]（ADR-0022 論点 8、AC-006）: モックの本文は T として検証され、合わなければ Err(schema)。
    // dump-ir には llm_generate と出力スキーマの定数が出る。WYS_AUDIT は監査イベントを JSON 行で書く
    let src = "type Plan = {title: str, steps: int}\n\ndef req() -> LlmRequest uses pure:\n    return LlmRequest(model=\"\", system=\"\", messages=[Message(role=\"user\", content=\"go\")], max_tokens=0)\n\nasync def main() -> None uses io, llm:\n    llm_mock_push(MockResponse(text=\"{\\\"title\\\": \\\"t\\\", \\\"steps\\\": 2}\", tool_calls=[], input_tokens=3, output_tokens=4, latency_ms=0))\n    llm_mock_push(MockResponse(text=\"{\\\"title\\\": \\\"t\\\"}\", tool_calls=[], input_tokens=1, output_tokens=1, latency_ms=0))\n    i = 0\n    while i < 2:\n        match await llm_generate[Plan](req()):\n            case Ok(p):\n                print(p.steps)\n            case Err(e):\n                print(e.kind + \": \" + e.message)\n        i = i + 1\n";
    let path = scratch("llm_validated.wys", src);
    let audit = path.with_extension("audit.jsonl");
    let _ = std::fs::remove_file(&audit);
    let out = cli_env(
        &["run", path.to_str().unwrap()],
        &[
            ("WYS_AUDIT", audit.to_str().unwrap()),
            ("WYS_VIRTUAL_TIME", "1"),
        ],
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "2\nschema: steps: expected int, found missing\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<serde_json::Value> = std::fs::read_to_string(&audit)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let kinds: Vec<&str> = lines.iter().map(|l| l["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["llm_called", "llm_called", "schema_rejected"]);
    assert_eq!(lines[0]["detail"], "tokens=7");
    let ir = cli(&["dump-ir", path.to_str().unwrap()]);
    let ir = String::from_utf8_lossy(&ir.stdout);
    assert!(ir.contains("call @llm_generate("), "{ir}");
    assert!(ir.contains("const schema r"), "{ir}");
}

#[test]
fn tools_run_after_the_runtime_checks_them() {
    // ツール表とトランポリン（ADR-0022 論点 15）、agent コンテキスト（ADR-0023 論点 9・10、AC-007）:
    // 一覧にあるツールは実行され、ないツールは本体が動かないまま拒否される。dump-ir に agent の出入りとトランポリンが出る
    let src = "@tool(description=\"Add.\")\ndef add(a: int, b: int) -> int uses io:\n    print(\"add\")\n    return a + b\n\n@tool(description=\"Secret.\")\ndef secret() -> None uses io:\n    print(\"secret ran\")\n\n@agent(tools=[add], capabilities=[\"io\", \"llm\"])\nasync def plan() -> Result[str, LlmError] uses io, llm:\n    req = LlmRequest(model=\"\", system=\"\", messages=[Message(role=\"user\", content=\"go\")], max_tokens=0)\n    return await llm_generate[str](req)\n\nasync def main() -> None uses io, llm:\n    llm_mock_push(MockResponse(text=\"\", tool_calls=[MockToolCall(name=\"add\", args_json=\"{\\\"a\\\": 1, \\\"b\\\": 2}\")], input_tokens=1, output_tokens=1, latency_ms=0))\n    llm_mock_push(MockResponse(text=\"\", tool_calls=[MockToolCall(name=\"secret\", args_json=\"{}\")], input_tokens=1, output_tokens=1, latency_ms=0))\n    match await plan():\n        case Ok(t):\n            print(t)\n        case Err(e):\n            print(e.kind)\n";
    let path = scratch("tools_checked.wys", src);
    let audit = path.with_extension("audit.jsonl");
    let _ = std::fs::remove_file(&audit);
    let out = cli_env(
        &["run", path.to_str().unwrap()],
        &[
            ("WYS_AUDIT", audit.to_str().unwrap()),
            ("WYS_VIRTUAL_TIME", "1"),
        ],
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "add\ndenied\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let kinds: Vec<String> = std::fs::read_to_string(&audit)
        .unwrap()
        .lines()
        .map(|l| {
            serde_json::from_str::<serde_json::Value>(l).unwrap()["kind"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(
        kinds,
        ["llm_called", "tool_called", "llm_called", "tool_denied"]
    );
    let ir = cli(&["dump-ir", path.to_str().unwrap()]);
    let ir = String::from_utf8_lossy(&ir.stdout);
    assert!(ir.contains("call @agent_enter()"), "{ir}");
    assert!(ir.contains("call @agent_exit()"), "{ir}");
    assert!(ir.contains("__wys_tool_entry_add(l0: str) -> str"), "{ir}");
}

/// `wys` を、stdin に `input` をつないで実行する（`WYS_APPROVAL=prompt` の検査）。
fn cli_stdin(args: &[&str], envs: &[(&str, &str)], input: &str) -> Output {
    use std::io::Write as _;
    let mut child = wys()
        .args(args)
        .env("WYS_RUNTIME_LIB", runtime_lib())
        .env("WYS_RUNTIME_PYTHON_LIB", python_runtime_lib())
        .envs(envs.iter().copied())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("run wys");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().expect("wait for wys")
}

#[test]
fn approval_prompt_reads_the_answer_from_stdin() {
    // WYS_APPROVAL=prompt（ADR-0023 論点 12、AI-005）: stderr に問い、stdin の 1 行で決める。
    // y / yes で実行、それ以外は「denied by approver」を LLM に返して続け、EOF は Err(approval)
    let src = "@tool(description=\"Remove.\", approval=\"required\")\ndef rm(path: str) -> None uses io:\n    print(\"removed \" + path)\n\n@agent(tools=[rm], capabilities=[\"io\", \"llm\"])\nasync def cleaner() -> Result[str, LlmError] uses io, llm:\n    req = LlmRequest(model=\"\", system=\"\", messages=[Message(role=\"user\", content=\"go\")], max_tokens=0)\n    return await llm_generate[str](req)\n\nasync def main() -> None uses io, llm:\n    llm_mock_push(MockResponse(text=\"\", tool_calls=[MockToolCall(name=\"rm\", args_json=\"{\\\"path\\\": \\\"/tmp/x\\\"}\")], input_tokens=1, output_tokens=1, latency_ms=0))\n    llm_mock_push(MockResponse(text=\"done\", tool_calls=[], input_tokens=1, output_tokens=1, latency_ms=0))\n    match await cleaner():\n        case Ok(t):\n            print(\"ok \" + t)\n        case Err(e):\n            print(e.kind + \": \" + e.message)\n";
    let path = scratch("approval_prompt.wys", src);
    let p = path.to_str().unwrap();
    let env = [("WYS_APPROVAL", "prompt")];
    for (input, stdout) in [
        ("y\n", "removed /tmp/x\nok done\n"),
        ("YES\n", "removed /tmp/x\nok done\n"),
        ("no\n", "ok done\n"),
        ("\n", "ok done\n"),
        (
            "",
            "approval: no answer to the approval of tool `rm` (end of input)\n",
        ),
    ] {
        let out = cli_stdin(&["run", p], &env, input);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            stdout,
            "input {input:?}: {stderr}"
        );
        assert!(
            stderr.contains(
                "approve tool rm (agent cleaner) with args {\"path\": \"/tmp/x\"}? [y/N] "
            ),
            "{stderr}"
        );
    }
}

#[test]
fn agent_timeout_uses_the_real_clock_without_virtual_time() {
    // 実時間のモード（WYS_VIRTUAL_TIME なし）でも、ツールの sleep_ms が期限に数えられる（ADR-0023 論点 14、AC-008）
    let src = "@tool(description=\"Waits.\")\nasync def wait() -> None uses pure:\n    await sleep_ms(60)\n\n@agent(tools=[wait], capabilities=[\"llm\"], timeout_ms=30)\nasync def slow() -> Result[str, LlmError] uses llm:\n    req = LlmRequest(model=\"\", system=\"\", messages=[Message(role=\"user\", content=\"go\")], max_tokens=0)\n    return await llm_generate[str](req)\n\nasync def main() -> None uses io, llm:\n    llm_mock_push(MockResponse(text=\"\", tool_calls=[MockToolCall(name=\"wait\", args_json=\"{}\")], input_tokens=1, output_tokens=1, latency_ms=0))\n    llm_mock_push(MockResponse(text=\"done\", tool_calls=[], input_tokens=1, output_tokens=1, latency_ms=0))\n    match await slow():\n        case Ok(t):\n            print(\"ok \" + t)\n        case Err(e):\n            print(e.kind + \": \" + e.message)\n";
    let (code, stdout, stderr) = run("agent_real_timeout.wys", src);
    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, "timeout: timeout_ms exceeded\n", "{stderr}");
}

/// stdin に `input` をつなぐが、stderr に承認の問いが出てから `delay` 待って書く（承認の後の再検査）。
fn cli_answer_late(
    args: &[&str],
    envs: &[(&str, &str)],
    input: &str,
    delay: std::time::Duration,
) -> Output {
    use std::io::{Read as _, Write as _};
    let mut child = wys()
        .args(args)
        .env("WYS_RUNTIME_LIB", runtime_lib())
        .env("WYS_RUNTIME_PYTHON_LIB", python_runtime_lib())
        .envs(envs.iter().copied())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("run wys");
    let mut err = child.stderr.take().unwrap();
    let mut seen = Vec::new();
    let mut b = [0u8; 1];
    while !seen.ends_with(b"[y/N] ") {
        if err.read(&mut b).unwrap() == 0 {
            break;
        }
        seen.push(b[0]);
    }
    std::thread::sleep(delay);
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let mut out = child.wait_with_output().expect("wait for wys");
    err.read_to_end(&mut seen).unwrap();
    out.stderr = seen;
    out
}

#[test]
fn approval_prompt_reads_only_its_own_line() {
    // prompt の答えは 1 行だけ読む（先読みしない）。残りは Python のツールの sys.stdin から読める（Fable のレビュー）。
    // stdin が端末でなければ、答えの後に stderr を改行して、続く出力が問いの行に連ならない
    let src = PY_STDIN;
    let path = scratch("approval_prompt_py_stdin.wys", src);
    let out = cli_stdin(
        &["run", path.to_str().unwrap()],
        &[("WYS_APPROVAL", "prompt"), ("WYS_AUDIT", "stderr")],
        "y\nsecret line\n",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "removed /tmp/x\npython read: [secret line\n]\nok done\n",
        "{stderr}"
    );
    assert!(stderr.contains("[y/N] \n{"), "{stderr}");
}

#[test]
fn approval_is_followed_by_a_deadline_check() {
    // 答えを待つ間に期限を過ぎたら、許可してもツールを実行しない（ai-sdk.md 16 章、ADR-0023 論点 14）
    let path = scratch("approval_then_timeout.wys", APPROVAL_THEN_TIMEOUT);
    let out = cli_answer_late(
        &["run", path.to_str().unwrap()],
        &[("WYS_APPROVAL", "prompt")],
        "y\n",
        // 期限（2 秒）を確実に過ぎてから答える。期限を短くすると、遅い環境（Linux のエミュレーション）では
        // 承認を尋ねる前に期限が切れる
        std::time::Duration::from_millis(4000),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "timeout: timeout_ms exceeded\nllm_called tokens=2\napproval_requested WYS_APPROVAL=prompt\napproval_granted WYS_APPROVAL=prompt\nbudget_exceeded timeout_ms\n",
        "{stderr}"
    );
}

#[test]
fn approval_without_an_answer_is_audited() {
    // EOF は Err(approval)、監査は approval_requested → approval_denied（no answer）
    let path = scratch("approval_eof_audit.wys", APPROVAL_THEN_TIMEOUT);
    let out = cli_stdin(
        &["run", path.to_str().unwrap()],
        &[("WYS_APPROVAL", "prompt")],
        "",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "approval: no answer to the approval of tool `rm` (end of input)\nllm_called tokens=2\napproval_requested WYS_APPROVAL=prompt\napproval_denied no answer (end of input)\n",
        "{stderr}"
    );
}

const PY_STDIN: &str = "@tool(description=\"Remove.\", approval=\"required\")\ndef rm(path: str) -> None uses io:\n    print(\"removed \" + path)\n\ndef read_impl() -> Result[str, PyError] uses python:\n    sys = py_import(\"sys\")?\n    stdin = sys.attr(\"stdin\")?\n    f = stdin.attr(\"readline\")?\n    r = f.call([])?\n    return r.to_str()\n\n@tool(description=\"Reads a line from stdin via Python.\")\ndef readline() -> str uses io, python:\n    match read_impl():\n        case Ok(s):\n            print(\"python read: [\" + s + \"]\")\n            return s\n        case Err(e):\n            print(\"python err: \" + e.message)\n            return \"\"\n\n@agent(tools=[rm, readline], capabilities=[\"io\", \"llm\", \"python\"])\nasync def cleaner() -> Result[str, LlmError] uses io, llm, python:\n    req = LlmRequest(model=\"\", system=\"\", messages=[Message(role=\"user\", content=\"go\")], max_tokens=0)\n    return await llm_generate[str](req)\n\nasync def main() -> None uses io, llm, python:\n    llm_mock_push(MockResponse(text=\"\", tool_calls=[MockToolCall(name=\"rm\", args_json=\"{\\\"path\\\": \\\"/tmp/x\\\"}\")], input_tokens=1, output_tokens=1, latency_ms=0))\n    llm_mock_push(MockResponse(text=\"\", tool_calls=[MockToolCall(name=\"readline\", args_json=\"{}\")], input_tokens=1, output_tokens=1, latency_ms=0))\n    llm_mock_push(MockResponse(text=\"done\", tool_calls=[], input_tokens=1, output_tokens=1, latency_ms=0))\n    match await cleaner():\n        case Ok(t):\n            print(\"ok \" + t)\n        case Err(e):\n            print(e.kind + \": \" + e.message)\n";

const APPROVAL_THEN_TIMEOUT: &str = "@tool(description=\"Remove.\", approval=\"required\")\ndef rm(path: str) -> None uses io:\n    print(\"removed \" + path)\n\n@agent(tools=[rm], capabilities=[\"io\", \"llm\"], timeout_ms=2000)\nasync def cleaner() -> Result[str, LlmError] uses io, llm:\n    req = LlmRequest(model=\"\", system=\"\", messages=[Message(role=\"user\", content=\"go\")], max_tokens=0)\n    return await llm_generate[str](req)\n\nasync def main() -> None uses io, llm:\n    llm_mock_push(MockResponse(text=\"\", tool_calls=[MockToolCall(name=\"rm\", args_json=\"{\\\"path\\\": \\\"/tmp/x\\\"}\")], input_tokens=1, output_tokens=1, latency_ms=0))\n    llm_mock_push(MockResponse(text=\"done\", tool_calls=[], input_tokens=1, output_tokens=1, latency_ms=0))\n    match await cleaner():\n        case Ok(t):\n            print(\"ok \" + t)\n        case Err(e):\n            print(e.kind + \": \" + e.message)\n    for ev in audit_events():\n        print(ev.kind + \" \" + ev.detail)\n";

// ---------------------------------------------------------------- 実プロバイダ（M5 T11、ADR-0023 論点 1〜6）

const FAKE_PROVIDER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fake_provider.sh");

/// `src` を、`replies` を順に返すテスト用のプロバイダ（`fake_provider.sh`）につないで実行する。
/// 戻り値は実行結果と、プロバイダが受け取った要求（1 行ずつの JSON）。
fn run_with_provider(
    name: &str,
    src: &str,
    replies: &[&str],
    envs: &[(&str, &str)],
) -> (Output, Vec<serde_json::Value>) {
    let path = scratch(name, src);
    let log = path.with_extension("requests.jsonl");
    let replies_path = path.with_extension("replies.jsonl");
    let _ = std::fs::remove_file(&log);
    std::fs::write(&replies_path, replies.join("\n") + "\n").unwrap();
    let provider = format!("cmd:{FAKE_PROVIDER}");
    let mut all: Vec<(&str, &str)> = vec![
        ("WYS_LLM_PROVIDER", provider.as_str()),
        ("FAKE_LOG", log.to_str().unwrap()),
        ("FAKE_REPLIES", replies_path.to_str().unwrap()),
    ];
    all.extend_from_slice(envs);
    let out = cli_env(&["run", path.to_str().unwrap()], &all);
    let requests = std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).expect("the runtime sends one JSON object per line"))
        .collect();
    (out, requests)
}

const PLANNER: &str = r#"type Plan = {title: str, steps: int}

@tool(description="Adds two integers.")
def add(a: int, b: int) -> int uses pure:
    return a + b

@agent(tools=[add], capabilities=["llm"], model="agent-model", timeout_ms=600000)
async def planner() -> Result[Plan, LlmError] uses llm:
    req = LlmRequest(model="", system="Be brief.", messages=[Message(role="user", content="plan")], max_tokens=100)
    return await llm_generate[Plan](req)

async def main() -> None uses io, llm:
    match await planner():
        case Ok(p):
            print(p.title)
            print(p.steps)
        case Err(e):
            print(e.kind + ": " + e.message)
    for ev in audit_events():
        print(ev.kind + " " + ev.detail)
"#;

const TOOL_USE_REPLY: &str = r#"{"text":"","tool_calls":[{"id":"t1","name":"add","args":{"a":2,"b":3}}],"usage":{"input_tokens":10,"output_tokens":5},"stop":"tool_use","assistant":[{"type":"thinking","thinking":"","signature":"sig"},{"type":"tool_use","id":"t1","name":"add","input":{"a":2,"b":3}}]}"#;
const PLAN_REPLY: &str = r#"{"text":"{\"title\":\"plan\",\"steps\":5}","tool_calls":[],"usage":{"input_tokens":20,"output_tokens":8},"stop":"end_turn","assistant":[{"type":"text","text":"{\"title\":\"plan\",\"steps\":5}"}]}"#;

#[test]
fn provider_round_trip_carries_tools_schema_and_the_opaque_assistant() {
    // ADR-0023 論点 2・3: 要求に v・model（agent の既定）・system・ツールの定義・出力スキーマ・max_tokens・残り時間が入り、
    // 前の応答の assistant は手を加えずに次の要求に戻る。ツールの結果には呼び出しの ID が付く。使用量は usage を数える
    let (out, reqs) = run_with_provider(
        "provider_round_trip.wys",
        PLANNER,
        &[TOOL_USE_REPLY, PLAN_REPLY],
        &[],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "plan\n5\nllm_called tokens=15\ntool_called args_len=13\nllm_called tokens=28\n",
        "{stderr}"
    );
    assert_eq!(reqs.len(), 2, "{reqs:?}");
    let first = &reqs[0];
    assert_eq!(first["v"], 1);
    assert_eq!(first["model"], "agent-model");
    assert_eq!(first["system"], "Be brief.");
    assert_eq!(first["max_tokens"], 100);
    assert_eq!(first["tools"][0]["name"], "add");
    assert_eq!(first["tools"][0]["description"], "Adds two integers.");
    assert_eq!(first["tools"][0]["input_schema"]["type"], "object");
    assert_eq!(first["schema"]["type"], "object");
    let t = first["timeout_ms"].as_i64().unwrap();
    assert!(t > 0 && t <= 600000, "{t}");
    assert_eq!(
        first["messages"],
        serde_json::json!([{"role": "user", "content": "plan"}])
    );
    let second = &reqs[1]["messages"];
    let tool_use: serde_json::Value = serde_json::from_str(TOOL_USE_REPLY).unwrap();
    assert_eq!(
        second[1],
        serde_json::json!({"role": "assistant", "content": tool_use["assistant"]})
    );
    assert_eq!(
        second[2],
        serde_json::json!({"role": "tool", "id": "t1", "content": "5", "is_error": false})
    );
}

#[test]
fn provider_model_falls_back_to_the_environment_outside_agents() {
    // 根のコンテキスト: ツールは空、T = str ならスキーマは null、model は WYS_LLM_MODEL（ADR-0023 論点 3）
    let src = r#"async def main() -> None uses io, llm:
    req = LlmRequest(model="", system="", messages=[Message(role="user", content="hi")], max_tokens=0)
    match await llm_generate[str](req):
        case Ok(t):
            print(t)
        case Err(e):
            print(e.kind + ": " + e.message)
"#;
    let reply = r#"{"text":"hello","tool_calls":[],"usage":{"input_tokens":1,"output_tokens":1},"stop":"end_turn","assistant":"hello"}"#;
    let (out, reqs) = run_with_provider(
        "provider_root.wys",
        src,
        &[reply],
        &[("WYS_LLM_MODEL", "env-model")],
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "hello\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(reqs[0]["model"], "env-model");
    assert_eq!(reqs[0]["tools"], serde_json::json!([]));
    assert_eq!(reqs[0]["schema"], serde_json::Value::Null);
    assert_eq!(reqs[0]["timeout_ms"], 0);
}

#[test]
fn provider_failures_are_err_provider() {
    // error 行・max_tokens で切れた応答・壊れた応答・子プロセスの終了・起動できないコマンド・未知のプロバイダは
    // すべて Err(kind="provider") で、パニックにしない（ADR-0023 論点 4、ai-sdk.md 5.3）
    let src = r#"async def ask() -> None uses io, llm:
    req = LlmRequest(model="", system="", messages=[Message(role="user", content="hi")], max_tokens=0)
    match await llm_generate[str](req):
        case Ok(t):
            print("ok " + t)
        case Err(e):
            print(e.kind + ": " + e.message)

async def main() -> None uses io, llm:
    await ask()
    await ask()
"#;
    let cases: [(&[&str], &str); 4] = [
        (
            &[r#"{"error":"HTTP 529 overloaded_error: Overloaded"}"#],
            "provider: HTTP 529 overloaded_error: Overloaded\nprovider: the provider exited\n",
        ),
        (
            &[
                r#"{"text":"cut","tool_calls":[],"usage":{"input_tokens":1,"output_tokens":1},"stop":"max_tokens","assistant":"cut"}"#,
            ],
            "provider: response truncated: max_tokens\nprovider: the provider exited\n",
        ),
        (
            &["not json"],
            "provider: malformed provider response: expected ident at line 1 column 2\nprovider: the provider exited\n",
        ),
        (
            &[],
            "provider: the provider exited\nprovider: the provider exited\n",
        ),
    ];
    for (i, (replies, expected)) in cases.into_iter().enumerate() {
        let (out, _) = run_with_provider(&format!("provider_fail_{i}.wys"), src, replies, &[]);
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            expected,
            "case {i}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(out.status.code(), Some(0));
    }
    let path = scratch("provider_bad_cmd.wys", src);
    for (provider, expected) in [
        (
            "cmd:/nonexistent/provider",
            "provider: cannot start the provider `/nonexistent/provider`",
        ),
        (
            "openai",
            "provider: unknown WYS_LLM_PROVIDER value `openai` (use anthropic, cmd:<path> or replay:<path>)",
        ),
        // 実行できないファイル（起動の前に確かめる。エミュレーションの下でも同じ結果になる）
        (
            "cmd:/etc/passwd",
            "provider: cannot start the provider `/etc/passwd`",
        ),
    ] {
        let out = cli_env(
            &["run", path.to_str().unwrap()],
            &[("WYS_LLM_PROVIDER", provider)],
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.starts_with(expected), "{provider}: {stdout}");
        assert_eq!(out.status.code(), Some(0));
    }
}

#[test]
fn anthropic_provider_starts_the_proxy_named_by_wys_llm_proxy() {
    // WYS_LLM_PROVIDER=anthropic は `<wys> llm-proxy` を起動する。探索の先頭は WYS_LLM_PROXY（ADR-0023 論点 4）
    let (out, reqs) = run_with_provider(
        "provider_anthropic.wys",
        PLANNER,
        &[TOOL_USE_REPLY, PLAN_REPLY],
        &[
            ("WYS_LLM_PROVIDER", "anthropic"),
            ("WYS_LLM_PROXY", FAKE_PROVIDER),
        ],
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).starts_with("plan\n5\n"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(reqs.len(), 2);
}

#[test]
fn llm_proxy_requires_the_api_key() {
    // API キーを読むのは proxy だけ（ADR-0023 論点 6）。なければ起動時に失敗し、キーの値は出さない
    let out = wys()
        .arg("llm-proxy")
        .env_remove("ANTHROPIC_API_KEY")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("ANTHROPIC_API_KEY is not set"));
}

#[test]
fn a_provider_that_stops_reading_does_not_kill_the_program() {
    // 要求を読まずに stdin を閉じるプロバイダに書いても、SIGPIPE で落ちずに Err(provider)（Linux の CI で見つけた）
    let dir = scratch("closing_provider.wys", "")
        .parent()
        .unwrap()
        .to_path_buf();
    let script = dir.join("closing_provider.sh");
    // 1 回目の要求に答えてから stdin を閉じる（2 回目の要求は必ず閉じたパイプへの書き込みになる）
    std::fs::write(
        &script,
        "#!/bin/sh\nread -r line\nexec 0<&-\nprintf '%s\\n' '{\"text\":\"one\",\"tool_calls\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1},\"stop\":\"end_turn\",\"assistant\":\"one\"}'\nsleep 1\n",
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let src = r#"async def ask() -> None uses io, llm:
    req = LlmRequest(model="", system="", messages=[Message(role="user", content="hi")], max_tokens=0)
    match await llm_generate[str](req):
        case Ok(t):
            print("ok " + t)
        case Err(e):
            print(e.kind + ": " + e.message)

async def main() -> None uses io, llm:
    await ask()
    await ask()
    print("still running")
"#;
    let path = scratch("closing_provider.wys", src);
    let provider = format!("cmd:{}", script.display());
    let out = cli_env(
        &["run", path.to_str().unwrap()],
        &[("WYS_LLM_PROVIDER", provider.as_str())],
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "ok one\nprovider: the provider exited\nstill running\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn provider_replies_are_read_leniently_where_the_protocol_allows() {
    // `"error": null` は失敗ではない、`assistant` がなければ `text` を次の要求に戻す（Proxy に null を渡さない）、
    // `error` 行の `usage` は数える（拒否でも入力は課金される）。Fable のレビュー
    let src = r#"async def ask() -> None uses io, llm:
    req = LlmRequest(model="", system="", messages=[Message(role="user", content="hi")], max_tokens=0)
    match await llm_generate[str](req):
        case Ok(t):
            print("ok " + t)
        case Err(e):
            print(e.kind + ": " + e.message)

@tool(description="Tick.")
def tick() -> None uses pure:
    pass

@agent(tools=[tick], capabilities=["io", "llm"])
async def agent() -> None uses io, llm:
    await ask()

async def main() -> None uses io, llm:
    await agent()
    await ask()
    for ev in audit_events():
        if ev.kind == "llm_called":
            print(ev.kind + " " + ev.detail)
"#;
    let replies = [
        r#"{"text":"","tool_calls":[{"id":"t1","name":"tick","args":{}}],"usage":{"input_tokens":1,"output_tokens":1},"stop":"tool_use"}"#,
        r#"{"text":"fine","tool_calls":[],"usage":{"input_tokens":1,"output_tokens":1},"stop":"end_turn","error":null}"#,
        r#"{"error":"the model declined the request (refusal)","usage":{"input_tokens":7,"output_tokens":0}}"#,
    ];
    let (out, reqs) = run_with_provider("provider_lenient.wys", src, &replies, &[]);
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "ok fine\nprovider: the model declined the request (refusal)\nllm_called tokens=2\nllm_called tokens=2\nllm_called tokens=7\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        reqs[1]["messages"][1],
        serde_json::json!({"role": "assistant", "content": ""})
    );
}

#[test]
fn a_provider_that_ignores_end_of_input_does_not_hold_the_exit() {
    // 終了時に stdin を閉じても終わらない子は、猶予の後に止める（Fable のレビュー）
    let dir = scratch("lingering_provider.wys", "")
        .parent()
        .unwrap()
        .to_path_buf();
    let script = dir.join("lingering_provider.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nread -r line\nprintf '%s\\n' '{\"text\":\"one\",\"tool_calls\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1},\"stop\":\"end_turn\",\"assistant\":\"one\"}'\nexec sleep 600\n",
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let src = r#"async def main() -> None uses io, llm:
    req = LlmRequest(model="", system="", messages=[Message(role="user", content="hi")], max_tokens=0)
    match await llm_generate[str](req):
        case Ok(t):
            print("ok " + t)
        case Err(e):
            print(e.kind + ": " + e.message)
"#;
    let path = scratch("lingering_provider.wys", src);
    let exe = path.with_extension("bin");
    let built = cli(&["build", path.to_str().unwrap(), "-o", exe.to_str().unwrap()]);
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let started = std::time::Instant::now();
    let out = Command::new(&exe)
        .env("WYS_LLM_PROVIDER", format!("cmd:{}", script.display()))
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "ok one\n");
    // 新しい実行ファイルの初回の起動は、負荷が高いと数秒かかる（macOS）。子の 600 秒を待たなければよい
    assert!(
        started.elapsed() < std::time::Duration::from_secs(60),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn anthropic_provider_finds_the_compiler_that_built_the_program() {
    // WYS_LLM_PROXY がなければ、実行ファイルに埋めたコンパイラ（このテストの wys）の `llm-proxy` を起動する
    // （ADR-0023 論点 4）。API キーを外すので proxy は起動してすぐ終わり、ネットワークには出ない
    let src = r#"async def main() -> None uses io, llm:
    req = LlmRequest(model="", system="", messages=[Message(role="user", content="hi")], max_tokens=0)
    match await llm_generate[str](req):
        case Ok(t):
            print("ok " + t)
        case Err(e):
            print(e.kind + ": " + e.message)
"#;
    let path = scratch("provider_embedded_compiler.wys", src);
    let out = wys()
        .args(["run", path.to_str().unwrap()])
        .env("WYS_RUNTIME_LIB", runtime_lib())
        .env("WYS_RUNTIME_PYTHON_LIB", python_runtime_lib())
        .env("WYS_LLM_PROVIDER", "anthropic")
        .env_remove("ANTHROPIC_API_KEY")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "provider: the provider exited\n"
    );
    // proxy の stderr は継承される: 起動したのが本物の `wys llm-proxy` だと分かる
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("ANTHROPIC_API_KEY is not set"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn malformed_provider_replies_count_against_the_call_budget() {
    // JSON でない応答も、届いた応答として max_llm_calls に数える（壊れた応答を返し続けるプロバイダで予算を素通りしない。
    // Codex のレビュー）
    let src = r#"@agent(tools=[], capabilities=["io", "llm"], max_llm_calls=1)
async def ask() -> None uses io, llm:
    req = LlmRequest(model="", system="", messages=[Message(role="user", content="hi")], max_tokens=0)
    for _i in range(2):
        match await llm_generate[str](req):
            case Ok(t):
                print("ok " + t)
            case Err(e):
                print(e.kind + ": " + e.message)

async def main() -> None uses io, llm:
    await ask()
    for ev in audit_events():
        if ev.kind == "llm_called" or ev.kind == "budget_exceeded":
            print(ev.kind + " " + ev.detail)
"#;
    let (out, reqs) = run_with_provider(
        "provider_malformed_budget.wys",
        src,
        &["not json", "not json"],
        &[],
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "provider: malformed provider response: expected ident at line 1 column 2\nbudget: max_llm_calls exceeded\nllm_called tokens=0\nbudget_exceeded max_llm_calls\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(reqs.len(), 1);
}

#[test]
fn agent_demo_runs_with_the_mock_provider() {
    let demo = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/agent_demo.wys");
    let trail =
        "0 llm_called\n1 tool_called char_count\n2 llm_called\n3 approval_requested save_note\n";
    let out = cli_env(&["run", demo], &[("WYS_APPROVAL", "deny")]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(
        stdout,
        format!("notes: 25 chars, saved=false\n{trail}4 approval_denied save_note\n5 llm_called\n")
    );
    let out = cli_env(&["run", demo], &[("WYS_APPROVAL", "allow")]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(
        stdout,
        format!(
            "saved notes\nnotes: 25 chars, saved=false\n{trail}4 approval_granted save_note\n5 tool_called save_note\n6 llm_called\n"
        )
    );
    // LLM とツールを使うプログラムも、HTTPS は proxy の側にあるので OS 標準のライブラリだけに依存する（ADR-0023 論点 1）
    let dir = std::env::temp_dir().join(format!("wys-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("agent_demo.bin");
    let out = cli(&["build", demo, "-o", exe.to_str().unwrap()]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_only_system_libraries(&exe);
}

const TRACED: &str = "@tool(description=\"Add.\")\ndef add(a: int, b: int) -> int uses pure:\n    return a + b\n\n@tool(description=\"Remove.\", approval=\"required\")\ndef rm(path: str) -> None uses io:\n    print(path)\n\n@agent(tools=[add, rm])\nasync def calc() -> Result[str, LlmError] uses io, llm:\n    req = LlmRequest(model=\"\", system=\"sys\", messages=[Message(role=\"user\", content=\"go\")], max_tokens=0)\n    return await llm_generate[str](req)\n\nasync def main() -> None uses io, llm:\n    llm_mock_push(MockResponse(text=\"\", tool_calls=[MockToolCall(name=\"add\", args_json=\"{\\\"a\\\": 2, \\\"b\\\": 3}\"), MockToolCall(name=\"rm\", args_json=\"{\\\"path\\\": \\\"/x\\\"}\")], input_tokens=3, output_tokens=1, latency_ms=40))\n    llm_mock_push(MockResponse(text=\"5\", tool_calls=[], input_tokens=5, output_tokens=2, latency_ms=10))\n    match await calc():\n        case Ok(t):\n            print(t)\n        case Err(e):\n            print(e.kind)\n";

#[test]
fn traces_are_deterministic_under_the_virtual_clock() {
    // スパンは終わったときに 1 行、監査イベントも同じ流れに写す。本文は既定では含めない（ADR-0025 論点 1〜4）
    let path = scratch("traced", TRACED);
    let trace = path.with_extension("trace.jsonl");
    let _ = std::fs::remove_file(&trace);
    let out = cli_env(
        &["run", path.to_str().unwrap()],
        &[
            ("WYS_VIRTUAL_TIME", "1"),
            ("WYS_TRACE", trace.to_str().unwrap()),
        ],
    );
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "5\n");
    let lines: Vec<serde_json::Value> = std::fs::read_to_string(&trace)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let brief: Vec<String> = lines
        .iter()
        .map(|v| match v["kind"].as_str().unwrap() {
            "span" => format!(
                "span {} {} parent={} {}..{} {}",
                v["span"],
                v["name"].as_str().unwrap(),
                v["parent"],
                v["start_ms"],
                v["end_ms"],
                v["result"].as_str().unwrap_or("-")
            ),
            _ => format!(
                "event {} span={} t={}",
                v["event"].as_str().unwrap(),
                v["span"],
                v["t_ms"]
            ),
        })
        .collect();
    assert_eq!(
        brief,
        vec![
            "event llm_called span=2 t=40",
            "span 2 llm_call parent=1 0..40 ok",
            "event tool_called span=3 t=40",
            "span 3 tool_call parent=1 40..40 ok",
            "event approval_requested span=4 t=40",
            "event approval_denied span=4 t=40",
            "span 4 tool_call parent=1 40..40 refused",
            "event llm_called span=5 t=50",
            "span 5 llm_call parent=1 40..50 ok",
            "span 1 agent parent=null 0..50 -",
        ]
    );
    assert_eq!(
        lines[1]["usage"],
        serde_json::json!({"input_tokens": 3, "output_tokens": 1})
    );
    assert_eq!(
        lines[9]["usage"],
        serde_json::json!({"input_tokens": 8, "output_tokens": 3})
    );
    let text = std::fs::read_to_string(&trace).unwrap();
    assert!(
        !text.contains("\"request\"") && !text.contains("/x"),
        "{text}"
    );
    // WYS_TRACE_CONTENT=1 なら本文（要求・応答・ツールの引数と戻り値）を足す
    let _ = std::fs::remove_file(&trace);
    cli_env(
        &["run", path.to_str().unwrap()],
        &[
            ("WYS_VIRTUAL_TIME", "1"),
            ("WYS_TRACE", trace.to_str().unwrap()),
            ("WYS_TRACE_CONTENT", "1"),
        ],
    );
    let lines: Vec<serde_json::Value> = std::fs::read_to_string(&trace)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines[1]["request"]["system"], "sys");
    assert_eq!(lines[1]["reply"]["tool_calls"][0]["name"], "add");
    assert_eq!(lines[3]["args"], "{\"a\": 2, \"b\": 3}");
    assert_eq!(lines[3]["output"], "5");
}

const NESTED_IN_TOOL: &str = "@agent(capabilities=[\"io\"])\nasync def inner() -> Result[str, LlmError] uses io, llm:\n    req = LlmRequest(model=\"\", system=\"\", messages=[Message(role=\"user\", content=\"x\")], max_tokens=0)\n    return await llm_generate[str](req)\n\n@tool(description=\"Delegate.\")\nasync def delegate() -> str uses io, llm:\n    match await inner():\n        case Ok(t):\n            return t\n        case Err(e):\n            return e.kind\n\n@agent(tools=[delegate])\nasync def outer() -> Result[str, LlmError] uses io, llm:\n    req = LlmRequest(model=\"\", system=\"\", messages=[Message(role=\"user\", content=\"go\")], max_tokens=0)\n    return await llm_generate[str](req)\n\nasync def main() -> None uses io, llm:\n    llm_mock_push(MockResponse(text=\"\", tool_calls=[MockToolCall(name=\"delegate\", args_json=\"{}\")], input_tokens=1, output_tokens=1, latency_ms=0))\n    llm_mock_push(MockResponse(text=\"done\", tool_calls=[], input_tokens=1, output_tokens=1, latency_ms=0))\n    match await outer():\n        case Ok(t):\n            print(t)\n        case Err(e):\n            print(e.kind)\n";

#[test]
fn events_of_an_agent_inside_a_tool_belong_to_that_agent() {
    // ツールの中で入った agent のイベントは、外側の `tool_call` ではなくその agent のスパンに付く（Fable のレビュー）
    let path = scratch("nested_in_tool", NESTED_IN_TOOL);
    let trace = path.with_extension("trace.jsonl");
    let _ = std::fs::remove_file(&trace);
    let out = cli_env(
        &["run", path.to_str().unwrap()],
        &[
            ("WYS_VIRTUAL_TIME", "1"),
            ("WYS_TRACE", trace.to_str().unwrap()),
        ],
    );
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "done\n");
    let lines: Vec<serde_json::Value> = std::fs::read_to_string(&trace)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let denied = lines
        .iter()
        .find(|v| v["event"] == "capability_denied")
        .unwrap();
    let inner = lines
        .iter()
        .find(|v| v["name"] == "agent" && v["agent"] == "inner")
        .unwrap();
    let tool = lines.iter().find(|v| v["name"] == "tool_call").unwrap();
    assert_eq!(denied["span"], inner["span"]);
    assert_eq!(inner["parent"], tool["parent"]);
}

#[test]
fn effects_of_unannotated_functions_are_inferred() {
    // 注釈のない関数を経由した Effect も、注釈のある呼び出し元で E1102 になる（ADR-0024）
    let bad = scratch(
        "inferred_bad",
        "def note(x: int) -> None:\n    print(x)\n\n@tool(description=\"Double.\")\ndef double(x: int) -> int uses pure:\n    note(x)\n    return x * 2\n\ndef main() -> None:\n    print(double(1))\n",
    );
    let out = cli(&["check", "--format", "json", bad.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(1));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let d = &v["diagnostics"][0];
    assert_eq!(d["code"], "E1102");
    assert_eq!(d["related"][0]["name"], "note");
    assert_eq!(d["related"][1]["name"], "print");
    assert_eq!(d["hints"][0]["replacement"]["text"], "io");
    // 推論した集合が宣言に収まれば通り、IR と `wys explain` に推論した Effect が出る
    let ok = scratch(
        "inferred_ok",
        "def note(x: int) -> None:\n    print(x)\n\ndef log(x: int) -> None uses io:\n    note(x)\n\ndef main() -> None:\n    log(1)\n",
    );
    let out = cli(&["run", ok.to_str().unwrap()]);
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "1\n");
    let out = cli(&["dump-ir", ok.to_str().unwrap()]);
    let ir = String::from_utf8(out.stdout).unwrap();
    assert!(ir.contains("note(l0: int) uses(inferred) io {"), "{ir}");
    assert!(ir.contains("log(l0: int) uses io {"), "{ir}");
    let out = cli(&["explain", ok.to_str().unwrap()]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["functions"][0]["effects"]["inferred"][0], "io");
    assert!(v["functions"][1]["effects"]["inferred"].is_null());
    assert_eq!(v["functions"][2]["effects"]["inferred"][0], "io");
}

#[test]
fn unused_effects_are_warned_with_one_fix() {
    // 宣言したが使われない Effect は W0203。修正案を適用すると警告が消える（ADR-0024 論点 7）
    let src = "def report(n: int) -> None uses python, io, exit:\n    print(n)\n\ndef main() -> None:\n    report(1)\n";
    let path = scratch("unused_effects", src);
    let out = cli(&["check", "--format", "json", path.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let ds = v["diagnostics"].as_array().unwrap();
    assert_eq!(ds.len(), 2);
    assert!(ds.iter().all(|d| d["code"] == "W0203"));
    let fix = &ds[0]["hints"][0]["replacement"];
    assert_eq!(fix, &ds[1]["hints"][0]["replacement"]);
    assert_eq!(fix["text"], "io");
    let (start, end) = (
        fix["span"]["start"]["byte"].as_u64().unwrap() as usize,
        fix["span"]["end"]["byte"].as_u64().unwrap() as usize,
    );
    let fixed = format!("{}{}{}", &src[..start], "io", &src[end..]);
    assert!(
        fixed.starts_with("def report(n: int) -> None uses io:\n"),
        "{fixed}"
    );
    let path = scratch("unused_effects_fixed", &fixed);
    let out = cli(&["check", "--format", "json", path.to_str().unwrap()]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["diagnostics"], serde_json::json!([]));
}

/// 記録の各行（ヘッダを含む）
fn recording_lines(path: &std::path::Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn record_then_replay_reproduces_the_run() {
    // AC-009: テスト用のプロバイダで記録し、プロバイダなしで再生すると、同じ出力と監査になる（ADR-0025 論点 6・7）
    let rec = scratch("replay_planner", "").with_extension("rec.jsonl");
    let rec_s = rec.to_str().unwrap();
    let (first, _) = run_with_provider(
        "replay_planner",
        PLANNER,
        &[TOOL_USE_REPLY, PLAN_REPLY],
        &[("WYS_LLM_RECORD", rec_s)],
    );
    let first_out = String::from_utf8(first.stdout).unwrap();
    assert!(first_out.starts_with("plan\n5\n"), "{first_out}");
    let lines = recording_lines(&rec);
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0]["kind"], "header");
    assert_eq!(lines[0]["provider"], "cmd");
    assert_eq!(lines[1]["request"]["system"], "Be brief.");
    assert_eq!(lines[2]["reply"]["stop"], "end_turn");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&rec).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "the recording holds prompts and replies"
        );
    }
    let path = scratch("replay_planner_again", PLANNER);
    let provider = format!("replay:{rec_s}");
    let again = cli_env(
        &["run", path.to_str().unwrap()],
        &[("WYS_LLM_PROVIDER", provider.as_str())],
    );
    assert_eq!(String::from_utf8(again.stdout).unwrap(), first_out);
    // プログラムが変わって要求が記録と違えば Err(replay)。回数は数えない
    let changed = scratch(
        "replay_planner_changed",
        &PLANNER.replace("Be brief.", "Be long."),
    );
    let out = cli_env(
        &["run", changed.to_str().unwrap()],
        &[("WYS_LLM_PROVIDER", provider.as_str())],
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(stdout, "replay: request 1: system differs\n", "{stdout}");
    // 記録が尽きた
    let short = rec.with_extension("short.jsonl");
    let text = std::fs::read_to_string(&rec).unwrap();
    let head: Vec<&str> = text.lines().take(2).collect();
    std::fs::write(&short, head.join("\n") + "\n").unwrap();
    let provider = format!("replay:{}", short.display());
    let out = cli_env(
        &["run", path.to_str().unwrap()],
        &[("WYS_LLM_PROVIDER", provider.as_str())],
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.starts_with("replay: recording exhausted after 1 exchanges\n"),
        "{stdout}"
    );
    // 記録と再生に同じファイルを指すと閉じる
    let provider = format!("replay:{rec_s}");
    let out = cli_env(
        &["run", path.to_str().unwrap()],
        &[
            ("WYS_LLM_PROVIDER", provider.as_str()),
            ("WYS_LLM_RECORD", rec_s),
        ],
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.starts_with("provider: the recording path is also the replay source\n"),
        "{stdout}"
    );
    assert_eq!(
        recording_lines(&rec).len(),
        3,
        "the source is not truncated"
    );
}

#[test]
fn recordings_keep_failed_replies_and_mock_runs() {
    // JSON でない行と、届かなかった応答も記録し、再生で同じ失敗になる（ADR-0025 論点 6）
    let rec = scratch("replay_failures", "").with_extension("rec.jsonl");
    let rec_s = rec.to_str().unwrap();
    let (first, _) = run_with_provider(
        "replay_failures",
        PLANNER,
        &["this is not json"],
        &[("WYS_LLM_RECORD", rec_s)],
    );
    let first_out = String::from_utf8(first.stdout).unwrap();
    assert!(
        first_out.starts_with("provider: malformed provider response"),
        "{first_out}"
    );
    let lines = recording_lines(&rec);
    assert_eq!(lines[1]["malformed"], "this is not json");
    let path = scratch("replay_failures_again", PLANNER);
    let provider = format!("replay:{rec_s}");
    let again = cli_env(
        &["run", path.to_str().unwrap()],
        &[("WYS_LLM_PROVIDER", provider.as_str())],
    );
    assert_eq!(String::from_utf8(again.stdout).unwrap(), first_out);
    // モックの実行も記録でき、LLM を呼ばなければヘッダだけの記録になる
    let mocked = scratch(
        "record_mock",
        "async def main() -> None uses io, llm:\n    llm_mock_push(MockResponse(text=\"hi\", tool_calls=[], input_tokens=1, output_tokens=1, latency_ms=7))\n    req = LlmRequest(model=\"\", system=\"\", messages=[Message(role=\"user\", content=\"x\")], max_tokens=0)\n    match await llm_generate[str](req):\n        case Ok(t):\n            print(t)\n        case Err(e):\n            print(e.kind)\n",
    );
    let out = cli_env(
        &["run", mocked.to_str().unwrap()],
        &[("WYS_LLM_RECORD", rec_s)],
    );
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "hi\n");
    let lines = recording_lines(&rec);
    assert_eq!(lines[0]["provider"], "mock");
    assert_eq!(lines[1]["reply"]["text"], "hi");
    assert_eq!(lines[1]["latency_ms"], 7);
    let none = scratch("record_none", "def main() -> None:\n    print(1)\n");
    cli_env(
        &["run", none.to_str().unwrap()],
        &[("WYS_LLM_RECORD", rec_s)],
    );
    let lines = recording_lines(&rec);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["kind"], "header");
}

/// `body` を本体にした `cmd:` のプロバイダを作る（シェルのスクリプト）
fn provider_script(name: &str, body: &str) -> std::path::PathBuf {
    let path = scratch(name, "").with_extension("sh");
    std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    path
}

/// `src` を `provider` で記録して実行し、同じプログラムを記録から再生する。2 つの標準出力
fn record_and_replay(
    name: &str,
    src: &str,
    provider: &str,
    envs: &[(&str, &str)],
) -> (String, String) {
    let path = scratch(name, src);
    let rec = path.with_extension("rec.jsonl");
    let rec_s = rec.to_str().unwrap();
    let mut first_env = vec![("WYS_LLM_PROVIDER", provider), ("WYS_LLM_RECORD", rec_s)];
    first_env.extend_from_slice(envs);
    let first = cli_env(&["run", path.to_str().unwrap()], &first_env);
    let replay = format!("replay:{rec_s}");
    let mut second_env = vec![("WYS_LLM_PROVIDER", replay.as_str())];
    second_env.extend_from_slice(envs);
    let second = cli_env(&["run", path.to_str().unwrap()], &second_env);
    (
        String::from_utf8(first.stdout).unwrap(),
        String::from_utf8(second.stdout).unwrap(),
    )
}

const TWO_CALLS: &str = "@agent(capabilities=[\"io\", \"llm\"], timeout_ms=150)\nasync def ask() -> None uses io, llm:\n    req = LlmRequest(model=\"\", system=\"\", messages=[Message(role=\"user\", content=\"x\")], max_tokens=0)\n    for i in range(2):\n        match await llm_generate[str](req):\n            case Ok(t):\n                print(\"ok \" + t)\n            case Err(e):\n                print(e.kind + \": \" + e.message)\n\nasync def main() -> None uses io, llm:\n    await ask()\n    for ev in audit_events():\n        print(ev.kind + \" \" + ev.detail)\n";

#[test]
fn replays_match_the_recorded_run_for_every_kind_of_reply() {
    // 仮想時計のもとでも、実際に待った時間を記録と同じく数える（Fable のレビュー）。1 回目は 0.2 秒待ってから
    // 失敗の行を返すので、2 回目は期限（150 ms）を過ぎる。再生も同じ結果になる
    let slow = provider_script(
        "slow_error_provider",
        "read l; sleep 0.2; echo '{\"error\":\"busy\",\"usage\":{\"input_tokens\":2}}'\nread l; echo '{\"text\":\"late\"}'\n",
    );
    let provider = format!("cmd:{}", slow.display());
    for envs in [&[][..], &[("WYS_VIRTUAL_TIME", "1")][..]] {
        let (first, second) = record_and_replay("replay_slow", TWO_CALLS, &provider, envs);
        assert!(
            first.starts_with("provider: busy\ntimeout: timeout_ms exceeded\n"),
            "{envs:?}: {first}"
        );
        assert_eq!(second, first, "{envs:?}");
    }
    // JSON でない行、`null` の行、届かなかった応答（期限なし。遅い環境で期限に当たらないように）
    let no_deadline = TWO_CALLS.replace(", timeout_ms=150", "");
    let odd = provider_script(
        "odd_provider",
        "read l; echo 'not json'\nread l; echo 'null'\n",
    );
    let provider = format!("cmd:{}", odd.display());
    let (first, second) = record_and_replay("replay_odd", &no_deadline, &provider, &[]);
    assert!(
        first.starts_with("provider: malformed provider response: expected"),
        "{first}"
    );
    assert!(
        first.contains("\nprovider: malformed provider response: `text` is missing\n"),
        "{first}"
    );
    assert_eq!(second, first);
    let gone = provider_script("gone_provider", "exit 0\n");
    let provider = format!("cmd:{}", gone.display());
    let (first, second) = record_and_replay("replay_gone", &no_deadline, &provider, &[]);
    assert!(
        first.starts_with("provider: the provider exited\n"),
        "{first}"
    );
    assert_eq!(second, first);
}

#[test]
fn recordings_are_recreated_at_start_with_owner_only_access() {
    // 既にあるファイル（0644）も 0600 にし、LLM を呼ぶ前に落ちたプログラムでも古い記録を残さない（Fable のレビュー）
    let rec = scratch("stale_recording", "").with_extension("rec.jsonl");
    std::fs::write(
        &rec,
        "{\"kind\":\"header\",\"v\":1}\n{\"kind\":\"exchange\"}\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&rec, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    let crash = scratch(
        "crash_before_llm",
        "def main() -> None:\n    panic(\"stop\")\n",
    );
    let out = cli_env(
        &["run", crash.to_str().unwrap()],
        &[("WYS_LLM_RECORD", rec.to_str().unwrap())],
    );
    assert_eq!(out.status.code(), Some(101));
    let lines = recording_lines(&rec);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["kind"], "header");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&rec).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // 読めない記録
    let path = scratch("replay_unreadable", TWO_CALLS);
    for (content, expected) in [
        (None, "replay: cannot read the recording "),
        (
            Some("{\"kind\":\"header\",\"v\":2}\n"),
            "replay: cannot read the recording ",
        ),
        (
            Some("{\"kind\":\"header\",\"v\":1}\n{\"kind\":\"note\"}\n"),
            "replay: cannot read the recording ",
        ),
    ] {
        let bad = path.with_extension("bad.jsonl");
        let _ = std::fs::remove_file(&bad);
        if let Some(c) = content {
            std::fs::write(&bad, c).unwrap();
        }
        let provider = format!("replay:{}", bad.display());
        let out = cli_env(
            &["run", path.to_str().unwrap()],
            &[("WYS_LLM_PROVIDER", provider.as_str())],
        );
        let stdout = String::from_utf8(out.stdout).unwrap();
        assert!(stdout.starts_with(expected), "{content:?}: {stdout}");
        assert!(
            stdout.lines().nth(1).unwrap().starts_with(expected),
            "sticky: {stdout}"
        );
    }
}
