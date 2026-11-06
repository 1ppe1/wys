//! AI Runtime（ADR-0023、M5 T8〜T9）: `llm_generate` の本体、モックのプロバイダ、agent コンテキスト、ツールの実行、監査。
//!
//! - 生成コードとは JSON の文字列で受け渡す（ランタイムはレコードの配置を知らない）。要求は `LlmRequest` の JSON、
//!   モックの応答は `MockResponse` の JSON、監査イベントは JSON の配列で返し、生成コードが `validate` の glue で型にする。
//! - agent コンテキストは**タスクごとのスタック**（ADR-0023 の訂正）。底は根のコンテキスト（ツールなし、Capability は
//!   すべて ∩ `WYS_CAPABILITIES`、予算なし）。`spawn` した子タスクは親のスタックを引き継ぐ。コンテキストの実体は
//!   共有の表（`arena`）にあり、スタックは添字の並び。予算の検査と加算はスタック上のすべての agent に行う。
//! - LLM がツールを要求したら、実行の前に一覧 → Capability → 予算 → 承認の順に検査する（論点 10）。承認は
//!   `WYS_APPROVAL`（`deny` 既定 / `allow` / `prompt`、論点 12）。ツールはコンパイラが合成したトランポリンを呼ぶ。トランポリンの中でタスクが
//!   切り替わり得るので、ランタイムの状態の借りはトランポリンを呼ぶ前に必ず返す。

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::ffi::{CStr, c_char};
use std::io::Write;
use std::time::Instant;

use crate::{Header, embed};

/// Effect / Capability のビット（Capability の名前は Effect の名前と同じ。ADR-0023 論点 7）。
/// バックエンドの `effect_bit` と同じ値にする
pub const CAP_IO: u64 = 1;
pub const CAP_EXIT: u64 = 2;
pub const CAP_PYTHON: u64 = 4;
pub const CAP_LLM: u64 = 8;
const CAP_ALL: u64 = CAP_IO | CAP_EXIT | CAP_PYTHON | CAP_LLM;

/// コンパイラが生成するツール表の 1 行（`@wys_tools`）。
#[repr(C)]
pub struct ToolDesc {
    pub name: *const c_char,
    pub description: *const c_char,
    /// 引数のレコードの JSON Schema（T11 でプロバイダに渡す）
    pub schema: *const c_char,
    /// ツールの `uses`
    pub effects: u64,
    /// 1 なら承認が要る
    pub approval: i64,
    /// トランポリン: JSON の引数（+0）を受け取り、結果の JSON（+1）を返す。引数が検証を通らなければ
    /// `wys_tool_args_rejected` を呼んで空の文字列を返す。結果が `Err` なら `wys_tool_err` を呼ぶ
    pub entry: extern "C" fn(args: *const Header) -> *mut Header,
}

/// `@agent(...)` の属性（`@.agent.*`）。
#[repr(C)]
pub struct AgentCfg {
    pub name: *const c_char,
    /// ツール表の番号の並び
    pub tools: *const i64,
    pub n_tools: i64,
    pub caps: u64,
    pub max_llm_calls: i64,
    pub max_tool_calls: i64,
    pub max_tokens: i64,
    pub timeout_ms: i64,
    /// `@agent(model=)`（空なら指定なし）
    pub model: *const c_char,
}

struct Tool {
    name: String,
    description: String,
    /// 引数のレコードの JSON Schema（プロバイダに渡す）
    schema: serde_json::Value,
    effects: u64,
    approval: bool,
    entry: extern "C" fn(*const Header) -> *mut Header,
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
    /// 入ったときのスケジューラの仮想時計（`WYS_VIRTUAL_TIME=1` のとき、ツールの中の `sleep_ms` も経過に数える）
    started_virtual_ms: i64,
    /// モックの応答の遅延の合計（どちらのモードでも待たずに時計を進める。スケジューラの時計は動かさない）
    virtual_ms: i64,
    /// `@agent(model=)`（空なら指定なし。要求の `model` が空のときに使う）
    model: String,
    /// このコンテキストを持つスタックの数（`spawn` で引き継ぐと増える）。0 になったら表の場所を再利用する
    refs: usize,
}

