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
    /// `@agent(max_cost_usd=)`（100 万分の 1 USD 単位。0 は上限なし。ADR-0025 論点 13）
    pub max_cost_micro_usd: i64,
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
    /// プロバイダの `usage` の合計（`max_tokens` は入力と出力の和で数える。ADR-0023 論点 13）
    usage: Usage,
    /// 費用の上限（USD。0 は上限なし）と、分かった費用の合計、費用の分からない応答が来たか（ADR-0025 論点 13）
    max_cost_usd: f64,
    cost_usd: f64,
    cost_unknown: bool,
    /// トークン数の分からない応答（`usage` の壊れた応答）が来たか。`max_tokens` のある agent は以後も超過（#143）
    tokens_unknown: bool,
    started: Instant,
    /// 入ったときのスケジューラの仮想時計（`WYS_VIRTUAL_TIME=1` のとき、ツールの中の `sleep_ms` も経過に数える）
    started_virtual_ms: i64,
    /// モックの応答の遅延の合計（どちらのモードでも待たずに時計を進める。スケジューラの時計は動かさない）
    virtual_ms: i64,
    /// `@agent(model=)`（空なら指定なし。要求の `model` が空のときに使う）
    model: String,
    /// このコンテキストを持つスタックの数（`spawn` で引き継ぐと増える）。0 になったら表の場所を再利用する
    refs: usize,
    /// トレースの agent のスパン（ADR-0025 論点 2。根は 0 = なし）と、その親・開始時刻
    span: u64,
    span_parent: u64,
    span_start: i64,
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
            usage: Usage::default(),
            max_cost_usd: 0.0,
            cost_usd: 0.0,
            cost_unknown: false,
            tokens_unknown: false,
            started: Instant::now(),
            started_virtual_ms: crate::task::virtual_now_ms(),
            virtual_ms: 0,
            model: String::new(),
            refs: 1,
            span: 0,
            span_parent: 0,
            span_start: 0,
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

/// トークンの使用量（入力と出力を分けて持つ。M6 のトレースと費用で使う）。
#[derive(Clone, Copy, Default, Debug, PartialEq)]
struct Usage {
    input_tokens: i64,
    output_tokens: i64,
}

impl Usage {
    fn total(self) -> i64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }

    fn add(&mut self, other: Usage) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
    }

    /// `usage` の JSON と、すべての数が正しかったか。欠けた数は 0。0 以上の整数でない数は 0 として数え、偽を返す
    /// （壊れた応答。負や小数で予算を迂回させない。正しいほうの数は数える。#143）
    fn parse(v: &serde_json::Value) -> (Usage, bool) {
        if !(v.is_null() || v.is_object()) {
            return (Usage::default(), false);
        }
        let mut ok = true;
        let mut field = |k: &str| match v.get(k) {
            None | Some(serde_json::Value::Null) => 0,
            Some(x) => x
                .as_u64()
                .and_then(|n| i64::try_from(n).ok())
                .unwrap_or_else(|| {
                    ok = false;
                    0
                }),
        };
        let usage = Usage {
            input_tokens: field("input_tokens"),
            output_tokens: field("output_tokens"),
        };
        (usage, ok)
    }

    fn from_json(v: &serde_json::Value) -> Usage {
        Self::parse(v).0
    }
}

/// 記録とモックの `latency_ms` の上限（1 日）。仮想時計の足し算が回り込まないように（#143）
const MAX_LATENCY_MS: i64 = 86_400_000;

