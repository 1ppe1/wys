//! AI Runtime（ADR-0023、M5 T8）: `llm_generate` の本体、モックのプロバイダ、agent コンテキストのスタック、監査イベント。
//!
//! - 生成コードとは JSON の文字列で受け渡す（ランタイムはレコードの配置を知らない）。要求は `LlmRequest` の JSON、
//!   モックの応答は `MockResponse` の JSON、監査イベントは JSON の配列で返し、生成コードが `validate` の glue で型にする。
//! - スタックの底は**根のコンテキスト**（ツールなし、Capability はすべて ∩ `WYS_CAPABILITIES`、予算なし。ADR-0023 論点 9）。
//!   `@agent` の本体は `wys_agent_enter` / `wys_agent_exit` で積む（lowering が呼ぶのは T9）。
//! - LLM がツールを要求したら、実行の前に一覧・Capability・予算を検査する（論点 10）。T8 ではツールを実行しない
//!   （根のコンテキストにはツールがないので拒否になる）。トランポリンの呼び出しは T9、承認は T10、実プロバイダは T11。

use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::{CStr, c_char};
use std::io::Write;
use std::time::Instant;

use crate::{Header, embed};

/// Effect / Capability のビット（Capability の名前は Effect の名前と同じ。ADR-0023 論点 7）。
pub const CAP_IO: u64 = 1;
pub const CAP_EXIT: u64 = 2;
pub const CAP_PYTHON: u64 = 4;
pub const CAP_LLM: u64 = 8;
const CAP_ALL: u64 = CAP_IO | CAP_EXIT | CAP_PYTHON | CAP_LLM;

/// コンパイラが生成するツール表の 1 行（T9 で使う。ABI は暫定）。
#[repr(C)]
pub struct ToolDesc {
    pub name: *const c_char,
    pub description: *const c_char,
    pub schema: *const c_char,
    pub effects: u64,
    pub approval: i64,
    pub entry: extern "C" fn(args: *const Header) -> *mut Header,
}

/// `@agent(...)` の属性（T9 で使う。ABI は暫定）。
#[repr(C)]
pub struct AgentCfg {
    pub name: *const c_char,
    pub tools: *const i64,
    pub n_tools: i64,
    pub caps: u64,
    pub max_llm_calls: i64,
    pub max_tool_calls: i64,
    pub max_tokens: i64,
    pub timeout_ms: i64,
}

#[allow(dead_code)]
struct Tool {
    name: String,
    effects: u64,
    approval: bool,
}

struct Ctx {
    name: String,
    tools: Vec<usize>,
    caps: u64,
    max_llm_calls: i64,
    max_tool_calls: i64,
    max_tokens: i64,
    timeout_ms: i64,
    llm_calls: i64,
    tool_calls: i64,
    tokens: i64,
    started: Instant,
    /// モックの応答の遅延の合計（`WYS_VIRTUAL_TIME=1` ならこれだけが時間を進める。スケジューラの時計は動かさない）
    virtual_ms: i64,
}

impl Ctx {
    fn root() -> Ctx {
        Ctx::new(String::new(), Vec::new(), CAP_ALL & deployed_caps(), [0; 4])
    }

    fn new(name: String, tools: Vec<usize>, caps: u64, limits: [i64; 4]) -> Ctx {
        Ctx {
            name,
            tools,
            caps,
            max_llm_calls: limits[0],
            max_tool_calls: limits[1],
            max_tokens: limits[2],
            timeout_ms: limits[3],
            llm_calls: 0,
            tool_calls: 0,
            tokens: 0,
            started: Instant::now(),
            virtual_ms: 0,
        }
    }

    fn elapsed_ms(&self) -> i64 {
        let real = if crate::task::virtual_time_enabled() {
            0
        } else {
            self.started.elapsed().as_millis() as i64
        };
        real + self.virtual_ms
    }
}

/// プロバイダの 1 回の応答。
struct Reply {
    text: String,
    /// (呼び出しの ID, ツール名, 引数の JSON)
    tool_calls: Vec<(String, String, String)>,
    tokens: i64,
    latency_ms: i64,
}

struct AuditEvent {
    kind: &'static str,
    agent: String,
    tool: Option<String>,
    detail: String,
}

struct State {
    tools: Vec<Tool>,
    stack: Vec<Ctx>,
    mock: VecDeque<Reply>,
    audit: Vec<AuditEvent>,
    failure: Option<(&'static str, String)>,
}

impl State {
    fn new() -> State {
        State {
            tools: Vec::new(),
            stack: vec![Ctx::root()],
            mock: VecDeque::new(),
            audit: Vec::new(),
            failure: None,
        }
    }

