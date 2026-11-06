//! PoC4（M5 T1）: AI 安全性のランタイムの試作。生成コードの代わりに `tests/ai_poc.c` から呼ぶ。
//!
//! 確かめること（`docs/plans/m5.md` の「PoC4 の判定」）:
//! - LLM がツールを要求するたびに、**実行の前に** (1) agent の一覧にあるか、(2) ツールの Effect が agent の
//!   Capability（静的な付与と `WYS_CAPABILITIES` の積）に含まれるか、(3) 予算（回数・トークン・時間）を検査する。
//!   拒否したら監査イベントを残す（AC-007 / AC-008）。保証はプロンプトではなくこの検査が担う。
//! - ツールは、コンパイラが生成する表（名前・説明・引数の JSON Schema・Effect・承認の要否・トランポリン）で呼ぶ。
//!   トランポリンは JSON の引数を型付きの値に検証してから関数を呼び、戻り値を JSON にする。
//! - LLM のプロバイダは、ランタイム内のモック（既定）と、別プロセス（`WYS_LLM_PROVIDER=cmd:<path>`、JSON 行）。
//!
//! 試作なので ABI は暫定。ADR-0022 / 0023 で決める。

use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::{CStr, c_char};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Instant;

use crate::{Header, embed};

/// Effect / Capability のビット（ADR-0023 の候補: Capability の名前は Effect の名前と同じ）。
pub const CAP_IO: u64 = 1;
pub const CAP_EXIT: u64 = 2;
pub const CAP_PYTHON: u64 = 4;
pub const CAP_LLM: u64 = 8;

/// コンパイラが生成するツール表の 1 行。
#[repr(C)]
pub struct ToolDesc {
    pub name: *const c_char,
    pub description: *const c_char,
    /// 引数のレコードの JSON Schema
    pub schema: *const c_char,
    /// ツールが宣言した Effect（`uses`）
    pub effects: u64,
    /// 0 = 承認不要、1 = 承認が要る
    pub approval: i64,
    /// トランポリン: JSON の引数（`str`）を受け取り、結果の JSON（参照カウント 1 の `str`）を返す。
    /// 引数が検証を通らなければ null を返し、理由を `wys_tool_fail` で残す
    pub entry: extern "C" fn(args: *const Header) -> *mut Header,
}

/// `@agent(...)` の属性。
#[repr(C)]
pub struct AgentCfg {
    pub name: *const c_char,
    /// ツール表の番号の並び
    pub tools: *const i64,
    pub n_tools: i64,
    pub caps: u64,
    /// 0 なら上限なし
    pub max_llm_calls: i64,
    pub max_tool_calls: i64,
    pub max_tokens: i64,
    pub timeout_ms: i64,
}

struct Tool {
    name: String,
    description: String,
    schema: String,
    effects: u64,
    approval: bool,
    entry: extern "C" fn(*const Header) -> *mut Header,
}

struct Agent {
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
    /// モックの応答の遅延の合計（`WYS_VIRTUAL_TIME=1` ならこれだけが時間を進める）
    virtual_ms: i64,
}

impl Agent {
    fn elapsed_ms(&self) -> i64 {
        let real = if crate::task::virtual_time_enabled() {
            0
        } else {
            self.started.elapsed().as_millis() as i64
        };
        real + self.virtual_ms
    }
}

/// モックの応答。`tool` があればツールの呼び出し、なければ最終の本文。
struct MockResponse {
    text: String,
    tool: Option<(String, String)>,
    input_tokens: i64,
    output_tokens: i64,
    latency_ms: i64,
}

/// 監査イベント（本文は含めない）。
pub struct AuditEvent {
    pub kind: &'static str,
    pub agent: String,
    pub tool: String,
    pub detail: String,
}

struct Proxy {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

#[derive(Default)]
struct State {
    tools: Vec<Tool>,
    agents: Vec<Agent>,
    mock: VecDeque<MockResponse>,
    audit: Vec<AuditEvent>,
    failure: Option<(&'static str, String)>,
    tool_failure: Option<String>,
    proxy: Option<Proxy>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
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

/// # Safety
/// `p` は null か生存している文字列オブジェクトでなければならない。
unsafe fn text(p: *const Header) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: 関数の安全条件どおり。
    String::from_utf8_lossy(unsafe { embed::bytes(p) }).into_owned()
}

/// 配備時の Capability の縮小（`WYS_CAPABILITIES=io,llm`）。未設定なら縮小しない。広げることはできない
fn deployed_caps() -> u64 {
    match std::env::var("WYS_CAPABILITIES") {
        Err(_) => u64::MAX,
        Ok(v) => v
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

/// ツール表を登録する（生成コードの `main` が最初に呼ぶ）。
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
            description: unsafe { cstr(d.description) },
            schema: unsafe { cstr(d.schema) },
            effects: d.effects,
            approval: d.approval != 0,
            entry: d.entry,
        })
        .collect();
    STATE.with(|s| s.borrow_mut().tools = list);
}

/// `@agent` の本体に入る。入れ子の agent は、Capability と予算を外側との積に縮める。
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
        let outer_caps = s.agents.last().map_or(u64::MAX, |a| a.caps);
        let agent = Agent {
            // SAFETY: 生成コードの文字列定数。
            name: unsafe { cstr(c.name) },
            tools,
            caps: c.caps & outer_caps & deployed_caps(),
            max_llm_calls: c.max_llm_calls,
            max_tool_calls: c.max_tool_calls,
            max_tokens: c.max_tokens,
            timeout_ms: c.timeout_ms,
            llm_calls: 0,
            tool_calls: 0,
            tokens: 0,
            started: Instant::now(),
            virtual_ms: 0,
        };
        s.agents.push(agent);
    });
}