/// プロバイダの 1 回の応答。
struct Reply {
    text: String,
    /// (呼び出しの ID, ツール名, 引数の JSON)
    tool_calls: Vec<(String, String, String)>,
    usage: Usage,
    /// プロバイダが添えた費用（USD。ADR-0025 論点 12）。モックと、費用を返さないプロバイダは `None`
    cost_usd: Option<f64>,
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

/// 記録の 1 回分の応答（ADR-0025 論点 6）。
#[derive(Clone)]
enum Outcome {
    /// 届いた JSON の応答（`error` 行と、`parse_reply` が拒む形を含む）
    Line(serde_json::Value),
    /// 届いた行が JSON でなかった（行そのものと、失敗の文言）
    Malformed(String, String),
    /// 応答が届かなかった（`the provider exited` など）
    Transport(String),
}

/// 記録の再生（`WYS_LLM_PROVIDER=replay:<path>`、ADR-0025 論点 7・8）。最初の呼び出しでファイルを全部読む
struct Replay {
    exchanges: Vec<Exchange>,
    next: usize,
    /// 一度失敗したら、以後の呼び出しも同じ失敗を返す
    failed: Option<String>,
}

/// 記録の 1 行（要求、応答、待ち時間）
struct Exchange {
    request: serde_json::Value,
    outcome: Outcome,
    latency_ms: i64,
}

/// 記録（`WYS_LLM_RECORD=<path>`、ADR-0025 論点 6）
#[derive(Default)]
struct Record {
    path: String,
    file: Option<std::fs::File>,
    seq: usize,
    failed: Option<String>,
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
    trace: Trace,
    /// 直前の応答の費用（トレースの `llm_call` のスパン用）
    last_cost: Option<f64>,
    replay: Option<Replay>,
    record: Record,
}

/// トレース（ADR-0025 論点 1〜4、`docs/design/ai-sdk.md` §18）。
struct Trace {
    /// `WYS_TRACE`（`<path>` か `stderr`。未設定なら出さない）
    dest: Option<String>,
    /// `WYS_TRACE_CONTENT=1`
    content: bool,
    next_span: u64,
    /// プロセス全体のモック（と再生）の応答の `latency_ms` の累計（トレースの時計に足す）
    latency_ms: i64,
    /// タスクごとの、いま開いているスパン（イベントをどのスパンに付けるか）
    active: HashMap<usize, Vec<u64>>,
    /// `WYS_TRACE=<path>` のファイル（最初の 1 行で開き、開けなければ以後は出さない）
    file: Option<std::fs::File>,
    file_failed: bool,
}

impl Trace {
    fn from_env() -> Trace {
        Trace {
            dest: std::env::var("WYS_TRACE").ok().filter(|d| !d.is_empty()),
            content: std::env::var("WYS_TRACE_CONTENT").is_ok_and(|v| v == "1"),
            next_span: 0,
            latency_ms: 0,
            active: HashMap::new(),
            file: None,
            file_failed: false,
        }
    }
}

/// トレースの時計の起点（ランタイムの初期化で取る。`trace_now_ms`）
static TRACE_START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// ランタイムの初期化（`wys_runtime_init`）で時計の起点を取る。
pub(crate) fn init_clock() {
    TRACE_START.get_or_init(Instant::now);
    // 記録はプロセスの最初に作り直す（LLM を呼ぶ前に落ちても、古い記録が残らないように。Fable のレビュー）。
    // 失敗は最初の LLM の呼び出しで `Err(kind="provider")` になる
    if record_path().is_some() {
        STATE.with(|cell| {
            let _ = record_prepare(&mut cell.borrow_mut());
        });
    }
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
            trace: Trace::from_env(),
            last_cost: None,
            replay: None,
            record: Record::default(),
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
    if s.trace.dest.is_some() {
        let span = current_span(s);
        let line = serde_json::json!({
            "kind": "event", "span": span_ref(span), "t_ms": trace_now_ms(s), "seq": s.audit.len(),
            "event": ev.kind, "agent": ev.agent, "tool": ev.tool, "detail": ev.detail
        });
        trace_write(s, &line);
    }
    s.audit.push(ev);
}

/// トレースの時計（ADR-0025 論点 3）: 有効な時計（仮想時計か、起点からの単調時計）に、プロセス全体の
/// モックと再生の `latency_ms` の累計を足したミリ秒。両方のモードで同じ式
fn trace_now_ms(s: &State) -> i64 {
    let base = if crate::task::virtual_time_enabled() {
        crate::task::virtual_now_ms()
    } else {
        let start = TRACE_START.get_or_init(Instant::now);
        i64::try_from(start.elapsed().as_millis()).unwrap_or(i64::MAX)
    };
    base.saturating_add(s.trace.latency_ms)
}

/// スパンの番号の JSON（0 はなし）
fn span_ref(span: u64) -> serde_json::Value {
    if span == 0 {
        serde_json::Value::Null
    } else {
        span.into()
    }
}

/// イベントを付けるスパン: このタスクで開いている最内のスパン（agent / `llm_call` / `tool_call`）。なければ
/// 現在のコンテキストの agent のスパン（`spawn` した子は親の agent のスパン）
fn current_span(s: &State) -> u64 {
    s.trace
        .active
        .get(&crate::task::current_task())
        .and_then(|v| v.last().copied())
        .unwrap_or_else(|| s.top().span)
}

fn new_span(s: &mut State) -> u64 {
    s.trace.next_span += 1;
    s.trace.next_span
}

/// `llm_call` / `tool_call` のスパンを開く（イベントはこのスパンに付く）。番号と開始時刻
fn open_span(s: &mut State) -> (u64, i64) {
    let id = new_span(s);
    s.trace
        .active
        .entry(crate::task::current_task())
        .or_default()
        .push(id);
    (id, trace_now_ms(s))
}

fn close_span(s: &mut State, id: u64) {
    if let Some(v) = s.trace.active.get_mut(&crate::task::current_task())
        && let Some(at) = v.iter().rposition(|&x| x == id)
    {
        v.remove(at);
    }
}

fn trace_write(s: &mut State, line: &serde_json::Value) {
    let t = &mut s.trace;
    let Some(dest) = &t.dest else {
        return;
    };
    let line = line.to_string();
    if dest == "stderr" {
        eprintln!("{line}");
        return;
    }
    if t.file.is_none() && !t.file_failed {
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dest)
        {
            Ok(f) => t.file = Some(f),
            Err(_) => t.file_failed = true,
        }
    }
    if let Some(f) = &mut t.file {
        let _ = writeln!(f, "{line}");
    }
}

