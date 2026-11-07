//! 共通IR（COMP-001）の定義・検証・ダンプ
//!
//! 設計（PoC1 時点）:
//! - 関数は基本ブロックの列。各ブロックは命令列と1つの終端命令を持つ。
//! - ローカル変数（`LocalId`）は可変のスロット。一時値（`TempId`）は一度だけ定義され、
//!   **定義したブロック内でのみ** 使える。ブロックをまたぐ値はローカルを経由する。
//! - 整数演算（Add/Sub/Mul/FloorDiv/Mod）はオーバーフローと0除算を実行時に検査する意味論を持つ。
//! - 値を持たない（`None` 型の）式は一時値を作らない。
//! - Union はタグ付き値（ADR-0009）。`Program::unions` に構成要素を登録し、構成要素の番号がタグになる。
//!   `None` の構成要素は中身（payload）を持たない。

pub mod arc;

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ty {
    Int,
    Bool,
    Str,
    /// IEEE 754 の倍精度（ADR-0019、M4 T12）。値型で、LLVM では `double`
    Float,
    Union(UnionId),
    Record(RecordId),
    /// `list[T]`（ヒープ上のリストへのポインタ1語、M3 T4a）
    List(ListId),
    /// `dict[K, V]`（ヒープ上の辞書へのポインタ1語、M3 T6a、ADR-0015）
    Dict(DictId),
    /// Python のオブジェクトのハンドル（ヒープオブジェクトへのポインタ1語、ADR-0018 論点 6、M4 T5）。
    /// 最後の release が Python の参照を手放す。
    Py,
    /// `Task[T]`（タスクのオブジェクトへのポインタ1語、ADR-0020）。結果の型は `Program::tasks`。
    /// 参照カウントの操作は型ごとの関数（最後の release が結果を release してからランタイムに返す）
    Task(TaskId),
    /// 未検証の外部値 `Unknown`（ADR-0011、ADR-0021 論点 3）。タグ 1 バイト + 1 語（Union と同じ配置）。
    /// 種類（タグ）は `UNKNOWN_KINDS` の順。`list` / `dict` の種類は `Program::unknown` の型のオブジェクトを指す
    Unknown,
}

/// `Unknown` の種類（タグの番号の順、ADR-0021 論点 3）。今までの 5 種類の Union と同じ番号を保ち、`list` / `dict` を足す。
/// ランタイム（`wys_runtime::json`）のタグの定数と一致させる
pub const UNKNOWN_KINDS: [&str; 7] = ["int", "bool", "str", "float", "None", "list", "dict"];

impl Ty {
    /// Union 型の ID。Union でなければパニックする。
    pub fn union_id(self) -> UnionId {
        match self {
            Ty::Union(u) => u,
            other => panic!("{other:?} is not a union"),
        }
    }

    pub fn name(self) -> String {
        match self {
            Ty::Int => "int".into(),
            Ty::Bool => "bool".into(),
            Ty::Str => "str".into(),
            Ty::Float => "float".into(),
            Ty::Union(u) => format!("u{}", u.0),
            Ty::Record(r) => format!("r{}", r.0),
            Ty::List(l) => format!("list{}", l.0),
            Ty::Dict(d) => format!("dict{}", d.0),
            Ty::Py => "py".into(),
            Ty::Task(k) => format!("task{}", k.0),
            Ty::Unknown => "unknown".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct UnionId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ListId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DictId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TaskId(pub u32);

/// 不変のレコード（ADR-0009: 値型、受け渡しはコピー）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RecordId(pub u32);

/// `@tool` の属性（ADR-0022 論点 13）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolAttr {
    pub description: String,
    pub approval: bool,
    /// トランポリン（`__wys_tool_entry_<名前>`、lowering が合成する。ADR-0022 論点 15）
    pub entry: Option<FuncId>,
    /// 引数を 1 つのレコードとみなした型（JSON Schema と検証に使う）
    pub arg_record: Option<RecordId>,
}

/// `@agent` の属性（ADR-0022 論点 14）。`tools` は関数の名前（IR の関数名）。予算の `0` は上限なし。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentAttr {
    pub tools: Vec<String>,
    pub capabilities: Vec<Effect>,
    pub model: String,
    pub max_llm_calls: i64,
    pub max_tool_calls: i64,
    pub max_tokens: i64,
    pub timeout_ms: i64,
    /// `max_cost_usd`（100 万分の 1 USD 単位。0 は上限なし）
    pub max_cost_micro_usd: i64,
}

/// Union の構成要素。`None` は中身を持たない構成要素。
pub type Member = Option<Ty>;

fn member_name(m: Member) -> String {
    m.map_or("None".into(), Ty::name)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LocalId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TempId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FuncId(pub u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StrId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub strings: Vec<String>,
    /// Union 型の構成要素。`Ty::Union(UnionId(i))` は `unions[i]` を指す。構成要素の Union（`Result` の表現）は `i` より前に登録したもの。
    pub unions: Vec<Vec<Member>>,
    /// レコード型のフィールド（名前と型、この順で配置する）。`Ty::Record(RecordId(i))` は `records[i]` を指す。
    pub records: Vec<Vec<(String, Ty)>>,
    /// `list[T]` の要素の型。`Ty::List(ListId(i))` は `lists[i]` を指す。
    pub lists: Vec<Ty>,
    /// `dict[K, V]` のキーと値の型。`Ty::Dict(DictId(i))` は `dicts[i]` を指す。キーは `int` / `bool` / `str`。
    pub dicts: Vec<(Ty, Ty)>,
    /// `Task[T]` の結果の型（`None` なら結果を持たない `Task[None]`）。`Ty::Task(TaskId(i))` は `tasks[i]` を指す。
    pub tasks: Vec<Option<Ty>>,
    /// `Unknown` のコンテナの種類の型（`list[Unknown]` と `dict[str, Unknown]`）。`Unknown` を使うプログラムだけ `Some`
    pub unknown: Option<(ListId, DictId)>,
    /// 表示用の情報（`print`、ADR-0016 論点 16）。型の同一性や配置には関係しない。
    pub display: DisplayInfo,
    pub funcs: Vec<Function>,
    /// エントリポイント（引数なし・戻り値なし）。
    pub entry: FuncId,
}

impl Program {
    /// `Unknown` の種類ごとの構成要素（`UNKNOWN_KINDS` の順）。`Unknown` を使わないプログラムではパニックする
    pub fn unknown_members(&self) -> [Member; 7] {
        let (l, d) = self.unknown.expect("the program uses `Unknown`");
        [
            Some(Ty::Int),
            Some(Ty::Bool),
            Some(Ty::Str),
            Some(Ty::Float),
            None,
            Some(Ty::List(l)),
            Some(Ty::Dict(d)),
        ]
    }
}

/// 表示用の情報。IR の型は構造だけを持つので、`print` に要る名前をここに置く。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DisplayInfo {
    /// レコード型の名前（`records` と同じ順）。最初に定義した `type` の名前で、型引数は含まない。
    pub record_names: Vec<String>,
    /// `Result` を表す Union（構成要素 0 が `Ok`、1 が `Err`）。
    pub results: Vec<UnionId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    pub name: String,
    /// 先頭 `param_count` 個のローカルが引数。
    pub param_count: u32,
    pub locals: Vec<Ty>,
    pub ret: Option<Ty>,
    pub temps: Vec<Ty>,
    pub blocks: Vec<Block>,
    /// 関数の Effect: `uses` 節で宣言した集合（ADR-0014）か、注釈がなければ推論した集合（ADR-0024。`effects_inferred`）。
    /// `uses pure` と推論した空の集合は空。lowering が合成する関数（ツールのトランポリン、`wys test` の入口）は `None`。
    /// バックエンドはツール表の Effect のビットに使う（`@tool` は `uses` が必須なので、常に宣言した集合）。
    pub effects: Option<Vec<Effect>>,
    /// `effects` が推論した集合か（`dump-ir` では `uses(inferred) io`）。
    pub effects_inferred: bool,
    /// `@tool(...)` の属性（ADR-0022 論点 13・15）。T9 でツール表を作る。いまはバックエンドは読まない
    pub tool: Option<ToolAttr>,
    /// `@agent(...)` の属性（ADR-0022 論点 14・15）。T9 で `wys_agent_enter` / `wys_agent_exit` を入れる
    pub agent: Option<AgentAttr>,
    /// Move 最適化（`arc::optimize`、M4 T3）が取り除いた retain / release の記録。`wys explain` が報告する
    /// （ADR-0017 の見直し条件）。バックエンドは読まない。
    pub elided_rc: Vec<ElidedRcOp>,
    /// `async def`（ADR-0020）。中断点（`await`、`sleep_ms`）と `spawn` はこの関数の中にだけ書ける（verify が確かめる）
    pub is_async: bool,
}

/// Move 最適化が取り除いた参照カウントの操作（M4 T3）。挿入した理由（`reason`）と取り除けた理由（`elision`）の両方を持つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElidedRcOp {
    /// `true` なら retain、`false` なら release
    pub retain: bool,
    pub reason: RcReason,
    /// 操作の対象だった一時値（取り除いた後の IR には残らないことがある）
    pub src: TempId,
    /// 操作があったブロックと、最適化の前の命令の位置
    pub block: u32,
    pub index: usize,
    /// 対象がローカル変数のスロットから読んだ値なら、そのスロット
    pub local: Option<LocalId>,
    pub elision: ElisionReason,
}

/// 参照カウントの操作を取り除けた理由（M4 T3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ElisionReason {
    /// 引数のスロットに関数の中で代入しないので、スロットは呼び出し側の参照を借りるだけでよい
    ParamNeverReassigned,
    /// 可変な場所から読んだ値を、使い終わるまでに場所を変え得る命令（関数の呼び出し、要素の書き換え、release）がない
    BorrowGuardUnneeded,
    /// ローカル変数の最後の使用で、スロットから所有権を移した（`take`）。スロットは空になる
    MovedFromSlot,
}

impl ElisionReason {
    pub fn id(self) -> &'static str {
        match self {
            ElisionReason::ParamNeverReassigned => "param_never_reassigned",
            ElisionReason::BorrowGuardUnneeded => "borrow_guard_unneeded",
            ElisionReason::MovedFromSlot => "moved_from_slot",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            ElisionReason::ParamNeverReassigned => {
                "the parameter is never reassigned, so its slot can borrow the caller's reference"
            }
            ElisionReason::BorrowGuardUnneeded => {
                "nothing that could change the list or dict runs before the value's last use"
            }
            ElisionReason::MovedFromSlot => {
                "this is the last use of the local, so its reference is moved out of the slot instead of copied"
            }
        }
    }
}