/// `@agent` の本体を出る（すべての出口で lowering が呼ぶ）。
#[unsafe(no_mangle)]
pub extern "C" fn wys_agent_exit() {
    STATE.with(|s| {
        if s.borrow_mut().agents.pop().is_none() {
            crate::embed::runtime_error("agent context mismatch (this is a compiler bug)");
        }
    });
}

/// モックの応答を積む（`llm_mock_push`）。`tool` が null なら最終の本文。
///
/// # Safety
/// 文字列は null か生存している文字列オブジェクトでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_llm_mock_push(
    text_: *const Header,
    tool: *const Header,
    args_json: *const Header,
    input_tokens: i64,
    output_tokens: i64,
    latency_ms: i64,
) {
    // SAFETY: 関数の安全条件どおり。
    let r = unsafe {
        MockResponse {
            text: text(text_),
            tool: (!tool.is_null()).then(|| (text(tool), text(args_json))),
            input_tokens,
            output_tokens,
            latency_ms,
        }
    };
    STATE.with(|s| s.borrow_mut().mock.push_back(r));
}

/// トランポリンが引数の検証に失敗したときの理由。
///
/// # Safety
/// `msg` は生存している文字列オブジェクトでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_tool_fail(msg: *const Header) {
    // SAFETY: 関数の安全条件どおり。
    let m = unsafe { text(msg) };
    STATE.with(|s| s.borrow_mut().tool_failure = Some(m));
}