/// 終わったスパンを 1 行出す（ADR-0025 論点 2）。`extra` は名前に固有の項目
fn emit_span(s: &mut State, id: u64, name: &str, start: i64, extra: serde_json::Value) {
    if s.trace.dest.is_none() {
        return;
    }
    let top = s.top();
    let mut line = serde_json::json!({
        "kind": "span", "span": id, "parent": span_ref(top.span), "task": crate::task::current_task(),
        "name": name, "agent": top.name, "tool": null, "start_ms": start, "end_ms": trace_now_ms(s)
    });
    if let (Some(o), serde_json::Value::Object(e)) = (line.as_object_mut(), extra) {
        o.extend(e);
    }
    trace_write(s, &line);
}

fn failure_result(s: &State) -> String {
    match &s.failure {
        Some((kind, _)) => format!("error:{kind}"),
        None => "ok".into(),
    }
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
/// 費用が分からないための超過（`over_budget` の理由。文言と監査の `detail` は `budget_message` / `budget_detail`）
const COST_UNKNOWN: &str = "max_cost_usd (cost unknown)";
/// トークン数の分からない応答の後の `max_tokens` の超過（#143）
const TOKENS_UNKNOWN: &str = "max_tokens (tokens unknown)";

/// 予算の超過の `message`（ADR-0025 論点 13: 費用が分からないときは `max_cost_usd exceeded (cost unknown)`）
fn budget_message(what: &str) -> String {
    if what == COST_UNKNOWN {
        "max_cost_usd exceeded (cost unknown)".into()
    } else if what == TOKENS_UNKNOWN {
        "max_tokens exceeded (tokens unknown)".into()
    } else {
        format!("{what} exceeded")
    }
}

/// 予算の超過の監査の `detail`（どの上限か）
fn budget_detail(what: &str) -> String {
    if what == COST_UNKNOWN {
        "max_cost_usd".into()
    } else if what == TOKENS_UNKNOWN {
        "max_tokens".into()
    } else {
        what.into()
    }
}

fn over_budget(s: &State, tools_next: bool) -> Option<(&'static str, &'static str)> {
    s.chain().into_iter().rev().find_map(|i| {
        let a = &s.arena[i];
        if a.timeout_ms > 0 && a.elapsed_ms() >= a.timeout_ms {
            Some(("timeout", "timeout_ms"))
        } else if a.max_tokens > 0 && a.usage.total() > a.max_tokens {
            Some(("budget", "max_tokens"))
        } else if a.max_tokens > 0 && a.tokens_unknown {
            // トークン数の分からない応答が一度でも来たら、以後も超過として扱う（閉じる方向。#143）
            Some(("budget", TOKENS_UNKNOWN))
        } else if a.max_cost_usd > 0.0 && a.cost_unknown {
            // 費用が分からない応答が一度でも来たら、以後も超過として扱う（閉じる方向。ADR-0025 論点 13）
            Some(("budget", COST_UNKNOWN))
        } else if a.max_cost_usd > 0.0 && a.cost_usd > a.max_cost_usd {
            Some(("budget", "max_cost_usd"))
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
        // タスクの番号は再利用されるので、開いたままのスパンを持ち越さない
        s.trace.active.remove(&id);
    });
}

/// 1 回のプロバイダの呼び出しと、その `llm_call` のスパン（ADR-0025 論点 2）。失敗なら `Err(null)`
fn provider_step(s: &mut State, call: &Call) -> Result<Reply, *mut Header> {
    let (id, start) = open_span(s);
    let before = (s.arena[0].llm_calls, s.arena[0].usage);
    let r = provider_step_inner(s, call);
    close_span(s, id);
    if s.trace.dest.is_some() {
        // 応答が届いた（根の回数が増えた）ときだけ使用量を持つ
        let usage = if s.arena[0].llm_calls > before.0 {
            let (a, b) = (s.arena[0].usage, before.1);
            serde_json::json!({
                "input_tokens": a.input_tokens - b.input_tokens,
                "output_tokens": a.output_tokens - b.output_tokens
            })
        } else {
            serde_json::Value::Null
        };
        let result = if r.is_ok() {
            "ok".to_string()
        } else {
            failure_result(s)
        };
        let cost = if s.arena[0].llm_calls > before.0 {
            s.last_cost
                .map_or(serde_json::Value::Null, serde_json::Value::from)
        } else {
            serde_json::Value::Null
        };
        let mut extra = serde_json::json!({"result": result, "usage": usage, "cost_usd": cost});
        if s.trace.content {
            extra["request"] = serde_json::json!({
                "system": call.req["system"], "messages": call.messages, "schema": call.schema
            });
            if let Ok(reply) = &r {
                let calls: Vec<serde_json::Value> = reply
                    .tool_calls
                    .iter()
                    .map(|(cid, name, args)| serde_json::json!({"id": cid, "name": name, "args": args}))
                    .collect();
                extra["reply"] = serde_json::json!({"text": reply.text, "tool_calls": calls});
            }
        }
        emit_span(s, id, "llm_call", start, extra);
    }
    r
}

/// プロバイダか記録から届いた応答を受け取る。届いた応答は、壊れていても回数とトークンを数える
/// （失敗を繰り返す呼び出しも予算で止まる。Codex のレビュー）。届かなかったら数えない。
/// `latency_ms` は時計に足す待ち時間（成功でも失敗でも、応答が届かなかったときも足す。Codex のレビュー）。
fn accept(s: &mut State, outcome: Outcome, latency_ms: i64) -> Result<Reply, *mut Header> {
    let parsed = match outcome {
        Outcome::Line(v) => parse_reply(&v),
        Outcome::Malformed(_, m) => Err(Rejected::new(m, Usage::default(), None)),
        Outcome::Transport(m) => {
            // 応答は届いていないので回数は数えないが、待った時間は数える（記録と再生を対称にする）
            for i in s.chain() {
                s.arena[i].virtual_ms = s.arena[i].virtual_ms.saturating_add(latency_ms);
            }
            s.trace.latency_ms = s.trace.latency_ms.saturating_add(latency_ms);
            return Err(fail(s, "provider", m));
        }
    };
    match parsed {
        Ok(mut r) => {
            r.latency_ms = latency_ms;
            Ok(r)
        }
        Err(Rejected {
            message: m,
            usage,
            cost,
            billed,
            tokens_unknown,
        }) => {
            for i in s.chain() {
                s.arena[i].tokens_unknown |= tokens_unknown;
                s.arena[i].llm_calls += 1;
                s.arena[i].usage.add(usage);
                s.arena[i].virtual_ms = s.arena[i].virtual_ms.saturating_add(latency_ms);
            }
            // `usage` がなければ課金はなかったとみなす。あれば費用が要る（ADR-0025 論点 13）
            charge(s, cost, billed);
            s.trace.latency_ms = s.trace.latency_ms.saturating_add(latency_ms);
            audit(s, "llm_called", None, format!("tokens={}", usage.total()));
            Err(fail(s, "provider", m))
        }
    }
}

/// 届いた応答の費用を、スタック上のすべての agent に数える。`billed` なのに費用が分からなければ、印を付ける
/// （上限のある agent では、以後の呼び出しも超過として扱う。ADR-0025 論点 13）
fn charge(s: &mut State, cost: Option<f64>, billed: bool) {
    s.last_cost = cost;
    for i in s.chain() {
        let a = &mut s.arena[i];
        a.cost_usd += cost.unwrap_or(0.0);
        if cost.is_none() && billed {
            a.cost_unknown = true;
        }
    }
}

/// モックの応答を、プロトコルの応答の形にする（記録用。`args_json` が JSON でなければ文字列のまま）
fn mock_reply_json(r: &Reply) -> serde_json::Value {
    let calls: Vec<serde_json::Value> = r
        .tool_calls
        .iter()
        .map(|(id, name, args)| {
            let args = serde_json::from_str::<serde_json::Value>(args)
                .unwrap_or_else(|_| serde_json::Value::String(args.clone()));
            serde_json::json!({"id": id, "name": name, "args": args})
        })
        .collect();
    serde_json::json!({
        "text": r.text, "tool_calls": calls,
        "usage": {"input_tokens": r.usage.input_tokens, "output_tokens": r.usage.output_tokens},
        "stop": r.stop, "assistant": r.assistant
    })
}

/// `WYS_LLM_PROVIDER=replay:<path>` の `<path>`
fn replay_source() -> Option<String> {
    std::env::var("WYS_LLM_PROVIDER")
        .ok()
        .and_then(|v| v.strip_prefix("replay:").map(str::to_string))
}

/// 再生のファイルを読む（最初の呼び出しで 1 回）。形が違えば失敗
fn replay_load(s: &mut State) -> Result<(), String> {
    if s.replay.is_some() {
        return Ok(());
    }
    let path = replay_source().unwrap_or_default();
    let bad = |why: String| format!("cannot read the recording {path}: {why}");
    let loaded = (|| -> Result<Replay, String> {
        let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let mut lines = text.lines().filter(|l| !l.trim().is_empty());
        let header: serde_json::Value =
            serde_json::from_str(lines.next().ok_or("the file is empty")?)
                .map_err(|e| e.to_string())?;
        if header["kind"] != "header" || header["v"] != 1 {
            return Err("the first line is not a version 1 header".into());
        }
        let mut exchanges = Vec::new();
        for (n, l) in lines.enumerate() {
            let v: serde_json::Value =
                serde_json::from_str(l).map_err(|e| format!("line {}: {e}", n + 2))?;
            if v["kind"] != "exchange" || !v["request"].is_object() {
                return Err(format!("line {}: not an exchange", n + 2));
            }
            // 応答の形は項目の有無で決める（JSON の型で決めると、`null` や文字列の行とぶつかる。Fable のレビュー）
            let outcome = if let Some(m) = v["transport"].as_str() {
                Outcome::Transport(m.to_string())
            } else if let Some(raw) = v["malformed"].as_str() {
                let m = match serde_json::from_str::<serde_json::Value>(raw) {
                    Err(e) => malformed_message(&e),
                    Ok(_) => "malformed provider response".into(),
                };
                Outcome::Malformed(raw.to_string(), m)
            } else if let Some(r) = v.get("reply") {
                Outcome::Line(r.clone())
            } else {
                return Err(format!("line {}: the exchange has no reply", n + 2));
            };
            exchanges.push(Exchange {
                request: v["request"].clone(),
                outcome,
                latency_ms: v["latency_ms"]
                    .as_i64()
                    .unwrap_or(0)
                    .clamp(0, MAX_LATENCY_MS),
            });
        }
        Ok(Replay {
            exchanges,
            next: 0,
            failed: None,
        })
    })();
    match loaded {
        Ok(r) => {
            s.replay = Some(r);
            Ok(())
        }
        Err(why) => {
            let m = bad(why);
            s.replay = Some(Replay {
                exchanges: Vec::new(),
                next: 0,
                failed: Some(m.clone()),
            });
            Err(m)
        }
    }
}

/// 要求の指紋（`v` と `timeout_ms` を除く）で、記録と違う最初の項目（ADR-0025 論点 7・8）
fn first_difference(recorded: &serde_json::Value, request: &serde_json::Value) -> Option<String> {
    for key in [
        "model",
        "system",
        "messages",
        "tools",
        "schema",
        "max_tokens",
    ] {
        let (a, b) = (&recorded[key], &request[key]);
        if a == b {
            continue;
        }
        if let (Some(xs), Some(ys)) = (a.as_array(), b.as_array()) {
            for i in 0..xs.len().max(ys.len()) {
                let (x, y) = (xs.get(i), ys.get(i));
                if x == y {
                    continue;
                }
                if let (Some(serde_json::Value::Object(xo)), Some(serde_json::Value::Object(yo))) =
                    (x, y)
                {
                    let mut names: Vec<&String> = xo.keys().chain(yo.keys()).collect();
                    names.sort();
                    names.dedup();
                    if let Some(k) = names.into_iter().find(|k| xo.get(*k) != yo.get(*k)) {
                        return Some(format!("{key}[{i}].{k}"));
                    }
                }
                return Some(format!("{key}[{i}]"));
            }
        }
        return Some(key.to_string());
    }
    None
}

/// 次の記録を返す。要求が記録と違う・尽きた・読めないなら失敗（`kind="replay"`。回数は数えない）
fn replay_next(s: &mut State, request: &serde_json::Value) -> Result<(Outcome, i64), String> {
    let r = s.replay.as_mut().expect("loaded before");
    if let Some(m) = &r.failed {
        return Err(m.clone());
    }
    let n = r.next;
    let Some(recorded) = r.exchanges.get(n).map(|e| &e.request) else {
        let m = format!("recording exhausted after {} exchanges", r.exchanges.len());
        r.failed = Some(m.clone());
        return Err(m);
    };
    if let Some(what) = first_difference(recorded, request) {
        let m = format!("request {}: {what} differs", n + 1);
        r.failed = Some(m.clone());
        return Err(m);
    }
    r.next += 1;
    let e = &r.exchanges[n];
    Ok((e.outcome.clone(), e.latency_ms))
}

/// `WYS_LLM_RECORD` の場所（未設定なら記録しない）
fn record_path() -> Option<String> {
    std::env::var("WYS_LLM_RECORD")
        .ok()
        .filter(|p| !p.is_empty())
}

/// 記録のファイルを作り直してヘッダを書く（最初の呼び出しで 1 回）。再生と同じファイルなら失敗
fn record_prepare(s: &mut State) -> Result<(), String> {
    let Some(path) = record_path() else {
        return Ok(());
    };
    if let Some(m) = &s.record.failed {
        return Err(m.clone());
    }
    if s.record.file.is_some() {
        return Ok(());
    }
    let result = (|| -> Result<std::fs::File, String> {
        if let Some(src) = replay_source() {
            let same = match (std::fs::canonicalize(&src), std::fs::canonicalize(&path)) {
                (Ok(a), Ok(b)) => a == b,
                _ => src == path,
            };
            if same {
                return Err("the recording path is also the replay source".into());
            }
        }
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).write(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(&path)
            .map_err(|e| format!("cannot write the recording {path}: {e}"))?;
        // `mode` は作るときにしか効かないので、既にあったファイルも 0600 にする（Fable のレビュー）
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| format!("cannot write the recording {path}: {e}"))?;
        }
        let header = serde_json::json!({
            "kind": "header", "v": 1, "provider": provider_kind(),
            "model": std::env::var("WYS_LLM_MODEL").ok()
        });
        writeln!(f, "{header}").map_err(|e| format!("cannot write the recording {path}: {e}"))?;
        Ok(f)
    })();
    match result {
        Ok(f) => {
            s.record.file = Some(f);
            s.record.path = path;
            Ok(())
        }
        Err(m) => {
            s.record.failed = Some(m.clone());
            Err(m)
        }
    }
}

