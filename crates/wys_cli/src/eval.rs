//! `wys eval`: 評価ケースを記録の再生で実行し、合否と使用量をまとめる（ADR-0025 論点 11、`docs/design/ai-sdk.md` §20）。
//!
//! 評価ケースはディレクトリ `<dir>/case.wys`（`test_*` 関数）と `<dir>/recordings/<テスト関数名>.jsonl`。
//! `wys test` と同じく、テスト関数ごとに別のプロセスで実行する。環境は Conformance の harness と同じにする:
//! `WYS_VIRTUAL_TIME=1` を付け、開発者のシェルの AI Runtime の変数を外す。
//!
//! 終了コード: すべて合格 0、失敗あり 1、ケースか記録が読めない 2（`Err`）。

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// 開発者のシェルから外す AI Runtime の変数（`wys_testharness::is_ai_env` と同じ集合）。
fn is_ai_env(name: &str) -> bool {
    name.starts_with("WYS_LLM_")
        || matches!(
            name,
            "WYS_CAPABILITIES" | "WYS_APPROVAL" | "WYS_AUDIT" | "WYS_TRACE" | "WYS_TRACE_CONTENT"
        )
}

/// `--record` のときに残す、プロバイダの設定（環境のプロバイダで実行する）。
fn is_provider_env(name: &str) -> bool {
    matches!(name, "WYS_LLM_PROVIDER" | "WYS_LLM_MODEL" | "WYS_LLM_PROXY")
}

struct TestResult {
    name: String,
    passed: bool,
    llm_calls: i64,
    input_tokens: i64,
    output_tokens: i64,
    cost_usd: f64,
    output: String,
    /// 失敗した `llm_call` のスパンの結果（`error:replay` など。失敗の原因の手がかり）
    llm_errors: Vec<String>,
}

/// 一時ファイルの接頭辞。落ちる経路を含めて、作ったファイルを消す
struct Temp(PathBuf);

impl Temp {
    fn path(&self, suffix: &str) -> PathBuf {
        PathBuf::from(format!("{}.{suffix}", self.0.display()))
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.path("bin"));
    }
}

struct CaseResult {
    name: String,
    tests: Vec<TestResult>,
}

