//! `tests/conformance/` 以下の全ケースを実行する。
//!
//! - すべてのケースを実行してから、失敗をまとめて報告する。
//! - `diagnostics/` 以下のケースは、診断 JSON を insta のスナップショットとしても固定する。
//! - `docs/spec/error-codes.md` のすべてのエラーコードに、少なくとも1つのケースがあることを確かめる。

use std::path::{Path, PathBuf};
use std::process::Command;
use wys_driver::BuildOptions;
use wys_testharness::{collect_cases, parse_expectations, run_case};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// ランタイム（Python を含まないものと含むもの）。
fn runtime_libs() -> (PathBuf, PathBuf) {
    let exe = std::env::current_exe().unwrap();
    // target/<profile>/deps/conformance-xxxx
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
}

/// `python/` のケースを実行できるか（Python 3.14 がある）。なければ飛ばす。`WYS_REQUIRE_PYTHON=1`（CI）なら失敗させる。
fn python_available() -> bool {
    let ok = wys_driver::python_link_flags().is_ok();
    if !ok {
        assert!(
            std::env::var_os("WYS_REQUIRE_PYTHON").is_none(),
            "WYS_REQUIRE_PYTHON is set but Python 3.14 (WYS_PYTHON) was not found"
        );
    }
    ok
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap().display().to_string()
}

