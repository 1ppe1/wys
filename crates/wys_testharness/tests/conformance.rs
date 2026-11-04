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

fn runtime_lib() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    // target/<profile>/deps/conformance-xxxx
    let profile_dir = exe.parent().unwrap().parent().unwrap().to_path_buf();
    let mut cmd = Command::new(env!("CARGO"));
    cmd.args(["build", "-q", "-p", "wys_runtime"]);
    if profile_dir.ends_with("release") {
        cmd.arg("--release");
    }
    assert!(
        cmd.status().unwrap().success(),
        "failed to build wys_runtime"
    );
    profile_dir.join("libwys_runtime.a")
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root).unwrap().display().to_string()
}

#[test]
fn conformance_suite() {
    let root = repo_root();
    let cases = collect_cases(&root.join("tests/conformance"));
    assert!(!cases.is_empty(), "no conformance cases found");
    // 最適化レベルとサニタイザは環境変数 WYS_OPT / WYS_SANITIZE で切り替える（CI が3つのモードで実行する）
    let opts = BuildOptions {
        runtime_lib: Some(runtime_lib()),
        ..BuildOptions::default()
    };
    let scratch = std::env::temp_dir().join(format!("wys-conformance-{}", std::process::id()));
    std::fs::create_dir_all(&scratch).unwrap();

    let mut failures = Vec::new();
    for case in &cases {
        let name = rel(&root, case);
        if let Err(e) = run_case(case, &name, &opts, &scratch) {
            failures.push(format!("--- {name}\n{e}"));
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    assert!(
        failures.is_empty(),
        "{} of {} conformance cases failed:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
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