/// 関数の Effect（ADR-0014）。型検査の `Effect` と同じ種類を持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Effect {
    Io,
    Exit,
    /// Python のコードの実行（ADR-0018 論点 12）
    Python,
    /// LLM の呼び出し（ADR-0022 / 0023）
    Llm,
}

impl Effect {
    pub fn name(self) -> &'static str {
        match self {
            Effect::Io => "io",
            Effect::Exit => "exit",
            Effect::Python => "python",
            Effect::Llm => "llm",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub insts: Vec<Inst>,
    pub term: Terminator,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Const {
    Int(i64),
    Bool(bool),
    Str(StrId),
    /// `f64` のビット列（`Eq` を保つため。`f64::from_bits` で戻す）
    Float(u64),
    /// 型の JSON Schema の文字列（不死の `str`。バックエンドが `json_schema` で作る。ADR-0022 論点 5）
    Schema(Ty),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    /// `float` の割り算（ADR-0019 論点 4）。0 での割り算は実行時エラー
    Div,
    FloorDiv,
    Mod,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl BinOp {
    pub fn name(self) -> &'static str {
        match self {
            BinOp::Add => "add",
            BinOp::Sub => "sub",
            BinOp::Mul => "mul",
            BinOp::Div => "div",
            BinOp::FloorDiv => "floordiv",
            BinOp::Mod => "mod",
            BinOp::Eq => "eq",
            BinOp::Ne => "ne",
            BinOp::Lt => "lt",
            BinOp::Le => "le",
            BinOp::Gt => "gt",
            BinOp::Ge => "ge",
        }
    }

    pub fn is_comparison(self) -> bool {
        matches!(
            self,
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    PrintInt,
    PrintBool,
    PrintStr,
    /// `float` の表示と改行（Python の `repr` と同じ書式、ADR-0019 論点 6）
    PrintFloat,
    /// `float(n)`（`int` から）
    FloatFromInt,
    /// `int(x)`（0 の方向に切り捨て。NaN・無限大・範囲外は実行時エラー、ADR-0019 論点 3）
    IntFromFloat,
    /// 構造を持つ値（レコード・`list`・`dict`・Union）の表示と改行（ADR-0016 論点 15〜17）。
    /// 引数は1つで、型は問わない。値は借用で読む。
    PrintValue,
    Exit,
    /// 外部入力（ADR-0011）。`arg` と `parse_scalar` は lowering がこれらを組み合わせて作る。
    ArgCount,
    ArgExists,
    /// `ArgExists` が真の番号にだけ使う。
    ArgStr,
    /// 0 = int、1 = bool、2 = str、3 = null、4 = float（ADR-0019）
    ScalarKind,
    ScalarInt,
    ScalarBool,
    /// `ScalarKind` が 4（float）の文字列の値（ADR-0019 論点 7）
    ScalarFloat,
    /// `wys test` で実行するテストの番号
    TestIndex,
    /// `assert` の失敗（引数は場所の文字列）。戻らない。
    AssertFail,
    /// `panic(msg)`（引数はメッセージと場所の文字列）。戻らない（ADR-0013）。
    Panic,
    /// 文字列の連結。新しいオブジェクトを +1 所有で返す（M3 T2b）
    StrConcat,
    /// 文字列の中身の比較
    StrEq,
    /// 文字列の長さ（文字数 = Unicode のコードポイント数）
    StrLen,
    // Python（ADR-0018、M4 T5）。失敗し得る操作の後は `PyFailed` で確かめ、失敗なら `PyErr*` で理由を読む。
    // 失敗したときの戻り値（null のハンドルなど）は使わない
    /// `import name`
    PyImport,
    /// `h.name`
    PyGetAttr,
    /// `h(*args)`（`args` はハンドルの `list`）
    PyCall,
    PyFromInt,
    PyFromBool,
    PyFromStr,
    PyFromFloat,
    /// `py_value` の `list` / `dict` / Union（型ごとの変換関数をバックエンドが生成する。引数の型は問わない。M4 T7）
    PyFromValue,
    PyNone,
    PyToInt,
    PyToBool,
    PyToStr,
    PyToFloat,
    /// 戻り値は `list[py]`（型の ID は呼び出しの dst で決まる。verify が確かめる）
    PyToList,
    /// 戻り値は `dict[str, py]`（同上）
    PyToDict,
    /// `h(*args, **kwargs)`（`args` は `list[py]`、`kwargs` は `dict[str, py]`）
    PyCallKw,
    /// 失敗しない
    PyIsNone,
    /// 直前の Python の操作が失敗したか
    PyFailed,
    /// 直前の失敗の例外の型名・メッセージ・トレースバック（`PyError` のフィールド）
    PyErrKind,
    PyErrMessage,
    PyErrTraceback,
    /// `sleep_ms(n)`: 現在のタスクを止める中断点（ADR-0020 論点 10）。`async` の関数の中だけ
    SleepMs,
    // JSON（ADR-0021 論点 5）。`JsonParse` の後は `JsonFailed` で確かめ、失敗なら `JsonErr*` で理由を読む
    /// `parse_json` の本体: 成功なら木を +1 所有で返す（失敗なら `None` の種類）
    JsonParse,
    JsonFailed,
    JsonErrMessage,
    JsonErrOffset,
    // スキーマ型（ADR-0022 論点 6・7）。glue はバックエンドが型ごとに作る
    /// `validate[T](u)`: `Unknown` の木を型 `T`（dst の一時値の型）として検証し、+1 所有の値を返す（失敗ならゼロの値）。
    /// 後で `SchemaFailed` を確かめ、失敗なら `SchemaErr*` で理由を読む
    Validate,
    SchemaFailed,
    SchemaErrPath,
    SchemaErrExpected,
    SchemaErrFound,
    /// `to_json(v)`: スキーマ型の値（型は問わない。verify が確かめる）を JSON の文字列にする
    ToJson,
    // AI SDK（ADR-0022 論点 8・10、ADR-0023）。ランタイムとは JSON の文字列で受け渡す（ランタイムは型の配置を知らない）
    /// `llm_generate`: 要求（`LlmRequest` の JSON）と出力スキーマ（空ならなし）を受け取り、本文を +1 所有で返す
    /// （失敗なら null。`LlmFailed` で確かめる）。中断点。`async` の関数の中だけ
    LlmGenerate,
    LlmFailed,
    LlmErrKind,
    LlmErrMessage,
    /// 直前の `parse_json` / `validate` の失敗の理由（監査 `schema_rejected` を残す）
    LlmJsonRejected,
    LlmSchemaRejected,
    /// `llm_mock_push`（`MockResponse` の JSON）
    LlmMockPush,
    /// `audit_events` の本体: 監査イベントの JSON の配列
    AuditJson,
    /// `llm_usage` の本体: 現在のコンテキストの使用量の JSON（ADR-0025 論点 5）
    UsageJson,
    /// `@agent` の本体に入る・出る（関数の `agent` の属性を、バックエンドが定数にして渡す。ADR-0023 論点 9）
    AgentEnter,
    AgentExit,
    /// トランポリン: ツールが `Err` を返した（結果は失敗として LLM に返す）
    ToolErr,
    /// トランポリン: 引数が検証を通らなかった（引数は `bool`: 偽なら JSON として壊れていた、真なら型に合わなかった）
    ToolArgsRejected,
}

impl Builtin {
    pub fn name(self) -> &'static str {
        match self {
            Builtin::PrintInt => "print_int",
            Builtin::PrintBool => "print_bool",
            Builtin::PrintStr => "print_str",
            Builtin::PrintFloat => "print_float",
            Builtin::FloatFromInt => "float_from_int",
            Builtin::IntFromFloat => "int_from_float",
            Builtin::PrintValue => "print_value",
            Builtin::Exit => "exit",
            Builtin::ArgCount => "arg_count",
            Builtin::ArgExists => "arg_exists",
            Builtin::ArgStr => "arg_str",
            Builtin::ScalarKind => "scalar_kind",
            Builtin::ScalarInt => "scalar_int",
            Builtin::ScalarBool => "scalar_bool",
            Builtin::ScalarFloat => "scalar_float",
            Builtin::TestIndex => "test_index",
            Builtin::AssertFail => "assert_fail",
            Builtin::Panic => "panic",
            Builtin::StrConcat => "str_concat",
            Builtin::StrEq => "str_eq",
            Builtin::StrLen => "str_len",
            Builtin::PyImport => "py_import",
            Builtin::PyGetAttr => "py_getattr",
            Builtin::PyCall => "py_call",
            Builtin::PyFromInt => "py_from_int",
            Builtin::PyFromBool => "py_from_bool",
            Builtin::PyFromStr => "py_from_str",
            Builtin::PyFromFloat => "py_from_float",
            Builtin::PyFromValue => "py_from_value",
            Builtin::PyToFloat => "py_to_float",
            Builtin::PyToList => "py_to_list",
            Builtin::PyToDict => "py_to_dict",
            Builtin::PyCallKw => "py_call_kw",
            Builtin::PyNone => "py_none",
            Builtin::PyToInt => "py_to_int",
            Builtin::PyToBool => "py_to_bool",
            Builtin::PyToStr => "py_to_str",
            Builtin::PyIsNone => "py_is_none",
            Builtin::PyFailed => "py_failed",
            Builtin::PyErrKind => "py_err_kind",
            Builtin::PyErrMessage => "py_err_message",
            Builtin::PyErrTraceback => "py_err_traceback",
            Builtin::SleepMs => "sleep_ms",
            Builtin::JsonParse => "json_parse",
            Builtin::JsonFailed => "json_failed",
            Builtin::JsonErrMessage => "json_err_message",
            Builtin::JsonErrOffset => "json_err_offset",
            Builtin::Validate => "validate",
            Builtin::SchemaFailed => "schema_failed",
            Builtin::SchemaErrPath => "schema_err_path",
            Builtin::SchemaErrExpected => "schema_err_expected",
            Builtin::SchemaErrFound => "schema_err_found",
            Builtin::ToJson => "to_json",
            Builtin::LlmGenerate => "llm_generate",
            Builtin::LlmFailed => "llm_failed",
            Builtin::LlmErrKind => "llm_err_kind",
            Builtin::LlmErrMessage => "llm_err_message",
            Builtin::LlmJsonRejected => "llm_json_rejected",
            Builtin::LlmSchemaRejected => "llm_schema_rejected",
            Builtin::LlmMockPush => "llm_mock_push",
            Builtin::AuditJson => "audit_json",
            Builtin::UsageJson => "usage_json",
            Builtin::AgentEnter => "agent_enter",
            Builtin::AgentExit => "agent_exit",
            Builtin::ToolErr => "tool_err",
            Builtin::ToolArgsRejected => "tool_args_rejected",
        }
    }

    /// Python のランタイムが要る組み込み関数か（ADR-0018 論点 4: これを呼ぶプログラムだけ `libpython` をリンクする）。
    pub fn is_python(self) -> bool {
        self.name().starts_with("py_")
    }

    /// 戻り値を +1 所有で返すか（新しく作ったオブジェクト）。そうでなければ借用（不死の文字列など）。
    pub fn returns_owned(self) -> bool {
        matches!(
            self,
            Builtin::StrConcat
                | Builtin::PyImport
                | Builtin::PyGetAttr
                | Builtin::PyCall
                | Builtin::PyFromInt
                | Builtin::PyFromBool
                | Builtin::PyFromStr
                | Builtin::PyFromFloat
                | Builtin::PyFromValue
                | Builtin::PyToList
                | Builtin::PyToDict
                | Builtin::PyCallKw
                | Builtin::PyNone
                | Builtin::PyToStr
                | Builtin::PyErrKind
                | Builtin::PyErrMessage
                | Builtin::PyErrTraceback
                | Builtin::JsonParse
                | Builtin::JsonErrMessage
                | Builtin::Validate
                | Builtin::SchemaErrPath
                | Builtin::SchemaErrExpected
                | Builtin::SchemaErrFound
                | Builtin::ToJson
                | Builtin::LlmGenerate
                | Builtin::LlmErrKind
                | Builtin::LlmErrMessage
                | Builtin::LlmJsonRejected
                | Builtin::LlmSchemaRejected
                | Builtin::AuditJson
                | Builtin::UsageJson
        )
    }

    /// 戻り値の型（なければ `None`）。
    pub fn ret(self) -> Option<Ty> {
        match self {
            Builtin::PrintInt
            | Builtin::PrintBool
            | Builtin::PrintStr
            | Builtin::PrintFloat
            | Builtin::PrintValue
            | Builtin::Exit
            | Builtin::AssertFail
            | Builtin::Panic
            | Builtin::SleepMs => None,
            Builtin::ArgCount | Builtin::ScalarKind | Builtin::ScalarInt | Builtin::TestIndex => {
                Some(Ty::Int)
            }
            Builtin::ArgExists | Builtin::ScalarBool | Builtin::StrEq => Some(Ty::Bool),
            Builtin::ArgStr | Builtin::StrConcat => Some(Ty::Str),
            Builtin::StrLen | Builtin::IntFromFloat => Some(Ty::Int),
            Builtin::FloatFromInt | Builtin::ScalarFloat => Some(Ty::Float),
            Builtin::PyImport
            | Builtin::PyGetAttr
            | Builtin::PyCall
            | Builtin::PyFromInt
            | Builtin::PyFromBool
            | Builtin::PyFromStr
            | Builtin::PyFromFloat
            | Builtin::PyFromValue
            | Builtin::PyCallKw
            | Builtin::PyNone => Some(Ty::Py),
            Builtin::PyToFloat => Some(Ty::Float),
            // 型の ID は呼び出しごとに違う（verify と lowering が扱う）
            Builtin::PyToList | Builtin::PyToDict => None,
            Builtin::PyToInt => Some(Ty::Int),
            Builtin::PyToBool | Builtin::PyIsNone | Builtin::PyFailed => Some(Ty::Bool),
            Builtin::PyToStr
            | Builtin::PyErrKind
            | Builtin::PyErrMessage
            | Builtin::PyErrTraceback => Some(Ty::Str),
            Builtin::JsonParse => Some(Ty::Unknown),
            Builtin::JsonFailed => Some(Ty::Bool),
            Builtin::JsonErrMessage => Some(Ty::Str),
            Builtin::JsonErrOffset => Some(Ty::Int),
            // 型は呼び出しごとに違う（verify が扱う）
            Builtin::Validate => None,
            Builtin::SchemaFailed => Some(Ty::Bool),
            Builtin::SchemaErrPath | Builtin::SchemaErrExpected | Builtin::SchemaErrFound => {
                Some(Ty::Str)
            }
            Builtin::ToJson => Some(Ty::Str),
            Builtin::LlmGenerate
            | Builtin::LlmErrKind
            | Builtin::LlmErrMessage
            | Builtin::LlmJsonRejected
            | Builtin::LlmSchemaRejected
            | Builtin::AuditJson
            | Builtin::UsageJson => Some(Ty::Str),
            Builtin::LlmFailed => Some(Ty::Bool),
            Builtin::LlmMockPush => None,
            Builtin::AgentEnter
            | Builtin::AgentExit
            | Builtin::ToolErr
            | Builtin::ToolArgsRejected => None,
        }
    }

    fn signature(self) -> &'static [Ty] {
        match self {
            Builtin::PrintInt | Builtin::Exit | Builtin::SleepMs => &[Ty::Int],
            Builtin::PrintBool => &[Ty::Bool],
            Builtin::PrintStr => &[Ty::Str],
            Builtin::PrintFloat | Builtin::IntFromFloat => &[Ty::Float],
            Builtin::FloatFromInt => &[Ty::Int],
            Builtin::PrintValue => {
                unreachable!("print_value takes any type; verify checks it separately")
            }
            Builtin::ArgCount | Builtin::TestIndex => &[],
            Builtin::AssertFail | Builtin::StrLen => &[Ty::Str],
            Builtin::StrConcat | Builtin::StrEq | Builtin::Panic => &[Ty::Str, Ty::Str],
            Builtin::ArgExists | Builtin::ArgStr => &[Ty::Int],
            Builtin::ScalarKind
            | Builtin::ScalarInt
            | Builtin::ScalarBool
            | Builtin::ScalarFloat => &[Ty::Str],
            Builtin::PyImport | Builtin::PyFromStr => &[Ty::Str],
            Builtin::PyGetAttr => &[Ty::Py, Ty::Str],
            Builtin::PyCall => {
                unreachable!("py_call takes a list of handles; verify checks it separately")
            }
            Builtin::PyFromInt => &[Ty::Int],
            Builtin::PyFromBool => &[Ty::Bool],
            Builtin::PyFromFloat => &[Ty::Float],
            Builtin::PyToInt
            | Builtin::PyToBool
            | Builtin::PyToStr
            | Builtin::PyToFloat
            | Builtin::PyIsNone => &[Ty::Py],
            Builtin::PyFromValue | Builtin::PyToList | Builtin::PyToDict | Builtin::PyCallKw => {
                unreachable!("verify checks these separately")
            }
            Builtin::PyNone
            | Builtin::PyFailed
            | Builtin::PyErrKind
            | Builtin::PyErrMessage
            | Builtin::PyErrTraceback => &[],
            Builtin::JsonParse => &[Ty::Str],
            Builtin::JsonFailed | Builtin::JsonErrMessage | Builtin::JsonErrOffset => &[],
            Builtin::SchemaFailed
            | Builtin::SchemaErrPath
            | Builtin::SchemaErrExpected
            | Builtin::SchemaErrFound => &[],
            Builtin::Validate | Builtin::ToJson => {
                unreachable!("verify checks these separately")
            }
            Builtin::LlmGenerate => &[Ty::Str, Ty::Str],
            Builtin::LlmMockPush => &[Ty::Str],
            Builtin::LlmFailed
            | Builtin::LlmErrKind
            | Builtin::LlmErrMessage
            | Builtin::LlmJsonRejected
            | Builtin::LlmSchemaRejected
            | Builtin::AuditJson
            | Builtin::UsageJson => &[],
            Builtin::AgentEnter | Builtin::AgentExit | Builtin::ToolErr => &[],
            Builtin::ToolArgsRejected => &[Ty::Bool],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Callee {
    Func(FuncId),
    Builtin(Builtin),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inst {
    Const {
        dst: TempId,
        value: Const,
    },
    Load {
        dst: TempId,
        src: LocalId,
    },
    /// スロットの値を +1 所有で取り出し、スロットを空（null / ゼロ）にする（Move 最適化が作る。M4 T3）。
    /// スロットの後の release は空の値に対するものになり、何もしない
    Take {
        dst: TempId,
        src: LocalId,
    },
    Store {
        dst: LocalId,
        src: TempId,
    },
    Binary {
        dst: TempId,
        op: BinOp,
        lhs: TempId,
        rhs: TempId,
    },
    Not {
        dst: TempId,
        src: TempId,
    },
    Call {
        dst: Option<TempId>,
        callee: Callee,
        args: Vec<TempId>,
    },
    /// 構成要素 `variant` の Union 値を作る。`None` の構成要素なら `payload` は `None`。
    MakeUnion {
        dst: TempId,
        union: UnionId,
        variant: u32,
        payload: Option<TempId>,
    },
    /// 種類 `kind`（`UNKNOWN_KINDS` の番号）の `Unknown` 値を作る（ADR-0021）。`None` の種類なら `payload` は `None`。
    /// 中身の所有は `MakeUnion` と同じ（+1 で消費する）
    MakeUnknown {
        dst: TempId,
        kind: u32,
        payload: Option<TempId>,
    },
    /// 参照カウントを1増やす（`str` はオブジェクト、Union / レコードは中の参照型すべて）。ARC パスが挿入する。
    Retain {
        src: TempId,
        /// 挿入した理由（ADR-0017）。バックエンドは読まない。
        reason: RcReason,
    },
    /// 参照カウントを1減らし、0 になったら解放する。ARC パスが挿入する。
    Release {
        src: TempId,
        /// 挿入した理由（ADR-0017）。バックエンドは読まない。
        reason: RcReason,
    },
    /// リストを作る（`items` を +1 で消費する）。
    ListNew {
        dst: TempId,
        list: ListId,
        items: Vec<TempId>,
    },
    ListLen {
        dst: TempId,
        src: TempId,
    },
    /// 要素の読み出し（借用。負の添字は末尾から、範囲外はパニック）。
    ListGet {
        dst: TempId,
        src: TempId,
        index: TempId,
    },
    /// 要素の置き換え（`value` を +1 で消費し、古い要素を release する）。
    ListSet {
        list: TempId,
        index: TempId,
        value: TempId,
    },
    /// 末尾への追加（`value` を +1 で消費する）。
    ListPush {
        list: TempId,
        value: TempId,
    },
    /// 辞書を作る（`keys` と `vals` を +1 で消費する。同じキーは後のものが勝つ）。
    DictNew {
        dst: TempId,
        dict: DictId,
        keys: Vec<TempId>,
        vals: Vec<TempId>,
    },
    DictLen {
        dst: TempId,
        src: TempId,
    },
    /// キーがあるか（`bool`）。
    DictContains {
        dst: TempId,
        src: TempId,
        key: TempId,
    },
    /// 値の読み出し（借用。キーがなければパニック）。
    DictGet {
        dst: TempId,
        src: TempId,
        key: TempId,
    },
    /// 値を取り出してキーを削除する（`pop`）。値は +1 所有で返す。キーがなければパニック。
    DictPop {
        dst: TempId,
        src: TempId,
        key: TempId,
    },
    /// 追加または置き換え（`key` と `value` を +1 で消費する。既存のキーなら、新しいキーと古い値を release する）。
    DictSet {
        dict: TempId,
        key: TempId,
        value: TempId,
    },
    /// エントリ `index` のキー（借用）。
    DictKeyAt {
        dst: TempId,
        src: TempId,
        index: TempId,
    },
    /// 反復: `pos` 以降の最初のエントリの番号、なければ -1。開始時のキーの数 `len0` と違えばパニック。
    DictIterNext {
        dst: TempId,
        src: TempId,
        pos: TempId,
        len0: TempId,
    },
    /// レコード値を作る。`fields` はレコードのフィールドの順。
    MakeRecord {
        dst: TempId,
        record: RecordId,
        fields: Vec<TempId>,
    },
    /// レコード値の `index` 番目のフィールド。
    Field {
        dst: TempId,
        src: TempId,
        index: u32,
    },
    /// Union 値のタグが `variant` かどうか（`bool`）。
    UnionIs {
        dst: TempId,
        src: TempId,
        variant: u32,
    },
    /// Union 値から構成要素 `variant` の中身を取り出す。タグが違う値に使ってはならない（lowering が保証する）。
    UnionPayload {
        dst: TempId,
        src: TempId,
        variant: u32,
    },
    /// `spawn(func(args))`（ADR-0020 論点 8）: 新しいタスクを作り、実行キューの末尾に入れる。`args` を +1 で消費して
    /// タスクに所有させ、`dst`（`Task`）を +1 所有で返す。`async` の関数の中だけ
    Spawn {
        dst: TempId,
        task: TaskId,
        func: FuncId,
        args: Vec<TempId>,
    },
    /// `await task`（ADR-0020 論点 5）: タスクが終わるまで現在のタスクを止め、結果を +1 所有で返す（`Task[None]` なら
    /// `dst` は `None`）。`task` は借用で読む。中断点で、`async` の関数の中だけ
    Await {
        dst: Option<TempId>,
        task: TempId,
    },
}

impl Inst {
    /// 命令が読む一時値（Move 最適化の判定用）。
    pub fn operands(&self) -> Vec<TempId> {
        match self {
            Inst::Const { .. } | Inst::Load { .. } | Inst::Take { .. } => vec![],
            Inst::Store { src, .. }
            | Inst::Not { src, .. }
            | Inst::Retain { src, .. }
            | Inst::Release { src, .. }
            | Inst::ListLen { src, .. }
            | Inst::DictLen { src, .. }
            | Inst::Field { src, .. }
            | Inst::UnionIs { src, .. }
            | Inst::UnionPayload { src, .. } => vec![*src],
            Inst::Spawn { args, .. } => args.clone(),
            Inst::Await { task, .. } => vec![*task],
            Inst::Binary { lhs, rhs, .. } => vec![*lhs, *rhs],
            Inst::Call { args, .. } => args.clone(),
            Inst::MakeUnion { payload, .. } | Inst::MakeUnknown { payload, .. } => {
                payload.iter().copied().collect()
            }
            Inst::ListNew { items, .. } => items.clone(),
            Inst::ListGet { src, index, .. } | Inst::DictKeyAt { src, index, .. } => {
                vec![*src, *index]
            }
            Inst::ListSet { list, index, value } => vec![*list, *index, *value],
            Inst::ListPush { list, value } => vec![*list, *value],
            Inst::DictNew { keys, vals, .. } => keys.iter().chain(vals).copied().collect(),
            Inst::DictContains { src, key, .. }
            | Inst::DictGet { src, key, .. }
            | Inst::DictPop { src, key, .. } => vec![*src, *key],
            Inst::DictSet { dict, key, value } => vec![*dict, *key, *value],
            Inst::DictIterNext { src, pos, len0, .. } => vec![*src, *pos, *len0],
            Inst::MakeRecord { fields, .. } => fields.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Terminator {
    Return(Option<TempId>),
    Jump(BlockId),
    Branch {
        cond: TempId,
        then_block: BlockId,
        else_block: BlockId,
    },
    Unreachable,
}

// ---------------------------------------------------------------- verifier

/// IR の整合性検査。エラーはコンパイラのバグを意味する。
pub fn verify(p: &Program) -> Result<(), String> {
    let entry = p
        .funcs
        .get(p.entry.0 as usize)
        .ok_or("entry function does not exist")?;
    if entry.param_count != 0 || entry.ret.is_some() {
        return Err("entry function must take no parameters and return nothing".into());
    }
    for (i, members) in p.unions.iter().enumerate() {
        // 構成要素の Union は、自分より前に登録したものだけ（`Result` は Union の構成要素になれる。ADR-0013）。
        // 循環した入れ子を防ぐ
        if members.len() < 2
            || members
                .iter()
                .any(|m| matches!(m, Some(Ty::Union(u)) if u.0 as usize >= i))
        {
            return Err(format!(
                "union u{i} must have at least two members, and nested unions must be registered before it"
            ));
        }
    }
    if let Some((l, d)) = p.unknown
        && (p.lists.get(l.0 as usize) != Some(&Ty::Unknown)
            || p.dicts.get(d.0 as usize) != Some(&(Ty::Str, Ty::Unknown)))
    {
        return Err(
            "the containers of `unknown` must be list[unknown] and dict[str, unknown]".into(),
        );
    }
    if p.display.record_names.len() != p.records.len() {
        return Err("every record type must have a display name".into());
    }
    if let Some(u) = p
        .display
        .results
        .iter()
        .find(|u| p.unions.get(u.0 as usize).is_none_or(|m| m.len() != 2))
    {
        return Err(format!("result u{} must be a union of two members", u.0));
    }
    for f in &p.funcs {
        verify_function(p, f).map_err(|e| format!("in function `{}`: {e}", f.name))?;
    }
    Ok(())
}

fn union_member(p: &Program, u: UnionId, variant: u32) -> Result<Member, String> {
    let members = p
        .unions
        .get(u.0 as usize)
        .ok_or_else(|| format!("unknown union u{}", u.0))?;
    members
        .get(variant as usize)
        .copied()
        .ok_or_else(|| format!("union u{} has no variant {variant}", u.0))
}

/// Union か `Unknown` の構成要素（`UnionIs` / `UnionPayload` は両方に使える。ADR-0021 論点 3）。
fn sum_member(p: &Program, t: Ty, variant: u32) -> Result<Member, String> {
    match t {
        Ty::Union(u) => union_member(p, u, variant),
        Ty::Unknown => {
            if p.unknown.is_none() {
                return Err("`unknown` is used but its container types are not registered".into());
            }
            p.unknown_members()
                .get(variant as usize)
                .copied()
                .ok_or_else(|| format!("unknown has no kind {variant}"))
        }
        other => Err(format!("{} is not a union", other.name())),
    }
}

fn dict_types(p: &Program, d: DictId) -> Result<(Ty, Ty), String> {
    p.dicts
        .get(d.0 as usize)
        .copied()
        .ok_or_else(|| format!("unknown dict type dict{}", d.0))
}

/// `py_from_value` で Python に変換できる型か（レコードは変換しない。型検査の E1201 と同じ規則）。
fn py_convertible(p: &Program, t: Ty) -> bool {
    match t {
        Ty::Int | Ty::Bool | Ty::Str | Ty::Float | Ty::Py => true,
        Ty::Record(_) | Ty::Task(_) | Ty::Unknown => false,
        Ty::List(l) => p
            .lists
            .get(l.0 as usize)
            .is_some_and(|e| py_convertible(p, *e)),
        Ty::Dict(d) => p
            .dicts
            .get(d.0 as usize)
            .is_some_and(|(k, v)| py_convertible(p, *k) && py_convertible(p, *v)),
        Ty::Union(u) => p
            .unions
            .get(u.0 as usize)
            .is_some_and(|ms| ms.iter().flatten().all(|m| py_convertible(p, *m))),
    }
}

/// スキーマ型か（ADR-0022 論点 4。型検査の E1501 と同じ規則）。Union の `None` の構成要素は可。
fn schema_convertible(p: &Program, t: Ty) -> bool {
    match t {
        Ty::Int | Ty::Bool | Ty::Str | Ty::Float | Ty::Unknown => true,
        Ty::Py | Ty::Task(_) => false,
        Ty::List(l) => p
            .lists
            .get(l.0 as usize)
            .is_some_and(|e| schema_convertible(p, *e)),
        Ty::Dict(d) => p
            .dicts
            .get(d.0 as usize)
            .is_some_and(|(k, v)| *k == Ty::Str && schema_convertible(p, *v)),
        Ty::Union(u) => p
            .unions
            .get(u.0 as usize)
            .is_some_and(|ms| ms.iter().flatten().all(|m| schema_convertible(p, *m))),
        Ty::Record(r) => p
            .records
            .get(r.0 as usize)
            .is_some_and(|fs| fs.iter().all(|(_, ft)| schema_convertible(p, *ft))),
    }
}

fn task_result(p: &Program, k: TaskId) -> Result<Option<Ty>, String> {
    p.tasks
        .get(k.0 as usize)
        .copied()
        .ok_or_else(|| format!("unknown task type task{}", k.0))
}

fn list_elem(p: &Program, l: ListId) -> Result<Ty, String> {
    p.lists
        .get(l.0 as usize)
        .copied()
        .ok_or_else(|| format!("unknown list type list{}", l.0))
}

fn verify_types(p: &Program, f: &Function) -> Result<(), String> {
    for t in f.locals.iter().chain(&f.temps).chain(f.ret.iter()) {
        match t {
            Ty::Union(u) if u.0 as usize >= p.unions.len() => {
                return Err(format!("unknown union u{}", u.0));
            }
            Ty::Record(r) if r.0 as usize >= p.records.len() => {
                return Err(format!("unknown record r{}", r.0));
            }
            Ty::List(l) if l.0 as usize >= p.lists.len() => {
                return Err(format!("unknown list type list{}", l.0));
            }
            Ty::Dict(d) if d.0 as usize >= p.dicts.len() => {
                return Err(format!("unknown dict type dict{}", d.0));
            }
            Ty::Task(k) if k.0 as usize >= p.tasks.len() => {
                return Err(format!("unknown task type task{}", k.0));
            }
            _ => {}
        }
    }
    Ok(())
}

fn verify_function(p: &Program, f: &Function) -> Result<(), String> {
    verify_types(p, f)?;
    if f.param_count as usize > f.locals.len() {
        return Err("param_count exceeds number of locals".into());
    }
    if f.blocks.is_empty() {
        return Err("function has no blocks".into());
    }
    let mut defined_in: Vec<Option<usize>> = vec![None; f.temps.len()];
    let temp_ty = |t: TempId| -> Result<Ty, String> {
        f.temps
            .get(t.0 as usize)
            .copied()
            .ok_or_else(|| format!("unknown temp t{}", t.0))
    };
    let local_ty = |l: LocalId| -> Result<Ty, String> {
        f.locals
            .get(l.0 as usize)
            .copied()
            .ok_or_else(|| format!("unknown local l{}", l.0))
    };
    let check_block = |b: BlockId| -> Result<(), String> {
        if (b.0 as usize) < f.blocks.len() {
            Ok(())
        } else {
            Err(format!("unknown block bb{}", b.0))
        }
    };
    for (bi, block) in f.blocks.iter().enumerate() {
        let define = |t: TempId, defined_in: &mut Vec<Option<usize>>| -> Result<(), String> {
            let slot = defined_in
                .get_mut(t.0 as usize)
                .ok_or_else(|| format!("unknown temp t{}", t.0))?;
            if slot.is_some() {
                return Err(format!("temp t{} defined more than once", t.0));
            }
            *slot = Some(bi);
            Ok(())
        };
        let use_ = |t: TempId, defined_in: &Vec<Option<usize>>| -> Result<Ty, String> {
            match defined_in.get(t.0 as usize) {
                Some(Some(b)) if *b == bi => temp_ty(t),
                Some(Some(_)) => Err(format!("temp t{} used outside its defining block", t.0)),
                _ => Err(format!("temp t{} used before definition", t.0)),
            }
        };
        let expect = |what: &str, got: Ty, want: Ty| -> Result<(), String> {
            if got == want {
                Ok(())
            } else {
                Err(format!(
                    "bb{bi}: {what}: expected {}, found {}",
                    want.name(),
                    got.name()
                ))
            }
        };
        for inst in &block.insts {
            match inst {
                Inst::Const { dst, value } => {
                    let ty = match value {
                        Const::Int(_) => Ty::Int,
                        Const::Float(_) => Ty::Float,
                        Const::Bool(_) => Ty::Bool,
                        Const::Str(s) => {
                            if s.0 as usize >= p.strings.len() {
                                return Err(format!("unknown string s{}", s.0));
                            }
                            Ty::Str
                        }
                        Const::Schema(t) => {
                            if !schema_convertible(p, *t) {
                                return Err(format!("bb{bi}: no JSON Schema for {}", t.name()));
                            }
                            Ty::Str
                        }
                    };
                    expect("const", temp_ty(*dst)?, ty)?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::Load { dst, src } | Inst::Take { dst, src } => {
                    expect("load", temp_ty(*dst)?, local_ty(*src)?)?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::Store { dst, src } => {
                    expect("store", use_(*src, &defined_in)?, local_ty(*dst)?)?;
                }
                Inst::Binary { dst, op, lhs, rhs } => {
                    let lt = use_(*lhs, &defined_in)?;
                    let rt = use_(*rhs, &defined_in)?;
                    expect("binary operands", rt, lt)?;
                    let operand_ok = match op {
                        // 構造を持つ値の比較は、バックエンドが型ごとの関数で行う（ADR-0016）
                        BinOp::Eq | BinOp::Ne => true,
                        // float（ADR-0019）: 四則と大小の比較。`//` と `%` は int だけ、`/` は float だけ
                        BinOp::Div => lt == Ty::Float,
                        BinOp::FloorDiv | BinOp::Mod => lt == Ty::Int,
                        _ => matches!(lt, Ty::Int | Ty::Float),
                    };
                    if !operand_ok {
                        return Err(format!("bb{bi}: {} on {}", op.name(), lt.name()));
                    }
                    let rty = if op.is_comparison() { Ty::Bool } else { lt };
                    expect("binary result", temp_ty(*dst)?, rty)?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::MakeUnion {
                    dst,
                    union,
                    variant,
                    payload,
                } => {
                    let member = union_member(p, *union, *variant)?;
                    match (member, payload) {
                        (None, None) => {}
                        (Some(want), Some(t)) => {
                            expect("union payload", use_(*t, &defined_in)?, want)?
                        }
                        _ => {
                            return Err(format!(
                                "bb{bi}: union payload does not match the variant"
                            ));
                        }
                    }
                    expect("make_union result", temp_ty(*dst)?, Ty::Union(*union))?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::MakeUnknown { dst, kind, payload } => {
                    let member = sum_member(p, Ty::Unknown, *kind)?;
                    match (member, payload) {
                        (None, None) => {}
                        (Some(want), Some(t)) => {
                            expect("unknown payload", use_(*t, &defined_in)?, want)?
                        }
                        _ => {
                            return Err(format!("bb{bi}: unknown payload does not match the kind"));
                        }
                    }
                    expect("make_unknown result", temp_ty(*dst)?, Ty::Unknown)?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::Retain { src, .. } | Inst::Release { src, .. } => {
                    use_(*src, &defined_in)?;
                }
                Inst::Spawn {
                    dst,
                    task,
                    func,
                    args,
                } => {
                    if !f.is_async {
                        return Err(format!("bb{bi}: spawn outside of an async function"));
                    }
                    let g = p
                        .funcs
                        .get(func.0 as usize)
                        .ok_or_else(|| format!("unknown function f{}", func.0))?;
                    if !g.is_async {
                        return Err(format!("bb{bi}: spawn of a non-async function f{}", func.0));
                    }
                    let result = task_result(p, *task)?;
                    if result != g.ret {
                        return Err(format!(
                            "bb{bi}: task{} result does not match f{}'s return type",
                            task.0, func.0
                        ));
                    }
                    if args.len() != g.param_count as usize {
                        return Err(format!("bb{bi}: spawn argument count mismatch"));
                    }
                    for (a, want) in args.iter().zip(&g.locals) {
                        expect("spawn argument", use_(*a, &defined_in)?, *want)?;
                    }
                    expect("spawn result", temp_ty(*dst)?, Ty::Task(*task))?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::Await { dst, task } => {
                    if !f.is_async {
                        return Err(format!("bb{bi}: await outside of an async function"));
                    }
                    let Ty::Task(k) = use_(*task, &defined_in)? else {
                        return Err(format!("bb{bi}: await on a non-task value"));
                    };
                    match (dst, task_result(p, k)?) {
                        (Some(d), Some(r)) => {
                            expect("await result", temp_ty(*d)?, r)?;
                            define(*d, &mut defined_in)?;
                        }
                        (None, None) => {}
                        _ => {
                            return Err(format!(
                                "bb{bi}: await destination does not match task{}",
                                k.0
                            ));
                        }
                    }
                }
                Inst::ListNew { dst, list, items } => {
                    let elem = list_elem(p, *list)?;
                    for it in items {
                        expect("list item", use_(*it, &defined_in)?, elem)?;
                    }
                    expect("list_new result", temp_ty(*dst)?, Ty::List(*list))?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::ListLen { dst, src } => {
                    let Ty::List(_) = use_(*src, &defined_in)? else {
                        return Err(format!("bb{bi}: list_len on a non-list value"));
                    };
                    expect("list_len result", temp_ty(*dst)?, Ty::Int)?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::ListGet { dst, src, index } => {
                    let Ty::List(l) = use_(*src, &defined_in)? else {
                        return Err(format!("bb{bi}: list_get on a non-list value"));
                    };
                    expect("list index", use_(*index, &defined_in)?, Ty::Int)?;
                    expect("list_get result", temp_ty(*dst)?, list_elem(p, l)?)?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::ListSet { list, index, value } => {
                    let Ty::List(l) = use_(*list, &defined_in)? else {
                        return Err(format!("bb{bi}: list_set on a non-list value"));
                    };
                    expect("list index", use_(*index, &defined_in)?, Ty::Int)?;
                    expect(
                        "list_set value",
                        use_(*value, &defined_in)?,
                        list_elem(p, l)?,
                    )?;
                }
                Inst::ListPush { list, value } => {
                    let Ty::List(l) = use_(*list, &defined_in)? else {
                        return Err(format!("bb{bi}: list_push on a non-list value"));
                    };
                    expect(
                        "list_push value",
                        use_(*value, &defined_in)?,
                        list_elem(p, l)?,
                    )?;
                }
                Inst::DictNew {
                    dst,
                    dict,
                    keys,
                    vals,
                } => {
                    let (kt, vt) = dict_types(p, *dict)?;
                    if keys.len() != vals.len() {
                        return Err(format!("bb{bi}: dict_new key / value count mismatch"));
                    }
                    for (k, v) in keys.iter().zip(vals) {
                        expect("dict key", use_(*k, &defined_in)?, kt)?;
                        expect("dict value", use_(*v, &defined_in)?, vt)?;
                    }
                    expect("dict_new result", temp_ty(*dst)?, Ty::Dict(*dict))?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::DictLen { dst, src } => {
                    let Ty::Dict(_) = use_(*src, &defined_in)? else {
                        return Err(format!("bb{bi}: dict_len on a non-dict value"));
                    };
                    expect("dict_len result", temp_ty(*dst)?, Ty::Int)?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::DictContains { dst, src, key }
                | Inst::DictGet { dst, src, key }
                | Inst::DictPop { dst, src, key } => {
                    let Ty::Dict(d) = use_(*src, &defined_in)? else {
                        return Err(format!("bb{bi}: dict lookup on a non-dict value"));
                    };
                    let (kt, vt) = dict_types(p, d)?;
                    expect("dict key", use_(*key, &defined_in)?, kt)?;
                    let want = if matches!(inst, Inst::DictGet { .. } | Inst::DictPop { .. }) {
                        vt
                    } else {
                        Ty::Bool
                    };
                    expect("dict lookup result", temp_ty(*dst)?, want)?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::DictSet { dict, key, value } => {
                    let Ty::Dict(d) = use_(*dict, &defined_in)? else {
                        return Err(format!("bb{bi}: dict_set on a non-dict value"));
                    };
                    let (kt, vt) = dict_types(p, d)?;
                    expect("dict key", use_(*key, &defined_in)?, kt)?;
                    expect("dict value", use_(*value, &defined_in)?, vt)?;
                }
                Inst::DictKeyAt { dst, src, index } => {
                    let Ty::Dict(d) = use_(*src, &defined_in)? else {
                        return Err(format!("bb{bi}: dict_key_at on a non-dict value"));
                    };
                    expect("dict entry index", use_(*index, &defined_in)?, Ty::Int)?;
                    expect("dict_key_at result", temp_ty(*dst)?, dict_types(p, d)?.0)?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::DictIterNext {
                    dst,
                    src,
                    pos,
                    len0,
                } => {
                    let Ty::Dict(_) = use_(*src, &defined_in)? else {
                        return Err(format!("bb{bi}: dict_iter_next on a non-dict value"));
                    };
                    expect("dict position", use_(*pos, &defined_in)?, Ty::Int)?;
                    expect("dict length", use_(*len0, &defined_in)?, Ty::Int)?;
                    expect("dict_iter_next result", temp_ty(*dst)?, Ty::Int)?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::MakeRecord {
                    dst,
                    record,
                    fields,
                } => {
                    let def = p
                        .records
                        .get(record.0 as usize)
                        .ok_or_else(|| format!("unknown record r{}", record.0))?;
                    if def.len() != fields.len() {
                        return Err(format!("bb{bi}: make_record field count mismatch"));
                    }
                    for (t, (_, want)) in fields.iter().zip(def) {
                        expect("record field", use_(*t, &defined_in)?, *want)?;
                    }
                    expect("make_record result", temp_ty(*dst)?, Ty::Record(*record))?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::Field { dst, src, index } => {
                    let Ty::Record(r) = use_(*src, &defined_in)? else {
                        return Err(format!("bb{bi}: field access on a non-record value"));
                    };
                    let (_, want) = p.records[r.0 as usize]
                        .get(*index as usize)
                        .ok_or_else(|| format!("bb{bi}: record r{} has no field {index}", r.0))?;
                    expect("field result", temp_ty(*dst)?, *want)?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::UnionIs { dst, src, variant } => {
                    let st = use_(*src, &defined_in)?;
                    if !matches!(st, Ty::Union(_) | Ty::Unknown) {
                        return Err(format!("bb{bi}: union_is on a non-union value"));
                    }
                    sum_member(p, st, *variant)?;
                    expect("union_is result", temp_ty(*dst)?, Ty::Bool)?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::UnionPayload { dst, src, variant } => {
                    let st = use_(*src, &defined_in)?;
                    if !matches!(st, Ty::Union(_) | Ty::Unknown) {
                        return Err(format!("bb{bi}: union_payload on a non-union value"));
                    }
                    let Some(want) = sum_member(p, st, *variant)? else {
                        return Err(format!("bb{bi}: the None variant has no payload"));
                    };
                    expect("union_payload result", temp_ty(*dst)?, want)?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::Not { dst, src } => {
                    expect("not operand", use_(*src, &defined_in)?, Ty::Bool)?;
                    expect("not result", temp_ty(*dst)?, Ty::Bool)?;
                    define(*dst, &mut defined_in)?;
                }
                Inst::Call { dst, callee, args } => {
                    let (params, ret): (Vec<Ty>, Option<Ty>) = match callee {
                        Callee::Builtin(Builtin::PrintValue) => match args.as_slice() {
                            [a] => (vec![use_(*a, &defined_in)?], None),
                            _ => return Err(format!("bb{bi}: print_value takes one argument")),
                        },
                        Callee::Builtin(b @ (Builtin::PyToList | Builtin::PyToDict)) => {
                            let [h] = args.as_slice() else {
                                return Err(format!("bb{bi}: {} takes one argument", b.name()));
                            };
                            expect("python handle", use_(*h, &defined_in)?, Ty::Py)?;
                            let d =
                                dst.ok_or_else(|| format!("bb{bi}: {} has a result", b.name()))?;
                            let rt = temp_ty(d)?;
                            let ok = match (b, rt) {
                                (Builtin::PyToList, Ty::List(l)) => list_elem(p, l)? == Ty::Py,
                                (Builtin::PyToDict, Ty::Dict(dd)) => {
                                    dict_types(p, dd)? == (Ty::Str, Ty::Py)
                                }
                                _ => false,
                            };
                            if !ok {
                                return Err(format!(
                                    "bb{bi}: {} result has type {}",
                                    b.name(),
                                    rt.name()
                                ));
                            }
                            (vec![Ty::Py], Some(rt))
                        }
                        Callee::Builtin(Builtin::Validate) => {
                            let [u] = args.as_slice() else {
                                return Err(format!("bb{bi}: validate takes one argument"));
                            };
                            expect("validate argument", use_(*u, &defined_in)?, Ty::Unknown)?;
                            let d = dst.ok_or_else(|| format!("bb{bi}: validate has a result"))?;
                            let rt = temp_ty(d)?;
                            if !schema_convertible(p, rt) {
                                return Err(format!(
                                    "bb{bi}: validate cannot produce {}",
                                    rt.name()
                                ));
                            }
                            (vec![Ty::Unknown], Some(rt))
                        }
                        Callee::Builtin(Builtin::ToJson) => match args.as_slice() {
                            [a] => {
                                let at = use_(*a, &defined_in)?;
                                if !schema_convertible(p, at) {
                                    return Err(format!(
                                        "bb{bi}: to_json cannot encode {}",
                                        at.name()
                                    ));
                                }
                                (vec![at], Some(Ty::Str))
                            }
                            _ => return Err(format!("bb{bi}: to_json takes one argument")),
                        },
                        Callee::Builtin(Builtin::PyFromValue) => match args.as_slice() {
                            [a] => {
                                let at = use_(*a, &defined_in)?;
                                if !py_convertible(p, at) {
                                    return Err(format!(
                                        "bb{bi}: py_from_value cannot convert {}",
                                        at.name()
                                    ));
                                }
                                (vec![at], Some(Ty::Py))
                            }
                            _ => return Err(format!("bb{bi}: py_from_value takes one argument")),
                        },
                        Callee::Builtin(Builtin::PyCallKw) => match args.as_slice() {
                            [h, l, d] => {
                                let lt = use_(*l, &defined_in)?;
                                let dt = use_(*d, &defined_in)?;
                                let list_ok = match lt {
                                    Ty::List(li) => list_elem(p, li)? == Ty::Py,
                                    _ => false,
                                };
                                let dict_ok = match dt {
                                    Ty::Dict(di) => dict_types(p, di)? == (Ty::Str, Ty::Py),
                                    _ => false,
                                };
                                if !(list_ok && dict_ok) {
                                    return Err(format!(
                                        "bb{bi}: py_call_kw takes list[py] and dict[str, py]"
                                    ));
                                }
                                (vec![use_(*h, &defined_in)?, lt, dt], Some(Ty::Py))
                            }
                            _ => return Err(format!("bb{bi}: py_call_kw takes three arguments")),
                        },
                        Callee::Builtin(Builtin::PyCall) => match args.as_slice() {
                            [h, l] => {
                                expect("py_call callee", use_(*h, &defined_in)?, Ty::Py)?;
                                let Ty::List(lid) = use_(*l, &defined_in)? else {
                                    return Err(format!(
                                        "bb{bi}: py_call arguments must be a list"
                                    ));
                                };
                                if list_elem(p, lid)? != Ty::Py {
                                    return Err(format!(
                                        "bb{bi}: py_call arguments must be handles"
                                    ));
                                }
                                (vec![Ty::Py, Ty::List(lid)], Some(Ty::Py))
                            }
                            _ => return Err(format!("bb{bi}: py_call takes two arguments")),
                        },
                        Callee::Builtin(b) => {
                            if matches!(b, Builtin::SleepMs | Builtin::LlmGenerate) && !f.is_async {
                                return Err(format!(
                                    "bb{bi}: {} outside of an async function",
                                    b.name()
                                ));
                            }
                            (b.signature().to_vec(), b.ret())
                        }
                        Callee::Func(id) => {
                            let g = p
                                .funcs
                                .get(id.0 as usize)
                                .ok_or_else(|| format!("unknown function f{}", id.0))?;
                            // async の関数は、async の関数からだけ直接呼べる（`await f()`。ADR-0020 論点 5）
                            if g.is_async && !f.is_async {
                                return Err(format!(
                                    "bb{bi}: async function f{} called from a non-async function",
                                    id.0
                                ));
                            }
                            (g.locals[..g.param_count as usize].to_vec(), g.ret)
                        }
                    };
                    if params.len() != args.len() {
                        return Err(format!("bb{bi}: call argument count mismatch"));
                    }
                    for (a, want) in args.iter().zip(params) {
                        expect("call argument", use_(*a, &defined_in)?, want)?;
                    }
                    match (dst, ret) {
                        (Some(d), Some(r)) => {
                            expect("call result", temp_ty(*d)?, r)?;
                            define(*d, &mut defined_in)?;
                        }
                        (None, _) => {}
                        (Some(_), None) => {
                            return Err(format!(
                                "bb{bi}: call to function without result has a destination"
                            ));
                        }
                    }
                }
            }
        }
        match &block.term {
            Terminator::Return(v) => match (v, f.ret) {
                (None, None) => {}
                (Some(t), Some(r)) => expect("return", use_(*t, &defined_in)?, r)?,
                _ => {
                    return Err(format!(
                        "bb{bi}: return does not match the function's return type"
                    ));
                }
            },
            Terminator::Jump(b) => check_block(*b)?,
            Terminator::Branch {
                cond,
                then_block,
                else_block,
            } => {
                expect("branch condition", use_(*cond, &defined_in)?, Ty::Bool)?;
                check_block(*then_block)?;
                check_block(*else_block)?;
            }
            Terminator::Unreachable => {}
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- dump

impl fmt::Display for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, s) in self.strings.iter().enumerate() {
            writeln!(f, "string s{i} = {s:?}")?;
        }
        for (i, fields) in self.records.iter().enumerate() {
            let fs: Vec<String> = fields
                .iter()
                .map(|(n, t)| format!("{n}: {}", t.name()))
                .collect();
            writeln!(f, "record r{i} = {{{}}}", fs.join(", "))?;
        }
        for (i, members) in self.unions.iter().enumerate() {
            let names: Vec<String> = members.iter().map(|m| member_name(*m)).collect();
            writeln!(f, "union u{i} = {}", names.join(" | "))?;
        }
        for (i, (k, v)) in self.dicts.iter().enumerate() {
            writeln!(f, "dict dict{i} = dict[{}, {}]", k.name(), v.name())?;
        }
        for (i, r) in self.tasks.iter().enumerate() {
            writeln!(f, "task task{i} = Task[{}]", member_name(*r))?;
        }
        if let Some((l, d)) = self.unknown {
            writeln!(
                f,
                "unknown = int | bool | str | float | None | list{} | dict{}",
                l.0, d.0
            )?;
        }
        writeln!(f, "entry f{}", self.entry.0)?;
        for (i, func) in self.funcs.iter().enumerate() {
            writeln!(f)?;
            let kw = if func.is_async { "async fn" } else { "fn" };
            write!(f, "{kw} f{i} {}(", func.name)?;
            for p in 0..func.param_count as usize {
                if p > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "l{p}: {}", func.locals[p].name())?;
            }
            write!(f, ")")?;
            if let Some(r) = func.ret {
                write!(f, " -> {}", r.name())?;
            }
            let uses = if func.effects_inferred {
                "uses(inferred)"
            } else {
                "uses"
            };
            match &func.effects {
                Some(es) if es.is_empty() => write!(f, " {uses} pure")?,
                Some(es) => {
                    let names: Vec<&str> = es.iter().map(|e| e.name()).collect();
                    write!(f, " {uses} {}", names.join(", "))?
                }
                None => {}
            }
            // 属性（ADR-0022 論点 16）
            if let Some(t) = &func.tool {
                if t.approval {
                    write!(f, " @tool(approval=required)")?;
                } else {
                    write!(f, " @tool")?;
                }
            }
            if let Some(a) = &func.agent {
                let mut parts = vec![
                    format!("tools=[{}]", a.tools.join(", ")),
                    format!(
                        "capabilities=[{}]",
                        a.capabilities
                            .iter()
                            .map(|e| e.name())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                ];
                if !a.model.is_empty() {
                    parts.push(format!("model={:?}", a.model));
                }
                for (k, v) in [
                    ("max_llm_calls", a.max_llm_calls),
                    ("max_tool_calls", a.max_tool_calls),
                    ("max_tokens", a.max_tokens),
                    ("timeout_ms", a.timeout_ms),
                ] {
                    if v > 0 {
                        parts.push(format!("{k}={v}"));
                    }
                }
                if a.max_cost_micro_usd > 0 {
                    parts.push(format!(
                        "max_cost_usd={}",
                        a.max_cost_micro_usd as f64 / 1e6
                    ));
                }
                write!(f, " @agent({})", parts.join(", "))?;
            }
            writeln!(f, " {{")?;
            for (l, ty) in func
                .locals
                .iter()
                .enumerate()
                .skip(func.param_count as usize)
            {
                writeln!(f, "  local l{l}: {}", ty.name())?;
            }
            for (bi, b) in func.blocks.iter().enumerate() {
                writeln!(f, "bb{bi}:")?;
                for inst in &b.insts {
                    write!(f, "  ")?;
                    match inst {
                        Inst::Const { dst, value } => match value {
                            Const::Int(v) => writeln!(f, "t{} = const {v}", dst.0)?,
                            Const::Float(b) => {
                                writeln!(f, "t{} = const {:?}", dst.0, f64::from_bits(*b))?
                            }
                            Const::Bool(v) => writeln!(f, "t{} = const {v}", dst.0)?,
                            Const::Str(s) => writeln!(f, "t{} = const s{}", dst.0, s.0)?,
                            Const::Schema(t) => {
                                writeln!(f, "t{} = const schema {}", dst.0, t.name())?
                            }
                        },
                        Inst::Load { dst, src } => writeln!(f, "t{} = load l{}", dst.0, src.0)?,
                        Inst::Take { dst, src } => writeln!(f, "t{} = take l{}", dst.0, src.0)?,
                        Inst::Store { dst, src } => writeln!(f, "store l{}, t{}", dst.0, src.0)?,
                        Inst::Binary { dst, op, lhs, rhs } => {
                            writeln!(f, "t{} = {} t{}, t{}", dst.0, op.name(), lhs.0, rhs.0)?
                        }
                        Inst::Not { dst, src } => writeln!(f, "t{} = not t{}", dst.0, src.0)?,
                        Inst::MakeUnion {
                            dst,
                            union,
                            variant,
                            payload,
                        } => match payload {
                            Some(t) => writeln!(
                                f,
                                "t{} = make_union u{}.{variant}(t{})",
                                dst.0, union.0, t.0
                            )?,
                            None => {
                                writeln!(f, "t{} = make_union u{}.{variant}()", dst.0, union.0)?
                            }
                        },
                        Inst::MakeUnknown { dst, kind, payload } => {
                            let k = UNKNOWN_KINDS[*kind as usize];
                            match payload {
                                Some(t) => writeln!(f, "t{} = make_unknown {k}(t{})", dst.0, t.0)?,
                                None => writeln!(f, "t{} = make_unknown {k}()", dst.0)?,
                            }
                        }
                        Inst::UnionIs { dst, src, variant } => {
                            writeln!(f, "t{} = union_is t{}, {variant}", dst.0, src.0)?
                        }
                        Inst::Retain { src, reason } => {
                            writeln!(f, "retain t{} ; {reason}", src.0)?
                        }
                        Inst::ListNew { dst, list, items } => {
                            let xs: Vec<String> =
                                items.iter().map(|t| format!("t{}", t.0)).collect();
                            writeln!(f, "t{} = list_new list{}[{}]", dst.0, list.0, xs.join(", "))?
                        }
                        Inst::ListLen { dst, src } => {
                            writeln!(f, "t{} = list_len t{}", dst.0, src.0)?
                        }
                        Inst::ListGet { dst, src, index } => {
                            writeln!(f, "t{} = list_get t{}, t{}", dst.0, src.0, index.0)?
                        }
                        Inst::ListSet { list, index, value } => {
                            writeln!(f, "list_set t{}, t{}, t{}", list.0, index.0, value.0)?
                        }
                        Inst::ListPush { list, value } => {
                            writeln!(f, "list_push t{}, t{}", list.0, value.0)?
                        }
                        Inst::Release { src, reason } => {
                            writeln!(f, "release t{} ; {reason}", src.0)?
                        }
                        Inst::DictNew {
                            dst,
                            dict,
                            keys,
                            vals,
                        } => {
                            let xs: Vec<String> = keys
                                .iter()
                                .zip(vals)
                                .map(|(k, v)| format!("t{}: t{}", k.0, v.0))
                                .collect();
                            writeln!(
                                f,
                                "t{} = dict_new dict{}{{{}}}",
                                dst.0,
                                dict.0,
                                xs.join(", ")
                            )?
                        }
                        Inst::DictLen { dst, src } => {
                            writeln!(f, "t{} = dict_len t{}", dst.0, src.0)?
                        }
                        Inst::DictContains { dst, src, key } => {
                            writeln!(f, "t{} = dict_contains t{}, t{}", dst.0, src.0, key.0)?
                        }
                        Inst::DictGet { dst, src, key } => {
                            writeln!(f, "t{} = dict_get t{}, t{}", dst.0, src.0, key.0)?
                        }
                        Inst::DictPop { dst, src, key } => {
                            writeln!(f, "t{} = dict_pop t{}, t{}", dst.0, src.0, key.0)?
                        }
                        Inst::DictSet { dict, key, value } => {
                            writeln!(f, "dict_set t{}, t{}, t{}", dict.0, key.0, value.0)?
                        }
                        Inst::DictKeyAt { dst, src, index } => {
                            writeln!(f, "t{} = dict_key_at t{}, t{}", dst.0, src.0, index.0)?
                        }
                        Inst::DictIterNext {
                            dst,
                            src,
                            pos,
                            len0,
                        } => writeln!(
                            f,
                            "t{} = dict_iter_next t{}, t{}, t{}",
                            dst.0, src.0, pos.0, len0.0
                        )?,
                        Inst::MakeRecord {
                            dst,
                            record,
                            fields,
                        } => {
                            let fs: Vec<String> =
                                fields.iter().map(|t| format!("t{}", t.0)).collect();
                            writeln!(
                                f,
                                "t{} = make_record r{}({})",
                                dst.0,
                                record.0,
                                fs.join(", ")
                            )?
                        }
                        Inst::Field { dst, src, index } => {
                            writeln!(f, "t{} = field t{}, {index}", dst.0, src.0)?
                        }
                        Inst::UnionPayload { dst, src, variant } => {
                            writeln!(f, "t{} = union_payload t{}, {variant}", dst.0, src.0)?
                        }
                        Inst::Spawn {
                            dst,
                            task,
                            func,
                            args,
                        } => {
                            let a: Vec<String> = args.iter().map(|a| format!("t{}", a.0)).collect();
                            writeln!(
                                f,
                                "t{} = spawn task{} f{}({})",
                                dst.0,
                                task.0,
                                func.0,
                                a.join(", ")
                            )?
                        }
                        Inst::Await { dst, task } => match dst {
                            Some(d) => writeln!(f, "t{} = await t{}", d.0, task.0)?,
                            None => writeln!(f, "await t{}", task.0)?,
                        },
                        Inst::Call { dst, callee, args } => {
                            if let Some(d) = dst {
                                write!(f, "t{} = ", d.0)?;
                            }
                            match callee {
                                Callee::Func(id) => write!(f, "call f{}(", id.0)?,
                                Callee::Builtin(b) => write!(f, "call @{}(", b.name())?,
                            }
                            for (i, a) in args.iter().enumerate() {
                                if i > 0 {
                                    write!(f, ", ")?;
                                }
                                write!(f, "t{}", a.0)?;
                            }
                            writeln!(f, ")")?;
                        }
                    }
                }
                match &b.term {
                    Terminator::Return(None) => writeln!(f, "  return")?,
                    Terminator::Return(Some(t)) => writeln!(f, "  return t{}", t.0)?,
                    Terminator::Jump(b) => writeln!(f, "  jump bb{}", b.0)?,
                    Terminator::Branch {
                        cond,
                        then_block,
                        else_block,
                    } => writeln!(
                        f,
                        "  branch t{}, bb{}, bb{}",
                        cond.0, then_block.0, else_block.0
                    )?,
                    Terminator::Unreachable => writeln!(f, "  unreachable")?,
                }
            }
            writeln!(f, "}}")?;
        }
        Ok(())
    }
}

/// `async`（ADR-0020）を使うプログラムか: エントリが `async` の関数なら、`main` はスケジューラの上で動かす。
pub fn uses_async(p: &Program) -> bool {
    p.funcs.get(p.entry.0 as usize).is_some_and(|f| f.is_async)
}

/// Python のランタイムを使うプログラムか（Python の組み込み関数を呼ぶか、ハンドルの型 `py` を使う）。
/// driver がリンクするランタイムを選び、バックエンドが宣言と終了処理を入れる（ADR-0018 論点 4・7）。
pub fn uses_python(p: &Program) -> bool {
    let has_py = |t: &Ty| *t == Ty::Py;
    p.lists.iter().any(has_py)
        || p.dicts.iter().any(|(_, v)| has_py(v))
        || p.unions.iter().flatten().flatten().any(has_py)
        || p.records.iter().flatten().any(|(_, t)| has_py(t))
        || p.funcs.iter().any(|f| {
            f.locals.iter().chain(&f.temps).any(has_py)
                || f.blocks.iter().flat_map(|b| &b.insts).any(
                    |i| matches!(i, Inst::Call { callee: Callee::Builtin(b), .. } if b.is_python()),
                )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello() -> Program {
        Program {
            strings: vec!["hi".into()],
            unions: vec![],
            records: vec![],
            lists: vec![],
            dicts: vec![],
            tasks: vec![],
            unknown: None,
            display: DisplayInfo::default(),
            entry: FuncId(0),
            funcs: vec![Function {
                name: "main".into(),
                param_count: 0,
                locals: vec![],
                ret: None,
                temps: vec![Ty::Str],
                blocks: vec![Block {
                    insts: vec![
                        Inst::Const {
                            dst: TempId(0),
                            value: Const::Str(StrId(0)),
                        },
                        Inst::Call {
                            dst: None,
                            callee: Callee::Builtin(Builtin::PrintStr),
                            args: vec![TempId(0)],
                        },
                    ],
                    term: Terminator::Return(None),
                }],
                elided_rc: Vec::new(),
                is_async: false,
                effects: None,
                effects_inferred: false,
                tool: None,
                agent: None,
            }],
        }
    }

    #[test]
    fn valid_program_verifies_and_dumps() {
        let p = hello();
        verify(&p).unwrap();
        let text = p.to_string();
        assert!(text.contains("t0 = const s0"), "{text}");
        assert!(text.contains("call @print_str(t0)"), "{text}");
    }

    #[test]
    fn rejects_cross_block_temp_use() {
        let mut p = hello();
        let f = &mut p.funcs[0];
        f.blocks[0].insts.pop();
        f.blocks[0].term = Terminator::Jump(BlockId(1));
        f.blocks.push(Block {
            insts: vec![Inst::Call {
                dst: None,
                callee: Callee::Builtin(Builtin::PrintStr),
                args: vec![TempId(0)],
            }],
            term: Terminator::Return(None),
        });
        let err = verify(&p).unwrap_err();
        assert!(err.contains("outside its defining block"), "{err}");
    }

    #[test]
    fn union_instructions_verify_and_dump() {
        let mut p = hello();
        p.unions = vec![vec![Some(Ty::Str), None]];
        let f = &mut p.funcs[0];
        f.temps.extend([Ty::Union(UnionId(0)), Ty::Bool, Ty::Str]);
        f.blocks[0].insts.insert(
            1,
            Inst::MakeUnion {
                dst: TempId(1),
                union: UnionId(0),
                variant: 0,
                payload: Some(TempId(0)),
            },
        );
        f.blocks[0].insts.insert(
            2,
            Inst::UnionIs {
                dst: TempId(2),
                src: TempId(1),
                variant: 1,
            },
        );
        f.blocks[0].insts.insert(
            3,
            Inst::UnionPayload {
                dst: TempId(3),
                src: TempId(1),
                variant: 0,
            },
        );
        verify(&p).unwrap();
        let text = p.to_string();
        assert!(text.contains("union u0 = str | None"), "{text}");
        assert!(text.contains("t1 = make_union u0.0(t0)"), "{text}");
        // None の構成要素から中身は取り出せない
        p.funcs[0].blocks[0].insts[3] = Inst::UnionPayload {
            dst: TempId(3),
            src: TempId(1),
            variant: 1,
        };
        assert!(verify(&p).is_err());
    }

    #[test]
    fn rejects_type_errors() {
        let mut p = hello();
        p.funcs[0].blocks[0].insts[1] = Inst::Call {
            dst: None,
            callee: Callee::Builtin(Builtin::PrintInt),
            args: vec![TempId(0)],
        };
        assert!(verify(&p).is_err());
    }
}

/// 値を +1 で消費する地点（ARC パスの `consume`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConsumeSite {
    Store,
    Return,
    MakeUnion,
    MakeRecord,
    ListNew,
    ListSet,
    ListPush,
    DictNew,
    DictSet,
    /// `spawn` の引数（タスクが所有する。ADR-0020）
    Spawn,
}

impl ConsumeSite {
    /// 安定した識別子（`wys explain` の JSON に出す）。
    pub fn id(self) -> &'static str {
        match self {
            ConsumeSite::Store => "store",
            ConsumeSite::Return => "return",
            ConsumeSite::MakeUnion => "make_union",
            ConsumeSite::MakeRecord => "make_record",
            ConsumeSite::ListNew => "list_new",
            ConsumeSite::ListSet => "list_set",
            ConsumeSite::ListPush => "list_push",
            ConsumeSite::DictNew => "dict_new",
            ConsumeSite::DictSet => "dict_set",
            ConsumeSite::Spawn => "spawn",
        }
    }
}

/// ARC パスが `Retain` / `Release` を挿入した理由（ADR-0017）。ARC パスの挿入地点と1対1に対応する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RcReason {
    /// retain: 関数の入口で、引数をスロットに所有させる。
    ParamSlotEntry,
    /// retain: 借用した値（ローカルからの読み出し、リテラル、取り出した値、不死の文字列）を +1 で消費する。
    ConsumeBorrowed(ConsumeSite),
    /// retain: すでに消費した +1 所有の値を、もう一度消費する。
    ConsumeSharedOwned(ConsumeSite),
    /// retain: 可変な場所（リストの要素、`dict` の値やキー）から読んだ値を、使い終わるまで保持する（借用の安全条件）。
    BorrowFromMutablePlace,
    /// release: `Store` で、スロットの古い値を手放す。
    StoreOverwritesSlot,
    /// release: ブロックの終わりまで消費されなかった +1 所有の値を手放す。
    UnconsumedOwnedAtBlockEnd,
    /// release: 関数を抜けるときに、スロットが持つ値を手放す。
    SlotAtFunctionExit,
}

impl RcReason {
    /// 安定した識別子。`retain` / `release` という語を含めない（IR テキストで命令を数えるテストを壊さないため）。
    pub fn id(self) -> &'static str {
        match self {
            RcReason::ParamSlotEntry => "param_slot_entry",
            RcReason::ConsumeBorrowed(_) => "consume_borrowed",
            RcReason::ConsumeSharedOwned(_) => "consume_shared_owned",
            RcReason::BorrowFromMutablePlace => "borrow_from_mutable_place",
            RcReason::StoreOverwritesSlot => "store_overwrites_slot",
            RcReason::UnconsumedOwnedAtBlockEnd => "unconsumed_owned_at_block_end",
            RcReason::SlotAtFunctionExit => "slot_at_function_exit",
        }
    }

    /// 消費する地点（`consume_*` のときだけ）。
    pub fn site(self) -> Option<ConsumeSite> {
        match self {
            RcReason::ConsumeBorrowed(s) | RcReason::ConsumeSharedOwned(s) => Some(s),
            _ => None,
        }
    }

    /// 人間と AI 向けの説明（英語。診断のメッセージと同じ扱い）。
    pub fn message(self) -> String {
        match self {
            RcReason::ParamSlotEntry => {
                "parameters are passed borrowed (+0); the callee's slot takes its own reference on entry".into()
            }
            RcReason::ConsumeBorrowed(s) => format!(
                "a borrowed value is consumed by `{}`, which needs an owned (+1) reference",
                s.id()
            ),
            RcReason::ConsumeSharedOwned(s) => format!(
                "an owned value that was already consumed is consumed again by `{}`",
                s.id()
            ),
            RcReason::BorrowFromMutablePlace => {
                "a value read from a mutable place (list element, dict value or key) is held while it is used (borrow safety, memory-model 2.2)".into()
            }
            RcReason::StoreOverwritesSlot => {
                "storing to a local gives up the reference held by its old value".into()
            }
            RcReason::UnconsumedOwnedAtBlockEnd => {
                "an owned (+1) value was not consumed in this block; together with its retain this is a candidate for the Move optimization".into()
            }
            RcReason::SlotAtFunctionExit => {
                "leaving the function gives up the reference held by a local slot".into()
            }
        }
    }
}

impl fmt::Display for RcReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.site() {
            Some(s) => write!(f, "{}:{}", self.id(), s.id()),
            None => f.write_str(self.id()),
        }
    }
}