fn audit(s: &mut State, kind: &'static str, tool: &str, detail: String) {
    let agent = s.agents.last().map_or(String::new(), |a| a.name.clone());
    let ev = AuditEvent {
        kind,
        agent,
        tool: tool.to_string(),
        detail,
    };
    if let Ok(dest) = std::env::var("WYS_AUDIT") {
        let line = serde_json::json!({
            "seq": s.audit.len(), "kind": ev.kind, "agent": ev.agent, "tool": ev.tool, "detail": ev.detail
        })
        .to_string();
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

fn fail(s: &mut State, kind: &'static str, message: String) -> *mut Header {
    s.failure = Some((kind, message));
    std::ptr::null_mut()
}

/// プロバイダの 1 回の応答。
struct Reply {
    text: String,
    /// (呼び出しの ID, ツール名, 引数の JSON)
    tool: Option<(String, String, String)>,
    tokens: i64,
    latency_ms: i64,
    /// プロバイダが返したアシスタントの発話（そのまま次の要求に戻す。thinking のブロックを含み得る）
    raw: serde_json::Value,
}

fn provider() -> Option<String> {
    std::env::var("WYS_LLM_PROVIDER")
        .ok()
        .filter(|v| !v.is_empty())
}

/// 別プロセスのプロバイダ（`cmd:<path>`）に 1 行の JSON を送り、1 行の JSON を受け取る。
fn call_proxy(
    s: &mut State,
    cmd: &str,
    args: &[&str],
    messages: &[serde_json::Value],
    tools: &[serde_json::Value],
    schema: Option<&str>,
) -> Result<Reply, String> {
    if s.proxy.is_none() {
        let mut child = Command::new(cmd)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot start the provider `{cmd}`: {e}"))?;
        let stdin = child.stdin.take().expect("piped");
        let stdout = BufReader::new(child.stdout.take().expect("piped"));
        s.proxy = Some(Proxy {
            child,
            stdin,
            stdout,
        });
    }
    let p = s.proxy.as_mut().expect("started above");
    let schema: Option<serde_json::Value> = schema.and_then(|t| serde_json::from_str(t).ok());
    let req = serde_json::json!({
        "model": std::env::var("WYS_LLM_MODEL").unwrap_or_default(),
        "messages": messages,
        "tools": tools,
        "schema": schema,
        "max_tokens": 1024,
    });
    writeln!(p.stdin, "{req}").map_err(|e| format!("provider write failed: {e}"))?;
    p.stdin
        .flush()
        .map_err(|e| format!("provider write failed: {e}"))?;
    let mut line = String::new();
    p.stdout
        .read_line(&mut line)
        .map_err(|e| format!("provider read failed: {e}"))?;
    let v: serde_json::Value =
        serde_json::from_str(&line).map_err(|e| format!("malformed provider reply: {e}"))?;
    if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
        return Err(err.to_string());
    }
    let usage = &v["usage"];
    let tokens =
        usage["input_tokens"].as_i64().unwrap_or(0) + usage["output_tokens"].as_i64().unwrap_or(0);
    let tool = v["tool_calls"]
        .as_array()
        .and_then(|calls| calls.first())
        .map(|c| {
            (
                c["id"].as_str().unwrap_or_default().to_string(),
                c["name"].as_str().unwrap_or_default().to_string(),
                c["args"].to_string(),
            )
        });
    Ok(Reply {
        text: v["text"].as_str().unwrap_or_default().to_string(),
        tool,
        tokens,
        latency_ms: 0,
        raw: v["assistant"].clone(),
    })
}

/// `llm_generate`: プロンプトを送り、ツールの要求を検査しながら呼び、最終の本文を返す（参照カウント 1）。
/// 失敗なら null（`wys_llm_failed` / `wys_llm_err_kind` / `wys_llm_err_message`）。
/// `schema`（null 可）は出力の JSON Schema。検証は呼び出し側の glue（`validate[T]`）が行う。
///
/// # Safety
/// `prompt` は生存している文字列オブジェクト、`schema` は null か NUL 終端の文字列でなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_llm_generate(
    prompt: *const Header,
    schema: *const c_char,
) -> *mut Header {
    // SAFETY: 関数の安全条件どおり。
    let (prompt, schema) = unsafe { (text(prompt), (!schema.is_null()).then(|| cstr(schema))) };
    STATE.with(|cell| {
        let mut s = cell.borrow_mut();
        s.failure = None;
        if s.agents.is_empty() {
            return fail(&mut s, "denied", "llm_generate is called outside an agent".into());
        }
        if s.agents.last().is_some_and(|a| a.caps & CAP_LLM == 0) {
            audit(&mut s, "llm_denied", "", "capability llm is not granted".into());
            return fail(&mut s, "denied", "the agent has no `llm` capability".into());
        }
        let mut messages = vec![serde_json::json!({"role": "user", "content": prompt})];
        loop {
            // 予算: 呼び出しの前に回数と時間を見る
            let a = s.agents.last().expect("checked above");
            if a.max_llm_calls > 0 && a.llm_calls >= a.max_llm_calls {
                audit(&mut s, "budget_exceeded", "", "max_llm_calls".into());
                return fail(&mut s, "budget", "max_llm_calls exceeded".into());
            }
            if a.timeout_ms > 0 && a.elapsed_ms() >= a.timeout_ms {
                audit(&mut s, "budget_exceeded", "", "timeout_ms".into());
                return fail(&mut s, "timeout", "timeout_ms exceeded".into());
            }
            // `cmd:<path>` は利用者のコマンド、`anthropic` は `wys llm-proxy`（`WYS_LLM_PROXY` か PATH の `wys`）
            let external = provider().map(|p| match p.strip_prefix("cmd:") {
                Some(c) => (c.to_string(), Vec::new()),
                None if p == "anthropic" => (
                    std::env::var("WYS_LLM_PROXY").unwrap_or_else(|_| "wys".into()),
                    vec!["llm-proxy"],
                ),
                None => (String::new(), Vec::new()),
            });
            let reply = match external {
                Some((cmd, _)) if cmd.is_empty() => {
                    return fail(&mut s, "provider", "unknown WYS_LLM_PROVIDER (use `anthropic` or `cmd:<path>`)".into());
                }
                Some((cmd, cmd_args)) => {
                    let a = s.agents.last().expect("checked above");
                    let tools: Vec<serde_json::Value> = a
                        .tools
                        .iter()
                        .map(|&i| {
                            let t = &s.tools[i];
                            serde_json::json!({
                                "name": t.name,
                                "description": t.description,
                                "input_schema": serde_json::from_str::<serde_json::Value>(&t.schema).unwrap_or_default(),
                            })
                        })
                        .collect();
                    match call_proxy(&mut s, &cmd, &cmd_args, &messages, &tools, schema.as_deref()) {
                        Ok(r) => r,
                        Err(e) => return fail(&mut s, "provider", e),
                    }
                }
                None => match s.mock.pop_front() {
                    Some(m) => Reply {
                        text: m.text,
                        tool: m.tool.map(|(n, a)| (String::new(), n, a)),
                        tokens: m.input_tokens + m.output_tokens,
                        latency_ms: m.latency_ms,
                        raw: serde_json::Value::Null,
                    },
                    None => return fail(&mut s, "mock_exhausted", "no mock response left".into()),
                },
            };
            let a = s.agents.last_mut().expect("checked above");
            a.llm_calls += 1;
            a.tokens += reply.tokens;
            a.virtual_ms += reply.latency_ms;
            let over_tokens = a.max_tokens > 0 && a.tokens > a.max_tokens;
            audit(&mut s, "llm_called", "", format!("tokens={}", reply.tokens));
            if over_tokens {
                audit(&mut s, "budget_exceeded", "", "max_tokens".into());
                return fail(&mut s, "budget", "max_tokens exceeded".into());
            }
            let Some((call_id, name, args)) = reply.tool else {
                return embed::new_object(reply.text.as_bytes());
            };
            let assistant = serde_json::json!({"role": "assistant", "raw": reply.raw});
            // ツールの要求: 実行の前に一覧・Capability・予算を検査する（AC-007 / AC-008）
            let a = s.agents.last().expect("checked above");
            let listed = a.tools.iter().copied().find(|&i| s.tools[i].name == name);
            let Some(ti) = listed else {
                audit(&mut s, "tool_denied", &name, "not in the agent's tools".into());
                return fail(&mut s, "denied", format!("tool `{name}` is not allowed for this agent"));
            };
            let missing = s.tools[ti].effects & !a.caps;
            if missing != 0 {
                audit(&mut s, "tool_denied", &name, format!("missing capabilities {missing:#x}"));
                return fail(&mut s, "denied", format!("tool `{name}` needs capabilities the agent does not have"));
            }
            if a.max_tool_calls > 0 && a.tool_calls >= a.max_tool_calls {
                audit(&mut s, "budget_exceeded", &name, "max_tool_calls".into());
                return fail(&mut s, "budget", "max_tool_calls exceeded".into());
            }
            if s.tools[ti].approval && std::env::var("WYS_APPROVAL").as_deref() != Ok("allow") {
                audit(&mut s, "approval_denied", &name, "WYS_APPROVAL is not allow".into());
                messages.push(assistant);
                messages.push(serde_json::json!({"role": "tool", "id": call_id, "content": "denied by approver", "is_error": true}));
                continue;
            }
            s.agents.last_mut().expect("checked above").tool_calls += 1;
            audit(&mut s, "tool_called", &name, format!("args_len={}", args.len()));
            let entry = s.tools[ti].entry;
            // ツールは本言語のコードなので、借りを返してから呼ぶ（ツールの中から llm_generate を呼べる）
            drop(s);
            let arg = embed::new_object(args.as_bytes());
            let out = entry(arg);
            // SAFETY: arg は作ったばかりの参照カウント 1 の文字列。
            unsafe { release_str(arg) };
            s = cell.borrow_mut();
            if out.is_null() {
                let why = s.tool_failure.take().unwrap_or_default();
                audit(&mut s, "schema_rejected", &name, why.clone());
                return fail(&mut s, "schema", format!("tool `{name}`: {why}"));
            }
            // SAFETY: out はトランポリンが返した参照カウント 1 の文字列。
            let result = unsafe { text(out) };
            // SAFETY: 同上。
            unsafe { release_str(out) };
            messages.push(assistant);
            messages.push(serde_json::json!({"role": "tool", "id": call_id, "content": result}));
        }
    })
}

