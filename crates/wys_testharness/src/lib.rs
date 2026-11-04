//! Conformance テストランナー
//!
//! `tests/conformance/**/*.wys` の各ファイルが1つのテストケース。期待値はファイル内のコメントで宣言する。
//!
//! ```text
//! # expect-error: E0201 5:18        期待する診断（コードと開始位置の 行:列）。複数書ける
//! # expect-stdout: 55               期待する標準出力の1行。複数行は繰り返して書く
//! # expect-exit: 101                期待する終了コード（省略時は 0）
//! # expect-stderr-contains: overflow 標準エラーに含まれるべき文字列
//! ```
//!
//! `expect-error` があるケースは型検査までを行い、診断が過不足なく一致することを確かめる。
//! ないケースはビルドして実行し、標準出力と終了コードを確かめる。

use std::path::{Path, PathBuf};
use std::process::Command;
use wys_driver::BuildOptions;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Expectations {
    pub errors: Vec<(String, u32, u32)>,
    pub stdout: Option<String>,
    pub exit: i32,
    pub stderr_contains: Vec<String>,
}

pub fn parse_expectations(src: &str) -> Result<Expectations, String> {
    let mut e = Expectations::default();
    let mut stdout = String::new();
    let mut has_stdout = false;
    for (i, line) in src.lines().enumerate() {
        let Some(rest) = line.trim_start().strip_prefix("# expect-") else {
            continue;
        };
        let (key, value) = rest
            .split_once(':')
            .ok_or_else(|| format!("line {}: malformed expectation", i + 1))?;
        let value = value.strip_prefix(' ').unwrap_or(value);
        match key {
            "error" => {
                let (code, pos) = value
                    .split_once(' ')
                    .ok_or_else(|| format!("line {}: expected `CODE LINE:COL`", i + 1))?;
                let (l, c) = pos
                    .trim()
                    .split_once(':')
                    .ok_or_else(|| format!("line {}: expected `LINE:COL`", i + 1))?;
                let num = |s: &str| {
                    s.parse::<u32>()
                        .map_err(|_| format!("line {}: bad number `{s}`", i + 1))
                };
                e.errors.push((code.to_string(), num(l)?, num(c)?));
            }
            "stdout" => {
                has_stdout = true;
                stdout.push_str(value);
                stdout.push('\n');
            }
            "exit" => {
                e.exit = value
                    .trim()
                    .parse()
                    .map_err(|_| format!("line {}: bad exit code", i + 1))?;
            }
            "stderr-contains" => e.stderr_contains.push(value.to_string()),
            other => return Err(format!("line {}: unknown expectation `{other}`", i + 1)),
        }
    }
    if has_stdout {
        e.stdout = Some(stdout);
    }
    if !e.errors.is_empty() && (e.stdout.is_some() || e.exit != 0) {
        return Err("a case cannot expect both compile errors and run results".into());
    }
    Ok(e)
}

/// `dir` 以下の `.wys` ファイルを名前順に集める。
pub fn collect_cases(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|e| e == "wys") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// 1ケースを実行する。`display_name` は診断に入るファイル名（スナップショットを安定させるため相対パス）。
pub fn run_case(
    path: &Path,
    display_name: &str,
    opts: &BuildOptions,
    scratch: &Path,
) -> Result<(), String> {
    let src = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let exp = parse_expectations(&src)?;
    let fe = wys_driver::frontend(display_name, &src);
    let actual: Vec<(String, u32, u32)> = fe
        .diagnostics
        .iter()
        .map(|d| {
            let p = fe.file.position(d.span.start);
            (d.code.to_string(), p.line, p.column)
        })
        .collect();
    if actual != exp.errors {
        return Err(format!(
            "diagnostics differ\n  expected: {:?}\n  actual:   {:?}\n{}",
            exp.errors,
            actual,
            wys_driver::to_text(&fe.file, &fe.diagnostics)
        ));
    }
    if let Some(msg) = &fe.unsupported {
        return Err(format!("cannot build: {msg}"));
    }
    let Some(program) = &fe.program else {
        return Ok(());
    };
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("case")
        .to_string();
    let exe = scratch.join(stem);
    wys_driver::link_program(program, &exe, opts).map_err(|e| format!("build failed: {e:?}"))?;
    let out = Command::new(&exe)
        .output()
        .map_err(|e| format!("cannot run: {e}"))?;
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let mut problems = Vec::new();
    if code != exp.exit {
        problems.push(format!("exit code: expected {}, got {code}", exp.exit));
    }
    if let Some(want) = &exp.stdout
        && want != &stdout
    {
        problems.push(format!(
            "stdout differs\n  expected: {want:?}\n  actual:   {stdout:?}"
        ));
    }
    for s in &exp.stderr_contains {
        if !stderr.contains(s.as_str()) {
            problems.push(format!("stderr does not contain {s:?}: {stderr:?}"));
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_expectations() {
        let e = parse_expectations(
            "# expect-stdout: 1\n# expect-stdout:\n# expect-exit: 3\n# expect-stderr-contains: boom\n",
        )
        .unwrap();
        assert_eq!(e.stdout.as_deref(), Some("1\n\n"));
        assert_eq!(e.exit, 3);
        assert_eq!(e.stderr_contains, vec!["boom"]);
        let e = parse_expectations("# expect-error: E0201 5:18\n").unwrap();
        assert_eq!(e.errors, vec![("E0201".to_string(), 5, 18)]);
    }

    #[test]
    fn rejects_contradictory_expectations() {
        assert!(parse_expectations("# expect-error: E0201 1:1\n# expect-exit: 1\n").is_err());
        assert!(parse_expectations("# expect-nonsense: 1\n").is_err());
    }
}