impl Ctx {
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
            started_virtual_ms: crate::task::virtual_now_ms(),
            virtual_ms: 0,
            model: String::new(),
            refs: 1,
        }
    }

    fn elapsed_ms(&self) -> i64 {
        // 実時間のモードは単調時計、仮想時間のモードはスケジューラの仮想時計（ADR-0023 論点 14）
        let clock = if crate::task::virtual_time_enabled() {
            crate::task::virtual_now_ms() - self.started_virtual_ms
        } else {
            self.started.elapsed().as_millis() as i64
        };
        clock + self.virtual_ms
    }
}

/// プロバイダの 1 回の応答。
struct Reply {
    text: String,
    /// (呼び出しの ID, ツール名, 引数の JSON)
    tool_calls: Vec<(String, String, String)>,
    tokens: i64,
    latency_ms: i64,
    /// 終わった理由（`max_tokens` なら切れている）。モックは空
    stop: String,
    /// 次の要求の `assistant` の `content` にそのまま戻す値（不透明。ADR-0023 論点 2）
    assistant: serde_json::Value,
}

/// 外部のプロバイダ（`WYS_LLM_PROVIDER`、ADR-0023 論点 1・4）。最初の `llm_generate` で起動し、プロセスに 1 つ。
enum Provider {
    Idle,
    Running {
        child: std::process::Child,
        stdin: std::process::ChildStdin,
        stdout: std::io::BufReader<std::process::ChildStdout>,
    },
    /// 起動できなかった・終わった。以後の呼び出しも同じ理由で失敗する
    Broken(String),
}

struct AuditEvent {
    kind: &'static str,
    agent: String,
    tool: Option<String>,
    detail: String,
}

struct State {
    tools: Vec<Tool>,
    /// コンテキストの実体（0 は根）
    arena: Vec<Ctx>,
    free: Vec<usize>,
    /// タスクの番号 → コンテキストの添字の並び（底が根）。登録がなければ `[0]`
    stacks: HashMap<usize, Vec<usize>>,
    mock: VecDeque<Reply>,
    audit: Vec<AuditEvent>,
    failure: Option<(&'static str, String)>,
    provider: Provider,
    /// ビルドしたコンパイラの絶対パス（生成コードが `wys_llm_set_compiler` で渡す。`wys llm-proxy` の探索に使う）
    compiler: Option<String>,
}

impl State {
    fn new() -> State {
        let mut root = Ctx::new(String::new(), Vec::new(), CAP_ALL & deployed_caps(), [0; 4]);
        root.refs = usize::MAX / 2;
        State {
            tools: Vec::new(),
            arena: vec![root],
            free: Vec::new(),
            stacks: HashMap::new(),
            mock: VecDeque::new(),
            audit: Vec::new(),
            failure: None,
            provider: Provider::Idle,
            compiler: None,
        }
    }

    fn chain(&self) -> Vec<usize> {
        self.stacks
            .get(&crate::task::current_task())
            .cloned()
            .unwrap_or_else(|| vec![0])
    }

    fn top(&self) -> &Ctx {
        let chain = self.chain();
        &self.arena[*chain.last().expect("the root is always there")]
    }

    fn alloc(&mut self, c: Ctx) -> usize {
        match self.free.pop() {
            Some(i) => {
                self.arena[i] = c;
                i
            }
            None => {
                self.arena.push(c);
                self.arena.len() - 1
            }
        }
    }

