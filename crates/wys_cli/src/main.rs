//! コマンドラインインターフェース（build/run/check）
//!
//! 実行ファイル名は M1 で決める（現在は `wys_cli`）。
//!
//! 終了コード: 0 = 成功、1 = ソースに診断エラーがある、2 = 使い方・入出力・ツールのエラー。
//! `run` はプログラム自身の終了コードを返す。

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use wys_driver::{BuildError, BuildOptions, Frontend};

const USAGE: &str = "\
usage:
  wys_cli check [--format text|json] <file>   check a file and report diagnostics
  wys_cli build <file> [-o <output>]          build a native executable
  wys_cli run <file>                          build and run
  wys_cli dump-ir <file>                      print the wys IR
  wys_cli dump-llvm <file>                    print the generated LLVM IR
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
    let mut output: Option<PathBuf> = None;
    let mut file: Option<PathBuf> = None;
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--format" => {
                format = match it.next().map(String::as_str) {
                    Some("json") => Format::Json,
                    Some("text") => Format::Text,
                    other => return Err(format!("unknown format {other:?}; use `text` or `json`")),
                }
            }
            "-o" => output = Some(PathBuf::from(it.next().ok_or("`-o` needs a path")?)),
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
        "dump-ir" | "dump-llvm" => {
            let fe = load(file.as_deref())?;
            let Some(p) = &fe.program else {
                report(&fe, format);
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
            match wys_driver::build_file(&src, &out, &opts) {
                Ok(()) => Ok(ExitCode::SUCCESS),
                Err(e) => build_error(e, format),
            }
        }
        "run" => {
            let src = file.ok_or("missing source file")?;
            let fe = load(Some(&src))?;
            let Some(p) = &fe.program else {
                report(&fe, format);
                return Ok(ExitCode::from(1));
            };
            let exe = std::env::temp_dir().join(format!("wys-run-{}", std::process::id()));
            if let Err(e) = wys_driver::link_program(p, &exe, &opts) {
                return build_error(e, format);
            }
            let status = std::process::Command::new(&exe)
                .status()
                .map_err(|e| format!("cannot run the program: {e}"));
            let _ = std::fs::remove_file(&exe);
            let code = status?.code().unwrap_or(1);
            Ok(ExitCode::from(code.clamp(0, 255) as u8))
        }
        other => Err(format!("unknown command `{other}`\n{USAGE}")),
    }
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