#[test]
fn conformance_suite() {
    let root = repo_root();
    let mut cases = collect_cases(&root.join("tests/conformance"));
    assert!(!cases.is_empty(), "no conformance cases found");
    // 開発中に一部だけ回す: `WYS_CONFORMANCE_FILTER=lsp/`（パスの部分一致）
    if let Ok(f) = std::env::var("WYS_CONFORMANCE_FILTER") {
        cases.retain(|c| c.display().to_string().contains(&f));
    }
    // 最適化レベルとサニタイザは環境変数 WYS_OPT / WYS_SANITIZE で切り替える（CI が3つのモードで実行する）
    let (rt, py) = runtime_libs();
    let opts = BuildOptions {
        runtime_lib: Some(rt),
        python_runtime_lib: Some(py),
        ..BuildOptions::default()
    };
    let scratch = std::env::temp_dir().join(format!("wys-conformance-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let python = python_available();

    let mut failures = Vec::new();
    let mut skipped = 0;
    for case in &cases {
        let name = rel(&root, case);
        // Python を使うケース（ADR-0018）は、Python 3.14 がなければ飛ばす
        if name.starts_with("tests/conformance/python/") && !python {
            skipped += 1;
            continue;
        }
        if let Err(e) = run_case(case, &name, &opts, &scratch) {
            failures.push(format!("--- {name}\n{e}"));
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    if skipped > 0 {
        eprintln!(
            "skipped {skipped} case(s) under python/: Python 3.14 was not found (set WYS_PYTHON)"
        );
    }
    assert!(
        failures.is_empty(),
        "{} of {} conformance cases failed:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}

/// `explain/` 以下のケースは、`wys explain` の JSON を insta のスナップショットとして固定する（ADR-0017）。
#[test]
fn explain_json_snapshots() {
    let root = repo_root();
    let cases = collect_cases(&root.join("tests/conformance/explain"));
    assert!(!cases.is_empty(), "no explain cases found");
    for case in cases {
        let name = rel(&root, &case);
        let src = std::fs::read_to_string(&case).unwrap();
        let fe = wys_testharness::frontend_for_case(&case, &name, &src);
        let json = wys_explain::explain_file(&fe).unwrap_or_else(|| panic!("{name}: has errors"));
        let snap_name = format!("explain_{}", case.file_stem().unwrap().to_str().unwrap());
        insta::assert_snapshot!(snap_name, json);
    }
}

/// エラーのないすべてのケースで、`wys explain` の JSON がスキーマ（`docs/spec/explain-schema.json`）に合う。
#[test]
fn all_explain_reports_match_schema() {
    let root = repo_root();
    let schema: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("docs/spec/explain-schema.json")).unwrap(),
    )
    .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let mut checked = 0;
    let mut failures = Vec::new();
    for case in collect_cases(&root.join("tests/conformance")) {
        let name = rel(&root, &case);
        let src = std::fs::read_to_string(&case).unwrap();
        let fe = wys_testharness::frontend_for_case(&case, &name, &src);
        let Some(json) = wys_explain::explain_file(&fe) else {
            continue;
        };
        checked += 1;
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let errors: Vec<String> = validator.iter_errors(&v).map(|e| e.to_string()).collect();
        if !errors.is_empty() {
            failures.push(format!("{name}: {errors:?}"));
        }
    }
    assert!(checked > 50, "only {checked} cases were explained");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn diagnostics_json_snapshots() {
    let root = repo_root();
    for case in collect_cases(&root.join("tests/conformance/diagnostics")) {
        let name = rel(&root, &case);
        let src = std::fs::read_to_string(&case).unwrap();
        let fe = wys_testharness::frontend_for_case(&case, &name, &src);
        let snap_name = case.file_stem().unwrap().to_str().unwrap().to_string();
        insta::assert_snapshot!(snap_name, wys_driver::to_json(&fe.file, &fe.diagnostics));
    }
}

#[test]
fn every_error_code_has_a_case() {
    let root = repo_root();
    let doc = std::fs::read_to_string(root.join("docs/spec/error-codes.md")).unwrap();
    let documented: Vec<String> = doc
        .lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("| ")?;
            let code = rest.split_whitespace().next()?;
            let ok = code.len() == 5
                && (code.starts_with('E') || code.starts_with('W'))
                && code[1..].chars().all(|c| c.is_ascii_digit());
            ok.then(|| code.to_string())
        })
        .collect();
    assert!(
        documented.len() > 10,
        "failed to read error codes from the doc"
    );
    let mut covered = std::collections::HashSet::new();
    for case in collect_cases(&root.join("tests/conformance")) {
        let src = std::fs::read_to_string(&case).unwrap();
        for (code, _, _) in parse_expectations(&src).unwrap().errors {
            covered.insert(code);
        }
    }
    let missing: Vec<_> = documented
        .iter()
        .filter(|c| !covered.contains(*c))
        .collect();
    assert!(
        missing.is_empty(),
        "error codes without a conformance case: {missing:?}"
    );
}

/// AC-005: すべてのケースの診断 JSON が `docs/spec/diagnostics-schema.json` に従う。
#[test]
fn all_diagnostics_match_schema() {
    let root = repo_root();
    let schema: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("docs/spec/diagnostics-schema.json")).unwrap(),
    )
    .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    let mut checked = 0;
    for case in collect_cases(&root.join("tests/conformance")) {
        let name = rel(&root, &case);
        let src = std::fs::read_to_string(&case).unwrap();
        let fe = wys_testharness::frontend_for_case(&case, &name, &src);
        let json: serde_json::Value =
            serde_json::from_str(&wys_driver::to_json(&fe.file, &fe.diagnostics)).unwrap();
        let errors: Vec<String> = validator
            .iter_errors(&json)
            .map(|e| e.to_string())
            .collect();
        assert!(errors.is_empty(), "{name}: schema violations: {errors:?}");
        checked += 1;
    }
    assert!(checked > 40, "too few cases checked: {checked}");
}

/// 利用者向けの文書（`docs/guide/*.md`）のコードブロックを、Conformance のケースと同じ方法で検査する（公開準備 T8）。
///
/// - 情報文字列が `wys` のブロック: 書かれた `# expect-stdout:` / `# expect-error:` などの期待どおりに動き、
///   `wys fmt` の結果と一致する（文書の例が実装とずれない）
/// - `wys python`: Python 3.14 があるときだけ同じ検査をする
/// - `wys fragment`: 完全なプログラムではない断片（検査しない）
#[test]
fn guide_code_blocks_run() {
    let root = repo_root();
    let dir = root.join("docs/guide");
    let mut pages: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default();
    pages.retain(|p| p.extension().is_some_and(|e| e == "md"));
    pages.sort();
    assert!(!pages.is_empty(), "no pages under docs/guide");
    let (rt, py) = runtime_libs();
    let opts = BuildOptions {
        runtime_lib: Some(rt),
        python_runtime_lib: Some(py),
        ..BuildOptions::default()
    };
    let scratch = std::env::temp_dir().join(format!("wys-guide-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();
    let python = python_available();
    let mut failures = Vec::new();
    let mut checked = 0;
    for page in &pages {
        let text = std::fs::read_to_string(page).unwrap();
        let page_name = rel(&root, page);
        let mut block: Option<(String, usize, String)> = None;
        for (i, line) in text.lines().enumerate() {
            match &mut block {
                None => {
                    if let Some(info) = line.strip_prefix("```") {
                        block = Some((info.trim().to_string(), i + 1, String::new()));
                    }
                }
                Some((info, start, body)) => {
                    if line == "```" {
                        let (info, start, body) = (info.clone(), *start, body.clone());
                        block = None;
                        let run = match info.as_str() {
                            "wys" => true,
                            "wys python" => python,
                            _ => false,
                        };
                        if !run {
                            continue;
                        }
                        checked += 1;
                        let name = format!("{page_name}:{start}");
                        if wys_fmt::format_source(&body).ok().as_deref() != Some(body.as_str()) {
                            failures.push(format!(
                                "--- {name}\nthe block is not formatted (`wys fmt`)"
                            ));
                            continue;
                        }
                        let file = scratch.join(format!("block{checked}.wys"));
                        std::fs::write(&file, &body).unwrap();
                        if let Err(e) = run_case(&file, &name, &opts, &scratch) {
                            failures.push(format!("--- {name}\n{e}"));
                        }
                    } else {
                        body.push_str(line);
                        body.push('\n');
                    }
                }
            }
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(checked > 0, "no `wys` code blocks under docs/guide");
    assert!(
        failures.is_empty(),
        "{} of {checked} guide code blocks failed:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