    fn unref(&mut self, i: usize) {
        if i == 0 {
            return;
        }
        self.arena[i].refs -= 1;
        if self.arena[i].refs == 0 {
            self.free.push(i);
        }
    }
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::new());
    /// トランポリンが立てる印（ツールの呼び出しの前に落とす）
    static TOOL_ERR: Cell<bool> = const { Cell::new(false) };
    /// `Some(真)` なら引数が型に合わなかった、`Some(偽)` なら JSON として壊れていた
    static TOOL_ARGS_REJECTED: Cell<Option<bool>> = const { Cell::new(None) };
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

fn cap_name(c: u64) -> &'static str {
    match c {
        CAP_IO => "io",
        CAP_EXIT => "exit",
        CAP_PYTHON => "python",
        _ => "llm",
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

/// # Safety
/// `p` は null か生存している文字列オブジェクトでなければならない（トランポリンは不死の定数を返すことがある）。
unsafe fn release_str(p: *mut Header) {
    // SAFETY: 関数の安全条件どおり。`wys_release` は不死のオブジェクトを数えない。
    unsafe { crate::wys_release(p) }
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
    s.chain().into_iter().rev().find_map(|i| {
        let a = &s.arena[i];
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

/// 入口の再検査（ADR-0023 論点 11）: 現在のコンテキストに Capability がなければ監査 `capability_denied` を残して偽。
pub fn capability_granted(cap: u64, entry: &str) -> bool {
    STATE.with(|cell| {
        let mut s = cell.borrow_mut();
        if s.top().caps & cap != 0 {
            return true;
        }
        let detail = format!("capability {} is not granted", cap_name(cap));
        audit(&mut s, "capability_denied", Some(entry), detail);
        false
    })
}

/// スケジューラが子タスクを始めるとき: 親のスタックを引き継ぐ（ADR-0023 の訂正）。
pub(crate) fn task_spawned(parent: usize, child: usize) {
    STATE.with(|cell| {
        let mut s = cell.borrow_mut();
        let chain = s.stacks.get(&parent).cloned().unwrap_or_else(|| vec![0]);
        for &i in &chain {
            if i != 0 {
                s.arena[i].refs += 1;
            }
        }
        if let Some(old) = s.stacks.insert(child, chain) {
            for i in old {
                s.unref(i);
            }
        }
    });
}

/// タスクが終わったとき: 引き継いだ分を返す（自分で入った agent はすべて出ているはず）。
pub(crate) fn task_finished(id: usize) {
    STATE.with(|cell| {
        let mut s = cell.borrow_mut();
        if let Some(chain) = s.stacks.remove(&id) {
            for i in chain {
                s.unref(i);
            }
        }
    });
}

/// 1 回のプロバイダの呼び出し（予算の検査、使用量の加算、監査）。失敗なら `Err(null)`
fn provider_step(s: &mut State, call: &Call) -> Result<Reply, *mut Header> {
    if let Some((kind, what)) = over_budget(s, false) {
        audit(s, "budget_exceeded", None, what.into());
        return Err(fail(s, kind, format!("{what} exceeded")));
    }
    let reply = if provider_configured() {
        let line = request_line(s, call);
        match exchange(s, &line) {
            // 応答の行が届いた（JSON でなくても、届いた応答として数える。Codex のレビュー）
            Ok(v) => match v.map_err(|m| (m, 0)).and_then(|v| parse_reply(&v)) {
                Ok(r) => r,
                Err((m, tokens)) => {
                    // 応答は届いたので、回数とトークンを数える（失敗を繰り返す呼び出しも予算で止まる）
                    for i in s.chain() {
                        s.arena[i].llm_calls += 1;
                        s.arena[i].tokens += tokens;
                    }
                    audit(s, "llm_called", None, format!("tokens={tokens}"));
                    return Err(fail(s, "provider", m));
                }
            },
            Err(m) => return Err(fail(s, "provider", m)),
        }
    } else {
        let Some(reply) = s.mock.pop_front() else {
            return Err(fail(s, "mock_exhausted", "no mock response left".into()));
        };
        reply
    };
    for i in s.chain() {
        let a = &mut s.arena[i];
        a.llm_calls += 1;
        a.tokens += reply.tokens;
        a.virtual_ms += reply.latency_ms;
    }
    audit(s, "llm_called", None, format!("tokens={}", reply.tokens));
    // 期限を過ぎて届いた応答は受け取らず、要求されたツールも呼ばない
    if let Some((kind, what)) = over_budget(s, false).filter(|(_, w)| *w != "max_llm_calls") {
        audit(s, "budget_exceeded", None, what.into());
        return Err(fail(s, kind, format!("{what} exceeded")));
    }
    Ok(reply)
}

/// `prompt` の答えを 1 行読む（改行を含まない）。EOF で何も読めなければ `None`。
/// fd 0 から 1 バイトずつ読み、先読みしない: `std::io::stdin()` のバッファは 1 行より多く読み込むので、
/// 残りが Python のツール（`sys.stdin`）など、fd を直接読むものから見えなくなる（Fable のレビュー）。
fn read_answer() -> Option<String> {
    use std::io::Read;
    use std::os::fd::FromRawFd;
    // SAFETY: fd 0 はプロセスの標準入力。ManuallyDrop で閉じない。
    let mut fd0 = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(0) });
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    loop {
        match fd0.read(&mut b) {
            Ok(0) => break,
            Ok(_) if b[0] == b'\n' => return Some(String::from_utf8_lossy(&line).into_owned()),
            Ok(_) => line.push(b[0]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    (!line.is_empty()).then(|| String::from_utf8_lossy(&line).into_owned())
}

/// 承認の方針（ADR-0023 論点 12）。許可なら真、拒否なら偽、答えられない（未知の値・EOF）なら `Err`。
/// 監査は `approval_requested` → `approval_granted` / `approval_denied`。引数の本文は監査に残さない。
fn approve(s: &mut State, name: &str, args: &str) -> Result<bool, *mut Header> {
    let policy = std::env::var("WYS_APPROVAL").unwrap_or_default();
    let policy = if policy.is_empty() {
        "deny".to_string()
    } else {
        policy
    };
    let detail = format!("WYS_APPROVAL={policy}");
    audit(s, "approval_requested", Some(name), detail.clone());
    let granted = match policy.as_str() {
        "deny" => false,
        "allow" => true,
        "prompt" => {
            let agent = s.top().name.clone();
            let mut err = std::io::stderr();
            let _ = write!(
                err,
                "approve tool {name} (agent {agent}) with args {args}? [y/N] "
            );
            let _ = err.flush();
            let answer = read_answer();
            // 端末でなければ答えはエコーされないので、続く出力（監査の行など）が問いの行に連ならないように改行する
            if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                let _ = writeln!(err);
            }
            let line = match answer {
                None => {
                    audit(
                        s,
                        "approval_denied",
                        Some(name),
                        "no answer (end of input)".into(),
                    );
                    return Err(fail(
                        s,
                        "approval",
                        format!("no answer to the approval of tool `{name}` (end of input)"),
                    ));
                }
                Some(line) => line,
            };
            matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
        }
        other => {
            audit(s, "approval_denied", Some(name), detail);
            return Err(fail(
                s,
                "approval",
                format!("unknown WYS_APPROVAL value `{other}` (use deny, allow or prompt)"),
            ));
        }
    };
    let kind = if granted {
        "approval_granted"
    } else {
        "approval_denied"
    };
    audit(s, kind, Some(name), detail);
    Ok(granted)
}

/// 1 回の `llm_generate` の要求の内容（プロバイダへの 1 行を組み立てる材料）。
struct Call {
    req: serde_json::Value,
    /// 出力スキーマ（`T = str` なら null）
    schema: serde_json::Value,
    messages: Vec<serde_json::Value>,
}

/// プロバイダへの要求の 1 行（ADR-0023 論点 2・3）。
fn request_line(s: &State, call: &Call) -> String {
    let top = s.top();
    let model = [
        call.req["model"].as_str().unwrap_or_default().to_string(),
        top.model.clone(),
        std::env::var("WYS_LLM_MODEL").unwrap_or_default(),
    ]
    .into_iter()
    .find(|m| !m.is_empty())
    .unwrap_or_default();
    let tools: Vec<serde_json::Value> = top
        .tools
        .iter()
        .map(|&i| {
            let t = &s.tools[i];
            serde_json::json!({"name": t.name, "description": t.description, "input_schema": t.schema})
        })
        .collect();
    // 残り時間はスタック上の agent の最小（0 = 期限なし）。期限の前の検査を通ったので 1 以上
    let timeout_ms = s
        .chain()
        .into_iter()
        .map(|i| &s.arena[i])
        .filter(|a| a.timeout_ms > 0)
        .map(|a| (a.timeout_ms - a.elapsed_ms()).max(1))
        .min()
        .unwrap_or(0);
    serde_json::json!({
        "v": 1,
        "model": model,
        "system": call.req["system"].as_str().unwrap_or_default(),
        "messages": call.messages,
        "tools": tools,
        "schema": call.schema,
        "max_tokens": call.req["max_tokens"].as_i64().unwrap_or(0).max(0),
        "timeout_ms": timeout_ms,
    })
    .to_string()
}

/// `wys llm-proxy` の場所: `WYS_LLM_PROXY` → ビルドしたコンパイラ → `PATH` の `wys`（ADR-0023 論点 4）。
fn proxy_program(s: &State) -> String {
    if let Some(p) = std::env::var("WYS_LLM_PROXY")
        .ok()
        .filter(|p| !p.is_empty())
    {
        return p;
    }
    s.compiler
        .clone()
        .filter(|p| std::path::Path::new(p).is_file())
        .unwrap_or_else(|| "wys".into())
}

/// 要求の 1 行を送り、応答の 1 行を読む。起動していなければ起動する。外側の `Err` は応答が届かなかった
/// （起動できない・子が終わった）、内側の `Err` は届いた行が JSON でなかった。
fn exchange(s: &mut State, line: &str) -> Result<Result<serde_json::Value, String>, String> {
    use std::io::{BufRead, Write as _};
    if matches!(s.provider, Provider::Idle) {
        let setting = std::env::var("WYS_LLM_PROVIDER").unwrap_or_default();
        let (program, args): (String, &[&str]) = if setting == "anthropic" {
            (proxy_program(s), &["llm-proxy"])
        } else if let Some(path) = setting.strip_prefix("cmd:") {
            (path.to_string(), &[])
        } else {
            s.provider = Provider::Broken(format!(
                "unknown WYS_LLM_PROVIDER value `{setting}` (use anthropic or cmd:<path>)"
            ));
            return exchange(s, line);
        };
        // パスで指したファイルは、起動の前に実行できるかを確かめる（エミュレーションの下などで、存在しない
        // ファイルの起動が失敗として返らず、子がすぐ終わるだけのことがある。Linux の CI で見つけた）
        if let Err(why) = check_executable(&program) {
            s.provider = Provider::Broken(format!("cannot start the provider `{program}`: {why}"));
            return exchange(s, line);
        }
        // シェルを通さない。stderr は継承する（proxy は要求や応答を書かない）
        let started = std::process::Command::new(&program)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn();
        s.provider = match started {
            Ok(mut child) => {
                let stdin = child.stdin.take().expect("piped");
                let stdout = std::io::BufReader::new(child.stdout.take().expect("piped"));
                Provider::Running {
                    child,
                    stdin,
                    stdout,
                }
            }
            Err(e) => Provider::Broken(format!("cannot start the provider `{program}`: {e}")),
        };
    }
    let Provider::Running { stdin, stdout, .. } = &mut s.provider else {
        let Provider::Broken(m) = &s.provider else {
            unreachable!("started above")
        };
        return Err(m.clone());
    };
    let mut reply = String::new();
    // 子が stdin を閉じていたら、書き込みは SIGPIPE でプログラムごと落とす（生成した実行ファイルは C の main で、
    // Rust の main のように SIGPIPE を無視していない）。書く間だけ無視して、EPIPE として扱う
    let written = without_sigpipe(|| writeln!(stdin, "{line}").and_then(|()| stdin.flush()));
    let ok = written.is_ok() && stdout.read_line(&mut reply).is_ok_and(|n| n > 0);
    if !ok {
        stop_provider(s);
        s.provider = Provider::Broken("the provider exited".into());
        return Err("the provider exited".into());
    }
    Ok(serde_json::from_str(&reply).map_err(|e| format!("malformed provider response: {e}")))
}

const SIGPIPE: i32 = 13;
const SIG_IGN: usize = 1;

unsafe extern "C" {
    fn signal(sig: i32, handler: usize) -> usize;
}

/// `f` の間だけ SIGPIPE を無視する（閉じたパイプへの書き込みは EPIPE のエラーになる）。
fn without_sigpipe<R>(f: impl FnOnce() -> R) -> R {
    // SAFETY: SIGPIPE の扱いを無視に変え、終わったら元に戻す。ランタイムはこの間にほかのスレッドで書かない
    let old = unsafe { signal(SIGPIPE, SIG_IGN) };
    let r = f();
    // SAFETY: 同上。
    unsafe { signal(SIGPIPE, old) };
    r
}

/// `/` を含むパス（`cmd:<path>`、`WYS_LLM_PROXY` など）が実行できるファイルか。`PATH` で探す名前は起動に任せる。
fn check_executable(program: &str) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt as _;
    if !program.contains('/') {
        return Ok(());
    }
    let meta = std::fs::metadata(program).map_err(|e| e.to_string())?;
    if !meta.is_file() || meta.permissions().mode() & 0o111 == 0 {
        return Err("not an executable file".into());
    }
    Ok(())
}

/// プロバイダの応答の 1 行を読む（`text` / `tool_calls` / `usage` / `stop` だけを解釈する）。`error` 行は `Err`。
/// 失敗なら理由と、`error` 行に `usage` があればそのトークン数（拒否でも入力は課金されるので予算に数える）。
fn parse_reply(v: &serde_json::Value) -> Result<Reply, (String, i64)> {
    let usage = &v["usage"];
    let tokens =
        usage["input_tokens"].as_i64().unwrap_or(0) + usage["output_tokens"].as_i64().unwrap_or(0);
    // `error` は文字列のときだけ失敗（`"error": null` は成功の応答に付く慣習なので無視する。Fable のレビュー）
    if let Some(e) = v["error"].as_str() {
        return Err((e.to_string(), tokens));
    }
    let malformed = |what: &str| Err((format!("malformed provider response: {what}"), tokens));
    if !v.is_object() || !v["text"].is_string() {
        return malformed("`text` is missing");
    }
    let mut tool_calls = Vec::new();
    for c in v["tool_calls"].as_array().map_or(&[][..], Vec::as_slice) {
        let (Some(id), Some(name)) = (c["id"].as_str(), c["name"].as_str()) else {
            return malformed("a tool call needs `id` and `name`");
        };
        tool_calls.push((id.to_string(), name.to_string(), c["args"].to_string()));
    }
    let text = v["text"].as_str().unwrap_or_default().to_string();
    // `assistant` がなければ `text` を戻す（null を次の要求に入れると、Messages API は拒否する）
    let assistant = match &v["assistant"] {
        serde_json::Value::Null => serde_json::Value::String(text.clone()),
        a => a.clone(),
    };
    Ok(Reply {
        text,
        tool_calls,
        tokens,
        latency_ms: 0,
        stop: v["stop"].as_str().unwrap_or_default().to_string(),
        assistant,
    })
}

/// 子プロセスの stdin を閉じて終わりを待つ。
fn stop_provider(s: &mut State) {
    if let Provider::Running {
        mut child, stdin, ..
    } = std::mem::replace(&mut s.provider, Provider::Idle)
    {
        drop(stdin);
        // stdin の終わりを無視する子で、プログラムの終了が止まらないように、猶予の後に止める（Fable のレビュー）
        let deadline = Instant::now() + std::time::Duration::from_secs(2);
        while matches!(child.try_wait(), Ok(None)) {
            if Instant::now() >= deadline {
                let _ = child.kill();
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = child.wait();
    }
}

/// 終了時（`wys_runtime_finish`）: プロバイダの子プロセスを片付ける。
pub(crate) fn shutdown() {
    STATE.with(|cell| stop_provider(&mut cell.borrow_mut()));
}

/// 生成コード: ビルドしたコンパイラの絶対パス（`llm_generate` を使うプログラムだけが呼ぶ）。
///
/// # Safety
/// `path` は NUL 終端の文字列定数。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_llm_set_compiler(path: *const c_char) {
    // SAFETY: 関数の安全条件どおり。
    let p = unsafe { cstr(path) };
    STATE.with(|cell| cell.borrow_mut().compiler = Some(p));
}

/// ツールの要求の検査（一覧 → Capability → 予算 → 承認）。実行してよければトランポリンを返す
enum Gate {
    Run(extern "C" fn(*const Header) -> *mut Header),
    /// 承認が拒否された（LLM に失敗として返して続ける）
    Refused,
    Stop(*mut Header),
}

fn gate(s: &mut State, name: &str, args: &str) -> Gate {
    let top = s.top();
    let caps = top.caps;
    let Some(ti) = top.tools.iter().copied().find(|&i| s.tools[i].name == name) else {
        audit(
            s,
            "tool_denied",
            Some(name),
            "not in the agent's tools".into(),
        );
        return Gate::Stop(fail(
            s,
            "denied",
            format!("tool `{name}` is not allowed for this agent"),
        ));
    };
    let missing = s.tools[ti].effects & !caps;
    if missing != 0 {
        let names: Vec<&str> = [CAP_IO, CAP_EXIT, CAP_PYTHON, CAP_LLM]
            .into_iter()
            .filter(|c| missing & c != 0)
            .map(cap_name)
            .collect();
        let detail = format!("missing capability {}", names.join(", "));
        audit(s, "tool_denied", Some(name), detail);
        return Gate::Stop(fail(
            s,
            "denied",
            format!("tool `{name}` needs capabilities the agent does not have"),
        ));
    }
    if let Some((kind, what)) = over_budget(s, true) {
        audit(s, "budget_exceeded", Some(name), what.into());
        return Gate::Stop(fail(s, kind, format!("{what} exceeded")));
    }
    if s.tools[ti].approval {
        match approve(s, name, args) {
            Ok(true) => {}
            Ok(false) => return Gate::Refused,
            Err(p) => return Gate::Stop(p),
        }
        // 人の答えを待つ間に期限を過ぎたかもしれない（実時間のモード）
        if let Some((kind, what)) = over_budget(s, true) {
            audit(s, "budget_exceeded", Some(name), what.into());
            return Gate::Stop(fail(s, kind, format!("{what} exceeded")));
        }
    }
    for i in s.chain() {
        s.arena[i].tool_calls += 1;
    }
    Gate::Run(s.tools[ti].entry)
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
    // SAFETY: 関数の安全条件どおり。出力スキーマは実プロバイダに渡す（`T = str` なら空）。モックは使わない
    let (req, schema) = unsafe { (text(req), text(schema)) };
    let first = STATE.with(|cell| {
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
            return Err(fail(
                &mut s,
                "denied",
                "the `llm` capability is not granted".into(),
            ));
        }
        // 2. 要求
        let req: serde_json::Value = match serde_json::from_str(&req) {
            Ok(v) => v,
            Err(e) => return Err(fail(&mut s, "provider", format!("invalid request: {e}"))),
        };
        if let Err(m) = check_request(&req) {
            return Err(fail(&mut s, "provider", m));
        }
        Ok(req)
    });
    let req = match first {
        Ok(r) => r,
        Err(p) => return p,
    };
    // 会話（外部のプロバイダに毎回すべてを送る。モックは読まない）
    let messages: Vec<serde_json::Value> = req["messages"].as_array().cloned().unwrap_or_default();
    let schema = serde_json::from_str(&schema).unwrap_or(serde_json::Value::Null);
    let mut call = Call {
        req,
        schema,
        messages,
    };
    loop {
        let reply = match STATE.with(|cell| provider_step(&mut cell.borrow_mut(), &call)) {
            Ok(r) => r,
            Err(p) => return p,
        };
        let messages = &mut call.messages;
        if reply.tool_calls.is_empty() {
            if reply.stop == "max_tokens" {
                return STATE.with(|cell| {
                    let mut s = cell.borrow_mut();
                    fail(&mut s, "provider", "response truncated: max_tokens".into())
                });
            }
            // ツールの中の `llm_generate` が失敗を残していても、この呼び出しは成功
            STATE.with(|cell| cell.borrow_mut().failure = None);
            return embed::new_object(reply.text.as_bytes());
        }
        messages.push(serde_json::json!({"role": "assistant", "content": reply.assistant}));
        for (id, name, args) in &reply.tool_calls {
            let g = STATE.with(|cell| gate(&mut cell.borrow_mut(), name, args));
            let entry = match g {
                Gate::Stop(p) => return p,
                Gate::Refused => {
                    messages.push(serde_json::json!({
                        "role": "tool", "id": id, "content": "denied by approver", "is_error": true
                    }));
                    continue;
                }
                Gate::Run(e) => e,
            };
            STATE.with(|cell| {
                let mut s = cell.borrow_mut();
                audit(
                    &mut s,
                    "tool_called",
                    Some(name),
                    format!("args_len={}", args.len()),
                );
            });
            // 借りを返してから呼ぶ（ツールの中でタスクが切り替わり、ほかのタスクがランタイムを使う）
            TOOL_ERR.with(|c| c.set(false));
            TOOL_ARGS_REJECTED.with(|c| c.set(None));
            let arg = embed::new_object(args.as_bytes());
            let out = entry(arg);
            // SAFETY: arg は作ったばかりの参照カウント 1 の文字列。
            unsafe { release_str(arg) };
            // SAFETY: out はトランポリンが返した参照カウント 1 の文字列。
            let result = unsafe { text(out) };
            // SAFETY: 同上。
            unsafe { release_str(out) };
            // 印は読んだ瞬間に落とす（トランポリンが立ててから、ここで読むまでに中断はない）。落とさないと、
            // ツールの中の llm_generate や別のタスクのツールの印が、後で読むツールの結果に持ち越す（Fable のレビュー）
            if let Some(schema_mismatch) = TOOL_ARGS_REJECTED.with(Cell::take) {
                let why = if schema_mismatch {
                    crate::schema::last_failure_message()
                } else {
                    crate::json::last_failure_message()
                };
                let msg = format!("tool `{name}`: {why}");
                return STATE.with(|cell| {
                    let mut s = cell.borrow_mut();
                    audit(&mut s, "schema_rejected", Some(name), why);
                    fail(&mut s, "schema", msg)
                });
            }
            let is_error = TOOL_ERR.with(|c| c.replace(false));
            messages.push(serde_json::json!({
                "role": "tool", "id": id, "content": result, "is_error": is_error
            }));
        }
    }
}

/// トランポリン: ツールが `Err` を返した。
#[unsafe(no_mangle)]
pub extern "C" fn wys_tool_err() {
    TOOL_ERR.with(|c| c.set(true));
}

/// トランポリン: 引数が検証を通らなかった（`schema` が真なら型の不一致、偽なら壊れた JSON）。
#[unsafe(no_mangle)]
pub extern "C" fn wys_tool_args_rejected(schema: bool) {
    TOOL_ARGS_REJECTED.with(|c| c.set(Some(schema)));
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
        stop: String::new(),
        assistant: serde_json::Value::String(v["text"].as_str().unwrap_or_default().to_string()),
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
            // SAFETY: 同上。
            description: unsafe { cstr(d.description) },
            // SAFETY: 同上。スキーマはバックエンドが `json_schema` で書いた JSON
            schema: serde_json::from_str(&unsafe { cstr(d.schema) }).unwrap_or_default(),
            effects: d.effects,
            approval: d.approval != 0,
            entry: d.entry,
        })
        .collect();
    STATE.with(|s| s.borrow_mut().tools = list);
}

/// `@agent` の本体に入る。Capability は外側との積に縮める。
///
/// # Safety
/// `cfg` は有効な `AgentCfg` を指さなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_agent_enter(cfg: *const AgentCfg) {
    // SAFETY: 関数の安全条件どおり。
    let c = unsafe { &*cfg };
    let tools = if c.n_tools == 0 {
        Vec::new()
    } else {
        // SAFETY: 同上。
        unsafe { std::slice::from_raw_parts(c.tools, c.n_tools as usize) }
            .iter()
            .map(|&i| i as usize)
            .collect()
    };
    // SAFETY: 生成コードの文字列定数。
    let name = unsafe { cstr(c.name) };
    STATE.with(|cell| {
        let mut s = cell.borrow_mut();
        let caps = c.caps & s.top().caps;
        let limits = [
            c.max_llm_calls,
            c.max_tool_calls,
            c.max_tokens,
            c.timeout_ms,
        ];
        let mut ctx = Ctx::new(name, tools, caps, limits);
        // SAFETY: 生成コードの文字列定数。
        ctx.model = unsafe { cstr(c.model) };
        let i = s.alloc(ctx);
        let task = crate::task::current_task();
        let mut chain = s.chain();
        chain.push(i);
        s.stacks.insert(task, chain);
    });
}

/// `@agent` の本体を出る（すべての出口で）。根のコンテキストは下ろせない。
#[unsafe(no_mangle)]
pub extern "C" fn wys_agent_exit() {
    STATE.with(|cell| {
        let mut s = cell.borrow_mut();
        let task = crate::task::current_task();
        let mut chain = s.chain();
        if chain.len() <= 1 {
            embed::runtime_error("agent context mismatch (this is a compiler bug)");
        }
        let i = chain.pop().expect("checked above");
        s.unref(i);
        s.stacks.insert(task, chain);
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

    #[test]
    fn spawned_tasks_inherit_and_release_the_parent_stack() {
        let mut s = State::new();
        let i = s.alloc(Ctx::new("outer".into(), Vec::new(), CAP_LLM, [0; 4]));
        s.stacks.insert(1, vec![0, i]);
        // 子タスク 2 は親 1 のスタックを引き継ぐ
        let chain = s.stacks[&1].clone();
        for &j in &chain {
            if j != 0 {
                s.arena[j].refs += 1;
            }
        }
        s.stacks.insert(2, chain);
        assert_eq!(s.arena[i].refs, 2);
        // 親が出ても、子が持っている間は再利用しない
        let mut c1 = s.stacks[&1].clone();
        let top = c1.pop().unwrap();
        s.unref(top);
        s.stacks.insert(1, c1);
        assert!(s.free.is_empty());
        for j in s.stacks.remove(&2).unwrap() {
            s.unref(j);
        }
        assert_eq!(s.free, vec![i]);
    }
}
