//! `wys llm-proxy`（M5 T11、ADR-0023 論点 1〜6）: 実プロバイダ（Anthropic Messages API）の adapter。
//!
//! 生成した実行ファイルのランタイム（`wys_runtime::ai`）が `WYS_LLM_PROVIDER=anthropic` でこの副コマンドを
//! 子プロセスとして起動し、stdin / stdout の JSON 行でやり取りする。HTTPS と API キー（`ANTHROPIC_API_KEY`）は
//! この副コマンドだけが持ち、本言語のプログラムとランタイムは秘密情報に触れない（論点 6）。
//!
//! 要求（1 行、`docs/design/ai-sdk.md` 8.2）: `{"v":1, "model", "system", "messages", "tools", "schema",
//! "max_tokens", "timeout_ms"}`。`messages` の要素: `{"role":"user","content":str}` /
//! `{"role":"assistant","content": str | 前の応答の assistant}` / `{"role":"tool","id","content","is_error"}`。
//! 応答（1 行）: `{"text", "tool_calls":[{"id","name","args"}], "usage":{"input_tokens","output_tokens"},
//! "stop", "assistant":[...]}`、失敗なら `{"error": str}`。要求・応答・キーはどこにも書かない。

use std::io::{BufRead, Write};
use std::time::Duration;

const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2026-10-21";
const DEFAULT_MODEL: &str = "claude-opus-5-5";
const DEFAULT_MAX_TOKENS: i64 = 16000;
/// 要求全体のソケットのタイムアウトの上限（要求の `timeout_ms` があれば小さいほう。論点 5）
const MAX_TIMEOUT: Duration = Duration::from_secs(600);

/// ランタイムの会話を Messages API の `messages` にする。続くツールの結果は 1 つの `user` の発話にまとめる
/// （1 つの `assistant` の発話のすべての `tool_use` の結果は、次の 1 つの発話に入れる）。
fn to_api_messages(messages: &[serde_json::Value]) -> Vec<serde_json::Value> {
    let mut out: Vec<serde_json::Value> = Vec::new();
    let mut results: Vec<serde_json::Value> = Vec::new();
    let flush = |out: &mut Vec<serde_json::Value>, results: &mut Vec<serde_json::Value>| {
        if !results.is_empty() {
            out.push(serde_json::json!({"role": "user", "content": std::mem::take(results)}));
        }
    };
    for m in messages {
        match m["role"].as_str() {
            Some("tool") => {
                let mut block = serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": m["id"],
                    "content": m["content"],
                });
                if m["is_error"].as_bool() == Some(true) {
                    block["is_error"] = serde_json::Value::Bool(true);
                }
                results.push(block);
            }
            // 前の応答の `assistant`（thinking のブロックを含む）は手を加えずに戻す（論点 2）
            Some("assistant") => {
                flush(&mut out, &mut results);
                out.push(serde_json::json!({"role": "assistant", "content": m["content"]}));
            }
            _ => {
                flush(&mut out, &mut results);
                out.push(serde_json::json!({"role": "user", "content": m["content"]}));
            }
        }
    }
    flush(&mut out, &mut results);
    out
}

/// `model` は要求 → `WYS_LLM_MODEL` → 既定の順（ランタイムが要求に入れなかったとき。論点 3）。
fn request_body(req: &serde_json::Value, env_model: Option<&str>) -> serde_json::Value {
    let model = req["model"]
        .as_str()
        .filter(|m| !m.is_empty())
        .or(env_model.filter(|m| !m.is_empty()))
        .unwrap_or(DEFAULT_MODEL);
    let max_tokens = req["max_tokens"]
        .as_i64()
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_MAX_TOKENS);
    let mut body = serde_json::json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": to_api_messages(req["messages"].as_array().map_or(&[][..], Vec::as_slice)),
        // 安全分類器が断ったときは、推奨のモデルで再実行する（refusal の既定の扱い）
        "fallbacks": "default",
    });
    if let Some(system) = req["system"].as_str().filter(|s| !s.is_empty()) {
        body["system"] = serde_json::Value::String(system.into());
    }
    if let Some(tools) = req["tools"].as_array().filter(|t| !t.is_empty()) {
        body["tools"] = serde_json::Value::Array(tools.clone());
        // ランタイムは 1 回に 1 つのツールの要求を検査して実行する
        body["tool_choice"] =
            serde_json::json!({"type": "auto", "disable_parallel_tool_use": true});
    }
    if req["schema"].is_object() {
        body["output_config"] =
            serde_json::json!({"format": {"type": "json_schema", "schema": req["schema"]}});
    }
    body
}