pub fn run(args: &[String]) -> Result<ExitCode, String> {
    let mut record = false;
    let mut json = false;
    let mut dirs = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--record" => record = true,
            "--format" => {
                json = match it.next().map(String::as_str) {
                    Some("json") => true,
                    Some("text") => false,
                    other => return Err(format!("unknown format {other:?}; use `text` or `json`")),
                }
            }
            s if s.starts_with('-') => return Err(format!("unknown option `{s}` for `wys eval`")),
            s => dirs.push(PathBuf::from(s)),
        }
    }
    if dirs.is_empty() {
        return Err("`wys eval` needs at least one case directory".into());
    }
    if record {
        if std::env::var("WYS_LLM_PROVIDER").map_or(true, |v| v.is_empty()) {
            return Err("`wys eval --record` runs the cases with the provider in WYS_LLM_PROVIDER; set it (for example `anthropic`)".into());
        }
        eprintln!("note: recordings contain the prompts and replies of the model");
    }
    let mut cases = Vec::new();
    for dir in &dirs {
        cases.push(run_case(dir, record)?);
    }
    let total: usize = cases.iter().map(|c| c.tests.len()).sum();
    let passed: usize = cases
        .iter()
        .map(|c| c.tests.iter().filter(|t| t.passed).count())
        .sum();
    let failed = total - passed;
    let rate = if total == 0 {
        None
    } else {
        Some(passed as f64 / total as f64)
    };
    if json {
        let v = serde_json::json!({
            "cases": cases.iter().map(|c| serde_json::json!({
                "name": c.name,
                "tests": c.tests.iter().map(|t| serde_json::json!({
                    "name": t.name, "passed": t.passed, "llm_calls": t.llm_calls,
                    "input_tokens": t.input_tokens, "output_tokens": t.output_tokens,
                    "cost_usd": t.cost_usd, "llm_errors": t.llm_errors,
                    "output": if t.passed { serde_json::Value::Null } else { t.output.clone().into() }
                })).collect::<Vec<_>>()
            })).collect::<Vec<_>>(),
            "passed": passed, "failed": failed, "success_rate": rate
        });
        println!("{v}");
    } else {
        for c in &cases {
            println!("case {}", c.name);
            for t in &c.tests {
                let verdict = if t.passed { "ok" } else { "FAILED" };
                println!(
                    "  test {} ... {verdict} (llm_calls={}, tokens={})",
                    t.name,
                    t.llm_calls,
                    t.input_tokens + t.output_tokens
                );
            }
        }
        for c in &cases {
            for t in c.tests.iter().filter(|t| !t.passed) {
                println!("\n---- {} / {} ----", c.name, t.name);
                for e in &t.llm_errors {
                    println!("llm_call: {e}");
                }
                print!("{}", t.output);
            }
        }
        let verdict = if failed == 0 { "ok" } else { "FAILED" };
        let rate = rate.map_or("-".to_string(), |r| format!("{r:.2}"));
        println!("\neval result: {verdict}. {passed} passed; {failed} failed; success rate {rate}");
    }
    Ok(if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// 記録の先頭のヘッダ（`{"kind":"header","v":1,...}`）。読めない・形が違えば失敗
fn read_header(rec: &Path) -> Result<serde_json::Value, String> {
    let bad = |why: String| format!("cannot read the recording {}: {why}", rec.display());
    let text = std::fs::read_to_string(rec).map_err(|e| bad(e.to_string()))?;
    let first = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .ok_or_else(|| bad("the file is empty".into()))?;
    let header: serde_json::Value = serde_json::from_str(first).map_err(|e| bad(e.to_string()))?;
    if header["kind"] != "header" || header["v"] != 1 {
        return Err(bad("the first line is not a version 1 header".into()));
    }
    Ok(header)
}

fn run_case(dir: &Path, record: bool) -> Result<CaseResult, String> {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string());
    let src = dir.join("case.wys");
    let text = std::fs::read_to_string(&src)
        .map_err(|e| format!("cannot read the case {}: {e}", src.display()))?;
    let (fe, tests) = wys_driver::frontend_tests(&src.display().to_string(), &text);
    if fe.has_errors() {
        eprint!("{}", wys_driver::to_text(&fe.file, &fe.diagnostics));
        return Err(format!("the case {} has errors", src.display()));
    }
    if tests.is_empty() {
        eprintln!("note: the case {} has no test_* functions", src.display());
        return Ok(CaseResult {
            name,
            tests: Vec::new(),
        });
    }
    if let Some(msg) = &fe.unsupported {
        return Err(msg.clone());
    }
    let Some(program) = &fe.program else {
        return Err(format!("the case {} cannot be built", src.display()));
    };
    let recordings = dir.join("recordings");
    if record {
        std::fs::create_dir_all(&recordings)
            .map_err(|e| format!("cannot create {}: {e}", recordings.display()))?;
    } else {
        // リンクの前に、すべての記録が読めて、先頭が版 1 のヘッダかを確かめる（読めない記録はテストの失敗ではなく 2。Codex のレビュー）
        for test in &tests {
            let rec = recordings.join(format!("{test}.jsonl"));
            read_header(&rec)?;
        }
    }
    // テストのない記録（消したテストの記録）を知らせる
    if let Ok(entries) = std::fs::read_dir(&recordings) {
        let mut stale: Vec<String> = entries
            .flatten()
            .filter_map(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                let stem = n.strip_suffix(".jsonl")?.to_string();
                (!tests.contains(&stem)).then_some(n)
            })
            .collect();
        stale.sort();
        for n in stale {
            eprintln!(
                "warning: {} has no test_* function for this recording",
                recordings.join(n).display()
            );
        }
    }
    let tmp = Temp(std::env::temp_dir().join(format!(
        "wys-eval-{}-{}",
        std::process::id(),
        name.replace(['/', '.'], "_")
    )));
    let exe = tmp.path("bin");
    wys_driver::link_program(program, &exe, &wys_driver::BuildOptions::default())
        .map_err(|e| format!("cannot build the case {}: {e:?}", src.display()))?;
    let mut results = Vec::new();
    for (i, test) in tests.iter().enumerate() {
        let rec = recordings.join(format!("{test}.jsonl"));
        let trace = tmp.path(&format!("{i}.trace.jsonl"));
        let _ = std::fs::remove_file(&trace);
        let mut cmd = Command::new(&exe);
        for (k, _) in std::env::vars() {
            if is_ai_env(&k) && !(record && is_provider_env(&k)) {
                cmd.env_remove(k);
            }
        }
        cmd.env("WYS_VIRTUAL_TIME", "1")
            .env("WYS_TEST_INDEX", i.to_string())
            .env("WYS_TRACE", &trace);
        if record {
            cmd.env("WYS_LLM_RECORD", &rec);
        } else {
            let header = read_header(&rec)?;
            cmd.env("WYS_LLM_PROVIDER", format!("replay:{}", rec.display()));
            // 記録したときのモデル（要求の `model` に入り、指紋を変える。ADR-0025 論点 11）
            if let Some(m) = header["model"].as_str() {
                cmd.env("WYS_LLM_MODEL", m);
            }
        }
        let out = cmd
            .output()
            .map_err(|e| format!("cannot run the case {}: {e}", src.display()))?;
        let mut r = TestResult {
            name: test.clone(),
            passed: out.status.success(),
            llm_calls: 0,
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: 0.0,
            output: format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
            llm_errors: Vec::new(),
        };
        // 使用量はトレースの `llm_call` のスパンから数える（応答が届いた呼び出しだけ = `llm_usage().llm_calls`）
        for line in std::fs::read_to_string(&trace).unwrap_or_default().lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if v["kind"] == "span"
                && v["name"] == "llm_call"
                && let Some(res) = v["result"].as_str()
                && res != "ok"
            {
                r.llm_errors.push(res.to_string());
            }
            if v["kind"] == "span" && v["name"] == "llm_call" && !v["usage"].is_null() {
                r.llm_calls += 1;
                r.input_tokens += v["usage"]["input_tokens"].as_i64().unwrap_or(0);
                r.output_tokens += v["usage"]["output_tokens"].as_i64().unwrap_or(0);
                r.cost_usd += v["cost_usd"].as_f64().unwrap_or(0.0);
            }
        }
        let _ = std::fs::remove_file(&trace);
        results.push(r);
    }
    Ok(CaseResult {
        name,
        tests: results,
    })
}
