//! コマンドラインインターフェース（build/run/check）
//!
//! 実行ファイル名は `wys`（内部コードネームと同じ。正式名称が決まったら変える）。
//!
//! 終了コード: 0 = 成功、1 = ソースに診断エラーがある、2 = 使い方・入出力・ツールのエラー。
//! `run` はプログラム自身の終了コードを返す。

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use wys_driver::{BuildError, BuildOptions, Frontend};

const USAGE: &str = "\
usage:
  wys check [--format text|json] <file>   check a file and report diagnostics
  wys build <file> [-o <output>]          build a native executable
  wys run <file> [-- <args>...]           build and run (arguments after -- go to the program)
  wys test <file>                         run the functions named test_* (no main needed)
  wys fmt [--check] <file>                format a file in place (--check: only report)
  wys dump-ir <file>                      print the wys IR
  wys dump-llvm <file>                    print the generated LLVM IR
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run_cli(&args) {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("error: {msg}");
            ExitCode::from(2)
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Text,
    Json,
}

fn run_cli(args: &[String]) -> Result<ExitCode, String> {
    let Some((cmd, rest)) = args.split_first() else {
        eprint!("{USAGE}");
        return Ok(ExitCode::from(2));
    };
    let mut format = Format::Text;
    let mut check_only = false;
    let mut output: Option<PathBuf> = None;
    let mut file: Option<PathBuf> = None;
    let mut program_args: Vec<String> = Vec::new();
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--" => {
                program_args.extend(it.by_ref().cloned());
                break;
            }
            "--format" => {
                format = match it.next().map(String::as_str) {
                    Some("json") => Format::Json,
                    Some("text") => Format::Text,
                    other => return Err(format!("unknown format {other:?}; use `text` or `json`")),
                }
            }
            "-o" => output = Some(PathBuf::from(it.next().ok_or("`-o` needs a path")?)),
            "--check" => check_only = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(ExitCode::SUCCESS);
            }
            s if s.starts_with('-') => return Err(format!("unknown option `{s}`\n{USAGE}")),
            s => {
                if file.replace(PathBuf::from(s)).is_some() {
                    return Err("only one source file is supported".into());
                }
            }
        }
    }
    let opts = BuildOptions::default();
    match cmd.as_str() {
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        "check" => {
            let fe = load(file.as_deref())?;
            report(&fe, format);
            Ok(if fe.has_errors() {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
        "fmt" => {
            let src = file.ok_or("missing source file")?;
            let text = std::fs::read_to_string(&src)
                .map_err(|e| format!("cannot read {}: {e}", src.display()))?;
            let formatted = match wys_fmt::format_source(&text) {
                Ok(f) => f,
                Err(diags) => {
                    let map = wys_driver::SourceMap::single(wys_driver::SourceFile::new(
                        src.display().to_string(),
                        text,
                    ));
                    eprint!("{}", wys_driver::to_text(&map, &diags));
                    return Ok(ExitCode::from(1));
                }
            };
            if formatted == text {
                return Ok(ExitCode::SUCCESS);
            }
            if check_only {
                println!("would reformat {}", src.display());
                return Ok(ExitCode::from(1));
            }
            std::fs::write(&src, formatted)
                .map_err(|e| format!("cannot write {}: {e}", src.display()))?;
            println!("reformatted {}", src.display());
            Ok(ExitCode::SUCCESS)
        }
        "test" => {
            let src = file.ok_or("missing source file")?;
            let text = std::fs::read_to_string(&src)
                .map_err(|e| format!("cannot read {}: {e}", src.display()))?;
            let (fe, tests) = wys_driver::frontend_tests(&src.display().to_string(), &text);
            if tests.is_empty() && !fe.has_errors() {
                println!("no tests found (define functions named `test_*`)");
                return Ok(ExitCode::SUCCESS);
            }
            let Some(p) = buildable(&fe, format)? else {
                return Ok(ExitCode::from(1));
            };
            let exe = std::env::temp_dir().join(format!("wys-test-{}", std::process::id()));
            if let Err(e) = wys_driver::link_program(p, &exe, &opts) {
                return build_error(e, format);
            }
            let result = run_tests(&exe, &tests);
            let _ = std::fs::remove_file(&exe);
            result
        }
        "dump-ir" | "dump-llvm" => {
            let fe = load(file.as_deref())?;
            let Some(p) = buildable(&fe, format)? else {
                return Ok(ExitCode::from(1));
            };
            if cmd == "dump-ir" {
                print!("{p}");
            } else {
                print!("{}", wys_backend::emit_llvm_ir(p));
            }
            Ok(ExitCode::SUCCESS)
        }
        "build" => {
            let src = file.ok_or("missing source file")?;
            let out = output.unwrap_or_else(|| default_output(&src));
            let fe = load(Some(&src))?;
            let Some(p) = buildable(&fe, format)? else {
                return Ok(ExitCode::from(1));
            };
            match wys_driver::link_program(p, &out, &opts) {
                Ok(()) => Ok(ExitCode::SUCCESS),
                Err(e) => build_error(e, format),
            }
        }
        "run" => {
            let src = file.ok_or("missing source file")?;
            let fe = load(Some(&src))?;
            let Some(p) = buildable(&fe, format)? else {
                return Ok(ExitCode::from(1));
            };
            let exe = std::env::temp_dir().join(format!("wys-run-{}", std::process::id()));
            if let Err(e) = wys_driver::link_program(p, &exe, &opts) {
                return build_error(e, format);
            }
            let status = std::process::Command::new(&exe)
                .args(&program_args)
                .status()
                .map_err(|e| format!("cannot run the program: {e}"));
            let _ = std::fs::remove_file(&exe);
            let code = status?.code().unwrap_or(1);
            Ok(ExitCode::from(code.clamp(0, 255) as u8))
        }
        other => Err(format!("unknown command `{other}`\n{USAGE}")),
    }
}

/// テストを1件ずつ別のプロセスで実行する（1件の失敗で、ほかのテストが止まらないように）。
fn run_tests(exe: &Path, tests: &[String]) -> Result<ExitCode, String> {
    let mut failed = Vec::new();
    for (i, name) in tests.iter().enumerate() {
        let out = std::process::Command::new(exe)
            .env("WYS_TEST_INDEX", i.to_string())
            .output()
            .map_err(|e| format!("cannot run the tests: {e}"))?;
        if out.status.success() {
            println!("test {name} ... ok");
        } else {
            println!("test {name} ... FAILED");
            failed.push((name, out));
        }
    }
    for (name, out) in &failed {
        println!("\n---- {name} ----");
        print!("{}", String::from_utf8_lossy(&out.stdout));
        print!("{}", String::from_utf8_lossy(&out.stderr));
    }
    let passed = tests.len() - failed.len();
    let verdict = if failed.is_empty() { "ok" } else { "FAILED" };
    println!(
        "\ntest result: {verdict}. {passed} passed; {} failed",
        failed.len()
    );
    Ok(if failed.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn default_output(src: &Path) -> PathBuf {
    let stem = src
        .file_stem()
        .map(|s| s.to_os_string())
        .unwrap_or_else(|| "a.out".into());
    src.with_file_name(stem)
}

fn load(file: Option<&Path>) -> Result<Frontend, String> {
    let file = file.ok_or("missing source file")?;
    let text = std::fs::read_to_string(file)
        .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
    Ok(wys_driver::frontend(&file.display().to_string(), &text))
}

/// ビルドできるなら IR を返す。エラーがあれば報告して `None`。警告は標準エラーに出してから続ける。
fn buildable(fe: &Frontend, format: Format) -> Result<Option<&wys_driver::Program>, String> {
    if let Some(msg) = &fe.unsupported {
        return Err(msg.clone());
    }
    if fe.has_errors() {
        report(fe, format);
        return Ok(None);
    }
    let warnings = fe.warnings();
    if !warnings.is_empty() {
        eprint!("{}", wys_driver::to_text(&fe.file, &warnings));
    }
    Ok(fe.program.as_ref())
}

fn report(fe: &Frontend, format: Format) {
    match format {
        Format::Json => println!("{}", wys_driver::to_json(&fe.file, &fe.diagnostics)),
        Format::Text => eprint!("{}", wys_driver::to_text(&fe.file, &fe.diagnostics)),
    }
}

fn build_error(e: BuildError, format: Format) -> Result<ExitCode, String> {
    match e {
        BuildError::Diagnostics(fe) => {
            report(&fe, format);
            Ok(ExitCode::from(1))
        }
        BuildError::Io(m) | BuildError::Tool(m) => Err(m),
    }
}