/// 価格表（100 万トークンあたりの USD: 入力、出力、キャッシュの書き込み（5 分の TTL、入力の 1.25 倍）、
/// キャッシュの読み込み）。ADR-0025 論点 12。価格は古くなるので、ランタイムではなくここに置く。
/// 2026-10 時点の Anthropic の API の単価（claude-api のスキルの表）。モデルの ID は完全一致で引き、知らない
/// モデルには費用を付けない（ランタイムは「費用が分からない」として扱う）。`claude-haiku-5-5` は 10 万トークン
/// までの単価（それを超える要求の単価は表に入れていない）
const PRICES: &[(&str, [f64; 4])] = &[
    ("claude-fable-5-1", [10.0, 50.0, 12.5, 0.25]),
    ("claude-opus-5-5", [4.0, 20.0, 5.0, 0.20]),
    ("claude-sonnet-5-5", [2.0, 10.0, 2.5, 0.20]),
    ("claude-haiku-5-5", [0.10, 0.50, 0.125, 0.01]),
];

/// 応答の `usage` の費用（USD）。知らないモデルなら `None`
fn cost_usd(model: &str, usage: &serde_json::Value) -> Option<f64> {
    let (_, [input, output, write, read]) = PRICES.iter().find(|(m, _)| *m == model)?;
    let n = |k: &str| usage[k].as_f64().unwrap_or(0.0);
    Some(
        (n("input_tokens") * input
            + n("output_tokens") * output
            + n("cache_creation_input_tokens") * write
            + n("cache_read_input_tokens") * read)
            / 1_000_000.0,
    )
}

/// ランタイムへの `usage`（キャッシュの内訳は、あれば任意の項目として添える）と費用
fn usage_and_cost(resp: &serde_json::Value, model: &str) -> (serde_json::Value, serde_json::Value) {
    let u = &resp["usage"];
    let mut usage = serde_json::json!({
        "input_tokens": u["input_tokens"],
        "output_tokens": u["output_tokens"],
    });
    for k in ["cache_creation_input_tokens", "cache_read_input_tokens"] {
        if u[k].is_number() {
            usage[k] = u[k].clone();
        }
    }
    let model = resp["model"].as_str().unwrap_or(model);
    let cost = cost_usd(model, u).map_or(serde_json::Value::Null, serde_json::Value::from);
    (usage, cost)
}

/// Messages API の応答を、ランタイムへの 1 行にする（`model` は要求したモデル。応答に `model` がなければ使う）。
fn reply_line(resp: &serde_json::Value, model: &str) -> serde_json::Value {
    let content = resp["content"].as_array().cloned().unwrap_or_default();
    let text: String = content
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect();
    let tool_calls: Vec<serde_json::Value> = content
        .iter()
        .filter(|b| b["type"] == "tool_use")
        .map(|b| serde_json::json!({"id": b["id"], "name": b["name"], "args": b["input"]}))
        .collect();
    let stop = resp["stop_reason"].as_str().unwrap_or_default();
    let (usage, cost) = usage_and_cost(resp, model);
    let mut line = if stop == "refusal" {
        // 拒否でも入力は課金されるので、使用量を添える（ランタイムが予算に数える）
        serde_json::json!({
            "error": "the model declined the request (refusal)",
            "usage": usage,
        })
    } else {
        serde_json::json!({
            "text": text,
            "tool_calls": tool_calls,
            "usage": usage,
            "stop": stop,
            "assistant": content,
        })
    };
    if !cost.is_null() {
        line["cost_usd"] = cost;
    }
    line
}

/// 要求全体のタイムアウト: 要求の `timeout_ms`（agent の残り時間。0 = なし）と上限の小さいほう。
fn request_timeout(req: &serde_json::Value) -> Duration {
    match req["timeout_ms"].as_u64().filter(|&t| t > 0) {
        Some(t) => Duration::from_millis(t).min(MAX_TIMEOUT),
        None => MAX_TIMEOUT,
    }
}