/// # Safety
/// `p` は参照カウント 1 以上の文字列オブジェクトでなければならない。
unsafe fn release_str(p: *mut Header) {
    // SAFETY: 関数の安全条件どおり。
    unsafe {
        if embed::release_last(p) {
            crate::wys_free_object(p);
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn wys_llm_failed() -> bool {
    STATE.with(|s| s.borrow().failure.is_some())
}

/// 直前の失敗の種類（`denied` / `budget` / `timeout` / `schema` / `provider` / `mock_exhausted`）。参照カウント 1
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

#[unsafe(no_mangle)]
pub extern "C" fn wys_audit_count() -> i64 {
    STATE.with(|s| s.borrow().audit.len() as i64)
}

/// 監査イベント `i` の種類とツール名（`kind:tool`）。参照カウント 1
#[unsafe(no_mangle)]
pub extern "C" fn wys_audit_entry(i: i64) -> *mut Header {
    STATE.with(|s| {
        let s = s.borrow();
        let e = &s.audit[i as usize];
        embed::new_object(format!("{}:{}", e.kind, e.tool).as_bytes())
    })
}

/// 終了時（`wys_runtime_finish` の前）: 別プロセスのプロバイダを閉じる。
#[unsafe(no_mangle)]
pub extern "C" fn wys_ai_finish() {
    STATE.with(|s| {
        if let Some(mut p) = s.borrow_mut().proxy.take() {
            drop(p.stdin);
            let _ = p.child.wait();
        }
    });
}
