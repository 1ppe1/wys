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
                "WYS_CAPABILITIES" | "WYS_APPROVAL" | "WYS_AUDIT"
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
        std::time::Duration::from_millis(300),
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

const APPROVAL_THEN_TIMEOUT: &str = "@tool(description=\"Remove.\", approval=\"required\")\ndef rm(path: str) -> None uses io:\n    print(\"removed \" + path)\n\n@agent(tools=[rm], capabilities=[\"io\", \"llm\"], timeout_ms=30)\nasync def cleaner() -> Result[str, LlmError] uses io, llm:\n    req = LlmRequest(model=\"\", system=\"\", messages=[Message(role=\"user\", content=\"go\")], max_tokens=0)\n    return await llm_generate[str](req)\n\nasync def main() -> None uses io, llm:\n    llm_mock_push(MockResponse(text=\"\", tool_calls=[MockToolCall(name=\"rm\", args_json=\"{\\\"path\\\": \\\"/tmp/x\\\"}\")], input_tokens=1, output_tokens=1, latency_ms=0))\n    llm_mock_push(MockResponse(text=\"done\", tool_calls=[], input_tokens=1, output_tokens=1, latency_ms=0))\n    match await cleaner():\n        case Ok(t):\n            print(\"ok \" + t)\n        case Err(e):\n            print(e.kind + \": \" + e.message)\n    for ev in audit_events():\n        print(ev.kind + \" \" + ev.detail)\n";