fn call(agent: &ureq::Agent, key: &str, req: &serde_json::Value) -> serde_json::Value {
    if req["v"] != 1 {
        return serde_json::json!({"error": format!("unsupported protocol version {}", req["v"])});
    }
    let env_model = std::env::var("WYS_LLM_MODEL").ok();
    let body = request_body(req, env_model.as_deref());
    let resp = agent
        .post(ENDPOINT)
        .timeout(request_timeout(req))
        .set("content-type", "application/json")
        .set("x-api-key", key)
        .set("anthropic-version", API_VERSION)
        .set("anthropic-beta", "server-side-fallback-2026-07-01")
        .send_string(&body.to_string());
    match resp {
        Ok(r) => match r
            .into_string()
            .map(|t| serde_json::from_str::<serde_json::Value>(&t))
        {
            Ok(Ok(v)) => reply_line(&v, body["model"].as_str().unwrap_or_default()),
            Ok(Err(e)) => serde_json::json!({"error": format!("malformed API response: {e}")}),
            Err(e) => serde_json::json!({"error": format!("cannot read the API response: {e}")}),
        },
        // エラーの本文（`error.type` と `error.message`）だけを返す。キーは含まれない
        Err(ureq::Error::Status(code, r)) => {
            let detail = r
                .into_string()
                .ok()
                .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
                .map(|v| {
                    format!(
                        "{}: {}",
                        v["error"]["type"].as_str().unwrap_or(""),
                        v["error"]["message"].as_str().unwrap_or("")
                    )
                })
                .unwrap_or_default();
            serde_json::json!({"error": format!("HTTP {code} {detail}")})
        }
        Err(e) => serde_json::json!({"error": format!("request failed: {e}")}),
    }
}