fn provider_kind() -> &'static str {
    match std::env::var("WYS_LLM_PROVIDER")
        .unwrap_or_default()
        .as_str()
    {
        "" => "mock",
        "anthropic" => "anthropic",
        v if v.starts_with("cmd:") => "cmd",
        v if v.starts_with("replay:") => "replay",
        _ => "unknown",
    }
}

/// 1 回の要求と応答を記録する
fn record_write(
    s: &mut State,
    request: &serde_json::Value,
    outcome: &Outcome,
    latency_ms: i64,
) -> Result<(), String> {
    let Some(f) = &mut s.record.file else {
        return Ok(());
    };
    s.record.seq += 1;
    let mut line = serde_json::json!({
        "kind": "exchange", "seq": s.record.seq, "request": request, "latency_ms": latency_ms
    });
    match outcome {
        Outcome::Line(v) => line["reply"] = v.clone(),
        Outcome::Malformed(raw, _) => line["malformed"] = raw.as_str().into(),
        Outcome::Transport(m) => line["transport"] = m.as_str().into(),
    }
    let path = s.record.path.clone();
    writeln!(f, "{line}").map_err(|e| {
        let m = format!("cannot write the recording {path}: {e}");
        s.record.failed = Some(m.clone());
        m
    })
}

/// 1 回のプロバイダの呼び出し（予算の検査、使用量の加算、監査）。失敗なら `Err(null)`
fn provider_step_inner(s: &mut State, call: &Call) -> Result<Reply, *mut Header> {
    if let Some((kind, what)) = over_budget(s, false) {
        audit(s, "budget_exceeded", None, budget_detail(what));
        return Err(fail(s, kind, budget_message(what)));
    }
    let replaying = replay_source().is_some();
    if replaying && let Err(m) = replay_load(s) {
        return Err(fail(s, "replay", m));
    }
    if let Err(m) = record_prepare(s) {
        return Err(fail(s, "provider", m));
    }
    let request = request_json(s, call);
    let reply = if replaying {
        let (outcome, latency) = match replay_next(s, &request) {
            Ok(x) => x,
            Err(m) => return Err(fail(s, "replay", m)),
        };
        if let Err(m) = record_write(s, &request, &outcome, latency) {
            return Err(fail(s, "provider", m));
        }
        accept(s, outcome, latency)?
    } else if provider_configured() {
        let started = Instant::now();
        let outcome = match exchange(s, &request.to_string()) {
            Ok(Ok(v)) => Outcome::Line(v),
            Ok(Err((raw, m))) => Outcome::Malformed(raw, m),
            Err(m) => Outcome::Transport(m),
        };
        let waited = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
        if let Err(m) = record_write(s, &request, &outcome, waited) {
            return Err(fail(s, "provider", m));
        }
        // 実時間のモードでは、実際に待った時間を時計がすでに数えている。仮想時計のモードでは数えないので、
        // 待った時間を足す（記録の `latency_ms` を再生で足すのと対称にする。Fable のレビュー）
        let counted = if crate::task::virtual_time_enabled() {
            waited
        } else {
            0
        };
        accept(s, outcome, counted)?
    } else {
        let Some(reply) = s.mock.pop_front() else {
            return Err(fail(s, "mock_exhausted", "no mock response left".into()));
        };
        if let Err(m) = record_write(
            s,
            &request,
            &Outcome::Line(mock_reply_json(&reply)),
            reply.latency_ms,
        ) {
            return Err(fail(s, "provider", m));
        }
        reply
    };
    for i in s.chain() {
        let a = &mut s.arena[i];
        a.llm_calls += 1;
        a.usage.add(reply.usage);
        a.virtual_ms = a.virtual_ms.saturating_add(reply.latency_ms);
    }
    charge(s, reply.cost_usd, true);
    s.trace.latency_ms = s.trace.latency_ms.saturating_add(reply.latency_ms);
    audit(
        s,
        "llm_called",
        None,
        format!("tokens={}", reply.usage.total()),
    );
    // 期限を過ぎて届いた応答は受け取らず、要求されたツールも呼ばない
    if let Some((kind, what)) = over_budget(s, false).filter(|(_, w)| *w != "max_llm_calls") {
        audit(s, "budget_exceeded", None, budget_detail(what));
        return Err(fail(s, kind, budget_message(what)));
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
fn request_json(s: &State, call: &Call) -> serde_json::Value {
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
fn exchange(
    s: &mut State,
    line: &str,
) -> Result<Result<serde_json::Value, (String, String)>, String> {
    use std::io::{BufRead, Write as _};
    if matches!(s.provider, Provider::Idle) {
        let setting = std::env::var("WYS_LLM_PROVIDER").unwrap_or_default();
        let (program, args): (String, &[&str]) = if setting == "anthropic" {
            (proxy_program(s), &["llm-proxy"])
        } else if let Some(path) = setting.strip_prefix("cmd:") {
            (path.to_string(), &[])
        } else {
            s.provider = Provider::Broken(format!(
                "unknown WYS_LLM_PROVIDER value `{setting}` (use anthropic, cmd:<path> or replay:<path>)"
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
    let raw = reply.trim_end_matches(['\n', '\r']).to_string();
    Ok(serde_json::from_str(&raw).map_err(|e| (raw.clone(), malformed_message(&e))))
}

fn malformed_message(e: &serde_json::Error) -> String {
    format!("malformed provider response: {e}")
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

/// 拒んだ応答: 理由、数えるトークン数と費用、課金があったとみなすか（`billed` なのに費用が分からなければ、
/// 上限のある agent は以後も超過として扱う。ADR-0025 論点 13）
struct Rejected {
    message: String,
    usage: Usage,
    cost: Option<f64>,
    billed: bool,
    tokens_unknown: bool,
}

impl Rejected {
    /// `usage` があれば課金があったとみなす
    fn new(message: String, usage: Usage, cost: Option<f64>) -> Self {
        let billed = usage.total() > 0;
        Rejected {
            message,
            usage,
            cost,
            billed,
            tokens_unknown: false,
        }
    }
}

/// プロバイダの応答の 1 行を読む（`text` / `tool_calls` / `usage` / `stop` だけを解釈する）。`error` 行は `Err`。
/// 失敗なら理由と、`error` 行に `usage` があればそのトークン数（拒否でも入力は課金されるので予算に数える）。
fn parse_reply(v: &serde_json::Value) -> Result<Reply, Rejected> {
    // 費用は任意。あれば 0 以上の有限の数（ADR-0025 論点 12・13）
    let cost = match v.get("cost_usd") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(c) => match c.as_f64() {
            Some(x) if x.is_finite() && x >= 0.0 => Ok(Some(x)),
            _ => Err(()),
        },
    };
    let (usage, usage_ok) = Usage::parse(&v["usage"]);
    if !usage_ok {
        // トークン数が分からない: 正しいほうの数と正しい `cost_usd` は数え、課金はあったとみなし（費用がなければ
        // 「分からない」）、トークン数も「分からない」として `max_tokens` を閉じる（Codex のレビュー）
        return Err(Rejected {
            message: "malformed provider response: `usage` must hold non-negative integers".into(),
            usage,
            cost: cost.unwrap_or(None),
            billed: true,
            tokens_unknown: true,
        });
    }
    // `error` は文字列のときだけ失敗（`"error": null` は成功の応答に付く慣習なので無視する。Fable のレビュー）
    if let Some(e) = v["error"].as_str() {
        let Ok(cost) = cost else {
            return Err(Rejected::new(
                "malformed provider response: `cost_usd` must be a non-negative number".into(),
                usage,
                None,
            ));
        };
        return Err(Rejected::new(e.to_string(), usage, cost));
    }
    // 壊れた応答でも、正しい `cost_usd` があれば費用は数える（Fable のレビュー）
    let known = cost.unwrap_or(None);
    let malformed = |what: &str| {
        Err(Rejected::new(
            format!("malformed provider response: {what}"),
            usage,
            known,
        ))
    };
    if !v.is_object() || !v["text"].is_string() {
        return malformed("`text` is missing");
    }
    let Ok(cost) = cost else {
        return malformed("`cost_usd` must be a non-negative number");
    };
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
        usage,
        cost_usd: cost,
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
    STATE.with(|cell| {
        let mut s = cell.borrow_mut();
        stop_provider(&mut s);
        // LLM を呼ばなかったら、ヘッダだけの記録を作る（評価ケースの記録の欠けと区別する。ADR-0025 論点 6）
        if s.record.file.is_none() && s.record.failed.is_none() {
            let _ = record_prepare(&mut s);
        }
    });
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
        audit(s, "budget_exceeded", Some(name), budget_detail(what));
        return Gate::Stop(fail(s, kind, budget_message(what)));
    }
    if s.tools[ti].approval {
        match approve(s, name, args) {
            Ok(true) => {}
            Ok(false) => return Gate::Refused,
            Err(p) => return Gate::Stop(p),
        }
        // 人の答えを待つ間に期限を過ぎたかもしれない（実時間のモード）
        if let Some((kind, what)) = over_budget(s, true) {
            audit(s, "budget_exceeded", Some(name), budget_detail(what));
            return Gate::Stop(fail(s, kind, budget_message(what)));
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
            // `tool_call` のスパン（ADR-0025 論点 2）。拒否されたものも 1 つ出す
            let (span, start) = STATE.with(|cell| open_span(&mut cell.borrow_mut()));
            let end = |result: &str, output: Option<&str>| {
                STATE.with(|cell| {
                    let mut s = cell.borrow_mut();
                    close_span(&mut s, span);
                    let mut extra = serde_json::json!({"tool": name, "result": result});
                    if s.trace.content {
                        extra["args"] = args.as_str().into();
                        if let Some(o) = output {
                            extra["output"] = o.into();
                        }
                    }
                    emit_span(&mut s, span, "tool_call", start, extra);
                });
            };
            let failed = || STATE.with(|cell| failure_result(&cell.borrow()));
            let g = STATE.with(|cell| gate(&mut cell.borrow_mut(), name, args));
            let entry = match g {
                Gate::Stop(p) => {
                    end(&failed(), None);
                    return p;
                }
                Gate::Refused => {
                    end("refused", None);
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
                let p = STATE.with(|cell| {
                    let mut s = cell.borrow_mut();
                    audit(&mut s, "schema_rejected", Some(name), why);
                    fail(&mut s, "schema", msg)
                });
                end("error:schema", None);
                return p;
            }
            let is_error = TOOL_ERR.with(|c| c.replace(false));
            end(if is_error { "tool_error" } else { "ok" }, Some(&result));
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
        usage: Usage::from_json(&v),
        cost_usd: None,
        latency_ms: v["latency_ms"]
            .as_i64()
            .unwrap_or(0)
            .clamp(0, MAX_LATENCY_MS),
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

/// `llm_usage()` の本体: 現在のコンテキスト（根なら全体）の使用量の JSON（参照カウント 1。ADR-0025 論点 5）。
#[unsafe(no_mangle)]
pub extern "C" fn wys_usage_json() -> *mut Header {
    STATE.with(|s| {
        let s = s.borrow();
        let c = s.top();
        let v = serde_json::json!({
            "llm_calls": c.llm_calls, "tool_calls": c.tool_calls,
            "input_tokens": c.usage.input_tokens, "output_tokens": c.usage.output_tokens,
            "cost_usd": c.cost_usd
        });
        embed::new_object(v.to_string().as_bytes())
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
        ctx.max_cost_usd = c.max_cost_micro_usd as f64 / 1_000_000.0;
        ctx.span_parent = s.top().span;
        ctx.span = new_span(&mut s);
        ctx.span_start = trace_now_ms(&s);
        // ツールの中で入った agent のイベントが、外側の `tool_call` に付かないように積む（Fable のレビュー）
        let span = ctx.span;
        s.trace
            .active
            .entry(crate::task::current_task())
            .or_default()
            .push(span);
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
        let i = *chain.last().expect("checked above");
        // agent のスパン（ADR-0025 論点 2）: 結果は持たず、出る時点のその agent の合計を持つ
        let c = &s.arena[i];
        let extra = serde_json::json!({
            "parent": span_ref(c.span_parent),
            "usage": {"input_tokens": c.usage.input_tokens, "output_tokens": c.usage.output_tokens},
            "cost_usd": c.cost_usd
        });
        let (span, start) = (c.span, c.span_start);
        close_span(&mut s, span);
        emit_span(&mut s, span, "agent", start, extra);
        chain.pop();
        s.unref(i);
        s.stacks.insert(task, chain);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_names_the_first_difference() {
        let base = serde_json::json!({
            "v": 1, "model": "m", "system": "s", "timeout_ms": 10, "max_tokens": 0, "schema": null,
            "messages": [{"role": "user", "content": "a"}],
            "tools": [{"name": "add", "description": "d", "input_schema": {}}]
        });
        // `v` と `timeout_ms` は指紋に入らない
        let mut other = base.clone();
        other["timeout_ms"] = 99.into();
        other["v"] = 2.into();
        assert_eq!(first_difference(&base, &other), None);
        let mut other = base.clone();
        other["messages"][0]["content"] = "b".into();
        assert_eq!(
            first_difference(&base, &other).as_deref(),
            Some("messages[0].content")
        );
        let mut other = base.clone();
        other["messages"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"role": "user", "content": "c"}));
        assert_eq!(
            first_difference(&base, &other).as_deref(),
            Some("messages[1]")
        );
        let mut other = base.clone();
        other["tools"][0]["name"] = "sub".into();
        assert_eq!(
            first_difference(&base, &other).as_deref(),
            Some("tools[0].name")
        );
        let mut other = base.clone();
        other["schema"] = serde_json::json!({"type": "object"});
        other["model"] = "n".into();
        assert_eq!(first_difference(&base, &other).as_deref(), Some("model"));
    }

    #[test]
    fn replies_keep_input_and_output_tokens_apart() {
        let v = serde_json::json!({"text": "x", "usage": {"input_tokens": 7, "output_tokens": 3}});
        let r = parse_reply(&v).ok().unwrap();
        assert_eq!(
            r.usage,
            Usage {
                input_tokens: 7,
                output_tokens: 3
            }
        );
        assert_eq!(r.usage.total(), 10);
        let e = serde_json::json!({"error": "refused", "usage": {"input_tokens": 5}});
        let u = parse_reply(&e).err().unwrap().usage;
        assert_eq!(
            u,
            Usage {
                input_tokens: 5,
                output_tokens: 0
            }
        );
        let mut sum = Usage {
            input_tokens: i64::MAX,
            output_tokens: 1,
        };
        sum.add(Usage {
            input_tokens: 1,
            output_tokens: 1,
        });
        assert_eq!(sum.total(), i64::MAX);
    }

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