    fn top(&self) -> &Ctx {
        self.stack.last().expect("the root context is never popped")
    }
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::new());
}

/// 配備時の Capability の縮小（`WYS_CAPABILITIES=io,llm`）。未設定なら縮小しない。広げることはできず、未知の名前は無視する
fn deployed_caps() -> u64 {
    deployed_caps_from(std::env::var("WYS_CAPABILITIES").ok().as_deref())
}

fn deployed_caps_from(v: Option<&str>) -> u64 {
    match v {
        None => CAP_ALL,
        Some(v) => v
            .split(',')
            .map(|s| match s.trim() {
                "io" => CAP_IO,
                "exit" => CAP_EXIT,
                "python" => CAP_PYTHON,
                "llm" => CAP_LLM,
                _ => 0,
            })
            .fold(0, |a, b| a | b),
    }
}

fn provider_configured() -> bool {
    std::env::var("WYS_LLM_PROVIDER").is_ok_and(|v| !v.is_empty())
}

/// # Safety
/// `p` は生存している文字列オブジェクトでなければならない。
unsafe fn text(p: *const Header) -> String {
    // SAFETY: 関数の安全条件どおり。
    String::from_utf8_lossy(unsafe { embed::bytes(p) }).into_owned()
}

/// # Safety
/// `p` は null か NUL 終端の文字列でなければならない。
unsafe fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: 関数の安全条件どおり。
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

fn audit(s: &mut State, kind: &'static str, tool: Option<&str>, detail: String) {
    let ev = AuditEvent {
        kind,
        agent: s.top().name.clone(),
        tool: tool.map(str::to_string),
        detail,
    };
    if let Ok(dest) = std::env::var("WYS_AUDIT") {
        let line = event_json(s.audit.len(), &ev).to_string();
        if dest == "stderr" {
            eprintln!("{line}");
        } else if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&dest)
        {
            let _ = writeln!(f, "{line}");
        }
    }
    s.audit.push(ev);
}

fn event_json(seq: usize, ev: &AuditEvent) -> serde_json::Value {
    serde_json::json!({
        "seq": seq, "kind": ev.kind, "agent": ev.agent, "tool": ev.tool, "detail": ev.detail
    })
}

fn fail(s: &mut State, kind: &'static str, message: String) -> *mut Header {
    s.failure = Some((kind, message));
    std::ptr::null_mut()
}

/// 予算を超えた agent（スタック上のすべてを見る。外側の残りを超えて使えない）があれば、その理由（ADR-0023 論点 13・14）。
fn over_budget(s: &State, tools_next: bool) -> Option<(&'static str, &'static str)> {
    s.stack.iter().rev().find_map(|a| {
        if a.timeout_ms > 0 && a.elapsed_ms() >= a.timeout_ms {
            Some(("timeout", "timeout_ms"))
        } else if a.max_tokens > 0 && a.tokens > a.max_tokens {
            Some(("budget", "max_tokens"))
        } else if !tools_next && a.max_llm_calls > 0 && a.llm_calls >= a.max_llm_calls {
            Some(("budget", "max_llm_calls"))
        } else if tools_next && a.max_tool_calls > 0 && a.tool_calls >= a.max_tool_calls {
            Some(("budget", "max_tool_calls"))
        } else {
            None
        }
    })
}

/// 要求（`LlmRequest` の JSON）を確かめる（ADR-0022 論点 9）。
fn check_request(req: &serde_json::Value) -> Result<(), String> {
    let messages = req["messages"]
        .as_array()
        .ok_or("invalid request: messages is not a list")?;
    if messages.is_empty() {
        return Err("invalid request: messages is empty".into());
    }
    for m in messages {
        let role = m["role"].as_str().unwrap_or_default();
        if role != "user" && role != "assistant" {
            return Err(format!(
                "invalid request: unknown role `{role}` (use `user` or `assistant`)"
            ));
        }
    }
    Ok(())
}

