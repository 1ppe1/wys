//! `wys llm-proxy`（PoC4、M5 T1）: 実プロバイダ（Anthropic Messages API）の adapter。
//!
//! 生成した実行ファイルのランタイム（`wys_runtime::ai`）が `WYS_LLM_PROVIDER=cmd:<path>` でこの副コマンドを
//! 子プロセスとして起動し、stdin / stdout の JSON 行でやり取りする。HTTPS と API キー（`ANTHROPIC_API_KEY`）は
//! この副コマンドだけが持ち、本言語のプログラムとランタイムは秘密情報に触れない（ADR-0023 の候補）。
//!
//! 要求（1 行）: `{"model", "messages", "tools", "schema", "max_tokens"}`。`messages` はランタイムの形:
//! `{"role":"user","content":str}` / `{"role":"assistant","raw":[...]}`（前の応答の `assistant` をそのまま）/
//! `{"role":"tool","id":str,"content":str,"is_error"?:bool}`。
//! 応答（1 行）: `{"text", "tool_calls":[{"id","name","args"}], "usage":{"input_tokens","output_tokens"},
//! "stop", "assistant":[...]}`、失敗なら `{"error": str}`。

use std::io::{BufRead, Write};
use std::time::Duration;

const ENDPOINT: &str = "https://api.anthropic.com/v1/messages";
const API_VERSION: &str = "2026-10-21";
const DEFAULT_MODEL: &str = "claude-opus-5-5";

/// ランタイムの会話を Messages API の `messages` にする。
fn to_api_messages(messages: &[serde_json::Value]) -> Vec<serde_json::Value> {
    messages
        .iter()
        .map(|m| match m["role"].as_str() {
            Some("assistant") => serde_json::json!({"role": "assistant", "content": m["raw"]}),
            Some("tool") => {
                let mut block = serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": m["id"],
                    "content": m["content"],
                });
                if m["is_error"].as_bool() == Some(true) {
                    block["is_error"] = serde_json::Value::Bool(true);
                }
                serde_json::json!({"role": "user", "content": [block]})
            }
            _ => serde_json::json!({"role": "user", "content": m["content"]}),
        })
        .collect()
}

fn request_body(req: &serde_json::Value) -> serde_json::Value {
    let model = req["model"]
        .as_str()
        .filter(|m| !m.is_empty())
        .unwrap_or(DEFAULT_MODEL);
    let mut body = serde_json::json!({
        "model": model,
        "max_tokens": 16000,
        "messages": to_api_messages(req["messages"].as_array().map_or(&[][..], Vec::as_slice)),
        // 安全分類器が断ったときは、推奨のモデルで再実行する（refusal の既定の扱い）
        "fallbacks": "default",
    });
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

/// Messages API の応答を、ランタイムへの 1 行にする。
fn reply_line(resp: &serde_json::Value) -> serde_json::Value {
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
    if stop == "refusal" {
        return serde_json::json!({"error": "the model declined the request (refusal)"});
    }
    serde_json::json!({
        "text": text,
        "tool_calls": tool_calls,
        "usage": {
            "input_tokens": resp["usage"]["input_tokens"],
            "output_tokens": resp["usage"]["output_tokens"],
        },
        "stop": stop,
        "assistant": content,
    })
}

fn call(agent: &ureq::Agent, key: &str, req: &serde_json::Value) -> serde_json::Value {
    let body = request_body(req);
    let resp = agent
        .post(ENDPOINT)
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
            Ok(Ok(v)) => reply_line(&v),
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
        .timeout(Duration::from_secs(600))
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
            "model": "",
            "messages": [
                {"role": "user", "content": "plan"},
                {"role": "assistant", "raw": [{"type": "thinking", "thinking": "", "signature": "s"}, {"type": "tool_use", "id": "t1", "name": "add", "input": {"a": 1}}]},
                {"role": "tool", "id": "t1", "content": "3"},
                {"role": "tool", "id": "t2", "content": "denied by approver", "is_error": true},
            ],
            "tools": [{"name": "add", "description": "d", "input_schema": {"type": "object"}}],
            "schema": {"type": "object"},
        });
        let b = request_body(&req);
        assert_eq!(b["model"], DEFAULT_MODEL);
        assert_eq!(b["fallbacks"], "default");
        // thinking のブロックを含む発話は、そのまま戻す
        assert_eq!(b["messages"][1]["content"][0]["type"], "thinking");
        assert_eq!(b["messages"][2]["content"][0]["tool_use_id"], "t1");
        assert_eq!(b["messages"][3]["content"][0]["is_error"], true);
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
        let r = reply_line(&resp);
        assert_eq!(r["text"], "ok");
        assert_eq!(r["tool_calls"][0]["name"], "add");
        assert_eq!(r["usage"]["output_tokens"], 7);
        assert_eq!(r["assistant"].as_array().map(Vec::len), Some(2));
        let refused = serde_json::json!({"content": [], "stop_reason": "refusal"});
        assert!(reply_line(&refused)["error"].is_string());
    }
}