/// stdin の要求を 1 行ずつ処理する。stdin が閉じたら終わる。
pub fn run() -> Result<(), String> {
    let key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| "llm-proxy: ANTHROPIC_API_KEY is not set".to_string())?;
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout(MAX_TIMEOUT)
        .build();
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| format!("llm-proxy: {e}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let out = match serde_json::from_str::<serde_json::Value>(&line) {
            Ok(req) => call(&agent, &key, &req),
            Err(e) => serde_json::json!({"error": format!("malformed request: {e}")}),
        };
        writeln!(stdout, "{out}").map_err(|e| format!("llm-proxy: {e}"))?;
        stdout.flush().map_err(|e| format!("llm-proxy: {e}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_map_to_the_messages_api() {
        let req = serde_json::json!({
            "v": 1,
            "model": "",
            "system": "Be brief.",
            "messages": [
                {"role": "user", "content": "plan"},
                {"role": "assistant", "content": [{"type": "thinking", "thinking": "", "signature": "s"}, {"type": "tool_use", "id": "t1", "name": "add", "input": {"a": 1}}]},
                {"role": "tool", "id": "t1", "content": "3", "is_error": false},
                {"role": "tool", "id": "t2", "content": "denied by approver", "is_error": true},
                {"role": "assistant", "content": "a string from the user's history"},
            ],
            "tools": [{"name": "add", "description": "d", "input_schema": {"type": "object"}}],
            "schema": {"type": "object"},
            "max_tokens": 0,
            "timeout_ms": 0,
        });
        let b = request_body(&req, None);
        assert_eq!(b["model"], DEFAULT_MODEL);
        assert_eq!(b["max_tokens"], DEFAULT_MAX_TOKENS);
        assert_eq!(b["system"], "Be brief.");
        assert_eq!(b["fallbacks"], "default");
        // thinking のブロックを含む発話は、そのまま戻す
        assert_eq!(b["messages"][1]["content"][0]["type"], "thinking");
        // 続くツールの結果は 1 つの user の発話にまとめる
        let results = &b["messages"][2]["content"];
        assert_eq!(results.as_array().map(Vec::len), Some(2));
        assert_eq!(results[0]["tool_use_id"], "t1");
        assert!(results[0].get("is_error").is_none());
        assert_eq!(results[1]["is_error"], true);
        assert_eq!(
            b["messages"][3]["content"],
            "a string from the user's history"
        );
        assert_eq!(b["messages"].as_array().map(Vec::len), Some(4));
        assert_eq!(b["tool_choice"]["type"], "auto");
        assert_eq!(b["output_config"]["format"]["type"], "json_schema");
    }

    #[test]
    fn replies_carry_tool_calls_usage_and_the_raw_content() {
        let resp = serde_json::json!({
            "content": [{"type": "text", "text": "ok"}, {"type": "tool_use", "id": "t1", "name": "add", "input": {"a": 1}}],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 5, "output_tokens": 7},
        });
        let r = reply_line(&resp, "claude-opus-5-5");
        assert_eq!(r["text"], "ok");
        assert_eq!(r["tool_calls"][0]["name"], "add");
        assert_eq!(r["usage"]["output_tokens"], 7);
        assert_eq!(r["assistant"].as_array().map(Vec::len), Some(2));
        let refused = serde_json::json!({"content": [], "stop_reason": "refusal"});
        assert!(reply_line(&refused, "claude-opus-5-5")["error"].is_string());
        let refused = serde_json::json!({"content": [], "stop_reason": "refusal", "usage": {"input_tokens": 9, "output_tokens": 0}});
        assert_eq!(
            reply_line(&refused, "claude-opus-5-5")["usage"]["input_tokens"],
            9
        );
    }

    #[test]
    fn replies_carry_the_cost_of_known_models() {
        // 入力 1,000,000 と出力 100,000、キャッシュの読み込み 1,000,000（claude-opus-5-5: $4 + $2 + $0.20）
        let resp = serde_json::json!({
            "model": "claude-opus-5-5",
            "content": [{"type": "text", "text": "x"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 1_000_000, "output_tokens": 100_000, "cache_read_input_tokens": 1_000_000}
        });
        let r = reply_line(&resp, "ignored");
        let cost = r["cost_usd"].as_f64().unwrap();
        assert!((cost - 6.2).abs() < 1e-9, "{cost}");
        assert_eq!(r["usage"]["cache_read_input_tokens"], 1_000_000);
        // 応答に `model` がなければ要求のモデルで引く。知らないモデルには費用を付けない
        let mut no_model = resp.clone();
        no_model.as_object_mut().unwrap().remove("model");
        assert!(reply_line(&no_model, "claude-haiku-5-5")["cost_usd"].is_number());
        assert!(
            reply_line(&no_model, "some-other-model")
                .get("cost_usd")
                .is_none()
        );
        // 拒否の行にも費用を付ける（入力は課金される）
        let refused = serde_json::json!({"model": "claude-sonnet-5-5", "content": [], "stop_reason": "refusal", "usage": {"input_tokens": 1_000_000, "output_tokens": 0}});
        assert_eq!(reply_line(&refused, "x")["cost_usd"], 2.0);
    }

    #[test]
    fn model_max_tokens_and_timeout_follow_the_request() {
        // model は要求 → WYS_LLM_MODEL → 既定、max_tokens は要求の値、タイムアウトは要求の残り時間と上限の小さいほう
        let req = serde_json::json!({"v": 1, "model": "m", "messages": [], "max_tokens": 50, "timeout_ms": 1500, "schema": null});
        let b = request_body(&req, Some("env"));
        assert_eq!(b["model"], "m");
        assert_eq!(b["max_tokens"], 50);
        assert!(b.get("system").is_none());
        assert!(b.get("output_config").is_none());
        assert!(b.get("tools").is_none());
        assert_eq!(request_timeout(&req), Duration::from_millis(1500));
        let req = serde_json::json!({"v": 1, "model": "", "messages": [], "timeout_ms": 0});
        assert_eq!(request_body(&req, Some("env"))["model"], "env");
        assert_eq!(request_timeout(&req), MAX_TIMEOUT);
        let late = serde_json::json!({"timeout_ms": 10_000_000});
        assert_eq!(request_timeout(&late), MAX_TIMEOUT);
    }

    #[test]
    fn other_protocol_versions_are_refused_without_a_request() {
        let agent = ureq::AgentBuilder::new().build();
        let r = call(
            &agent,
            "unused",
            &serde_json::json!({"v": 2, "messages": []}),
        );
        assert_eq!(r["error"], "unsupported protocol version 2");
    }
}