/// `llm_generate` の本体（`docs/design/ai-sdk.md` 5.1）。本文を +1 所有で返す。失敗なら null（`wys_llm_failed`）。
///
/// # Safety
/// `req` と `schema` は生存している文字列オブジェクトでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_llm_generate(
    req: *const Header,
    schema: *const Header,
) -> *mut Header {
    // SAFETY: 関数の安全条件どおり。出力スキーマは実プロバイダ（T11）に渡す。モックは使わない
    let (req, _schema) = unsafe { (text(req), text(schema)) };
    STATE.with(|cell| {
        let mut s = cell.borrow_mut();
        s.failure = None;
        // 1. Capability（`WYS_CAPABILITIES` で外せる。入口の再検査、ADR-0023 論点 11）
        if s.top().caps & CAP_LLM == 0 {
            audit(
                &mut s,
                "capability_denied",
                Some("llm_generate"),
                "capability llm is not granted".into(),
            );
            return fail(
                &mut s,
                "denied",
                "the `llm` capability is not granted".into(),
            );
        }
        // 2. 要求
        let req: serde_json::Value = match serde_json::from_str(&req) {
            Ok(v) => v,
            Err(e) => return fail(&mut s, "provider", format!("invalid request: {e}")),
        };
        if let Err(m) = check_request(&req) {
            return fail(&mut s, "provider", m);
        }
        if provider_configured() {
            return fail(
                &mut s,
                "provider",
                "external providers (WYS_LLM_PROVIDER) are not available yet".into(),
            );
        }
        // 3. 予算（前）
        if let Some((kind, what)) = over_budget(&s, false) {
            audit(&mut s, "budget_exceeded", None, what.into());
            return fail(&mut s, kind, format!("{what} exceeded"));
        }
        // 4. プロバイダ（モック）
        let Some(reply) = s.mock.pop_front() else {
            return fail(&mut s, "mock_exhausted", "no mock response left".into());
        };
        // 5. 使用量を数える（すべての agent に）
        for a in &mut s.stack {
            a.llm_calls += 1;
            a.tokens += reply.tokens;
            a.virtual_ms += reply.latency_ms;
        }
        audit(
            &mut s,
            "llm_called",
            None,
            format!("tokens={}", reply.tokens),
        );
        // 6. 予算（後）: 期限を過ぎて届いた応答は受け取らない
        if let Some((kind, what)) = over_budget(&s, false).filter(|(_, w)| *w != "max_llm_calls") {
            audit(&mut s, "budget_exceeded", None, what.into());
            return fail(&mut s, kind, format!("{what} exceeded"));
        }
        // 7. ツールの要求がなければ本文
        let Some((_, name, _)) = reply.tool_calls.first() else {
            return embed::new_object(reply.text.as_bytes());
        };
        // 8. ツールの要求: 実行の前に一覧を見る（T8 では実行しない。トランポリンは T9）
        let name = name.clone();
        let listed = s.top().tools.iter().any(|&i| s.tools[i].name == name);
        if !listed {
            audit(
                &mut s,
                "tool_denied",
                Some(&name),
                "not in the agent's tools".into(),
            );
            return fail(
                &mut s,
                "denied",
                format!("tool `{name}` is not allowed for this agent"),
            );
        }
        fail(
            &mut s,
            "denied",
            format!("tool `{name}` cannot be run yet (tool execution comes with M5 T9)"),
        )
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn wys_llm_failed() -> bool {
    STATE.with(|s| s.borrow().failure.is_some())
}

/// 直前の失敗の種類（参照カウント 1）。
#[unsafe(no_mangle)]
pub extern "C" fn wys_llm_err_kind() -> *mut Header {
    STATE.with(|s| embed::new_object(s.borrow().failure.as_ref().map_or("", |f| f.0).as_bytes()))
}

#[unsafe(no_mangle)]
pub extern "C" fn wys_llm_err_message() -> *mut Header {
    STATE.with(|s| {
        let m = s
            .borrow()
            .failure
            .as_ref()
            .map(|f| f.1.clone())
            .unwrap_or_default();
        embed::new_object(m.as_bytes())
    })
}

fn rejected(message: String) -> *mut Header {
    STATE.with(|cell| {
        let mut s = cell.borrow_mut();
        audit(&mut s, "schema_rejected", None, message.clone());
        embed::new_object(message.as_bytes())
    })
}

/// LLM の本文が JSON として読めなかった（監査 `schema_rejected` を残し、理由を返す）。
#[unsafe(no_mangle)]
pub extern "C" fn wys_llm_json_rejected() -> *mut Header {
    rejected(crate::json::last_failure_message())
}

/// LLM の本文が型に合わなかった（監査 `schema_rejected` を残し、経路付きの理由を返す）。
#[unsafe(no_mangle)]
pub extern "C" fn wys_llm_schema_rejected() -> *mut Header {
    rejected(crate::schema::last_failure_message())
}

/// `llm_mock_push(r)`（`MockResponse` の JSON）。実プロバイダが設定されていれば実行時エラー（ADR-0022 論点 10）。
///
/// # Safety
/// `resp` は生存している文字列オブジェクトでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_llm_mock_push(resp: *const Header) {
    if provider_configured() {
        embed::runtime_error("llm_mock_push is not allowed when WYS_LLM_PROVIDER is set");
    }
    // SAFETY: 関数の安全条件どおり。
    let v: serde_json::Value = serde_json::from_str(&unsafe { text(resp) }).unwrap_or_default();
    let tool_calls = v["tool_calls"]
        .as_array()
        .map(|cs| {
            cs.iter()
                .enumerate()
                .map(|(i, c)| {
                    (
                        format!("mock{i}"),
                        c["name"].as_str().unwrap_or_default().to_string(),
                        c["args_json"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let r = Reply {
        text: v["text"].as_str().unwrap_or_default().to_string(),
        tool_calls,
        tokens: v["input_tokens"].as_i64().unwrap_or(0) + v["output_tokens"].as_i64().unwrap_or(0),
        latency_ms: v["latency_ms"].as_i64().unwrap_or(0),
    };
    STATE.with(|s| s.borrow_mut().mock.push_back(r));
}

/// `audit_events()` の本体: 監査イベントの JSON の配列（参照カウント 1）。
#[unsafe(no_mangle)]
pub extern "C" fn wys_audit_json() -> *mut Header {
    STATE.with(|s| {
        let s = s.borrow();
        let all: Vec<serde_json::Value> = s
            .audit
            .iter()
            .enumerate()
            .map(|(i, e)| event_json(i, e))
            .collect();
        embed::new_object(serde_json::Value::Array(all).to_string().as_bytes())
    })
}

/// ツール表を登録する（T9 で生成コードの `main` が呼ぶ）。
///
/// # Safety
/// `tools` は `n` 個の有効な `ToolDesc` を指さなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_tools_register(tools: *const ToolDesc, n: i64) {
    // SAFETY: 関数の安全条件どおり。
    let descs = unsafe { std::slice::from_raw_parts(tools, n as usize) };
    let list: Vec<Tool> = descs
        .iter()
        .map(|d| Tool {
            // SAFETY: 生成コードの文字列定数。
            name: unsafe { cstr(d.name) },
            effects: d.effects,
            approval: d.approval != 0,
        })
        .collect();
    STATE.with(|s| s.borrow_mut().tools = list);
}

/// `@agent` の本体に入る（T9）。Capability は外側との積に縮める。
///
/// # Safety
/// `cfg` は有効な `AgentCfg` を指さなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_agent_enter(cfg: *const AgentCfg) {
    // SAFETY: 関数の安全条件どおり。
    let c = unsafe { &*cfg };
    // SAFETY: 同上。
    let tools = unsafe { std::slice::from_raw_parts(c.tools, c.n_tools as usize) }
        .iter()
        .map(|&i| i as usize)
        .collect();
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let caps = c.caps & s.top().caps;
        // SAFETY: 生成コードの文字列定数。
        let name = unsafe { cstr(c.name) };
        let limits = [
            c.max_llm_calls,
            c.max_tool_calls,
            c.max_tokens,
            c.timeout_ms,
        ];
        s.stack.push(Ctx::new(name, tools, caps, limits));
    });
}

/// `@agent` の本体を出る（T9）。根のコンテキストは下ろせない。
#[unsafe(no_mangle)]
pub extern "C" fn wys_agent_exit() {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        if s.stack.len() <= 1 {
            embed::runtime_error("agent context mismatch (this is a compiler bug)");
        }
        s.stack.pop();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deployed_capabilities_only_narrow() {
        assert_eq!(deployed_caps_from(None), CAP_ALL);
        assert_eq!(deployed_caps_from(Some("io,llm")), CAP_IO | CAP_LLM);
        assert_eq!(deployed_caps_from(Some("")), 0);
        assert_eq!(deployed_caps_from(Some("net, llm")), CAP_LLM);
    }

    #[test]
    fn requests_need_messages_with_known_roles() {
        let ok = serde_json::json!({"messages": [{"role": "user", "content": "x"}]});
        assert!(check_request(&ok).is_ok());
        let empty = serde_json::json!({"messages": []});
        assert_eq!(
            check_request(&empty).unwrap_err(),
            "invalid request: messages is empty"
        );
        let system = serde_json::json!({"messages": [{"role": "system", "content": "x"}]});
        assert!(
            check_request(&system)
                .unwrap_err()
                .contains("unknown role `system`")
        );
    }
}
