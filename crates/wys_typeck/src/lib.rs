//! 型検査・型推論
//!
//! - 関数シグネチャは注釈必須、ローカル変数は最初の代入から推論する（LANG-002）。
//! - 暗黙の型変換はしない。条件式は `bool` でなければならない（LANG-007）。
//! - Union 型（`int | str`、`str | None`）は平坦化・正規化して ID で管理する（ADR-0006）。
//! - 流れ解析: 各地点で「確実に代入済みの変数」と「絞り込み後の型」を環境（`Env`）として流す。
//!   `x is None` / `x is not None`（`and` / `or` / `not` の組み合わせを含む）でローカル変数を絞り込む。
//!   `None` の可能性がある値を、`None` を許さない場所で使うと E0301（AC-002）。
//! - 値を返す関数は、すべての経路で `return` しなければならない。

pub mod experimental;

use std::collections::{HashMap, HashSet};
use wys_ast::*;
use wys_diagnostics::{Diagnostic, Hint, RelatedKind};
use wys_span::Span;

/// 型。`Union` は `Types` に登録した構成要素の集合を指す。
/// 順序（`Ord`）は Union の構成要素の正規順として使う（int, bool, str, None）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Ty {
    Int,
    Bool,
    Str,
    /// IEEE 754 の倍精度（ADR-0019）
    Float,
    /// Python のオブジェクト（組み込みの名目的な型、ADR-0018 論点 6）。比較・表示・`dict` のキーには使えない（E1202）
    PyObject,
    /// 構造的レコード（ADR-0007、不変）。
    Record(RecordId),
    /// 型パラメータ（`def f[T]` の `T`）。中身の分からない型として扱い、単相化で具体的な型に置き換える。
    Param(ParamId),
    /// `list[T]`（ヒープ上の可変なリスト、参照を共有する。M3 T4a、ADR-0012）
    List(ListId),
    /// `Result[T, E]`（組み込みの名目的な Generic 型。構築子は `Ok` / `Err`。ADR-0013）
    Result(ResultId),
    /// `dict[K, V]`（ヒープ上の可変な辞書、参照を共有する。キーは `int` / `bool` / `str`。ADR-0015）
    Dict(DictId),
    /// `Task[T]`（`spawn` が返す、並行に進む処理。組み込みの名目的な Generic 型。ADR-0020）。
    /// 表示・比較・`dict` のキーには使えない
    Task(TaskId),
    /// 未検証の外部値（ADR-0011）。`int` / `bool` / `str` / `float`（ADR-0019）/ `None` のどれかだが、開いた型として扱い、
    /// `match` で確かめるまで具体的な型として使えない。
    Unknown,
    None,
    Union(UnionId),
    /// エラーから回復するための型。これを含む式では追加の診断を出さない。
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UnionId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RecordId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ParamId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ListId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ResultId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DictId(pub u32);

/// 型パラメータから具体的な型への対応。
pub type Subst = HashMap<ParamId, Ty>;

/// レコード型の定義。フィールドは名前順（同じフィールド集合なら同じ型）。
#[derive(Debug, Clone)]
pub struct RecordDef {
    /// 最初に定義した `type` の名前（表示用）。構造的なので、名前は型の同一性に関係しない。
    pub name: Option<String>,
    /// Generic な `type` の型引数（表示用。`Pair[int, str]`）。
    pub args: Vec<Ty>,
    pub fields: Vec<(String, Ty)>,
    /// 型の入れ子の深さ（登録時に計算する。同じ型のフィールドを持つ入れ子で、再帰的にたどると指数的になるため）
    pub depth: usize,
    /// フィールドに型パラメータを含むか（登録時に計算する）
    pub has_param: bool,
    /// フィールドに `Unknown` を含むか（登録時に計算する）
    pub has_unknown: bool,
}

impl RecordDef {
    pub fn field(&self, name: &str) -> Option<(usize, Ty)> {
        self.fields
            .iter()
            .position(|(n, _)| n == name)
            .map(|i| (i, self.fields[i].1))
    }
}

/// Union 型の登録表。同じ構成要素の集合には同じ ID を返す。
#[derive(Debug, Clone, Default)]
pub struct Types {
    unions: Vec<Vec<Ty>>,
    index: HashMap<Vec<Ty>, UnionId>,
    records: Vec<RecordDef>,
    record_index: HashMap<Vec<(String, Ty)>, RecordId>,
    params: Vec<String>,
    /// `list[T]` の要素の型（`ListId` の順）
    lists: Vec<Ty>,
    list_index: HashMap<Ty, ListId>,
    /// `Result[T, E]` の型引数（`ResultId` の順）
    results: Vec<(Ty, Ty)>,
    result_index: HashMap<(Ty, Ty), ResultId>,
    /// `dict[K, V]` のキーと値の型（`DictId` の順）
    dicts: Vec<(Ty, Ty)>,
    dict_index: HashMap<(Ty, Ty), DictId>,
    /// `Task[T]` の結果の型（`TaskId` の順）
    tasks: Vec<Ty>,
    task_index: HashMap<Ty, TaskId>,
    /// 組み込みのレコード `PyError` の ID（`py_error` で登録する。E1202 の判定に使う）
    py_error_id: Option<RecordId>,
}

impl Types {
    /// 構成要素から型を作る。Union は平坦化し（`(A | B) | B` = `A | B`）、要素が1つならその型を返す。
    pub fn union(&mut self, items: impl IntoIterator<Item = Ty>) -> Ty {
        let mut members = Vec::new();
        for t in items {
            match t {
                Ty::Error => return Ty::Error,
                Ty::Union(u) => members.extend(self.unions[u.0 as usize].iter().copied()),
                t => members.push(t),
            }
        }
        members.sort();
        members.dedup();
        if members.contains(&Ty::Unknown) {
            // Unknown はスカラーの値をすべて含む
            members.retain(|m| !matches!(m, Ty::Int | Ty::Bool | Ty::Str | Ty::Float | Ty::None));
        }
        match members.len() {
            0 => Ty::Error,
            1 => members[0],
            _ => {
                if let Some(&id) = self.index.get(&members) {
                    return Ty::Union(id);
                }
                let id = UnionId(self.unions.len() as u32);
                self.unions.push(members.clone());
                self.index.insert(members, id);
                Ty::Union(id)
            }
        }
    }

    /// 型の構成要素（Union でなければその型だけ）。
    pub fn members(&self, t: Ty) -> Vec<Ty> {
        match t {
            Ty::Union(u) => self.unions[u.0 as usize].clone(),
            t => vec![t],
        }
    }

    /// 表示用の型名。入れ子が深い型は `…` で省略する（`Pair[Pair[...]]` で文字列が指数的に長くならないように）。
    pub fn name(&self, t: Ty) -> String {
        self.name_within(t, 6)
    }

    fn name_within(&self, t: Ty, depth: usize) -> String {
        if depth == 0
            && matches!(
                t,
                Ty::Union(_) | Ty::Record(_) | Ty::Result(_) | Ty::Dict(_)
            )
        {
            return "…".into();
        }
        let name = |t: Ty| self.name_within(t, depth.saturating_sub(1));
        match t {
            Ty::Int => "int".into(),
            Ty::Float => "float".into(),
            Ty::PyObject => "PyObject".into(),
            Ty::Bool => "bool".into(),
            Ty::Str => "str".into(),
            Ty::None => "None".into(),
            Ty::Error => "<error>".into(),
            Ty::Param(p) => self.params[p.0 as usize].clone(),
            Ty::Unknown => "Unknown".into(),
            Ty::List(l) => format!("list[{}]", name(self.lists[l.0 as usize])),
            Ty::Task(k) => format!("Task[{}]", name(self.tasks[k.0 as usize])),
            Ty::Result(r) => {
                let (ok, err) = self.results[r.0 as usize];
                format!("Result[{}, {}]", name(ok), name(err))
            }
            Ty::Dict(d) => {
                let (k, v) = self.dicts[d.0 as usize];
                format!("dict[{}, {}]", name(k), name(v))
            }
            Ty::Record(r) => {
                let def = &self.records[r.0 as usize];
                match &def.name {
                    Some(n) if def.args.is_empty() => n.clone(),
                    Some(n) => format!(
                        "{n}[{}]",
                        def.args
                            .iter()
                            .map(|a| name(*a))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    None => format!(
                        "{{{}}}",
                        def.fields
                            .iter()
                            .map(|(n, t)| format!("{n}: {}", name(*t)))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                }
            }
            Ty::Union(_) => self
                .members(t)
                .into_iter()
                .map(name)
                .collect::<Vec<_>>()
                .join(" | "),
        }
    }

    pub fn contains_none(&self, t: Ty) -> bool {
        self.members(t).contains(&Ty::None)
    }

    /// `None` を取り除いた型。`None` しか残らない場合は `Error`（到達しない値）。
    pub fn without_none(&mut self, t: Ty) -> Ty {
        let rest: Vec<Ty> = self
            .members(t)
            .into_iter()
            .filter(|m| *m != Ty::None)
            .collect();
        self.union(rest)
    }

    /// `actual` の値をすべて `expected` の場所に置けるか（レコードは幅部分型、ADR-0007）。
    pub fn fits(&self, actual: Ty, expected: Ty) -> bool {
        if actual == Ty::Error || expected == Ty::Error {
            return true;
        }
        let e = self.members(expected);
        self.members(actual)
            .iter()
            .all(|a| e.iter().any(|x| self.member_fits(*a, *x)))
    }

    /// Union の構成要素どうしの互換性。レコードは、要求されるフィールドをすべて同じ型で持てば互換。
    pub fn member_fits(&self, a: Ty, e: Ty) -> bool {
        match (a, e) {
            _ if a == e => true,
            (Ty::Int | Ty::Bool | Ty::Str | Ty::Float | Ty::None, Ty::Unknown) => true,
            (Ty::Record(x), Ty::Record(y)) => {
                let have = &self.records[x.0 as usize].fields;
                self.records[y.0 as usize]
                    .fields
                    .iter()
                    .all(|f| have.contains(f))
            }
            _ => false,
        }
    }

    /// レコード型を作る（フィールドは名前順に正規化する）。
    pub fn record(&mut self, fields: Vec<(String, Ty)>, name: Option<String>) -> Ty {
        self.record_named(fields, name, Vec::new())
    }

    pub fn record_named(
        &mut self,
        mut fields: Vec<(String, Ty)>,
        name: Option<String>,
        args: Vec<Ty>,
    ) -> Ty {
        fields.sort_by(|a, b| a.0.cmp(&b.0));
        if let Some(&id) = self.record_index.get(&fields) {
            let def = &mut self.records[id.0 as usize];
            if def.name.is_none() {
                def.name = name;
                def.args = args;
            }
            return Ty::Record(id);
        }
        let id = RecordId(self.records.len() as u32);
        let depth = 1 + fields
            .iter()
            .map(|(_, t)| self.depth(*t))
            .max()
            .unwrap_or(0);
        let has_param = fields.iter().any(|(_, t)| self.contains_param(*t));
        let has_unknown = fields.iter().any(|(_, t)| self.contains_unknown(*t));
        self.records.push(RecordDef {
            name,
            args,
            fields: fields.clone(),
            depth,
            has_param,
            has_unknown,
        });
        self.record_index.insert(fields, id);
        Ty::Record(id)
    }

    pub fn new_param(&mut self, name: &str) -> ParamId {
        self.params.push(name.to_string());
        ParamId(self.params.len() as u32 - 1)
    }

    /// 型パラメータを置き換える。Union は置き換えた後に平坦化し直す（`T | None` で `T = str | None` なら `str | None`）。
    pub fn subst(&mut self, t: Ty, map: &Subst) -> Ty {
        match t {
            Ty::Param(p) => map.get(&p).copied().unwrap_or(t),
            Ty::List(l) => {
                let e = self.lists[l.0 as usize];
                if !self.contains_param(e) {
                    return t;
                }
                let e = self.subst(e, map);
                self.list(e)
            }
            Ty::Task(k) => {
                let e = self.tasks[k.0 as usize];
                if !self.contains_param(e) {
                    return t;
                }
                let e = self.subst(e, map);
                self.task(e)
            }
            Ty::Result(r) => {
                if !self.contains_param(t) {
                    return t;
                }
                let (ok, err) = self.results[r.0 as usize];
                let (ok, err) = (self.subst(ok, map), self.subst(err, map));
                self.result(ok, err)
            }
            Ty::Dict(d) => {
                if !self.contains_param(t) {
                    return t;
                }
                let (k, v) = self.dicts[d.0 as usize];
                let (k, v) = (self.subst(k, map), self.subst(v, map));
                self.dict(k, v)
            }
            Ty::Union(_) => {
                let ms: Vec<Ty> = self
                    .members(t)
                    .into_iter()
                    .map(|m| self.subst(m, map))
                    .collect();
                self.union(ms)
            }
            Ty::Record(r) => {
                if !self.contains_param(t) {
                    return t;
                }
                let def = self.records[r.0 as usize].clone();
                let fields = def
                    .fields
                    .iter()
                    .map(|(n, ft)| (n.clone(), self.subst(*ft, map)))
                    .collect();
                let args = def.args.iter().map(|a| self.subst(*a, map)).collect();
                self.record_named(fields, def.name, args)
            }
            _ => t,
        }
    }

    /// 型パラメータを含むか。レコードは登録時に計算した値を使うので、入れ子が深くても定数時間。
    pub fn contains_param(&self, t: Ty) -> bool {
        match t {
            Ty::Param(_) => true,
            // Union の構成要素は Union を含まない（平坦化済み）
            Ty::Union(_) => self.members(t).iter().any(|m| self.contains_param(*m)),
            Ty::Record(r) => self.records[r.0 as usize].has_param,
            Ty::List(l) => self.contains_param(self.lists[l.0 as usize]),
            Ty::Task(k) => self.contains_param(self.tasks[k.0 as usize]),
            Ty::Result(r) => {
                let (ok, err) = self.results[r.0 as usize];
                self.contains_param(ok) || self.contains_param(err)
            }
            Ty::Dict(d) => {
                let (k, v) = self.dicts[d.0 as usize];
                self.contains_param(k) || self.contains_param(v)
            }
            _ => false,
        }
    }

    /// `Unknown` を含むか。レコードは登録時に計算した値を使う。
    /// Python のオブジェクト（`PyObject`）か `PyError` を含む型か（ADR-0018 論点 13: 比較・表示に使えない）。
    /// `PyError` は構造的なレコードなので、同じフィールドを持つ型（`{kind: str, message: str, traceback: str}`）も含む
    pub fn contains_py(&self, t: Ty) -> bool {
        fn go(ts: &Types, t: Ty, seen: &mut HashSet<Ty>) -> bool {
            if !seen.insert(t) {
                return false;
            }
            match t {
                Ty::PyObject => true,
                // `PyError` も表示・比較できない（ADR-0018 論点 13）。フィールドは読める
                Ty::Record(id) if ts.py_error_id == Some(id) => true,
                Ty::List(l) => go(ts, ts.list_elem(l), seen),
                Ty::Task(k) => go(ts, ts.task_elem(k), seen),
                Ty::Dict(d) => {
                    let (k, v) = ts.dict_args(d);
                    go(ts, k, seen) || go(ts, v, seen)
                }
                Ty::Result(r) => {
                    let (o, e) = ts.result_args(r);
                    go(ts, o, seen) || go(ts, e, seen)
                }
                Ty::Union(_) => ts.members(t).into_iter().any(|m| go(ts, m, seen)),
                Ty::Record(id) => ts
                    .record_def(id)
                    .fields
                    .clone()
                    .into_iter()
                    .any(|(_, f)| go(ts, f, seen)),
                _ => false,
            }
        }
        go(self, t, &mut HashSet::new())
    }

    /// Python の例外を表す組み込みのレコード `PyError`（ADR-0018 論点 8）。最初に登録するので、
    /// 同じフィールドを持つ構造的な型も `PyError` の名前で表示する。
    pub fn py_error(&mut self) -> Ty {
        let t = self.record_named(
            vec![
                ("kind".into(), Ty::Str),
                ("message".into(), Ty::Str),
                ("traceback".into(), Ty::Str),
            ],
            Some("PyError".into()),
            vec![],
        );
        if let Ty::Record(id) = t {
            self.py_error_id = Some(id);
        }
        t
    }

    /// `parse_json` の失敗を表す組み込みのレコード `JsonError`（ADR-0021 論点 5）。`offset` は UTF-8 のバイト位置。
    pub fn json_error(&mut self) -> Ty {
        self.record_named(
            vec![("message".into(), Ty::Str), ("offset".into(), Ty::Int)],
            Some("JsonError".into()),
            vec![],
        )
    }

    /// `validate` の失敗を表す組み込みのレコード `SchemaError`（ADR-0022 論点 6）。フィールドは名前順に並ぶ
    /// （`expected`、`found`、`path`）。
    pub fn schema_error(&mut self) -> Ty {
        self.record_named(
            vec![
                ("path".into(), Ty::Str),
                ("expected".into(), Ty::Str),
                ("found".into(), Ty::Str),
            ],
            Some("SchemaError".into()),
            vec![],
        )
    }

    /// AI SDK の組み込みのレコード（ADR-0022 論点 3）。構造的で、同じフィールドの利用者のレコードと同じ型。
    pub fn message(&mut self) -> Ty {
        self.record_named(
            vec![("role".into(), Ty::Str), ("content".into(), Ty::Str)],
            Some("Message".into()),
            vec![],
        )
    }

    pub fn llm_request(&mut self) -> Ty {
        let m = self.message();
        let ms = self.list(m);
        self.record_named(
            vec![
                ("model".into(), Ty::Str),
                ("system".into(), Ty::Str),
                ("messages".into(), ms),
                ("max_tokens".into(), Ty::Int),
            ],
            Some("LlmRequest".into()),
            vec![],
        )
    }

    pub fn llm_error(&mut self) -> Ty {
        self.record_named(
            vec![("kind".into(), Ty::Str), ("message".into(), Ty::Str)],
            Some("LlmError".into()),
            vec![],
        )
    }

    pub fn audit_event(&mut self) -> Ty {
        let tool = self.union([Ty::Str, Ty::None]);
        self.record_named(
            vec![
                ("seq".into(), Ty::Int),
                ("kind".into(), Ty::Str),
                ("agent".into(), Ty::Str),
                ("tool".into(), tool),
                ("detail".into(), Ty::Str),
            ],
            Some("AuditEvent".into()),
            vec![],
        )
    }

    /// `llm_usage()` の結果（ADR-0025 論点 5）。
    pub fn llm_usage(&mut self) -> Ty {
        self.record_named(
            vec![
                ("llm_calls".into(), Ty::Int),
                ("tool_calls".into(), Ty::Int),
                ("input_tokens".into(), Ty::Int),
                ("output_tokens".into(), Ty::Int),
                ("cost_usd".into(), Ty::Float),
            ],
            Some("LlmUsage".into()),
            vec![],
        )
    }

    pub fn mock_tool_call(&mut self) -> Ty {
        self.record_named(
            vec![("name".into(), Ty::Str), ("args_json".into(), Ty::Str)],
            Some("MockToolCall".into()),
            vec![],
        )
    }

    pub fn mock_response(&mut self) -> Ty {
        let c = self.mock_tool_call();
        let cs = self.list(c);
        self.record_named(
            vec![
                ("text".into(), Ty::Str),
                ("tool_calls".into(), cs),
                ("input_tokens".into(), Ty::Int),
                ("output_tokens".into(), Ty::Int),
                ("latency_ms".into(), Ty::Int),
            ],
            Some("MockResponse".into()),
            vec![],
        )
    }

    /// 組み込みのレコードの名前（`PyError`、`JsonError`、`SchemaError` と AI SDK の 6 つ）。
    pub fn builtin_record(&mut self, name: &str) -> Option<Ty> {
        Some(match name {
            "PyError" => self.py_error(),
            "JsonError" => self.json_error(),
            "SchemaError" => self.schema_error(),
            "Message" => self.message(),
            "LlmRequest" => self.llm_request(),
            "LlmError" => self.llm_error(),
            "AuditEvent" => self.audit_event(),
            "LlmUsage" => self.llm_usage(),
            "MockToolCall" => self.mock_tool_call(),
            "MockResponse" => self.mock_response(),
            _ => return None,
        })
    }

    /// スキーマ型か（ADR-0022 論点 4 と訂正）。`top` なら一番外側（`None` だけの型は使えない）。
    pub fn is_schema_type(&self, t: Ty, top: bool) -> bool {
        match t {
            Ty::Int | Ty::Bool | Ty::Str | Ty::Float | Ty::Unknown | Ty::Error => true,
            Ty::None => !top,
            Ty::List(l) => self.is_schema_type(self.list_elem(l), false),
            Ty::Dict(d) => {
                let (k, v) = self.dict_args(d);
                k == Ty::Str && self.is_schema_type(v, false)
            }
            Ty::Union(_) => self
                .members(t)
                .into_iter()
                .all(|m| self.is_schema_type(m, false)),
            Ty::Record(r) => {
                self.py_error_id != Some(r)
                    && self.records[r.0 as usize]
                        .fields
                        .iter()
                        .all(|(_, ft)| self.is_schema_type(*ft, false))
            }
            _ => false,
        }
    }

    pub fn contains_unknown(&self, t: Ty) -> bool {
        match t {
            Ty::Unknown => true,
            Ty::Union(_) => self.members(t).iter().any(|m| self.contains_unknown(*m)),
            Ty::Record(r) => self.records[r.0 as usize].has_unknown,
            Ty::List(l) => self.contains_unknown(self.lists[l.0 as usize]),
            Ty::Task(k) => self.contains_unknown(self.tasks[k.0 as usize]),
            Ty::Result(r) => {
                let (ok, err) = self.results[r.0 as usize];
                self.contains_unknown(ok) || self.contains_unknown(err)
            }
            Ty::Dict(d) => {
                let (k, v) = self.dicts[d.0 as usize];
                self.contains_unknown(k) || self.contains_unknown(v)
            }
            _ => false,
        }
    }

    /// `list[elem]` の型を作る（同じ要素の型なら同じ ID）。
    pub fn list(&mut self, elem: Ty) -> Ty {
        if elem == Ty::Error {
            return Ty::Error;
        }
        if let Some(&id) = self.list_index.get(&elem) {
            return Ty::List(id);
        }
        let id = ListId(self.lists.len() as u32);
        self.lists.push(elem);
        self.list_index.insert(elem, id);
        Ty::List(id)
    }

    /// `list[T]` の要素の型。
    pub fn list_elem(&self, l: ListId) -> Ty {
        self.lists[l.0 as usize]
    }

    /// `Task[result]` の型を作る（同じ結果の型なら同じ ID。ADR-0020）。
    pub fn task(&mut self, result: Ty) -> Ty {
        if result == Ty::Error {
            return Ty::Error;
        }
        if let Some(&id) = self.task_index.get(&result) {
            return Ty::Task(id);
        }
        let id = TaskId(self.tasks.len() as u32);
        self.tasks.push(result);
        self.task_index.insert(result, id);
        Ty::Task(id)
    }

    /// `Task[T]` の結果の型。
    pub fn task_elem(&self, k: TaskId) -> Ty {
        self.tasks[k.0 as usize]
    }

    /// `Result[ok, err]` の型を作る（同じ型引数なら同じ ID）。
    pub fn result(&mut self, ok: Ty, err: Ty) -> Ty {
        if ok == Ty::Error || err == Ty::Error {
            return Ty::Error;
        }
        if let Some(&id) = self.result_index.get(&(ok, err)) {
            return Ty::Result(id);
        }
        let id = ResultId(self.results.len() as u32);
        self.results.push((ok, err));
        self.result_index.insert((ok, err), id);
        Ty::Result(id)
    }

    /// `dict[k, v]` の型を作る（同じ型引数なら同じ ID）。
    pub fn dict(&mut self, k: Ty, v: Ty) -> Ty {
        if k == Ty::Error || v == Ty::Error {
            return Ty::Error;
        }
        if let Some(&id) = self.dict_index.get(&(k, v)) {
            return Ty::Dict(id);
        }
        let id = DictId(self.dicts.len() as u32);
        self.dicts.push((k, v));
        self.dict_index.insert((k, v), id);
        Ty::Dict(id)
    }

    /// `dict[K, V]` のキーと値の型 `(K, V)`。
    pub fn dict_args(&self, d: DictId) -> (Ty, Ty) {
        self.dicts[d.0 as usize]
    }

    /// `Result[T, E]` の型引数 `(T, E)`。
    pub fn result_args(&self, r: ResultId) -> (Ty, Ty) {
        self.results[r.0 as usize]
    }

    /// 型の入れ子の深さ（基本型は 0）。レコードは登録時に計算した値を使う。
    pub fn depth(&self, t: Ty) -> usize {
        match t {
            Ty::Union(_) => {
                1 + self
                    .members(t)
                    .iter()
                    .map(|m| self.depth(*m))
                    .max()
                    .unwrap_or(0)
            }
            Ty::Record(r) => self.records[r.0 as usize].depth,
            Ty::List(l) => 1 + self.depth(self.lists[l.0 as usize]),
            Ty::Task(k) => 1 + self.depth(self.tasks[k.0 as usize]),
            Ty::Result(r) => {
                let (ok, err) = self.results[r.0 as usize];
                1 + self.depth(ok).max(self.depth(err))
            }
            Ty::Dict(d) => {
                let (k, v) = self.dicts[d.0 as usize];
                1 + self.depth(k).max(self.depth(v))
            }
            _ => 0,
        }
    }

    /// `==` で比べられる型か（ADR-0016 論点 11）。構成要素も再帰的に比べられること。
    /// 型パラメータと `Unknown` は比べられない。同じ型を二度たどらない（入れ子が深くても線形）。
    pub fn comparable(&self, t: Ty) -> bool {
        fn go(ts: &Types, t: Ty, seen: &mut HashSet<Ty>) -> bool {
            if !seen.insert(t) {
                return true;
            }
            match t {
                Ty::Int | Ty::Bool | Ty::Str | Ty::Float | Ty::None => true,
                // `Task` は比べられない・表示できない（ADR-0020 論点 14）
                Ty::Param(_) | Ty::Unknown | Ty::Error | Ty::PyObject | Ty::Task(_) => false,
                Ty::List(l) => go(ts, ts.list_elem(l), seen),
                Ty::Dict(d) => {
                    let (k, v) = ts.dict_args(d);
                    go(ts, k, seen) && go(ts, v, seen)
                }
                Ty::Result(r) => {
                    let (o, e) = ts.result_args(r);
                    go(ts, o, seen) && go(ts, e, seen)
                }
                Ty::Union(_) => ts.members(t).into_iter().all(|m| go(ts, m, seen)),
                Ty::Record(id) => ts
                    .record_def(id)
                    .fields
                    .clone()
                    .into_iter()
                    .all(|(_, f)| go(ts, f, seen)),
            }
        }
        go(self, t, &mut HashSet::new())
    }

    /// 型引数を置き換えた結果、`None` だけを持つ場所（レコードのフィールド、`dict` の値）ができたか。
    /// できていれば、その場所の説明（`field `second` of `Pair``）を返す。型の定義では E0215 で禁じているが、
    /// Generic な型に `None` を与えると置き換えで現れる。
    pub fn none_slot(&self, t: Ty) -> Option<String> {
        fn go(ts: &Types, t: Ty, seen: &mut HashSet<Ty>) -> Option<String> {
            if !seen.insert(t) {
                return None;
            }
            match t {
                Ty::List(l) => go(ts, ts.list_elem(l), seen),
                // `Task[None]` は結果を持たないタスク（`None` だけの場所ではない）
                Ty::Task(k) => go(ts, ts.task_elem(k), seen),
                Ty::Dict(d) => {
                    let (k, v) = ts.dict_args(d);
                    if v == Ty::None {
                        return Some("the value of a dict".into());
                    }
                    go(ts, k, seen).or_else(|| go(ts, v, seen))
                }
                Ty::Result(r) => {
                    let (o, e) = ts.result_args(r);
                    go(ts, o, seen).or_else(|| go(ts, e, seen))
                }
                Ty::Union(_) => ts.members(t).into_iter().find_map(|m| go(ts, m, seen)),
                Ty::Record(id) => {
                    let def = ts.record_def(id);
                    def.fields.iter().find_map(|(n, f)| {
                        if *f == Ty::None {
                            Some(format!(
                                "field `{n}` of `{}`",
                                def.name.as_deref().unwrap_or("record")
                            ))
                        } else {
                            go(ts, *f, seen)
                        }
                    })
                }
                _ => None,
            }
        }
        go(self, t, &mut HashSet::new())
    }

    pub fn record_def(&self, id: RecordId) -> &RecordDef {
        &self.records[id.0 as usize]
    }

    pub fn records(&self) -> &[RecordDef] {
        &self.records
    }

    pub fn is_union(&self, t: Ty) -> bool {
        matches!(t, Ty::Union(_))
    }

    /// 登録済みの Union の構成要素（`UnionId` の順）。
    pub fn unions(&self) -> &[Vec<Ty>] {
        &self.unions
    }
}

/// 組み込みの async 関数（`await` の直下にだけ書け、タスクにはできない。ADR-0020 論点 10、ADR-0022 論点 8）。
fn is_async_builtin(name: &str) -> bool {
    matches!(name, "sleep_ms" | "llm_generate")
}

/// 組み込みのレコードの名前（再定義は E0209。`Name(...)` で作れる）。
pub const BUILTIN_RECORDS: [&str; 10] = [
    "PyError",
    "JsonError",
    "SchemaError",
    "Message",
    "LlmRequest",
    "LlmError",
    "AuditEvent",
    "LlmUsage",
    "MockToolCall",
    "MockResponse",
];

#[derive(Debug, Clone)]
pub struct FuncSig {
    pub name: String,
    /// 型パラメータ（Generic 関数でなければ空）。
    pub type_params: Vec<ParamId>,
    pub params: Vec<Ty>,
    pub ret: Ty,
    pub span: Span,
    /// `uses` 節で宣言した Effect（ADR-0014）。注釈がなければ `None`（検査しない）。
    pub effects: Option<EffectSet>,
    /// `async def`（ADR-0020）
    pub is_async: bool,
    /// `@tool(...)`（ADR-0022 論点 13）
    pub tool: Option<ToolAttr>,
    /// `@agent(...)`（ADR-0022 論点 14）
    pub agent: Option<AgentAttr>,
}

/// `@tool` の属性（ADR-0022 論点 13）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolAttr {
    pub description: String,
    /// `approval="required"`
    pub approval: bool,
}

/// `@agent` の属性（ADR-0022 論点 14）。予算の `0` は上限なし。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentAttr {
    /// ツールの関数の内部名
    pub tools: Vec<String>,
    pub capabilities: EffectSet,
    pub model: String,
    pub max_llm_calls: i64,
    pub max_tool_calls: i64,
    pub max_tokens: i64,
    pub timeout_ms: i64,
}

/// 変数の参照で、その地点の型が宣言型と異なる（絞り込まれている）こと。
#[derive(Debug, Clone)]
pub struct Narrowing {
    pub name: String,
    /// 参照の位置。
    pub span: Span,
    pub declared: Ty,
    pub narrowed: Ty,
}

#[derive(Debug, Clone)]
pub struct FuncInfo {
    /// 関数内のローカル変数（先頭は引数）。名前と宣言された型。
    pub locals: Vec<(String, Ty)>,
    /// `locals` と同じ順の、各ローカル変数を宣言した位置（`wys explain` 用）。
    pub local_spans: Vec<Span>,
    /// 変数の参照のうち、型が宣言型から絞り込まれていたもの（位置の順。`wys explain` 用）。
    pub narrowings: Vec<Narrowing>,
    /// 関数の呼び出しと、呼び出し先の Effect（位置の順。`wys explain` 用）。
    pub effect_sites: Vec<EffectSite>,
}

#[derive(Debug, Clone)]
pub struct TypeckResult {
    /// `ExprId` ごとの型（絞り込み後の型）。
    pub expr_types: Vec<Ty>,
    /// `module.funcs` と同じ順序。
    pub sigs: Vec<FuncSig>,
    pub funcs: Vec<FuncInfo>,
    pub types: Types,
    /// Generic 関数の呼び出しごとの型引数（`FuncSig::type_params` の順）。呼び出し元の型パラメータを含むことがある。
    pub call_type_args: HashMap<ExprId, Vec<Ty>>,
    /// `case` の型パターン（`case Id():` など）が表す型。パターンの位置（開始の位置）で引く。
    /// lowering は型の別名を知らないので、型検査で解決したものを使う（M4 T0）
    pub pattern_types: HashMap<u32, Ty>,
}

impl TypeckResult {
    pub fn ty(&self, e: &Expr) -> Ty {
        self.expr_types[e.id.0 as usize]
    }
}

/// Effect（ADR-0014）。Phase 1 は2種類だけ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Effect {
    /// 外界との入出力（`print`、`arg` / `arg_count`）
    Io,
    /// プロセスの終了（`exit`）
    Exit,
    /// Python のコードの実行（ADR-0018 論点 12）。任意の副作用を起こし得る。`io` を含意しない
    Python,
    /// LLM の呼び出し（`llm_generate`、ADR-0022 論点 8 / ADR-0023 論点 7）
    Llm,
}

impl Effect {
    pub const ALL: [Effect; 4] = [Effect::Io, Effect::Exit, Effect::Python, Effect::Llm];

    pub fn name(self) -> &'static str {
        match self {
            Effect::Io => "io",
            Effect::Exit => "exit",
            Effect::Python => "python",
            Effect::Llm => "llm",
        }
    }

    pub fn parse(s: &str) -> Option<Effect> {
        Effect::ALL.into_iter().find(|e| e.name() == s)
    }

    fn bit(self) -> u8 {
        match self {
            Effect::Io => 1,
            Effect::Exit => 2,
            Effect::Python => 4,
            Effect::Llm => 8,
        }
    }
}

/// Effect の集合。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct EffectSet(u8);

impl EffectSet {
    pub fn insert(&mut self, e: Effect) {
        self.0 |= e.bit();
    }

    pub fn contains(self, e: Effect) -> bool {
        self.0 & e.bit() != 0
    }

    pub fn union(self, other: EffectSet) -> EffectSet {
        EffectSet(self.0 | other.0)
    }

    /// 宣言の順（`io`, `exit`）の一覧。
    pub fn iter(self) -> Vec<Effect> {
        Effect::ALL
            .into_iter()
            .filter(|e| self.contains(*e))
            .collect()
    }

    /// `self` にあって `other` にないもの。
    pub fn missing_from(self, other: EffectSet) -> Vec<Effect> {
        self.iter()
            .into_iter()
            .filter(|e| !other.contains(*e))
            .collect()
    }

    pub fn names(self) -> Vec<&'static str> {
        self.iter().into_iter().map(Effect::name).collect()
    }
}

/// 組み込み関数の Effect（ADR-0014 論点 6）。組み込み関数でなければ `None`。
pub fn builtin_effects(name: &str) -> Option<EffectSet> {
    // `PyObject` のメソッドは Python のコードを動かす（ADR-0018 論点 12。ADR-0014 論点 5 の例外）
    let py_method = name.starts_with("PyObject.");
    if !BUILTINS.contains(&name) && !py_method {
        return None;
    }
    let mut s = EffectSet::default();
    match name {
        "print" | "arg" | "arg_count" => s.insert(Effect::Io),
        "exit" => s.insert(Effect::Exit),
        "py_import" | "py_value" => s.insert(Effect::Python),
        "llm_generate" => s.insert(Effect::Llm),
        _ if py_method => s.insert(Effect::Python),
        _ => {}
    }
    Some(s)
}

/// 関数の呼び出しと、呼び出し先の Effect（`wys explain` 用）。
#[derive(Debug, Clone)]
pub struct EffectSite {
    pub callee: String,
    pub span: Span,
    /// 呼び出し先の Effect。注釈のない関数なら `None`。
    pub effects: Option<EffectSet>,
}

use wys_ast::BUILTINS;

/// 一度も読まれないローカル変数（W0201、ADR-0016 論点 20）。代入先、`for` の変数、`case Ok(x)` の束縛が対象で、
/// `_` で始まる名前と引数は除く。読まれたかどうかは関数の中の名前で判定する（スコープは区別しない）。
fn unused_locals(f: &FuncDef) -> Vec<Diagnostic> {
    fn bind<'a>(id: &'a Ident, out: &mut Vec<&'a Ident>) {
        if !out.iter().any(|b| b.name == id.name) {
            out.push(id);
        }
    }
    fn stmts<'a>(ss: &'a [Stmt], binds: &mut Vec<&'a Ident>, reads: &mut HashSet<&'a str>) {
        for s in ss {
            match &s.kind {
                StmtKind::Assign { target, value, .. } => {
                    bind(target, binds);
                    expr(value, reads);
                }
                StmtKind::Expr(e) => expr(e, reads),
                StmtKind::Return(v) => {
                    if let Some(e) = v {
                        expr(e, reads);
                    }
                }
                StmtKind::If {
                    branches,
                    else_body,
                } => {
                    for (c, b) in branches {
                        expr(c, reads);
                        stmts(b, binds, reads);
                    }
                    if let Some(b) = else_body {
                        stmts(b, binds, reads);
                    }
                }
                StmtKind::While { cond, body } => {
                    expr(cond, reads);
                    stmts(body, binds, reads);
                }
                StmtKind::For { var, iter, body } => {
                    bind(var, binds);
                    expr(iter, reads);
                    stmts(body, binds, reads);
                }
                // `xs[i] = v` は `xs` を読んで書き換える
                StmtKind::IndexAssign { target, value } => {
                    expr(target, reads);
                    expr(value, reads);
                }
                StmtKind::Match { subject, arms } => {
                    expr(subject, reads);
                    for a in arms {
                        if let PatternKind::Bind(_, Some(v)) = &a.pattern.kind {
                            bind(v, binds);
                        }
                        stmts(&a.body, binds, reads);
                    }
                }
                StmtKind::Pass => {}
            }
        }
    }
    fn expr<'a>(e: &'a Expr, reads: &mut HashSet<&'a str>) {
        match &e.kind {
            ExprKind::Name(n) => {
                reads.insert(n);
            }
            // `a.b.f(...)` の `a` は変数ではない（修飾名）
            ExprKind::Call { args, kwargs, .. } => {
                for a in args {
                    expr(a, reads);
                }
                for (_, v) in kwargs {
                    expr(v, reads);
                }
            }
            // `xs.append(1)` はメソッドの受け手を読む
            ExprKind::MethodCall {
                receiver,
                args,
                kwargs,
                ..
            } => {
                expr(receiver, reads);
                for a in args {
                    expr(a, reads);
                }
                for (_, v) in kwargs {
                    expr(v, reads);
                }
            }
            ExprKind::Unary { operand, .. } => expr(operand, reads),
            ExprKind::Binary { lhs, rhs, .. } | ExprKind::Is { lhs, rhs, .. } => {
                expr(lhs, reads);
                expr(rhs, reads);
            }
            ExprKind::Field { base, .. } | ExprKind::Try(base) | ExprKind::Await(base) => {
                expr(base, reads)
            }
            ExprKind::List(items) => items.iter().for_each(|i| expr(i, reads)),
            ExprKind::Dict(entries) => {
                for (k, v) in entries {
                    expr(k, reads);
                    expr(v, reads);
                }
            }
            ExprKind::Index { base, index } => {
                expr(base, reads);
                expr(index, reads);
            }
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Str(_)
            | ExprKind::None => {}
        }
    }
    let mut binds = Vec::new();
    let mut reads = HashSet::new();
    stmts(&f.body, &mut binds, &mut reads);
    binds
        .into_iter()
        .filter(|b| {
            !b.name.starts_with('_')
                && !reads.contains(b.name.as_str())
                && !f.params.iter().any(|p| p.name.name == b.name)
        })
        .map(|b| {
            Diagnostic::warning(
                "W0201",
                format!("variable `{}` is assigned but never read", b.name),
                b.span,
            )
            .with_hint(format!(
                "remove it, or rename it to `_{}` if it is intentionally unused",
                b.name
            ))
        })
        .collect()
}

pub fn check(module: &Module) -> (TypeckResult, Vec<Diagnostic>) {
    check_program(module, true)
}

/// `require_main` が偽なら `main` がなくてもよい（`wys test` で使う）。
pub fn check_program(module: &Module, require_main: bool) -> (TypeckResult, Vec<Diagnostic>) {
    let mut cx = Ctx {
        diags: Vec::new(),
        expr_types: vec![Ty::Error; module.expr_count as usize],
        sigs: Vec::new(),
        sig_index: HashMap::new(),
        types: Types::default(),
        type_defs: module.types.clone(),
        aliases: HashMap::new(),
        generics: HashMap::new(),
        type_scope: Vec::new(),
        call_type_args: HashMap::new(),
        pattern_types: HashMap::new(),
    };
    // `PyError` を最初に登録する（同じフィールドの構造的な型より先に名前を付ける）
    cx.types.py_error();
    cx.types.json_error();
    cx.types.schema_error();
    for n in BUILTIN_RECORDS {
        cx.types.builtin_record(n);
    }
    declare_types(&mut cx, module);
    for f in &module.funcs {
        let type_params: Vec<ParamId> = f
            .type_params
            .iter()
            .map(|t| cx.types.new_param(&t.name))
            .collect();
        cx.push_type_scope(&f.type_params, &type_params);
        let params = f.params.iter().map(|p| cx.resolve(&p.ty)).collect();
        let ret = cx.resolve(&f.ret);
        cx.type_scope.pop();
        if BUILTINS.contains(&f.name.name.as_str()) {
            cx.diags.push(Diagnostic::error(
                "E0209",
                format!(
                    "`{}` is a built-in function and cannot be redefined",
                    f.name.name
                ),
                f.name.span,
            ));
        } else if BUILTIN_RECORDS.contains(&f.name.name.as_str()) {
            // `Message(...)` は組み込みのレコードの構築になるので、同じ名前の関数は呼べない
            cx.diags.push(Diagnostic::error(
                "E0209",
                format!(
                    "`{}` is a built-in type and cannot be redefined as a function",
                    f.name.name
                ),
                f.name.span,
            ));
        } else if let Some(&prev) = cx.sig_index.get(&f.name.name) {
            let prev_span = cx.sigs[prev].span;
            cx.diags.push(
                Diagnostic::error(
                    "E0209",
                    format!("function `{}` is defined more than once", f.name.name),
                    f.name.span,
                )
                .with_related(RelatedKind::Function, &f.name.name, Some(prev_span)),
            );
        } else {
            cx.sig_index.insert(f.name.name.clone(), cx.sigs.len());
        }
        let effects = f.effects.as_ref().map(|c| effect_clause(&mut cx, c));
        cx.sigs.push(FuncSig {
            name: f.name.name.clone(),
            type_params,
            params,
            ret,
            span: f.name.span,
            effects,
            is_async: f.is_async,
            tool: None,
            agent: None,
        });
    }
    check_decorators(&mut cx, module);
    if require_main || cx.sig_index.contains_key("main") {
        check_main(&mut cx, module);
    }
    for t in &module.types {
        if BUILTINS.contains(&t.name.name.as_str()) || cx.sig_index.contains_key(&t.name.name) {
            cx.diags.push(Diagnostic::error(
                "E0209",
                format!(
                    "`{}` is defined both as a type and as a function",
                    t.name.name
                ),
                t.name.span,
            ));
        }
    }

    let mut funcs = Vec::new();
    let mut ranges = Vec::new();
    for (i, f) in module.funcs.iter().enumerate() {
        let start = cx.diags.len();
        funcs.push(FnCk::run(&mut cx, f, i));
        ranges.push(start..cx.diags.len());
    }
    // 未使用の変数（W0201）。エラーのある関数（署名のエラーを含む。範囲で判定する）では出さない
    // （修正の途中の雑音になる。DX-005）。出すときは、その関数の警告と位置の順に並べる
    for (f, range) in module.funcs.iter().zip(ranges).rev() {
        let inside = |d: &Diagnostic| d.span.start >= f.span.start && d.span.end <= f.span.end;
        if cx.diags.iter().any(|d| d.is_error() && inside(d)) {
            continue;
        }
        let unused = unused_locals(f);
        if unused.is_empty() {
            continue;
        }
        let mut merged: Vec<Diagnostic> = cx.diags.splice(range.clone(), []).collect();
        merged.extend(unused);
        merged.sort_by_key(|d| d.span.start);
        cx.diags.splice(range.start..range.start, merged);
    }
    (
        TypeckResult {
            expr_types: cx.expr_types,
            sigs: cx.sigs,
            funcs,
            types: cx.types,
            call_type_args: cx.call_type_args,
            pattern_types: cx.pattern_types,
        },
        cx.diags,
    )
}

/// `type` 定義を登録し、すべて解決する（使われていない定義の誤りも報告するため）。
fn declare_types(cx: &mut Ctx, module: &Module) {
    for (i, t) in module.types.iter().enumerate() {
        let name = &t.name.name;
        if matches!(
            name.as_str(),
            "int"
                | "bool"
                | "str"
                | "float"
                | "None"
                | "Result"
                | "list"
                | "dict"
                | "PyObject"
                | "Task"
        ) || BUILTIN_RECORDS.contains(&name.as_str())
        {
            cx.diags.push(Diagnostic::error(
                "E0209",
                format!("`{name}` is a built-in type and cannot be redefined"),
                t.name.span,
            ));
        } else if cx.aliases.contains_key(name) {
            cx.diags.push(Diagnostic::error(
                "E0209",
                format!("type `{name}` is defined more than once"),
                t.name.span,
            ));
        } else {
            cx.aliases.insert(name.clone(), (i, AliasState::Unresolved));
        }
    }
    for t in &module.types {
        if cx
            .aliases
            .get(&t.name.name)
            .is_some_and(|(i, _)| module.types[*i].name.span == t.name.span)
        {
            cx.resolve_alias(&t.name.name);
        }
    }
}

/// `uses` 節を Effect の集合にする。未知の名前（E1101）と、重複や `pure` の併記（E1104）を報告する。
/// デコレータ（ADR-0022 論点 11〜14）。M5 ではコンパイル時の属性で、`tool` と `agent` だけを解釈する。
/// 先にすべての `@tool` を読み、次に `@agent` の `tools` をそれと突き合わせる。
fn check_decorators(cx: &mut Ctx, module: &Module) {
    let bad_arg = |cx: &mut Ctx, msg: String, span: Span| {
        cx.diags.push(Diagnostic::error("E1402", msg, span));
    };
    let mut agents: Vec<(usize, &FuncDef, &Decorator)> = Vec::new();
    for (i, f) in module.funcs.iter().enumerate() {
        if f.decorators.is_empty() {
            continue;
        }
        let mut seen: Vec<&str> = Vec::new();
        for d in &f.decorators {
            let name = d.name.name.as_str();
            if !matches!(name, "tool" | "agent") {
                cx.diags.push(
                    Diagnostic::error("E1401", format!("unknown decorator `@{name}`"), d.name.span)
                        .with_hint("the decorators are `@tool(...)` and `@agent(...)` (ADR-0022)"),
                );
                continue;
            }
            if seen.contains(&name) {
                bad_arg(cx, format!("`@{name}` is written more than once"), d.span);
                continue;
            }
            seen.push(name);
        }
        let (is_tool, is_agent) = (seen.contains(&"tool"), seen.contains(&"agent"));
        if !is_tool && !is_agent {
            continue;
        }
        let which = if is_tool { "@tool" } else { "@agent" };
        let sig_error = |cx: &mut Ctx, msg: String, hint: &str| {
            cx.diags
                .push(Diagnostic::error("E1403", msg, f.name.span).with_hint(hint.to_string()));
        };
        if is_tool && is_agent {
            sig_error(
                cx,
                format!("`{}` cannot be both `@tool` and `@agent`", f.name.name),
                "split it into a tool and an agent that lists the tool",
            );
            continue;
        }
        if f.effects.is_none() {
            sig_error(
                cx,
                format!("a `{which}` function needs a `uses` clause"),
                "declare its effects, e.g. `uses pure` or `uses io, llm` (ADR-0022)",
            );
        }
        let base = f.name.name.rsplit('.').next().unwrap_or_default();
        if base == "main" || base.starts_with("test_") {
            sig_error(
                cx,
                format!("`{which}` cannot be put on `{base}`"),
                "move the work into a separate function",
            );
            continue;
        }
        if is_tool && !f.type_params.is_empty() {
            // ツールは型引数を決める呼び出しなしで LLM から呼ばれるので、Generic にできない
            sig_error(
                cx,
                format!("tool `{}` cannot be generic", f.name.name),
                "remove the type parameters; tools are called by the model, so their argument types must be concrete",
            );
            continue;
        }
        let d = f
            .decorators
            .iter()
            .find(|d| d.name.name == if is_tool { "tool" } else { "agent" })
            .expect("seen above");
        // 同じキーワード引数を 2 回書けない（後の値が承認や予算を黙って上書きしないように）
        let mut keys: Vec<&str> = Vec::new();
        for a in &d.args {
            if let Some(k) = &a.key {
                if keys.contains(&k.name.as_str()) {
                    bad_arg(
                        cx,
                        format!("argument `{}` is given more than once", k.name),
                        k.span,
                    );
                }
                keys.push(&k.name);
            }
        }
        if is_tool {
            let mut attr = ToolAttr {
                description: String::new(),
                approval: false,
            };
            for a in &d.args {
                let Some(k) = &a.key else {
                    bad_arg(
                        cx,
                        "decorator arguments must be written as `name=value`".into(),
                        a.span,
                    );
                    continue;
                };
                match (k.name.as_str(), &a.value) {
                    ("description", AttrValue::Str(v)) => attr.description = v.clone(),
                    ("approval", AttrValue::Str(v)) if v == "none" || v == "required" => {
                        attr.approval = v == "required";
                    }
                    ("approval", _) => bad_arg(
                        cx,
                        "`approval` must be \"none\" or \"required\"".into(),
                        a.span,
                    ),
                    ("description", _) => {
                        bad_arg(cx, "`description` must be a string".into(), a.span)
                    }
                    (other, _) => bad_arg(cx, format!("`@tool` has no argument `{other}`"), k.span),
                }
            }
            // 引数と戻り値はスキーマ型（E1501）。戻り値の一番外側だけ `Result[T, E]` と `None` を認める
            let sig = cx.sigs[i].clone();
            for (p, &t) in f.params.iter().zip(&sig.params) {
                if cx.types.contains_param(t) || !cx.types.is_schema_type(t, true) {
                    schema_type_error(cx, t, p.ty.span, "a tool argument");
                }
            }
            let ok_ret = match sig.ret {
                Ty::None => true,
                Ty::Result(r) => {
                    let (o, e) = cx.types.result_args(r);
                    !cx.types.contains_param(sig.ret)
                        && cx.types.is_schema_type(o, false)
                        && cx.types.is_schema_type(e, false)
                }
                t => !cx.types.contains_param(t) && cx.types.is_schema_type(t, true),
            };
            if !ok_ret {
                schema_type_error(cx, sig.ret, f.ret.span, "a tool result");
            }
            cx.sigs[i].tool = Some(attr);
        } else {
            agents.push((i, f, d));
        }
    }
    for (i, f, d) in agents {
        let mut attr = AgentAttr {
            tools: Vec::new(),
            capabilities: EffectSet::default(),
            model: String::new(),
            max_llm_calls: 0,
            max_tool_calls: 0,
            max_tokens: 0,
            timeout_ms: 0,
        };
        attr.capabilities.insert(Effect::Io);
        attr.capabilities.insert(Effect::Llm);
        let mut caps_span = d.span;
        let mut tool_elems: Vec<(String, Span)> = Vec::new();
        for a in &d.args {
            let Some(k) = &a.key else {
                bad_arg(
                    cx,
                    "decorator arguments must be written as `name=value`".into(),
                    a.span,
                );
                continue;
            };
            match (k.name.as_str(), &a.value) {
                ("tools", AttrValue::List(es)) => {
                    for e in es {
                        match e {
                            AttrElem::Name(n, sp) => tool_elems.push((n.clone(), *sp)),
                            AttrElem::Str(_, sp) => bad_arg(
                                cx,
                                "`tools` lists functions by name, not strings".into(),
                                *sp,
                            ),
                        }
                    }
                }
                ("capabilities", AttrValue::List(es)) => {
                    caps_span = a.span;
                    attr.capabilities = EffectSet::default();
                    for e in es {
                        match e {
                            AttrElem::Str(n, sp) => match Effect::parse(n) {
                                Some(eff) => attr.capabilities.insert(eff),
                                None => cx.diags.push(
                                    Diagnostic::error("E1101", format!("unknown capability `{n}`"), *sp)
                                        .with_hint("capabilities are the effect names `io`, `exit`, `python` and `llm` (ADR-0023)"),
                                ),
                            },
                            AttrElem::Name(_, sp) => bad_arg(cx, "`capabilities` lists effect names as strings, e.g. `[\"io\", \"llm\"]`".into(), *sp),
                        }
                    }
                }
                ("model", AttrValue::Str(v)) => attr.model = v.clone(),
                (
                    key @ ("max_llm_calls" | "max_tool_calls" | "max_tokens" | "timeout_ms"),
                    AttrValue::Int(v),
                ) => {
                    if *v < 0 {
                        bad_arg(
                            cx,
                            format!("`{key}` cannot be negative (0 means no limit)"),
                            a.span,
                        );
                        continue;
                    }
                    match key {
                        "max_llm_calls" => attr.max_llm_calls = *v,
                        "max_tool_calls" => attr.max_tool_calls = *v,
                        "max_tokens" => attr.max_tokens = *v,
                        _ => attr.timeout_ms = *v,
                    }
                }
                ("tools" | "capabilities", _) => {
                    bad_arg(cx, format!("`{}` must be a list", k.name), a.span)
                }
                ("model", _) => bad_arg(cx, "`model` must be a string".into(), a.span),
                ("max_llm_calls" | "max_tool_calls" | "max_tokens" | "timeout_ms", _) => {
                    bad_arg(cx, format!("`{}` must be an integer", k.name), a.span)
                }
                (other, _) => bad_arg(cx, format!("`@agent` has no argument `{other}`"), k.span),
            }
        }
        for (n, sp) in tool_elems {
            let tool = cx
                .sig_index
                .get(&n)
                .copied()
                .filter(|&ti| cx.sigs[ti].tool.is_some());
            let Some(ti) = tool else {
                cx.diags.push(
                    Diagnostic::error("E1404", format!("`{n}` is not a `@tool` function"), sp)
                        .with_hint("put `@tool(description=...)` on the function, or remove it from `tools`"),
                );
                continue;
            };
            if attr.tools.contains(&n) {
                cx.diags.push(Diagnostic::error(
                    "E1404",
                    format!("tool `{n}` is listed more than once"),
                    sp,
                ));
                continue;
            }
            // LLM はツールを基底名で呼ぶ（ツール表の名前）ので、モジュールが違っても基底名の重複は区別できない
            let base = |q: &str| q.rsplit('.').next().unwrap_or(q).to_string();
            if let Some(other) = attr.tools.iter().find(|t| base(t) == base(&n)) {
                cx.diags.push(
                    Diagnostic::error(
                        "E1404",
                        format!(
                            "tools `{other}` and `{n}` have the same name `{}`",
                            base(&n)
                        ),
                        sp,
                    )
                    .with_hint(
                        "the model calls tools by name; list only one of them in this agent",
                    ),
                );
                continue;
            }
            // ツールの `uses` ⊆ agent の `capabilities`（E1405、ADR-0022 論点 14）
            let tool_effects = cx.sigs[ti].effects.unwrap_or_default();
            let missing = tool_effects.missing_from(attr.capabilities);
            if !missing.is_empty() {
                let names: Vec<&str> = missing.into_iter().map(Effect::name).collect();
                let tool_span = cx.sigs[ti].span;
                cx.diags.push(
                    Diagnostic::error(
                        "E1405",
                        format!(
                            "tool `{n}` uses {} but the agent does not grant it",
                            names.join(", ")
                        ),
                        sp,
                    )
                    .with_hint(format!(
                        "add {} to `capabilities`",
                        names
                            .iter()
                            .map(|x| format!("\"{x}\""))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                    .with_related(RelatedKind::Function, &n, Some(tool_span)),
                );
            }
            attr.tools.push(n);
        }
        // `capabilities` ⊆ 自身の `uses`（E1406）
        if let Some(own) = cx.sigs[i].effects {
            let extra = attr.capabilities.missing_from(own);
            if !extra.is_empty() {
                let names: Vec<&str> = extra.into_iter().map(Effect::name).collect();
                cx.diags.push(
                    Diagnostic::error(
                        "E1406",
                        format!(
                            "agent `{}` grants {} but its `uses` clause does not declare it",
                            f.name.name,
                            names.join(", ")
                        ),
                        caps_span,
                    )
                    .with_cause("an agent can cause the effects of the tools it runs, so its signature declares them (ADR-0022 point 14)")
                    .with_hint(format!("add {} to `uses`, or narrow `capabilities`", names.join(", "))),
                );
            }
        }
        cx.sigs[i].agent = Some(attr);
    }
}

fn schema_type_error(cx: &mut Ctx, t: Ty, span: Span, what: &str) {
    let found = cx.name(t);
    cx.diags.push(
        Diagnostic::error(
            "E1501",
            format!("`{found}` is not a schema type, so it cannot be {what}"),
            span,
        )
        .with_cause("tool arguments and results cross the boundary as JSON: `int`, `float`, `bool`, `str`, `Unknown`, and `list`, `dict[str, ..]`, records and unions of them; a tool may also return `Result[T, E]` or `None` (ADR-0022)")
        .with_hint("declare the JSON shape as a record, or use `Unknown` for parts whose shape is open"),
    );
}

fn effect_clause(cx: &mut Ctx, c: &EffectClause) -> EffectSet {
    let mut set = EffectSet::default();
    let mut seen: Vec<&str> = Vec::new();
    for n in &c.names {
        if seen.contains(&n.name.as_str()) {
            cx.diags.push(
                Diagnostic::error(
                    "E1104",
                    format!("effect `{}` is listed more than once", n.name),
                    n.span,
                )
                .with_hint("remove the duplicate"),
            );
            continue;
        }
        seen.push(&n.name);
        if n.name == "pure" {
            continue;
        }
        if let Some(e) = Effect::parse(&n.name) {
            set.insert(e);
        } else {
            cx.diags.push(
                Diagnostic::error("E1101", format!("unknown effect `{}`", n.name), n.span)
                    .with_hint(
                        "the effects are `io`, `exit`, `python` and `llm`; write `uses pure` for none",
                    ),
            );
        }
    }
    if let Some(pure) = c.names.iter().find(|p| p.name == "pure")
        && c.names.len() > 1
        && c.names.iter().any(|n| n.name != "pure")
    {
        cx.diags.push(
            Diagnostic::error(
                "E1104",
                "`pure` cannot be combined with other effects",
                pure.span,
            )
            .with_hint(
                "`uses pure` means the function has no effects; remove `pure` or the other names",
            ),
        );
    }
    set
}

fn check_main(cx: &mut Ctx, module: &Module) {
    match cx.sig_index.get("main") {
        None => cx.diags.push(
            Diagnostic::error(
                "E0210",
                "the program has no `main` function",
                Span::new(0, 0),
            )
            .with_hint("add `def main() -> None:`"),
        ),
        Some(&i) => {
            let sig = &cx.sigs[i];
            if !sig.params.is_empty() || sig.ret != Ty::None {
                let span = module.funcs[i].name.span;
                cx.diags.push(
                    Diagnostic::error(
                        "E0210",
                        "`main` must take no parameters and return `None`",
                        span,
                    )
                    .with_hint(
                        "declare it as `def main() -> None:`; use `exit(code)` for an exit status",
                    ),
                );
            }
        }
    }
}

struct Ctx {
    diags: Vec<Diagnostic>,
    expr_types: Vec<Ty>,
    sigs: Vec<FuncSig>,
    sig_index: HashMap<String, usize>,
    types: Types,
    type_defs: Vec<TypeDef>,
    /// `type` の名前 → (`type_defs` の番号, 解決状態)
    aliases: HashMap<String, (usize, AliasState)>,
    /// Generic な `type` の型パラメータと本体（型パラメータを含む型）。
    generics: HashMap<String, (Vec<ParamId>, Ty)>,
    /// 型パラメータの名前のスコープ（内側が後ろ）。
    type_scope: Vec<HashMap<String, Ty>>,
    call_type_args: HashMap<ExprId, Vec<Ty>>,
    pattern_types: HashMap<u32, Ty>,
}

#[derive(Debug, Clone, Copy)]
enum AliasState {
    Unresolved,
    Resolving,
    Done(Ty),
    /// Generic な `type`（`Ctx::generics` に本体がある）。
    Generic,
}

impl Ctx {
    fn resolve(&mut self, t: &TypeExpr) -> Ty {
        match &t.kind {
            TypeExprKind::Name(name)
                if self.type_scope.iter().rev().any(|sc| sc.contains_key(name)) =>
            {
                *self
                    .type_scope
                    .iter()
                    .rev()
                    .find_map(|sc| sc.get(name))
                    .expect("checked above")
            }
            TypeExprKind::Apply(name, args) if name == "list" => {
                if args.len() != 1 {
                    self.diags.push(Diagnostic::error(
                        "E0402",
                        format!("`list` takes 1 type argument but {} were given", args.len()),
                        t.span,
                    ));
                    return Ty::Error;
                }
                let elem = self.resolve(&args[0]);
                self.types.list(elem)
            }
            TypeExprKind::Apply(name, args) if name == "Result" => {
                if args.len() != 2 {
                    self.diags.push(Diagnostic::error(
                        "E0402",
                        format!(
                            "`Result` takes 2 type arguments but {} were given",
                            args.len()
                        ),
                        t.span,
                    ));
                    return Ty::Error;
                }
                let ok = self.resolve(&args[0]);
                let err = self.resolve(&args[1]);
                if matches!(err, Ty::None | Ty::Unknown) {
                    self.diags.push(
                        Diagnostic::error(
                            "E1002",
                            format!("the error type of `Result` cannot be `{}`", self.name(err)),
                            args[1].span,
                        )
                        .with_cause(
                            "an error must carry a value that says what went wrong (ADR-0013)",
                        )
                        .with_hint(
                            "use a concrete type such as `str`, or `T | None` instead of `Result`",
                        ),
                    );
                    return Ty::Error;
                }
                self.types.result(ok, err)
            }
            TypeExprKind::Apply(name, args) if name == "dict" => {
                if args.len() != 2 {
                    self.diags.push(Diagnostic::error(
                        "E0402",
                        format!(
                            "`dict` takes 2 type arguments but {} were given",
                            args.len()
                        ),
                        t.span,
                    ));
                    return Ty::Error;
                }
                let k = self.resolve(&args[0]);
                let v = self.resolve(&args[1]);
                if !valid_dict_key(k) {
                    self.diags
                        .push(dict_key_error(&self.types.name(k), args[0].span));
                    return Ty::Error;
                }
                if v == Ty::None {
                    self.diags.push(
                        Diagnostic::error(
                            "E0215",
                            "a dict value cannot have type `None`",
                            args[1].span,
                        )
                        .with_hint("use an optional type, e.g. `dict[str, int | None]`"),
                    );
                    return Ty::Error;
                }
                self.types.dict(k, v)
            }
            TypeExprKind::Name(name) if name == "dict" => {
                self.diags.push(Diagnostic::error(
                    "E0402",
                    "`dict` needs its key and value types, e.g. `dict[str, int]`",
                    t.span,
                ));
                Ty::Error
            }
            TypeExprKind::Name(name) if name == "Result" => {
                self.diags.push(Diagnostic::error(
                    "E0402",
                    "`Result` needs its type arguments, e.g. `Result[int, str]`",
                    t.span,
                ));
                Ty::Error
            }
            TypeExprKind::Name(name) if name == "list" => {
                self.diags.push(Diagnostic::error(
                    "E0402",
                    "`list` needs its element type, e.g. `list[int]`",
                    t.span,
                ));
                Ty::Error
            }
            TypeExprKind::Apply(name, args) if name == "Task" => {
                if args.len() != 1 {
                    self.diags.push(Diagnostic::error(
                        "E0402",
                        format!("`Task` takes 1 type argument but {} were given", args.len()),
                        t.span,
                    ));
                    return Ty::Error;
                }
                let r = self.resolve(&args[0]);
                self.types.task(r)
            }
            TypeExprKind::Name(name) if name == "Task" => {
                self.diags.push(Diagnostic::error(
                    "E0402",
                    "`Task` needs its result type, e.g. `Task[int]`",
                    t.span,
                ));
                Ty::Error
            }
            TypeExprKind::Name(name)
                if matches!(self.aliases.get(name), Some((_, AliasState::Generic))) =>
            {
                let n = self.generics[name].0.len();
                self.diags.push(Diagnostic::error(
                    "E0402",
                    format!("type `{name}` needs {n} type argument(s), e.g. `{name}[...]`"),
                    t.span,
                ));
                Ty::Error
            }
            TypeExprKind::Apply(name, args) => {
                let arg_tys: Vec<Ty> = args.iter().map(|a| self.resolve(a)).collect();
                if matches!(self.aliases.get(name), Some((_, AliasState::Resolving))) {
                    return self.resolve_alias(name); // 自己参照（E0504）
                }
                if self.aliases.contains_key(name) && !self.generics.contains_key(name) {
                    self.resolve_alias(name);
                }
                let Some((params, body)) = self.generics.get(name).cloned() else {
                    self.diags.push(Diagnostic::error(
                        "E0402",
                        format!("type `{name}` takes no type arguments"),
                        t.span,
                    ));
                    return Ty::Error;
                };
                if params.len() != arg_tys.len() {
                    self.diags.push(Diagnostic::error(
                        "E0402",
                        format!(
                            "type `{name}` takes {} type argument(s) but {} were given",
                            params.len(),
                            arg_tys.len()
                        ),
                        t.span,
                    ));
                    return Ty::Error;
                }
                if arg_tys.contains(&Ty::Error) || body == Ty::Error {
                    return Ty::Error;
                }
                let map: Subst = params.into_iter().zip(arg_tys).collect();
                let ty = self.types.subst(body, &map);
                if let Some(slot) = self.types.none_slot(ty) {
                    self.diags.push(
                        none_type_arg_error(&slot, t.span)
                            .with_hint("use an optional type argument such as `int | None`"),
                    );
                    return Ty::Error;
                }
                ty
            }
            TypeExprKind::Name(name) => match name.as_str() {
                "int" => Ty::Int,
                "bool" => Ty::Bool,
                "str" => Ty::Str,
                "float" => Ty::Float,
                "None" => Ty::None,
                "Unknown" => Ty::Unknown,
                "PyObject" => Ty::PyObject,
                name if BUILTIN_RECORDS.contains(&name) => self
                    .types
                    .builtin_record(name)
                    .expect("listed in BUILTIN_RECORDS"),
                other if self.aliases.contains_key(other) => self.resolve_alias(other),
                other => {
                    self.diags.push(
                        Diagnostic::error("E0203", format!("unknown type `{other}`"), t.span)
                            .with_hint("supported types are `int`, `bool`, `str`, `float`, `None`, types defined with `type`, and unions of them (`A | B`)"),
                    );
                    Ty::Error
                }
            },
            TypeExprKind::Union(items) => {
                let tys: Vec<Ty> = items.iter().map(|i| self.resolve(i)).collect();
                if tys.contains(&Ty::Unknown) {
                    self.diags.push(
                        Diagnostic::error("E0603", "`Unknown` cannot be part of a union", t.span)
                            .with_cause("`Unknown` already includes every value; it is narrowed only with `match` (ADR-0011)")
                            .with_hint("write just `Unknown`"),
                    );
                    return Ty::Unknown;
                }
                let u = self.types.union(tys);
                let records = self
                    .types
                    .members(u)
                    .iter()
                    .filter(|m| matches!(m, Ty::Record(_)))
                    .count();
                let results = self
                    .types
                    .members(u)
                    .iter()
                    .filter(|m| matches!(m, Ty::Result(_)))
                    .count();
                if results > 1 {
                    self.diags.push(
                        Diagnostic::error("E1002", "a union can contain at most one `Result` type", t.span)
                            .with_cause("`case Result():` must tell the members apart (ADR-0013)")
                            .with_hint("combine the error types into one `Result`, e.g. `Result[int, str | int]`"),
                    );
                    return Ty::Error;
                }
                if records > 1 {
                    self.diags.push(
                        Diagnostic::error("E0508", "a union can contain at most one record type", t.span)
                            .with_cause("structural records cannot be told apart at run time (ADR-0007)")
                            .with_hint("use a single record type with an optional field, or separate variables"),
                    );
                    return Ty::Error;
                }
                u
            }
        }
    }

    /// `type` で定義した名前を解決する。自分自身を（間接的にでも）参照していたら E0504。
    fn resolve_alias(&mut self, name: &str) -> Ty {
        let (idx, state) = self.aliases[name];
        match state {
            AliasState::Done(t) => t,
            AliasState::Resolving => {
                let span = self.type_defs[idx].name.span;
                self.diags.push(
                    Diagnostic::error("E0504", format!("type `{name}` refers to itself"), span)
                        .with_cause("recursive types, including recursion through `list`, are not supported; for recursive JSON data, use an `Unknown` field and validate it separately (ADR-0021 point 10)"),
                );
                self.aliases
                    .insert(name.to_string(), (idx, AliasState::Done(Ty::Error)));
                Ty::Error
            }
            AliasState::Generic => Ty::Error,
            AliasState::Unresolved => {
                self.aliases
                    .insert(name.to_string(), (idx, AliasState::Resolving));
                let def = self.type_defs[idx].clone();
                let params: Vec<ParamId> = def
                    .params
                    .iter()
                    .map(|p| self.types.new_param(&p.name))
                    .collect();
                self.push_type_scope(&def.params, &params);
                let param_tys: Vec<Ty> = params.iter().map(|p| Ty::Param(*p)).collect();
                let t = match &def.value {
                    TypeDefValue::Alias(te) => self.resolve(te),
                    TypeDefValue::Record(fields) => {
                        let mut seen = HashSet::new();
                        let mut fs = Vec::new();
                        let mut failed = false;
                        for (fname, te) in fields {
                            if !seen.insert(fname.name.clone()) {
                                self.diags.push(Diagnostic::error(
                                    "E0503",
                                    format!("field `{}` is defined more than once", fname.name),
                                    fname.span,
                                ));
                                continue;
                            }
                            let ft = self.resolve(te);
                            if ft == Ty::None {
                                self.diags.push(
                                    Diagnostic::error(
                                        "E0215",
                                        format!("field `{}` cannot have type `None`", fname.name),
                                        te.span,
                                    )
                                    .with_hint("use an optional type such as `int | None`"),
                                );
                                failed = true;
                            }
                            failed |= ft == Ty::Error;
                            fs.push((fname.name.clone(), ft));
                        }
                        if failed {
                            Ty::Error
                        } else {
                            self.types
                                .record_named(fs, Some(name.to_string()), param_tys)
                        }
                    }
                };
                self.type_scope.pop();
                let t = match self.aliases[name].1 {
                    AliasState::Done(Ty::Error) => Ty::Error,
                    _ => t,
                };
                if params.is_empty() {
                    self.aliases
                        .insert(name.to_string(), (idx, AliasState::Done(t)));
                    t
                } else {
                    self.generics.insert(name.to_string(), (params, t));
                    self.aliases
                        .insert(name.to_string(), (idx, AliasState::Generic));
                    Ty::Error
                }
            }
        }
    }

    fn name(&self, t: Ty) -> String {
        self.types.name(t)
    }

    fn push_type_scope(&mut self, names: &[Ident], params: &[ParamId]) {
        self.type_scope.push(
            names
                .iter()
                .zip(params)
                .map(|(n, p)| (n.name.clone(), Ty::Param(*p)))
                .collect(),
        );
    }

    /// `actual` を `expected` の場所に置けなければ診断を出す。
    /// `None` を取り除けば置ける場合は E0301（未検証の `None`）、それ以外は E0201。
    fn require(
        &mut self,
        actual: Ty,
        expected: Ty,
        span: Span,
        context: &str,
        related: Option<(RelatedKind, &str, Span)>,
    ) {
        if self.types.fits(actual, expected) {
            return;
        }
        if self.types.members(actual).contains(&Ty::Unknown) {
            self.diags.push(Diagnostic::error("E0601", "this `Unknown` value has not been checked", span)
                    .with_cause("values from outside the program must be checked before use (LANG-006, ADR-0011)")
                    .with_hint("inspect it with `match`: `case int():`, `case str():`, ..., `case _:`"));
            return;
        }
        let d = if self.types.contains_none(actual) && {
            let rest = self.types.without_none(actual);
            self.types.fits(rest, expected)
        } {
            Diagnostic::error(
                "E0301",
                format!(
                    "this value may be `None`: expected `{}`, found `{}`",
                    self.name(expected),
                    self.name(actual)
                ),
                span,
            )
            .with_cause(context.to_string())
            .with_hint("check it first, e.g. `if x is not None:` or `if x is None: return ...`")
        } else {
            Diagnostic::error(
                "E0201",
                format!(
                    "mismatched types: expected `{}`, found `{}`",
                    self.name(expected),
                    self.name(actual)
                ),
                span,
            )
            .with_cause(context.to_string())
        };
        let d = match related {
            Some((kind, name, rspan)) => d.with_related(kind, name, Some(rspan)),
            None => d,
        };
        self.diags.push(d);
    }
}

struct Var {
    ty: Ty,
    decl: Span,
}

/// 流れ解析の環境: 確実に代入済みの変数と、その地点での（絞り込み後の）型。
type Env = HashMap<String, Ty>;

/// 到達しない経路の環境に付ける目印（変数名には使えない文字列）。合流ではこの環境を数えない。
const UNREACHABLE: &str = "\u{0}unreachable";

fn unreachable_env(env: &Env) -> bool {
    env.contains_key(UNREACHABLE)
}

struct FnCk<'a> {
    cx: &'a mut Ctx,
    ret: Ty,
    vars: HashMap<String, Var>,
    locals: Vec<(String, Ty)>,
    local_spans: Vec<Span>,
    /// 式ごとの絞り込み（ループで同じ式を検査し直したら上書きする）。
    narrowings: HashMap<ExprId, Narrowing>,
    /// 検査している関数の名前と `uses` 節（ADR-0014）。
    fn_name: String,
    effects: Option<(EffectSet, EffectClause)>,
    effect_sites: Vec<EffectSite>,
    /// E1102 の診断の位置（`cx.diags` の添字）と、関数全体で足りない Effect（修正案を1つにまとめるため）。
    missing_effects: (Vec<usize>, EffectSet),
    /// フィールド参照の式ごとの、レコード定義上のフィールドの型（絞り込む前の型。ADR-0016）。
    field_decl: HashMap<ExprId, Ty>,
    /// 検査している関数が `async def` か、その名前の位置（E1301 の related。ADR-0020）
    is_async: bool,
    fn_span: Span,
    /// 次に検査する呼び出しが `await` の直下か `spawn` の引数で、`async def` を呼んでよいこと（E1303）
    allow_async_call: bool,
}

impl<'a> FnCk<'a> {
    fn run(cx: &'a mut Ctx, f: &FuncDef, index: usize) -> FuncInfo {
        let sig = cx.sigs[index].clone();
        let mut ck = FnCk {
            cx,
            ret: sig.ret,
            vars: HashMap::new(),
            locals: Vec::new(),
            local_spans: Vec::new(),
            narrowings: HashMap::new(),
            fn_name: f.name.name.clone(),
            // 未知の名前（E1101）がある `uses` 節では本体を検査しない（E1102 を連鎖させない）
            effects: sig.effects.zip(f.effects.clone()).filter(|(_, c)| {
                c.names
                    .iter()
                    .all(|n| n.name == "pure" || Effect::parse(&n.name).is_some())
            }),
            effect_sites: Vec::new(),
            missing_effects: (Vec::new(), EffectSet::default()),
            field_decl: HashMap::new(),
            is_async: f.is_async,
            fn_span: f.name.span,
            allow_async_call: false,
        };
        ck.cx.push_type_scope(&f.type_params, &sig.type_params);
        let mut env = Env::new();
        for (p, &ty) in f.params.iter().zip(&sig.params) {
            if ck.vars.contains_key(&p.name.name) {
                ck.cx.diags.push(Diagnostic::error(
                    "E0214",
                    format!("duplicate parameter `{}`", p.name.name),
                    p.name.span,
                ));
                continue;
            }
            if ty == Ty::None {
                ck.cx.diags.push(Diagnostic::error(
                    "E0215",
                    "a parameter cannot have type `None`",
                    p.ty.span,
                ));
            }
            ck.declare(&p.name.name, ty, p.name.span);
            env.insert(p.name.name.clone(), ty);
        }
        let returns = ck.block(&f.body, &mut env);
        ck.cx.type_scope.pop();
        if !returns && sig.ret != Ty::None && sig.ret != Ty::Error {
            let ret_name = ck.cx.name(sig.ret);
            ck.cx.diags.push(
                Diagnostic::error(
                    "E0206",
                    format!(
                        "function `{}` may reach the end without returning a value",
                        f.name.name
                    ),
                    f.name.span,
                )
                .with_cause(format!("the return type is `{ret_name}`"))
                .with_hint("add a `return` at the end of the function"),
            );
        }
        ck.attach_effect_fix();
        let mut narrowings: Vec<Narrowing> = ck.narrowings.into_values().collect();
        narrowings.sort_by_key(|n| (n.span.start, n.span.end));
        let mut effect_sites = ck.effect_sites;
        effect_sites.sort_by_key(|s| (s.span.start, s.span.end));
        FuncInfo {
            locals: ck.locals,
            local_spans: ck.local_spans,
            narrowings,
            effect_sites,
        }
    }

    fn declare(&mut self, name: &str, ty: Ty, decl: Span) {
        self.vars.insert(name.to_string(), Var { ty, decl });
        self.locals.push((name.to_string(), ty));
        self.local_spans.push(decl);
    }

    /// 複数の経路の環境を合流させる: 代入済みの変数は共通部分、型は和集合。
    fn join(&mut self, envs: Vec<Env>) -> Env {
        // 到達しない経路は合流に数えない（すべて到達しないなら、そのうちの1つを返す）
        let (reachable, unreachable): (Vec<Env>, Vec<Env>) =
            envs.into_iter().partition(|e| !unreachable_env(e));
        if reachable.is_empty() {
            return unreachable.into_iter().next().unwrap_or_default();
        }
        let mut it = reachable.into_iter();
        let Some(first) = it.next() else {
            return Env::new();
        };
        let rest: Vec<Env> = it.collect();
        let mut out = Env::new();
        for (name, &t) in &first {
            let mut tys = vec![t];
            let mut everywhere = true;
            for e in &rest {
                match e.get(name) {
                    Some(&u) => tys.push(u),
                    None => everywhere = false,
                }
            }
            if everywhere {
                out.insert(name.clone(), self.cx.types.union(tys));
            }
        }
        out
    }

    /// 文の列を検査する。必ず `return` するなら true。
    fn block(&mut self, stmts: &[Stmt], env: &mut Env) -> bool {
        let mut returns = false;
        for s in stmts {
            if self.stmt(s, env) {
                returns = true;
            }
        }
        returns
    }

    fn stmt(&mut self, s: &Stmt, env: &mut Env) -> bool {
        match &s.kind {
            StmtKind::Pass => false,
            // `assert(cond)` の後では、条件が真の場合の環境（絞り込み）を引き継ぐ
            StmtKind::Expr(
                e @ Expr {
                    kind:
                        ExprKind::Call {
                            callee,
                            type_args,
                            args,
                            kwargs,
                        },
                    ..
                },
            ) if callee.name == "assert"
                && type_args.is_empty()
                && args.len() == 1
                && kwargs.is_empty() =>
            {
                self.check_effects(callee);
                let (t, t_env, _) = self.cond(&args[0], env);
                self.require_condition(t, args[0].span);
                *env = t_env;
                self.record(e, Ty::None);
                false
            }
            // exit() / panic() は戻らない: この文の後には到達しない（絞り込みと return の検査のため）
            StmtKind::Expr(
                e @ Expr {
                    kind: ExprKind::Call { callee, .. },
                    ..
                },
            ) if matches!(callee.name.as_str(), "exit" | "panic") => {
                self.expr(e, env);
                true
            }
            StmtKind::Expr(e) => {
                self.expr(e, env);
                false
            }
            StmtKind::Assign { target, ann, value } => {
                self.assign(target, ann.as_ref(), value, env);
                false
            }
            StmtKind::Return(value) => {
                self.ret_stmt(s.span, value.as_ref(), env);
                true
            }
            StmtKind::If {
                branches,
                else_body,
            } => {
                // 各分岐の条件は「それより前の条件がすべて偽」の環境で評価する。
                let mut cur = env.clone();
                let mut live = Vec::new();
                for (cond, body) in branches {
                    let (t, t_env, f_env) = self.cond(cond, &cur);
                    self.require_condition(t, cond.span);
                    let mut b = t_env;
                    if !self.block(body, &mut b) && !unreachable_env(&b) {
                        live.push(b);
                    }
                    cur = f_env;
                }
                match else_body {
                    Some(body) => {
                        let mut b = cur;
                        if !self.block(body, &mut b) && !unreachable_env(&b) {
                            live.push(b);
                        }
                    }
                    None if !unreachable_env(&cur) => live.push(cur),
                    None => {}
                }
                if live.is_empty() {
                    return true; // すべての分岐が return する
                }
                *env = self.join(live);
                false
            }
            StmtKind::Match { subject, arms } => self.match_stmt(subject, arms, env),
            StmtKind::For { var, iter, body } => {
                let elem = self.for_iter(iter, env);
                let declared = match self.vars.get(&var.name) {
                    Some(v) => {
                        let (vty, decl) = (v.ty, v.decl);
                        let ctx = format!(
                            "loop variable `{}` was declared as `{}`",
                            var.name,
                            self.cx.name(vty)
                        );
                        self.cx.require(
                            elem,
                            vty,
                            iter.span,
                            &ctx,
                            Some((RelatedKind::Variable, &var.name, decl)),
                        );
                        vty
                    }
                    None => {
                        self.declare(&var.name, elem, var.span);
                        elem
                    }
                };
                // while と同じく、本体で代入される変数（ループ変数を含む）の絞り込みを解除する
                let mut assigned = HashSet::new();
                assigned_vars(body, &mut assigned);
                // ループ変数自身も毎回代入される: 入口とループの後では宣言された型に戻し、経路の絞り込みを捨てる
                assigned.insert(var.name.clone());
                for name in assigned {
                    if let (Some(slot), Some(v)) = (env.get_mut(&name), self.vars.get(&name)) {
                        *slot = v.ty;
                    }
                    Self::forget_paths(env, &name);
                }
                let mut b = env.clone();
                b.insert(var.name.clone(), declared);
                self.block(body, &mut b);
                false
            }
            StmtKind::IndexAssign { target, value } => {
                let ExprKind::Index { base, index } = &target.kind else {
                    unreachable!("the parser only builds IndexAssign from an index expression")
                };
                let bt = self.expr(base, env);
                self.check_mutable(bt, target.span);
                if let Ty::Dict(d) = bt {
                    // `d[k] = v`: 追加または置き換え（ADR-0015）
                    let (k, v) = self.cx.types.dict_args(d);
                    let it = self.expr_hint(index, env, Some(k));
                    self.cx
                        .require(it, k, index.span, "a key of the dict", None);
                    let vt = self.expr_hint(value, env, Some(v));
                    self.cx
                        .require(vt, v, value.span, "a value of the dict", None);
                    self.record(target, Ty::None);
                    return false;
                }
                let it = self.expr(index, env);
                self.cx.require(
                    it,
                    Ty::Int,
                    index.span,
                    "a list index must be an `int`",
                    None,
                );
                let elem = self.list_elem_of(bt, base.span);
                let vt = self.expr_hint(value, env, elem);
                if let Some(e) = elem {
                    self.cx
                        .require(vt, e, value.span, "an element of the list", None);
                }
                self.record(target, Ty::None);
                false
            }
            StmtKind::While { cond, body } => {
                // ループ本体で代入される変数は、2回目以降の条件評価で型が変わり得るので、絞り込みを解除する。
                let mut assigned = HashSet::new();
                assigned_vars(body, &mut assigned);
                for name in assigned {
                    if let (Some(slot), Some(var)) = (env.get_mut(&name), self.vars.get(&name)) {
                        *slot = var.ty;
                    }
                    Self::forget_paths(env, &name);
                }
                let (t, t_env, f_env) = self.cond(cond, env);
                self.require_condition(t, cond.span);
                let mut b = t_env;
                self.block(body, &mut b);
                *env = f_env;
                false
            }
        }
    }

    /// `match` の検査。各 `case` で被検査変数を絞り込み、網羅性（E0303）と到達しない `case`（W0301）を調べる。
    /// 必ず `return` するなら true。
    fn match_stmt(&mut self, subject: &Expr, arms: &[MatchArm], env: &mut Env) -> bool {
        let st = self.expr(subject, env);
        if let Ty::Result(r) = st {
            let declared = self.path(subject).map(|(_, d)| d);
            return self.result_match(subject, r, declared, arms, env);
        }
        let subject_path = self.path(subject);
        let subject_declared = subject_path.as_ref().map(|(_, d)| *d);
        let subject_var = subject_path.map(|(k, _)| k);
        // Unknown は int / bool / str / float / None に加えて「将来増える種類」を持つ開いた型（float は ADR-0019）。
        // 最後の Unknown は `case _:` だけが網羅できる（ADR-0011）。
        // コンテナの種類 `list[Unknown]` / `dict[str, Unknown]` は ADR-0021 論点 1・2
        let members = (st != Ty::Error).then(|| {
            if st == Ty::Unknown {
                let l = self.cx.types.list(Ty::Unknown);
                let d = self.cx.types.dict(Ty::Str, Ty::Unknown);
                vec![
                    Ty::Int,
                    Ty::Bool,
                    Ty::Str,
                    Ty::Float,
                    Ty::None,
                    l,
                    d,
                    Ty::Unknown,
                ]
            } else {
                self.cx.types.members(st)
            }
        });
        let mut remaining = members.clone().unwrap_or_default();
        let mut live = Vec::new();
        for arm in arms {
            let mut pattern_failed = false;
            let mut error_binding: Option<String> = None;
            let covered: Vec<Ty> = match &arm.pattern.kind {
                PatternKind::Wildcard => remaining.clone(),
                PatternKind::Bind(name, b) => {
                    // 誤りを報告し、束縛変数は `Error` 型として置く（後続の診断を連鎖させない）
                    pattern_failed = true;
                    if let Some(b) = b
                        && !self.vars.contains_key(&b.name)
                    {
                        error_binding = Some(b.name.clone());
                    }
                    if st != Ty::Error {
                        self.bad_binding(name, st, arm.pattern.span);
                    }
                    // `int(n)` は型パターン `int()` として網羅性に数える
                    match (&members, is_result_ctor(name)) {
                        (Some(ms), false) => {
                            let pt = self
                                .pattern_type(&PatternKind::Type(name.clone()), arm.pattern.span);
                            if ms.contains(&pt) && remaining.contains(&pt) {
                                vec![pt]
                            } else {
                                Vec::new()
                            }
                        }
                        _ => Vec::new(),
                    }
                }
                kind => {
                    let pt = match kind {
                        // `case Result():` は被検査値の Union の中の `Result` に一致する（ADR-0013）
                        PatternKind::Type(n) if n == "Result" => {
                            match members.as_ref().and_then(|ms| {
                                ms.iter().find(|m| matches!(m, Ty::Result(_))).copied()
                            }) {
                                Some(r) => r,
                                None => {
                                    if st != Ty::Error {
                                        let msg = format!(
                                            "this pattern can never match: no `Result` is part of `{}`",
                                            self.cx.name(st)
                                        );
                                        self.cx.diags.push(Diagnostic::error(
                                            "E0305",
                                            msg,
                                            arm.pattern.span,
                                        ));
                                    }
                                    Ty::Error
                                }
                            }
                        }
                        // `case list():` / `case dict():` は `Unknown` の被検査値にだけ書ける（ADR-0021 論点 2）
                        PatternKind::Type(n)
                            if matches!(n.as_str(), "list" | "dict")
                                && !self.cx.aliases.contains_key(n) =>
                        {
                            if st == Ty::Unknown {
                                let t = if n == "list" {
                                    self.cx.types.list(Ty::Unknown)
                                } else {
                                    self.cx.types.dict(Ty::Str, Ty::Unknown)
                                };
                                self.cx.pattern_types.insert(arm.pattern.span.start, t);
                                t
                            } else {
                                if st != Ty::Error {
                                    self.cx.diags.push(
                                        Diagnostic::error(
                                            "E0203",
                                            format!("a `{n}()` pattern can only match an `Unknown` value"),
                                            arm.pattern.span,
                                        )
                                        .with_cause("`list()` / `dict()` patterns read JSON data held in `Unknown` (ADR-0021)")
                                        .with_hint("check `is None` or use `case _:` for a value whose type is already known"),
                                    );
                                }
                                Ty::Error
                            }
                        }
                        _ => self.pattern_type(kind, arm.pattern.span),
                    };
                    pattern_failed = pt == Ty::Error;
                    let on_param = members
                        .as_ref()
                        .is_some_and(|ms| ms.iter().any(|m| matches!(m, Ty::Param(_))));
                    match &members {
                        _ if pt == Ty::Error => Vec::new(),
                        _ if on_param && matches!(kind, PatternKind::Type(_)) => {
                            let msg = format!(
                                "cannot match a type pattern against `{}`, which contains a type parameter",
                                self.cx.name(st)
                            );
                            self.cx.diags.push(
                                Diagnostic::error("E0401", msg, arm.pattern.span)
                                    .with_cause("generic code must work for every type argument (ADR-0006)")
                                    .with_hint("match on `None` or use `case _:`, or make the function non-generic"),
                            );
                            pattern_failed = true;
                            Vec::new()
                        }
                        // 宣言された型には含まれるが、絞り込みで外れた型: エラーではなく到達しない case（W0301）
                        Some(ms)
                            if !ms.contains(&pt)
                                && subject_declared
                                    .is_some_and(|d| self.cx.types.members(d).contains(&pt)) =>
                        {
                            Vec::new()
                        }
                        Some(ms) if !ms.contains(&pt) => {
                            let msg = format!(
                                "this pattern can never match: `{}` is not part of `{}`",
                                self.cx.name(pt),
                                self.cx.name(st)
                            );
                            self.cx
                                .diags
                                .push(Diagnostic::error("E0305", msg, arm.pattern.span));
                            Vec::new()
                        }
                        Some(_) if !remaining.contains(&pt) => Vec::new(),
                        Some(_) => vec![pt],
                        None => Vec::new(),
                    }
                }
            };
            let already_reported = self
                .cx
                .diags
                .last()
                .is_some_and(|d| d.code == "E0305" && d.span == arm.pattern.span);
            if covered.is_empty() && members.is_some() && !already_reported && !pattern_failed {
                self.cx.diags.push(
                    Diagnostic::warning(
                        "W0301",
                        "this `case` is unreachable: earlier cases already cover it",
                        arm.pattern.span,
                    )
                    .with_hint("remove this `case` or move it before the case that covers it"),
                );
            }
            remaining.retain(|m| !covered.contains(m));
            let mut b = env.clone();
            if let Some(n) = &subject_var
                && !covered.is_empty()
            {
                // `case _:` が `Unknown` を網羅したら、絞り込んだ型も `Unknown` のまま
                let narrowed = if covered.contains(&Ty::Unknown) {
                    Ty::Unknown
                } else {
                    self.cx.types.union(covered.iter().copied())
                };
                b.insert(n.clone(), narrowed);
            }
            if let Some(n) = &error_binding {
                self.vars.insert(
                    n.clone(),
                    Var {
                        ty: Ty::Error,
                        decl: arm.pattern.span,
                    },
                );
                b.insert(n.clone(), Ty::Error);
                Self::forget_paths(&mut b, n);
            }
            let returns = self.block(&arm.body, &mut b);
            if let Some(n) = &error_binding {
                self.vars.remove(n);
                b.remove(n);
                Self::forget_paths(&mut b, n);
            }
            if !returns {
                live.push(b);
            }
        }
        if st == Ty::Unknown && !remaining.is_empty() {
            self.cx.diags.push(
                Diagnostic::error(
                    "E0602",
                    "a `match` on an `Unknown` value needs `case _:`",
                    subject.span,
                )
                .with_cause(
                    "`Unknown` is open: values of other kinds may appear in the future (ADR-0011)",
                )
                .with_hint("add `case _:` to handle every other value"),
            );
            live.push(env.clone());
        } else if members.is_some() && !remaining.is_empty() {
            let missing: Vec<String> = remaining.iter().map(|m| case_text(*m)).collect();
            let msg = format!(
                "non-exhaustive `match`: `{}` {} not covered",
                remaining
                    .iter()
                    .map(|m| self.cx.name(*m))
                    .collect::<Vec<_>>()
                    .join("`, `"),
                if remaining.len() == 1 { "is" } else { "are" }
            );
            self.cx.diags.push(
                Diagnostic::error("E0303", msg, subject.span)
                    .with_cause(format!("the value has type `{}`", self.cx.name(st)))
                    .with_hint(format!("add {}", missing.join(", ")))
                    .with_hint("or add `case _:` to handle every remaining value"),
            );
            live.push(env.clone());
        }
        if live.is_empty() {
            return true;
        }
        *env = self.join(live);
        false
    }

    /// `Result` ではない被検査値に束縛パターンを書いた（E1003）、または `Ok` / `Err` 以外に束縛を書いた（E1004）。
    fn bad_binding(&mut self, name: &str, st: Ty, span: Span) {
        if is_result_ctor(name) {
            let msg = format!(
                "`case {name}(...)` needs a value of type `Result`, but this value has type `{}`",
                self.cx.name(st)
            );
            let d = Diagnostic::error("E1003", msg, span)
                .with_cause("`Ok` / `Err` patterns can only match a value whose type is exactly `Result` (ADR-0013)");
            let d = if self
                .cx
                .types
                .members(st)
                .iter()
                .any(|m| matches!(m, Ty::Result(_)))
            {
                d.with_hint("narrow it first with `case Result():` (or `is None`), then match `Ok` / `Err` inside")
            } else {
                d
            };
            self.cx.diags.push(d);
        } else {
            self.cx.diags.push(
                Diagnostic::error(
                    "E1004",
                    format!("`case {name}(...)` cannot bind a value"),
                    span,
                )
                .with_cause("only `Ok(x)` and `Err(e)` bind values (ADR-0013); type patterns narrow the matched variable instead")
                .with_hint(format!("write `case {name}():` and use the matched variable")),
            );
        }
    }

    /// `Result` に対する `match`: `Ok` と `Err` の2つで網羅性を数え、束縛変数は `case` の本体だけで使える（ADR-0013）。
    /// `declared` は被検査値が変数のときの宣言された型（絞り込みで外れた構成要素の `case` は W0301 にする）。
    fn result_match(
        &mut self,
        subject: &Expr,
        r: ResultId,
        declared: Option<Ty>,
        arms: &[MatchArm],
        env: &mut Env,
    ) -> bool {
        let (ok_ty, err_ty) = self.cx.types.result_args(r);
        let st = Ty::Result(r);
        // [Ok, Err] がまだ一致していないか
        let mut remaining = [true, true];
        let mut live = Vec::new();
        for arm in arms {
            let span = arm.pattern.span;
            let mut failed = false;
            let mut binding: Option<(&Ident, Ty)> = None;
            let covered: Vec<usize> = match &arm.pattern.kind {
                PatternKind::Wildcard => vec![0, 1],
                PatternKind::Type(n) if n == "Result" => vec![0, 1],
                PatternKind::Bind(name, b) if is_result_ctor(name) => {
                    let piece = usize::from(name == "Err");
                    let ty = if piece == 0 { ok_ty } else { err_ty };
                    binding = b.as_ref().map(|b| (b, ty));
                    vec![piece]
                }
                PatternKind::Bind(name, _) => {
                    self.bad_binding(name, st, span);
                    failed = true;
                    Vec::new()
                }
                PatternKind::Type(n) if is_result_ctor(n) => {
                    self.cx.diags.push(
                        Diagnostic::error("E1004", format!("`case {n}()` needs one binding"), span)
                            .with_hint(format!(
                                "write `case {n}(x):`, or `case {n}(_):` to ignore the value"
                            )),
                    );
                    failed = true;
                    Vec::new()
                }
                PatternKind::Type(_) | PatternKind::None
                    if declared.is_some_and(|d| {
                        let pt = match &arm.pattern.kind {
                            PatternKind::Type(n) => match n.as_str() {
                                "int" => Ty::Int,
                                "bool" => Ty::Bool,
                                "str" => Ty::Str,
                                "float" => Ty::Float,
                                _ => Ty::Error,
                            },
                            _ => Ty::None,
                        };
                        self.cx.types.members(d).contains(&pt)
                    }) =>
                {
                    // 宣言された型には含まれるが、絞り込みで外れた構成要素: 到達しない case（W0301）
                    Vec::new()
                }
                PatternKind::Type(_) | PatternKind::None => {
                    let what = match &arm.pattern.kind {
                        PatternKind::Type(n) => format!("{n}()"),
                        _ => "None".into(),
                    };
                    let msg = format!(
                        "this pattern can never match: `{what}` is not a case of `{}`",
                        self.cx.name(st)
                    );
                    self.cx.diags.push(
                        Diagnostic::error("E0305", msg, span)
                            .with_hint("match a `Result` with `case Ok(x):` and `case Err(e):`"),
                    );
                    failed = true;
                    Vec::new()
                }
            };
            let covered: Vec<usize> = covered.into_iter().filter(|&i| remaining[i]).collect();
            if covered.is_empty() && !failed {
                self.cx.diags.push(
                    Diagnostic::warning(
                        "W0301",
                        "this `case` is unreachable: earlier cases already cover it",
                        span,
                    )
                    .with_hint("remove this `case` or move it before the case that covers it"),
                );
            }
            for &i in &covered {
                remaining[i] = false;
            }
            let mut b = env.clone();
            let mut bound = None;
            if let Some((ident, ty)) = binding {
                if let Some(var) = self.vars.get(&ident.name) {
                    let decl = var.decl;
                    self.cx.diags.push(
                        Diagnostic::error(
                            "E1005",
                            format!(
                                "the binding `{}` has the same name as an existing variable",
                                ident.name
                            ),
                            ident.span,
                        )
                        .with_cause(
                            "a binding is a new variable that exists only in its `case` (ADR-0013)",
                        )
                        .with_hint("choose another name for the binding")
                        .with_related(
                            RelatedKind::Variable,
                            &ident.name,
                            Some(decl),
                        ),
                    );
                } else {
                    self.vars.insert(
                        ident.name.clone(),
                        Var {
                            ty,
                            decl: ident.span,
                        },
                    );
                    b.insert(ident.name.clone(), ty);
                    // 束縛は新しい値: 同じ名前の以前の経路の絞り込みを引き継がない（ADR-0016 論点 4）
                    Self::forget_paths(&mut b, &ident.name);
                    bound = Some(ident.name.clone());
                }
            }
            let returns = self.block(&arm.body, &mut b);
            if let Some(n) = bound {
                self.vars.remove(&n);
                b.remove(&n);
                // 束縛のスコープ（case の本体）で付いた経路を、match の後に漏らさない
                Self::forget_paths(&mut b, &n);
            }
            if !returns {
                live.push(b);
            }
        }
        if remaining.iter().any(|&m| m) {
            let names: Vec<&str> = ["Ok", "Err"]
                .iter()
                .zip(remaining)
                .filter(|(_, m)| *m)
                .map(|(n, _)| *n)
                .collect();
            let hints: Vec<&str> = ["`case Ok(x):`", "`case Err(e):`"]
                .iter()
                .zip(remaining)
                .filter(|(_, m)| *m)
                .map(|(n, _)| *n)
                .collect();
            let msg = format!(
                "non-exhaustive `match`: `{}` {} not covered",
                names.join("`, `"),
                if names.len() == 1 { "is" } else { "are" }
            );
            self.cx.diags.push(
                Diagnostic::error("E0303", msg, subject.span)
                    .with_cause(format!("the value has type `{}`", self.cx.name(st)))
                    .with_hint(format!("add {}", hints.join(", ")))
                    .with_hint("or add `case _:` to handle every remaining value"),
            );
            live.push(env.clone());
        }
        if live.is_empty() {
            return true;
        }
        *env = self.join(live);
        false
    }

    fn pattern_type(&mut self, kind: &PatternKind, span: Span) -> Ty {
        let t = self.pattern_type_inner(kind, span);
        if matches!(kind, PatternKind::Type(_)) {
            self.cx.pattern_types.insert(span.start, t);
        }
        t
    }

    fn pattern_type_inner(&mut self, kind: &PatternKind, span: Span) -> Ty {
        match kind {
            PatternKind::None => Ty::None,
            PatternKind::Wildcard | PatternKind::Bind(..) => Ty::Error,
            PatternKind::Type(name) if self.cx.aliases.contains_key(name) => {
                match self.cx.resolve_alias(name) {
                    t @ (Ty::Int | Ty::Bool | Ty::Str | Ty::Float | Ty::Error) => t,
                    Ty::Record(_) => {
                        self.cx.diags.push(
                            Diagnostic::error(
                                "E0304",
                                format!("record type `{name}` cannot be used in a `case` pattern"),
                                span,
                            )
                            .with_cause("structural records cannot be told apart by `case` (ADR-0007)")
                            .with_hint("match on the optional part instead (e.g. `case None:` / `case _:`), or use separate variables"),
                        );
                        Ty::Error
                    }
                    _ => {
                        self.cx.diags.push(Diagnostic::error(
                            "E0203",
                            format!("type `{name}` cannot be used as a pattern"),
                            span,
                        ));
                        Ty::Error
                    }
                }
            }
            PatternKind::Type(name) => match name.as_str() {
                "int" => Ty::Int,
                "bool" => Ty::Bool,
                "str" => Ty::Str,
                "float" => Ty::Float,
                other => {
                    self.cx.diags.push(
                        Diagnostic::error("E0203", format!("unknown type `{other}` in pattern"), span)
                            .with_hint("type patterns are `int()`, `bool()`, `str()` and `float()` (and `list()` / `dict()` on `Unknown`); use `None` or `_` for the others"),
                    );
                    Ty::Error
                }
            },
        }
    }

    /// 型の手掛かり（`hint`）付きで式を検査する。今はリストのリテラル（特に空の `[]`）だけが手掛かりを使う。
    fn expr_hint(&mut self, e: &Expr, env: &Env, hint: Option<Ty>) -> Ty {
        match &e.kind {
            ExprKind::List(items) => {
                let t = self.list_literal(items, env, hint, e.span);
                self.record(e, t)
            }
            ExprKind::Dict(entries) => {
                let t = self.dict_literal(entries, env, hint, e.span);
                self.record(e, t)
            }
            ExprKind::Call {
                callee,
                type_args,
                args,
                kwargs,
            } if is_result_ctor(&callee.name) && kwargs.is_empty() && type_args.is_empty() => {
                let t = self.result_ctor(callee, args, hint, e.span, env);
                self.record(e, t)
            }
            _ => self.expr(e, env),
        }
    }

    /// `operand?`（ADR-0013）: 被演算子は `Result`（E1010）、囲む関数は `Result` を返す（E1011）、
    /// 被演算子のエラー型は宣言されたエラー型に収まる（E1012）。式の型は成功時の型。
    fn try_expr(&mut self, inner: &Expr, span: Span, env: &Env) -> Ty {
        let t = self.expr(inner, env);
        let Ty::Result(r) = t else {
            if t != Ty::Error {
                let msg = format!(
                    "`?` can only be applied to a `Result`, but this value has type `{}`",
                    self.cx.name(t)
                );
                let d = Diagnostic::error("E1010", msg, span).with_cause(
                    "`?` propagates the `Err` of a `Result`; it is not used for `None` (ADR-0006)",
                );
                let d = if self.cx.types.contains_none(t) {
                    d.with_hint("check for `None` with `if x is None:` instead")
                } else {
                    d.with_hint("remove the `?`")
                };
                self.cx.diags.push(d);
            }
            return Ty::Error;
        };
        let (ok, err) = self.cx.types.result_args(r);
        match self.ret {
            Ty::Result(fr) => {
                let (_, ferr) = self.cx.types.result_args(fr);
                if !self.cx.types.fits(err, ferr) {
                    let missing: Vec<String> = self
                        .cx
                        .types
                        .members(err)
                        .into_iter()
                        .filter(|m| !self.cx.types.fits(*m, ferr))
                        .map(|m| format!("`{}`", self.cx.name(m)))
                        .collect();
                    let msg = format!(
                        "`?` cannot propagate the error type `{}`: the function's error type is `{}`",
                        self.cx.name(err),
                        self.cx.name(ferr)
                    );
                    let union = self.cx.types.union([ferr, err]);
                    self.cx.diags.push(
                        Diagnostic::error("E1012", msg, span)
                            .with_cause(format!(
                                "{} {} not part of the declared error type",
                                missing.join(", "),
                                if missing.len() == 1 { "is" } else { "are" }
                            ))
                            .with_hint(format!(
                                "widen the declared error type, e.g. `Result[..., {}]`, or handle the error with `match`",
                                self.cx.name(union)
                            )),
                    );
                }
            }
            Ty::Error => {}
            other => {
                let msg = format!(
                    "`?` can only be used in a function that returns a `Result`, but this function returns `{}`",
                    self.cx.name(other)
                );
                self.cx.diags.push(
                    Diagnostic::error("E1011", msg, span)
                        .with_cause("`?` returns the `Err` from the enclosing function (ADR-0013)")
                        .with_hint("handle the error with `match` and `case Err(e):`, or change the return type to a `Result`"),
                );
            }
        }
        ok
    }

    /// `{k: v, ...}`: 手掛かりが `dict[K, V]`（か、それを1つだけ含む Union）なら各要素がそれに収まるかを確かめる。
    /// なければキーと値それぞれの型の Union（キーは `int` / `bool` / `str` のどれか1つでなければ E0907）。
    fn dict_literal(
        &mut self,
        entries: &[(Expr, Expr)],
        env: &Env,
        hint: Option<Ty>,
        span: Span,
    ) -> Ty {
        let dict_hint = hint.and_then(|h| {
            let ds: Vec<Ty> = self
                .cx
                .types
                .members(h)
                .into_iter()
                .filter(|m| matches!(m, Ty::Dict(_)))
                .collect();
            (ds.len() == 1).then(|| ds[0])
        });
        let kv = match dict_hint {
            Some(Ty::Dict(d)) => Some(self.cx.types.dict_args(d)),
            _ => None,
        };
        let mut kts = Vec::new();
        let mut vts = Vec::new();
        for (k, v) in entries {
            kts.push(self.expr_hint(k, env, kv.map(|p| p.0)));
            vts.push(self.expr_hint(v, env, kv.map(|p| p.1)));
        }
        if let (Some(t), Some((k, v))) = (dict_hint, kv) {
            for (i, (ke, ve)) in entries.iter().enumerate() {
                self.cx
                    .require(kts[i], k, ke.span, "a key of the dict", None);
                self.cx
                    .require(vts[i], v, ve.span, "a value of the dict", None);
            }
            return t;
        }
        if hint == Some(Ty::Error) {
            return Ty::Error;
        }
        if entries.is_empty() {
            self.cx.diags.push(
                Diagnostic::error(
                    "E0906",
                    "cannot infer the key and value types of an empty dict",
                    span,
                )
                .with_hint("annotate the variable, e.g. `d: dict[str, int] = {}`"),
            );
            return Ty::Error;
        }
        let k = self.cx.types.union(kts);
        let v = self.cx.types.union(vts);
        if k == Ty::Error || v == Ty::Error {
            return Ty::Error;
        }
        if !valid_dict_key(k) {
            let name = self.cx.name(k);
            self.cx.diags.push(dict_key_error(&name, span));
            return Ty::Error;
        }
        if v == Ty::None {
            self.cx.diags.push(
                Diagnostic::error("E0215", "a dict value cannot have type `None`", span)
                    .with_hint("annotate the variable with an optional value type, e.g. `d: dict[str, int | None] = ...`"),
            );
            return Ty::Error;
        }
        self.cx.types.dict(k, v)
    }

    /// `k in d` / `k not in d`: 右辺は `dict`、左辺はそのキーの型（ADR-0015）。
    fn in_expr(&mut self, lhs: &Expr, rhs: &Expr, env: &Env) -> Ty {
        let rt = self.expr(rhs, env);
        match rt {
            Ty::Dict(d) => {
                let (k, _) = self.cx.types.dict_args(d);
                let lt = self.expr_hint(lhs, env, Some(k));
                self.cx.require(lt, k, lhs.span, "a key of the dict", None);
                Ty::Bool
            }
            Ty::Error => {
                self.expr(lhs, env);
                Ty::Error
            }
            other => {
                self.expr(lhs, env);
                let msg = format!(
                    "`in` needs a `dict` on the right, but this value has type `{}`",
                    self.cx.name(other)
                );
                self.cx.diags.push(
                    Diagnostic::error("E0909", msg, rhs.span)
                        .with_cause("`in` checks whether a key exists in a `dict` (ADR-0015)")
                        .with_hint("for a list, loop over it and compare each element"),
                );
                Ty::Error
            }
        }
    }

    /// `Ok(v)` / `Err(e)`: もう一方の型引数は手掛かり（期待型）から決める（ADR-0013）。
    /// 手掛かりが `Result` か、`Result` を1つだけ含む Union（`Result[int, str] | None`）でなければ E1001。
    fn result_ctor(
        &mut self,
        callee: &Ident,
        args: &[Expr],
        hint: Option<Ty>,
        span: Span,
        env: &Env,
    ) -> Ty {
        let target = hint.and_then(|h| {
            let rs: Vec<Ty> = self
                .cx
                .types
                .members(h)
                .into_iter()
                .filter(|m| matches!(m, Ty::Result(_)))
                .collect();
            (rs.len() == 1).then(|| rs[0])
        });
        let is_ok = callee.name == "Ok";
        let payload = target.map(|r| {
            let Ty::Result(id) = r else {
                unreachable!("filtered above")
            };
            let (ok, err) = self.cx.types.result_args(id);
            if is_ok { ok } else { err }
        });
        if args.len() != 1 {
            for a in args {
                self.expr(a, env);
            }
            self.cx.diags.push(
                Diagnostic::error(
                    "E0205",
                    format!(
                        "`{}` takes 1 argument but {} were given",
                        callee.name,
                        args.len()
                    ),
                    span,
                )
                .with_hint(format!("write `{}(value)`", callee.name)),
            );
            return target.unwrap_or(Ty::Error);
        }
        let at = self.expr_hint(&args[0], env, payload);
        if hint == Some(Ty::Error) {
            return Ty::Error;
        }
        match (target, payload) {
            (Some(r), Some(p)) => {
                let ctx = format!("the value of `{}` for `{}`", callee.name, self.cx.name(r));
                self.cx.require(at, p, args[0].span, &ctx, None);
                r
            }
            _ => {
                let other = if is_ok { "error" } else { "success" };
                self.cx.diags.push(
                    Diagnostic::error(
                        "E1001",
                        format!(
                            "cannot infer the {other} type of `{}(...)`",
                            callee.name
                        ),
                        span,
                    )
                    .with_cause("`Ok` / `Err` take the other type argument from the expected type (ADR-0013)")
                    .with_hint("annotate the variable, e.g. `r: Result[int, str] = Ok(1)`, or return it from a function declared to return a `Result`"),
                );
                Ty::Error
            }
        }
    }

    /// `[a, b, c]`: 手掛かりが `list[T]` なら各要素が `T` に収まるかを確かめる。なければ要素の型の Union。
    fn list_literal(&mut self, items: &[Expr], env: &Env, hint: Option<Ty>, span: Span) -> Ty {
        // 手掛かりがリスト型か、リスト型を1つだけ含む Union（`list[int] | None`）なら、その要素の型を使う
        let list_hint = hint.and_then(|h| {
            let lists: Vec<Ty> = self
                .cx
                .types
                .members(h)
                .into_iter()
                .filter(|m| matches!(m, Ty::List(_)))
                .collect();
            (lists.len() == 1).then(|| lists[0])
        });
        let elem_hint = match list_hint {
            Some(Ty::List(l)) => Some(self.cx.types.list_elem(l)),
            _ => None,
        };
        let tys: Vec<Ty> = items
            .iter()
            .map(|i| self.expr_hint(i, env, elem_hint))
            .collect();
        if let Some(elem) = elem_hint {
            for (i, t) in items.iter().zip(&tys) {
                self.cx
                    .require(*t, elem, i.span, "an element of the list", None);
            }
            return self.cx.types.list(elem);
        }
        if tys.is_empty() {
            self.cx.diags.push(
                Diagnostic::error(
                    "E0901",
                    "cannot infer the element type of an empty list",
                    span,
                )
                .with_hint("annotate the variable, e.g. `xs: list[int] = []`"),
            );
            return Ty::Error;
        }
        let elem = self.cx.types.union(tys);
        self.cx.types.list(elem)
    }

    /// 添字を付けられる値なら要素の型。そうでなければ診断を出して `None`。
    fn list_elem_of(&mut self, t: Ty, span: Span) -> Option<Ty> {
        match t {
            Ty::List(l) => Some(self.cx.types.list_elem(l)),
            Ty::Error => None,
            Ty::Unknown => {
                self.cx
                    .require(t, Ty::Int, span, "only lists can be indexed", None);
                None
            }
            t => {
                let rest = self.cx.types.without_none(t);
                let msg = if self.cx.types.contains_none(t)
                    && matches!(rest, Ty::List(_) | Ty::Dict(_))
                {
                    Diagnostic::error(
                        "E0301",
                        format!(
                            "this value may be `None`: cannot index `{}`",
                            self.cx.name(t)
                        ),
                        span,
                    )
                    .with_hint("check it first, e.g. `if xs is not None:`")
                } else {
                    Diagnostic::error(
                        "E0904",
                        format!("`{}` cannot be indexed", self.cx.name(t)),
                        span,
                    )
                    .with_cause("only `list` and `dict` values can be indexed")
                };
                self.cx.diags.push(msg);
                None
            }
        }
    }

    /// `for` の反復対象: `range(n)` / `range(a, b)` なら `int`、`list[T]` なら `T`。
    fn for_iter(&mut self, iter: &Expr, env: &Env) -> Ty {
        if let ExprKind::Call {
            callee,
            type_args,
            args,
            kwargs,
        } = &iter.kind
            && callee.name == "range"
            && kwargs.is_empty()
            && type_args.is_empty()
        {
            if args.is_empty() || args.len() > 2 {
                self.cx.diags.push(Diagnostic::error(
                    "E0205",
                    format!(
                        "`range` takes 1 or 2 arguments but {} were given",
                        args.len()
                    ),
                    iter.span,
                ));
            }
            for a in args {
                let t = self.expr(a, env);
                self.cx
                    .require(t, Ty::Int, a.span, "`range` takes `int` arguments", None);
            }
            self.record(iter, Ty::Int);
            return Ty::Int;
        }
        let t = self.expr(iter, env);
        match t {
            Ty::List(l) => self.cx.types.list_elem(l),
            // `for k in d:` はキーを挿入順に反復する（ADR-0015）
            Ty::Dict(d) => self.cx.types.dict_args(d).0,
            Ty::Error => Ty::Error,
            other => {
                let msg = format!("cannot iterate over `{}`", self.cx.name(other));
                self.cx
                    .diags
                    .push(Diagnostic::error("E0903", msg, iter.span).with_hint(
                        "iterate over a `list[T]`, a `dict[K, V]` (its keys) or `range(n)`",
                    ));
                Ty::Error
            }
        }
    }

    /// 要素（`dict` は値）の型が `Unknown` を含む `list` / `dict` は読み取り専用（E1503、ADR-0021 論点 4）。
    /// `Unknown` から届く値は変更のできない木なので、循環参照を作れない。
    fn check_mutable(&mut self, t: Ty, span: Span) {
        let inner = match t {
            Ty::List(l) => self.cx.types.list_elem(l),
            Ty::Dict(d) => self.cx.types.dict_args(d).1,
            _ => return,
        };
        if self.cx.types.contains_unknown(inner) {
            let msg = format!(
                "cannot modify `{}`: a list or dict whose elements are `Unknown` is read-only",
                self.cx.name(t)
            );
            self.cx.diags.push(
                Diagnostic::error("E1503", msg, span)
                    .with_cause("data reached from `Unknown` is an immutable tree, so it cannot form a reference cycle (ADR-0021 point 4)")
                    .with_hint("build a new list, or validate the data into a concrete type before changing it"),
            );
        }
    }

    /// `xs.append(v)`（ローカル変数のリストへのメソッド呼び出し）。
    fn method_call(
        &mut self,
        recv: &str,
        method: &str,
        mspan: Span,
        args: &[Expr],
        span: Span,
        env: &Env,
    ) -> Ty {
        let rt = match env.get(recv) {
            Some(t) => *t,
            None => self.vars[recv].ty,
        };
        if matches!(method, "append" | "pop") {
            self.check_mutable(rt, span);
        }
        match (rt, method) {
            (Ty::List(l), "append") => {
                let elem = self.cx.types.list_elem(l);
                if args.len() != 1 {
                    self.cx.diags.push(Diagnostic::error(
                        "E0205",
                        format!("`append` takes 1 argument but {} were given", args.len()),
                        span,
                    ));
                    for a in args {
                        self.expr(a, env);
                    }
                    return Ty::None;
                }
                let vt = self.expr_hint(&args[0], env, Some(elem));
                self.cx
                    .require(vt, elem, args[0].span, "an element of the list", None);
                Ty::None
            }
            (Ty::Dict(d), "pop") => {
                // d.pop(k): 値を取り出してキーを削除する。キーがなければパニック（ADR-0015）
                let (k, v) = self.cx.types.dict_args(d);
                if args.len() != 1 {
                    self.cx.diags.push(Diagnostic::error(
                        "E0205",
                        format!("`pop` takes 1 argument but {} were given", args.len()),
                        span,
                    ));
                    for a in args {
                        self.expr(a, env);
                    }
                    return Ty::Error;
                }
                let kt = self.expr_hint(&args[0], env, Some(k));
                self.cx
                    .require(kt, k, args[0].span, "a key of the dict", None);
                v
            }
            (Ty::Dict(d), "get") => {
                let (k, v) = self.cx.types.dict_args(d);
                if args.len() != 1 {
                    self.cx.diags.push(Diagnostic::error(
                        "E0205",
                        format!("`get` takes 1 argument but {} were given", args.len()),
                        span,
                    ));
                    for a in args {
                        self.expr(a, env);
                    }
                    return Ty::Error;
                }
                let kt = self.expr_hint(&args[0], env, Some(k));
                self.cx
                    .require(kt, k, args[0].span, "a key of the dict", None);
                // `Unknown` は JSON の `null` を持つので、`None` を含む値と同じ扱い（ADR-0021 論点 7）
                if self.cx.types.contains_none(v) || v == Ty::Unknown {
                    let msg = format!(
                        "`get` cannot be used on `{}`, whose values may be `None`",
                        self.cx.name(rt)
                    );
                    self.cx.diags.push(
                        Diagnostic::error("E0908", msg, span)
                            .with_cause("`None` from `get` would mean both \"no key\" and \"the value is None\" (ADR-0015)")
                            .with_hint(format!("check the key with `if k in {recv}:` and then read `{recv}[k]`")),
                    );
                    return Ty::Error;
                }
                self.cx.types.union([v, Ty::None])
            }
            (Ty::Error, _) => Ty::Error,
            (t, m) => {
                for a in args {
                    self.expr(a, env);
                }
                let msg = format!("`{}` has no method `{m}`", self.cx.name(t));
                self.cx
                    .diags
                    .push(Diagnostic::error("E0905", msg, mspan).with_cause(
                        "only `append` on `list`, and `get` / `pop` on `dict` are supported in M3",
                    ));
                Ty::Error
            }
        }
    }

    fn require_condition(&mut self, t: Ty, span: Span) {
        if t == Ty::Bool || t == Ty::Error {
            return;
        }
        let before = self.cx.diags.len();
        self.cx
            .require(t, Ty::Bool, span, "a condition must be a `bool`", None);
        if let Some(d) = self.cx.diags.get_mut(before)
            && d.code == "E0201"
        {
            d.hints.push(wys_diagnostics::Hint {
                message:
                    "there is no implicit conversion to `bool`; compare explicitly, e.g. `x != 0`"
                        .into(),
                replacement: None,
            });
        }
    }

    fn assign(&mut self, target: &Ident, ann: Option<&TypeExpr>, value: &Expr, env: &mut Env) {
        let ann_ty = ann.map(|a| self.cx.resolve(a));
        // 空のリスト `[]` などのために、注釈か既存の変数の型を手掛かりとして渡す
        let hint = ann_ty.or_else(|| self.vars.get(&target.name).map(|v| v.ty));
        let vt = self.expr_hint(value, env, hint);
        let declared = if let Some(var) = self.vars.get(&target.name) {
            let (decl_ty, decl) = (var.ty, var.decl);
            if let Some(at) = ann_ty
                && at != decl_ty
                && at != Ty::Error
                && decl_ty != Ty::Error
            {
                let msg = format!(
                    "mismatched types: expected `{}`, found `{}`",
                    self.cx.name(decl_ty),
                    self.cx.name(at)
                );
                self.cx.diags.push(
                    Diagnostic::error("E0201", msg, ann.expect("ann_ty implies ann").span)
                        .with_cause("a variable keeps the type of its first assignment")
                        .with_related(RelatedKind::Variable, &target.name, Some(decl)),
                );
            } else {
                let ctx = format!(
                    "variable `{}` was declared as `{}`",
                    target.name,
                    self.cx.name(decl_ty)
                );
                self.cx.require(
                    vt,
                    decl_ty,
                    value.span,
                    &ctx,
                    Some((RelatedKind::Variable, &target.name, decl)),
                );
            }
            decl_ty
        } else {
            let ty = match ann_ty {
                Some(at) => {
                    let ctx = format!(
                        "variable `{}` is annotated as `{}`",
                        target.name,
                        self.cx.name(at)
                    );
                    self.cx.require(vt, at, value.span, &ctx, None);
                    at
                }
                None => vt,
            };
            if ty == Ty::None {
                self.cx.diags.push(
                    Diagnostic::error(
                        "E0215",
                        format!("variable `{}` cannot hold only `None`", target.name),
                        value.span,
                    )
                    .with_cause("a variable of type `None` is not useful")
                    .with_hint(
                        "annotate the variable with an optional type, e.g. `x: int | None = None`",
                    ),
                );
            }
            self.declare(&target.name, ty, target.span);
            ty
        };
        // 代入した値の型で絞り込む。ただし宣言が Union で、値の型の構成要素がすべて宣言の構成要素そのものの場合だけ
        // （レコードの幅部分型ではスロットの値は変換後の型なので、元の型として読めない）。それ以外は宣言された型に戻す。
        // `None` だけへの絞り込みはしない（Generics の推論で T = None になり、E0215 を招く）
        let narrowed = if vt != Ty::Error && vt != Ty::None && self.cx.types.is_union(declared) && {
            let ms = self.cx.types.members(declared);
            self.cx.types.members(vt).iter().all(|m| ms.contains(m))
        } {
            vt
        } else {
            declared
        };
        env.insert(target.name.clone(), narrowed);
        Self::forget_paths(env, &target.name);
    }

    fn ret_stmt(&mut self, span: Span, value: Option<&Expr>, env: &Env) {
        match (self.ret, value) {
            (Ty::Error, Some(e)) => {
                self.expr_hint(e, env, Some(Ty::Error));
            }
            (Ty::Error, None) | (Ty::None, None) => {}
            (Ty::None, Some(e)) => {
                let t = self.expr(e, env);
                if t != Ty::None && t != Ty::Error {
                    let msg = format!(
                        "this function returns `None`, but a `{}` value is returned",
                        self.cx.name(t)
                    );
                    self.cx.diags.push(
                        Diagnostic::error("E0211", msg, e.span)
                            .with_hint("change the return type annotation or remove the value"),
                    );
                }
            }
            (r, None) => {
                let msg = format!("this function must return a `{}` value", self.cx.name(r));
                let d = Diagnostic::error("E0211", msg, span);
                let d = if self.cx.types.contains_none(r) {
                    d.with_hint("write `return None` explicitly")
                } else {
                    d
                };
                self.cx.diags.push(d);
            }
            (r, Some(e)) => {
                let t = self.expr_hint(e, env, Some(r));
                let ctx = format!("the function's return type is `{}`", self.cx.name(r));
                self.cx.require(t, r, e.span, &ctx, None);
            }
        }
    }

    fn record(&mut self, e: &Expr, t: Ty) -> Ty {
        self.cx.expr_types[e.id.0 as usize] = t;
        t
    }

    /// 式を検査して型を返す。
    fn expr(&mut self, e: &Expr, env: &Env) -> Ty {
        match &e.kind {
            ExprKind::Is { .. }
            | ExprKind::Unary {
                op: UnaryOp::Not, ..
            }
            | ExprKind::Binary {
                op: BinOp::And | BinOp::Or,
                ..
            } => self.cond(e, env).0,
            _ => {
                let t = self.expr_inner(e, env);
                self.record(e, t)
            }
        }
    }

    /// 真偽値になり得る式を検査し、`(型, 真のときの環境, 偽のときの環境)` を返す。
    fn cond(&mut self, e: &Expr, env: &Env) -> (Ty, Env, Env) {
        match &e.kind {
            ExprKind::Is { lhs, negated, rhs } => {
                let lt = self.expr(lhs, env);
                self.expr(rhs, env);
                let mut t_env = env.clone();
                let mut f_env = env.clone();
                if !matches!(rhs.kind, ExprKind::None) {
                    self.cx.diags.push(
                        Diagnostic::error(
                            "E0207",
                            "`is` can only be used to compare with `None`",
                            e.span,
                        )
                        .with_hint("use `==` to compare values"),
                    );
                } else if lt != Ty::Error && self.cx.types.members(lt).contains(&Ty::Unknown) {
                    self.cx.diags.push(Diagnostic::error("E0601", "this `Unknown` value has not been checked", lhs.span)
                    .with_cause("values from outside the program must be checked before use (LANG-006, ADR-0011)")
                    .with_hint("inspect it with `match`: `case int():`, `case str():`, ..., `case _:`"));
                } else if lt != Ty::Error {
                    // ローカル変数なら、宣言された型も見る（代入で絞り込まれていても、宣言が None を含めば検査は正当）
                    let var_decl = self.path(lhs);
                    let declared_has_none = var_decl
                        .as_ref()
                        .is_some_and(|(_, d)| self.cx.types.contains_none(*d));
                    if !self.cx.types.contains_none(lt) && !declared_has_none {
                        let msg = format!(
                            "this value can never be `None` (its type is `{}`), so the check is always {}",
                            self.cx.name(lt),
                            if *negated { "True" } else { "False" }
                        );
                        self.cx.diags.push(
                            Diagnostic::error("E0302", msg, lhs.span)
                                .with_hint("remove the check, or give the value an optional type such as `T | None`"),
                        );
                    } else if let Some((n, declared)) = var_decl {
                        // 今の型（絞り込み後）で分岐を決める。どちらかの分岐に到達しない場合は目印を付け、
                        // その分岐の中では変数を宣言された型から分かる範囲の型として検査する（エラー型にしない）
                        let none_reachable = self.cx.types.contains_none(lt);
                        let some_now = self.cx.types.without_none(lt);
                        let some_reachable = some_now != Ty::Error;
                        let some = if some_reachable {
                            some_now
                        } else {
                            let d = self.cx.types.without_none(declared);
                            if d == Ty::Error { lt } else { d }
                        };
                        let (on_true, on_false) = if *negated {
                            (some, Ty::None)
                        } else {
                            (Ty::None, some)
                        };
                        t_env.insert(n.clone(), on_true);
                        f_env.insert(n.clone(), on_false);
                        let (true_reachable, false_reachable) = if *negated {
                            (some_reachable, none_reachable)
                        } else {
                            (none_reachable, some_reachable)
                        };
                        if !true_reachable {
                            t_env.insert(UNREACHABLE.to_string(), Ty::Error);
                        }
                        if !false_reachable {
                            f_env.insert(UNREACHABLE.to_string(), Ty::Error);
                        }
                    }
                }
                self.record(e, Ty::Bool);
                (Ty::Bool, t_env, f_env)
            }
            ExprKind::Unary {
                op: UnaryOp::Not,
                operand,
            } => {
                let (t, a, b) = self.cond(operand, env);
                let ty = self.unary_result(UnaryOp::Not, t, e.span);
                self.record(e, ty);
                (ty, b, a)
            }
            ExprKind::Binary {
                op: op @ (BinOp::And | BinOp::Or),
                lhs,
                rhs,
            } => {
                let (lt, t1, f1) = self.cond(lhs, env);
                let (rt, t_env, f_env) = if *op == BinOp::And {
                    // 右辺は左辺が真のときだけ評価される
                    let (rt, t2, f2) = self.cond(rhs, &t1);
                    (rt, t2, self.join(vec![f1, f2]))
                } else {
                    // 右辺は左辺が偽のときだけ評価される
                    let (rt, t2, f2) = self.cond(rhs, &f1);
                    (rt, self.join(vec![t1, t2]), f2)
                };
                let ty = self.binary(*op, lt, rt, e.span);
                self.record(e, ty);
                (ty, t_env, f_env)
            }
            _ => {
                let t = self.expr(e, env);
                (t, env.clone(), env.clone())
            }
        }
    }

    fn expr_inner(&mut self, e: &Expr, env: &Env) -> Ty {
        match &e.kind {
            ExprKind::Int(_) => Ty::Int,
            ExprKind::Float(_) => Ty::Float,
            ExprKind::Bool(_) => Ty::Bool,
            ExprKind::Str(_) => Ty::Str,
            ExprKind::None => Ty::None,
            ExprKind::Name(name) => {
                if let Some(&t) = env.get(name) {
                    match self.vars.get(name) {
                        Some(v) if v.ty != t && t != Ty::Error && v.ty != Ty::Error => {
                            let n = Narrowing {
                                name: name.clone(),
                                span: e.span,
                                declared: v.ty,
                                narrowed: t,
                            };
                            self.narrowings.insert(e.id, n);
                        }
                        _ => {
                            self.narrowings.remove(&e.id);
                        }
                    }
                    t
                } else if let Some(var) = self.vars.get(name) {
                    let (ty, decl) = (var.ty, var.decl);
                    self.cx.diags.push(
                        Diagnostic::error(
                            "E0204",
                            format!("variable `{name}` may be used before it is assigned"),
                            e.span,
                        )
                        .with_cause("it is not assigned on every path that reaches this use")
                        .with_related(
                            RelatedKind::Variable,
                            name,
                            Some(decl),
                        ),
                    );
                    ty
                } else if self.cx.sig_index.contains_key(name) || BUILTINS.contains(&name.as_str())
                {
                    self.cx.diags.push(
                        Diagnostic::error(
                            "E0213",
                            format!("function `{name}` cannot be used as a value"),
                            e.span,
                        )
                        .with_hint(format!("call it with `{name}(...)`")),
                    );
                    Ty::Error
                } else {
                    self.cx.diags.push(Diagnostic::error(
                        "E0202",
                        format!("undefined variable `{name}`"),
                        e.span,
                    ));
                    Ty::Error
                }
            }
            ExprKind::Unary { op, operand } => {
                let t = self.expr(operand, env);
                self.unary_result(*op, t, e.span)
            }
            ExprKind::Binary {
                op: BinOp::In | BinOp::NotIn,
                lhs,
                rhs,
            } => self.in_expr(lhs, rhs, env),
            ExprKind::Binary { op, lhs, rhs } => {
                let lt = self.expr(lhs, env);
                let rt = self.expr(rhs, env);
                self.binary(*op, lt, rt, e.span)
            }
            ExprKind::List(items) => self.list_literal(items, env, None, e.span),
            ExprKind::Dict(entries) => self.dict_literal(entries, env, None, e.span),
            ExprKind::Index { base, index } => {
                let bt = self.expr(base, env);
                if let Ty::Dict(d) = bt {
                    let (k, v) = self.cx.types.dict_args(d);
                    let it = self.expr_hint(index, env, Some(k));
                    self.cx
                        .require(it, k, index.span, "a key of the dict", None);
                    return v;
                }
                let it = self.expr(index, env);
                self.cx.require(
                    it,
                    Ty::Int,
                    index.span,
                    "a list index must be an `int`",
                    None,
                );
                self.list_elem_of(bt, base.span).unwrap_or(Ty::Error)
            }
            ExprKind::MethodCall {
                receiver,
                method,
                args,
                kwargs,
                ..
            } => {
                let span = Span::new(receiver.span.start, method.span.end);
                if let Some(n) = receiver.as_name()
                    && !self.vars.contains_key(n)
                {
                    // 名前解決がモジュールの参照に直さなかった（ローカル変数でもモジュールでもない）
                    self.cx.diags.push(Diagnostic::error(
                        "E0208",
                        format!("undefined function `{n}.{}`", method.name),
                        span,
                    ));
                    return Ty::Error;
                }
                if let Some((k, _)) = kwargs.first() {
                    self.cx.diags.push(Diagnostic::error(
                        "E0205",
                        format!("`{}` takes no keyword arguments", method.name),
                        k.span,
                    ));
                    for (_, v) in kwargs {
                        self.expr(v, env);
                    }
                }
                // 受け手を式として検査する（変数なら「代入の前の使用」E0204 もここで出る）
                let rt = self.expr(receiver, env);
                match (rt, receiver.as_name()) {
                    (Ty::Error, _) => Ty::Error,
                    (Ty::PyObject, _) => self.py_method(&method.name, span, args, e.span, env),
                    (_, Some(n)) => self.method_call(n, &method.name, span, args, e.span, env),
                    _ => {
                        // `list` / `dict` のメソッドは受け手を書き換えるので、変数に対してだけ呼べる
                        self.cx.diags.push(
                            Diagnostic::error(
                                "E0905",
                                format!(
                                    "`{}` on `{}` can only be called on a variable",
                                    method.name,
                                    self.cx.name(rt)
                                ),
                                span,
                            )
                            .with_hint("assign the value to a variable first"),
                        );
                        for a in args {
                            self.expr(a, env);
                        }
                        Ty::Error
                    }
                }
            }
            ExprKind::Call {
                callee,
                type_args,
                args,
                kwargs,
            } if is_result_ctor(&callee.name) && kwargs.is_empty() && type_args.is_empty() => {
                self.result_ctor(callee, args, None, e.span, env)
            }
            ExprKind::Call {
                callee,
                type_args,
                args,
                kwargs,
            } => {
                let explicit = self.explicit_type_args(callee, type_args);
                if self.cx.aliases.contains_key(&callee.name)
                    || BUILTIN_RECORDS.contains(&callee.name.as_str())
                {
                    self.construct(callee, explicit.as_deref(), args, kwargs, e.span, env)
                } else if !kwargs.is_empty() {
                    self.check_effects(callee);
                    for a in args {
                        self.expr(a, env);
                    }
                    for (k, v) in kwargs {
                        self.expr(v, env);
                        self.cx.diags.push(
                            Diagnostic::error(
                                "E0216",
                                "keyword arguments are only supported when creating a record",
                                k.span,
                            )
                            .with_hint("pass the arguments by position"),
                        );
                    }
                    // 引数の個数や型の検査はしない（キーワード引数の誤りから連鎖させない）
                    match self.cx.sig_index.get(&callee.name) {
                        Some(&i) => self.cx.sigs[i].ret,
                        None => Ty::Error,
                    }
                } else {
                    self.call(callee, explicit.as_deref(), args, e.span, e.id, env)
                }
            }
            ExprKind::Field { base, name } => self.field(e, base, name, env),
            ExprKind::Try(inner) => self.try_expr(inner, e.span, env),
            ExprKind::Await(inner) => self.await_expr(inner, e.span, env),
            ExprKind::Is { .. } => unreachable!("handled by cond"),
        }
    }

    /// 演算子に使えない型への診断。`None` を取り除けば使える場合は E0301。
    fn operator_error(
        &mut self,
        desc: String,
        operands: &[Ty],
        ok: impl Fn(&[Ty]) -> bool,
        span: Span,
    ) {
        if operands
            .iter()
            .any(|t| self.cx.types.members(*t).contains(&Ty::Unknown))
        {
            self.cx.diags.push(Diagnostic::error("E0601", "this `Unknown` value has not been checked", span)
                    .with_cause("values from outside the program must be checked before use (LANG-006, ADR-0011)")
                    .with_hint("inspect it with `match`: `case int():`, `case str():`, ..., `case _:`"));
            return;
        }
        let stripped: Vec<Ty> = operands
            .iter()
            .map(|t| self.cx.types.without_none(*t))
            .collect();
        let maybe_none = operands.iter().any(|t| self.cx.types.contains_none(*t));
        if maybe_none && ok(&stripped) {
            self.cx.diags.push(
                Diagnostic::error("E0301", format!("{desc}: an operand may be `None`"), span)
                    .with_hint("check it first, e.g. `if x is not None:`"),
            );
        } else {
            self.cx.diags.push(
                Diagnostic::error("E0207", desc, span)
                    .with_cause("there are no implicit conversions between types"),
            );
        }
    }

    fn unary_result(&mut self, op: UnaryOp, t: Ty, span: Span) -> Ty {
        if t == Ty::Error {
            return Ty::Error;
        }
        let (want, sym) = match op {
            UnaryOp::Neg => (Ty::Int, "-"),
            UnaryOp::Not => (Ty::Bool, "not"),
        };
        if t == want || (op == UnaryOp::Neg && t == Ty::Float) {
            return t;
        }
        let desc = format!(
            "operator `{sym}` cannot be applied to `{}`",
            self.cx.name(t)
        );
        self.operator_error(desc, &[t], |ts| ts[0] == want, span);
        Ty::Error
    }

    /// 構造を持つ値の `==` / `!=`（ADR-0016 論点 11〜13）。決まらなければ `None` を返し、従来の規則と診断に任せる。
    fn equality(&mut self, op: BinOp, lt: Ty, rt: Ty, span: Span) -> Option<Ty> {
        if lt == Ty::None || rt == Ty::None {
            self.cx.diags.push(
                Diagnostic::error(
                    "E0207",
                    format!("operator `{}` cannot compare with `None`", op.symbol()),
                    span,
                )
                .with_hint(if op == BinOp::Eq {
                    "use `x is None` to check for `None`"
                } else {
                    "use `x is not None` to check for `None`"
                }),
            );
            return Some(Ty::Error);
        }
        if self.cx.types.members(lt).contains(&Ty::Unknown)
            || self.cx.types.members(rt).contains(&Ty::Unknown)
        {
            return None; // トップレベルの Unknown の E0601 は従来の経路で報告する
        }
        if self.cx.types.contains_unknown(lt) || self.cx.types.contains_unknown(rt) {
            // 構造の中の Unknown（`list[Unknown]` など）も E0601
            self.cx.diags.push(
                Diagnostic::error("E0601", "this `Unknown` value has not been checked", span)
                    .with_cause("values from outside the program must be checked before use (LANG-006, ADR-0011)")
                    .with_hint("inspect the values with `match` before comparing them"),
            );
            return Some(Ty::Error);
        }
        let types = &self.cx.types;
        // 片方の構成要素がすべて、もう片方の構成要素に含まれる（`o == 1`、`o: int | None`）
        let fits = |a: Ty, b: Ty| {
            let bm = types.members(b);
            types.members(a).iter().all(|m| bm.contains(m))
        };
        let related = lt == rt || fits(lt, rt) || fits(rt, lt);
        (related && types.comparable(lt) && types.comparable(rt)).then_some(Ty::Bool)
    }

    fn binary(&mut self, op: BinOp, lt: Ty, rt: Ty, span: Span) -> Ty {
        if lt == Ty::Error || rt == Ty::Error {
            return Ty::Error;
        }
        if op == BinOp::Div && lt == Ty::Int && rt == Ty::Int {
            // `/` は float だけ（ADR-0019 論点 4）。暗黙に float にしない
            self.cx.diags.push(
                Diagnostic::error(
                    "E0207",
                    "operator `/` cannot be applied to `int` and `int`",
                    span,
                )
                .with_cause("`/` divides floats; there are no implicit conversions")
                .with_hint("use `//` for integer (floor) division, or `float(a) / float(b)`"),
            );
            return Ty::Error;
        }
        if self.cx.types.contains_py(lt) || self.cx.types.contains_py(rt) {
            let place = if matches!(op, BinOp::Eq | BinOp::Ne) {
                "an operand of `==` / `!=`".to_string()
            } else {
                format!("an operand of `{}`", op.symbol())
            };
            self.cx.diags.push(py_value_use_error(&place, span));
            return Ty::Error;
        }
        if matches!(op, BinOp::FloorDiv | BinOp::Mod) && lt == Ty::Float && rt == Ty::Float {
            self.cx.diags.push(
                Diagnostic::error(
                    "E0207",
                    format!(
                        "operator `{}` cannot be applied to `float` and `float`",
                        op.symbol()
                    ),
                    span,
                )
                .with_cause("`//` and `%` work on `int` only (ADR-0019)")
                .with_hint("use `/` for floats, or convert with `int(x)` first"),
            );
            return Ty::Error;
        }
        if matches!((lt, rt), (Ty::Int, Ty::Float) | (Ty::Float, Ty::Int)) {
            // int と float を混ぜた演算（ADR-0019 論点 3）
            self.cx.diags.push(
                Diagnostic::error(
                    "E0207",
                    format!(
                        "operator `{}` cannot be applied to `{}` and `{}`",
                        op.symbol(),
                        self.cx.name(lt),
                        self.cx.name(rt)
                    ),
                    span,
                )
                .with_cause("there are no implicit conversions between `int` and `float`")
                .with_hint("convert explicitly with `float(n)` or `int(x)`"),
            );
            return Ty::Error;
        }
        if matches!(op, BinOp::Eq | BinOp::Ne)
            && let Some(t) = self.equality(op, lt, rt, span)
        {
            return t;
        }
        let rule = move |ts: &[Ty]| -> Option<Ty> {
            let (l, r) = (ts[0], ts[1]);
            match op {
                // 文字列の連結（M3 T2b）
                BinOp::Add if l == Ty::Str && r == Ty::Str => Some(Ty::Str),
                BinOp::Add | BinOp::Sub | BinOp::Mul => {
                    (l == r && matches!(l, Ty::Int | Ty::Float)).then_some(l)
                }
                BinOp::FloorDiv | BinOp::Mod => (l == Ty::Int && r == Ty::Int).then_some(Ty::Int),
                BinOp::Div => (l == Ty::Float && r == Ty::Float).then_some(Ty::Float),
                BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                    (l == r && matches!(l, Ty::Int | Ty::Float)).then_some(Ty::Bool)
                }
                BinOp::Eq | BinOp::Ne => (l == r
                    && matches!(l, Ty::Int | Ty::Bool | Ty::Str | Ty::Float))
                .then_some(Ty::Bool),
                BinOp::And | BinOp::Or => (l == Ty::Bool && r == Ty::Bool).then_some(Ty::Bool),
                // `in` は `in_expr` で検査する
                BinOp::In | BinOp::NotIn => None,
            }
        };
        if let Some(t) = rule(&[lt, rt]) {
            return t;
        }
        let desc = format!(
            "operator `{}` cannot be applied to `{}` and `{}`",
            op.symbol(),
            self.cx.name(lt),
            self.cx.name(rt)
        );
        self.operator_error(desc, &[lt, rt], move |ts| rule(ts).is_some(), span);
        Ty::Error
    }

    /// `py_value` に渡せる型（ADR-0018 論点 10）: `int` / `bool` / `str` / `float` / `None` / `PyObject` と、
    /// それらを要素に持つ `list` / `dict`、構成要素がすべて渡せる Union（M4 T7）。
    /// スキーマ型でなければ E1501（ADR-0022 論点 4 と訂正）。型パラメータを含む型も E1501。
    fn require_schema_type(&mut self, t: Ty, span: Span, what: &str) {
        let has_param = self.cx.types.contains_param(t);
        if !has_param && self.cx.types.is_schema_type(t, true) {
            return;
        }
        let found = self.cx.name(t);
        self.cx.diags.push(
            Diagnostic::error(
                "E1501",
                format!("`{found}` is not a schema type, so it cannot be {what}"),
                span,
            )
            .with_cause("schema types are `int`, `float`, `bool`, `str`, `Unknown`, and `list`, `dict[str, ..]`, records and unions of them (`None` only inside a union); a type parameter is not allowed (ADR-0022)")
            .with_hint("declare the JSON shape as a record, or use `Unknown` for parts whose shape is open"),
        );
    }

    fn require_py_convertible(&mut self, t: Ty, span: Span) {
        fn ok(ts: &Types, t: Ty) -> bool {
            match t {
                Ty::Int | Ty::Bool | Ty::Str | Ty::Float | Ty::None | Ty::PyObject | Ty::Error => {
                    true
                }
                Ty::List(l) => ok(ts, ts.list_elem(l)),
                Ty::Dict(d) => {
                    let (k, v) = ts.dict_args(d);
                    ok(ts, k) && ok(ts, v)
                }
                Ty::Union(_) => ts.members(t).into_iter().all(|m| ok(ts, m)),
                _ => false,
            }
        }
        if ok(&self.cx.types, t) {
            return;
        }
        let found = self.cx.name(t);
        self.cx.diags.push(
            Diagnostic::error(
                "E1201",
                format!("`py_value` cannot convert `{found}` to a Python object"),
                span,
            )
            .with_cause("`py_value` accepts `int`, `bool`, `str`, `float`, `None`, `PyObject`, and `list`, `dict` and unions of them (ADR-0018)")
            .with_hint("pass the fields one by one, or convert the value to one of these types first"),
        );
    }

    /// `PyObject` のメソッド（ADR-0018 論点 9）。すべて Effect `python` を持ち、失敗し得るものは `Result[.., PyError]`。
    fn py_method(&mut self, m: &str, mspan: Span, args: &[Expr], span: Span, env: &Env) -> Ty {
        let err = self.cx.types.py_error();
        let list_of_py = self.cx.types.list(Ty::PyObject);
        let dict_of_py = self.cx.types.dict(Ty::Str, Ty::PyObject);
        let (params, ok): (Vec<Ty>, Option<Ty>) = match m {
            "attr" => (vec![Ty::Str], Some(Ty::PyObject)),
            "call" => (vec![list_of_py], Some(Ty::PyObject)),
            "call_kw" => (vec![list_of_py, dict_of_py], Some(Ty::PyObject)),
            "to_int" => (vec![], Some(Ty::Int)),
            "to_bool" => (vec![], Some(Ty::Bool)),
            "to_str" => (vec![], Some(Ty::Str)),
            "to_float" => (vec![], Some(Ty::Float)),
            "to_list" => (vec![], Some(list_of_py)),
            "to_dict" => (vec![], Some(dict_of_py)),
            "is_none" => (vec![], None),
            _ => {
                for a in args {
                    self.expr(a, env);
                }
                self.cx.diags.push(
                    Diagnostic::error("E0905", format!("`PyObject` has no method `{m}`"), mspan)
                        .with_cause("the methods of `PyObject` are `attr`, `call`, `call_kw`, `to_int`, `to_bool`, `to_str`, `to_float`, `to_list`, `to_dict` and `is_none` (ADR-0018)"),
                );
                return Ty::Error;
            }
        };
        // 未知のメソッド（E0905）では Effect を記録しない
        self.check_effects(&Ident {
            name: format!("PyObject.{m}"),
            span: mspan,
        });
        if args.len() != params.len() {
            for a in args {
                self.expr(a, env);
            }
            self.cx.diags.push(Diagnostic::error(
                "E0205",
                format!(
                    "`{m}` takes {} argument(s) but {} were given",
                    params.len(),
                    args.len()
                ),
                span,
            ));
            return Ty::Error;
        }
        for (a, &pt) in args.iter().zip(&params) {
            let at = self.expr_hint(a, env, Some(pt));
            let ctx = format!("argument of `{m}`");
            self.cx.require(at, pt, a.span, &ctx, None);
        }
        match ok {
            Some(t) => self.cx.types.result(t, err),
            None => Ty::Bool,
        }
    }

    /// `print` の引数は `int` / `bool` / `str`、レコード、`list`、`dict`、`Result` と、それらの Union（ADR-0016 論点 15）。
    /// トップレベルの `None` は E0301、構造の中の `Unknown` は E0601、型パラメータは E0201。
    /// 検査のために Union 型を登録しない（使われない Union が IR に入らないようにする）。
    fn require_printable(&mut self, t: Ty, span: Span) {
        if t == Ty::Error {
            return;
        }
        if self.cx.types.contains_py(t) {
            self.cx
                .diags
                .push(py_value_use_error("an argument of `print`", span));
            return;
        }
        if self.cx.types.members(t).contains(&Ty::Unknown) {
            self.cx.diags.push(Diagnostic::error("E0601", "this `Unknown` value has not been checked", span)
                    .with_cause("values from outside the program must be checked before use (LANG-006, ADR-0011)")
                    .with_hint("inspect it with `match`: `case int():`, `case str():`, ..., `case _:`"));
            return;
        }
        if self.cx.types.contains_unknown(t) {
            // 構造の中の Unknown（`list[Unknown]` など）も E0601（ADR-0016 論点 15）
            self.cx.diags.push(Diagnostic::error("E0601", "this `Unknown` value has not been checked", span)
                    .with_cause("values from outside the program must be checked before use (LANG-006, ADR-0011)")
                    .with_hint("check the values with `match` before putting them into a structure"));
            return;
        }
        // 構成要素がすべて表示できればよい（ADR-0016 論点 15）。構造の中の `None` は表示できるが、
        // トップレベルの `None` は検査のし忘れとして E0301。
        let members = self.cx.types.members(t);
        let printable = |m: &Ty| *m != Ty::None && self.cx.types.comparable(*m);
        if members.iter().all(printable) {
            return;
        }
        const ACCEPTS: &str = "`print` accepts `int`, `bool`, `str`, `float`, records, `list`, `dict`, `Result`, and unions of them";
        let found = self.cx.name(t);
        let d = if members.iter().all(|m| printable(m) || *m == Ty::None) {
            Diagnostic::error(
                "E0301",
                format!("this value may be `None`: `print` cannot print `{found}`"),
                span,
            )
            .with_cause(ACCEPTS)
            .with_hint("check it first, e.g. `if x is not None:`")
        } else {
            Diagnostic::error(
                "E0201",
                format!("mismatched types: `print` cannot print `{found}`"),
                span,
            )
            .with_cause(ACCEPTS)
        };
        self.cx.diags.push(d);
    }

    /// `Point(x=1, y=2)`: レコードの作成。フィールドはすべてキーワード引数でちょうど1回ずつ指定する。
    /// Generic な `type`（`Pair(first=1, second="a")`）なら、キーワード引数の型から型引数を推論する。
    fn construct(
        &mut self,
        callee: &Ident,
        explicit: Option<&[Ty]>,
        args: &[Expr],
        kwargs: &[(Ident, Expr)],
        span: Span,
        env: &Env,
    ) -> Ty {
        for a in args {
            self.expr(a, env);
        }
        // 型引数を明示した Generic なレコードなら、それを入れたフィールドの型を値の手掛かりにする
        // （`Box[list[int]](value=[])`、`Box[Result[int, str]](value=Ok(1))`）
        let hints: Vec<Option<Ty>> = match (explicit, self.cx.generics.get(&callee.name).cloned()) {
            (Some(ts), Some((params, Ty::Record(r)))) => {
                let map: Subst = params.iter().copied().zip(ts.iter().copied()).collect();
                kwargs
                    .iter()
                    .map(|(k, _)| {
                        let ft = self.cx.types.record_def(r).field(&k.name).map(|(_, t)| t);
                        ft.map(|t| self.cx.types.subst(t, &map))
                    })
                    .collect()
            }
            // Generic でないレコード（組み込みのレコードを含む）は、フィールドの型そのもの（`MockResponse(tool_calls=[])`）
            (_, None) => {
                let rt = match self.cx.types.builtin_record(&callee.name) {
                    Some(t) => t,
                    None if self.cx.aliases.contains_key(&callee.name) => {
                        self.cx.resolve_alias(&callee.name)
                    }
                    None => Ty::Error,
                };
                match rt {
                    Ty::Record(r) => kwargs
                        .iter()
                        .map(|(k, _)| self.cx.types.record_def(r).field(&k.name).map(|(_, t)| t))
                        .collect(),
                    _ => vec![None; kwargs.len()],
                }
            }
            _ => vec![None; kwargs.len()],
        };
        let kw_tys: Vec<Ty> = kwargs
            .iter()
            .zip(hints)
            .map(|((_, v), h)| self.expr_hint(v, env, h))
            .collect();
        let ty = match self.cx.generics.get(&callee.name).cloned() {
            Some((_, Ty::Error)) => Ty::Error,
            Some((params, body)) => {
                let mut map: Subst = explicit
                    .map(|ts| params.iter().copied().zip(ts.iter().copied()).collect())
                    .unwrap_or_default();
                if let Ty::Record(r) = body {
                    for ((k, _), &kt) in kwargs.iter().zip(&kw_tys) {
                        if let Some((_, ft)) = self.cx.types.record_def(r).field(&k.name) {
                            self.unify(ft, kt, &params, &mut map);
                        }
                    }
                }
                let what = format!("record `{}`", callee.name);
                self.require_inferred(&params, &mut map, &what, span);
                let ty = self.cx.types.subst(body, &map);
                if let Some(slot) = self.cx.types.none_slot(ty) {
                    self.cx.diags.push(
                        none_type_arg_error(&slot, span).with_hint(
                            "the type argument is inferred from the field values: pass a value of an optional type, e.g. `v: int | None = None`",
                        ),
                    );
                    return Ty::Error;
                }
                ty
            }
            None => match self.cx.types.builtin_record(&callee.name) {
                Some(t) => t,
                None => self.cx.resolve_alias(&callee.name),
            },
        };
        let id = match ty {
            Ty::Record(id) => id,
            Ty::Error => return Ty::Error,
            other => {
                let msg = format!(
                    "type `{}` is `{}`, not a record, so it cannot be called",
                    callee.name,
                    self.cx.name(other)
                );
                self.cx
                    .diags
                    .push(Diagnostic::error("E0208", msg, callee.span));
                return Ty::Error;
            }
        };
        let fields = self.cx.types.record_def(id).fields.clone();
        if let Some(a) = args.first() {
            let example = fields
                .iter()
                .map(|(n, _)| format!("{n}=..."))
                .collect::<Vec<_>>()
                .join(", ");
            self.cx.diags.push(
                Diagnostic::error(
                    "E0505",
                    format!(
                        "record `{}` must be created with keyword arguments",
                        callee.name
                    ),
                    a.span,
                )
                .with_hint(format!("write `{}({example})`", callee.name)),
            );
        }
        let mut seen = HashSet::new();
        for ((k, v), &vt) in kwargs.iter().zip(&kw_tys) {
            let Some(&(_, ft)) = fields.iter().find(|(n, _)| *n == k.name) else {
                let msg = format!("record `{}` has no field `{}`", callee.name, k.name);
                self.cx.diags.push(Diagnostic::error("E0501", msg, k.span));
                continue;
            };
            if !seen.insert(k.name.clone()) {
                self.cx.diags.push(Diagnostic::error(
                    "E0503",
                    format!("field `{}` is given more than once", k.name),
                    k.span,
                ));
                continue;
            }
            let ctx = format!("field `{}` of `{}`", k.name, callee.name);
            self.cx.require(vt, ft, v.span, &ctx, None);
        }
        let missing: Vec<&str> = fields
            .iter()
            .filter(|(n, _)| !seen.contains(n))
            .map(|(n, _)| n.as_str())
            .collect();
        if !missing.is_empty() && args.is_empty() {
            self.cx.diags.push(Diagnostic::error(
                "E0502",
                format!(
                    "missing field{} `{}` for record `{}`",
                    if missing.len() == 1 { "" } else { "s" },
                    missing.join("`, `"),
                    callee.name
                ),
                span,
            ));
        }
        Ty::Record(id)
    }

    /// `p.x`: レコードのフィールド参照。
    /// 絞り込みの対象になる経路（ADR-0016 論点 1）: ローカル変数か引数を根とし、フィールド参照だけを連ねたもの。
    /// 環境のキーは `p` / `p.x` / `a.b.c`（識別子に `.` は使えないので、変数と衝突しない）。
    fn path_key(&self, e: &Expr) -> Option<String> {
        match &e.kind {
            ExprKind::Name(n) if self.vars.contains_key(n) => Some(n.clone()),
            ExprKind::Field { base, name } => {
                self.path_key(base).map(|b| format!("{b}.{}", name.name))
            }
            _ => None,
        }
    }

    /// 経路と、その宣言された型（変数なら宣言型、フィールドならレコード定義上の型）。式は検査済みであること。
    fn path(&self, e: &Expr) -> Option<(String, Ty)> {
        let key = self.path_key(e)?;
        let declared = match &e.kind {
            ExprKind::Name(n) => self.vars.get(n)?.ty,
            _ => *self.field_decl.get(&e.id)?,
        };
        Some((key, declared))
    }

    /// 経路 `root` に代入したので、`root.` で始まる経路の絞り込みを捨てる（ADR-0016 論点 4）。
    fn forget_paths(env: &mut Env, root: &str) {
        let prefix = format!("{root}.");
        env.retain(|k, _| !k.starts_with(&prefix));
    }

    fn field(&mut self, e: &Expr, base: &Expr, name: &Ident, env: &Env) -> Ty {
        let bt = self.expr(base, env);
        match bt {
            Ty::Error => Ty::Error,
            Ty::Unknown => {
                self.cx.diags.push(Diagnostic::error("E0601", "this `Unknown` value has not been checked", base.span)
                    .with_cause("values from outside the program must be checked before use (LANG-006, ADR-0011)")
                    .with_hint("inspect it with `match`: `case int():`, `case str():`, ..., `case _:`"));
                Ty::Error
            }
            Ty::Record(id) => match self.cx.types.record_def(id).field(&name.name) {
                Some((_, t)) => {
                    self.field_decl.insert(e.id, t);
                    // 不変レコードのフィールドは、根が再代入されない限り絞り込みを保てる（ADR-0016）
                    let Some(key) = self.path_key(e) else {
                        return t;
                    };
                    match env.get(&key) {
                        Some(&n) if n != t && n != Ty::Error && t != Ty::Error => {
                            self.narrowings.insert(
                                e.id,
                                Narrowing {
                                    name: key,
                                    span: e.span,
                                    declared: t,
                                    narrowed: n,
                                },
                            );
                            n
                        }
                        _ => {
                            self.narrowings.remove(&e.id);
                            t
                        }
                    }
                }
                None => {
                    let msg = format!("`{}` has no field `{}`", self.cx.name(bt), name.name);
                    self.cx
                        .diags
                        .push(Diagnostic::error("E0501", msg, name.span));
                    Ty::Error
                }
            },
            t => {
                let rest = self.cx.types.without_none(t);
                let d = if self.cx.types.contains_none(t)
                    && matches!(rest, Ty::Record(id) if self.cx.types.record_def(id).field(&name.name).is_some())
                {
                    Diagnostic::error(
                        "E0301",
                        format!(
                            "this value may be `None`: cannot read field `{}` of `{}`",
                            name.name,
                            self.cx.name(t)
                        ),
                        base.span,
                    )
                    .with_hint("check it first, e.g. `if x is not None:`")
                } else {
                    Diagnostic::error(
                        "E0501",
                        format!("`{}` has no field `{}`", self.cx.name(t), name.name),
                        name.span,
                    )
                };
                self.cx.diags.push(d);
                Ty::Error
            }
        }
    }

    /// 呼び出しの引数から、Generic 関数の型パラメータを推論する。
    /// `pattern` は仮引数の型（`own` の型パラメータを含む）、`actual` は実引数の型。最初に決まった型を採用し、
    /// 矛盾は推論後の型検査（`require`）で報告する。
    fn unify(&mut self, pattern: Ty, actual: Ty, own: &[ParamId], map: &mut Subst) {
        if actual == Ty::Error {
            return;
        }
        match pattern {
            Ty::Param(p) if own.contains(&p) => {
                map.entry(p).or_insert(actual);
            }
            Ty::Union(_) => {
                // `T | None` に `str | None` を渡したら `T = str`（パターン側の具体的な構成要素を除いた残り）
                let ms = self.cx.types.members(pattern);
                let is_own = |m: &Ty| matches!(m, Ty::Param(p) if own.contains(p));
                let ps: Vec<Ty> = ms.iter().filter(|m| is_own(m)).copied().collect();
                if let [Ty::Param(p)] = ps.as_slice()
                    && !map.contains_key(p)
                {
                    let concrete: Vec<Ty> = ms.iter().filter(|m| !is_own(m)).copied().collect();
                    let rest: Vec<Ty> = self
                        .cx
                        .types
                        .members(actual)
                        .into_iter()
                        .filter(|a| !concrete.iter().any(|c| self.cx.types.member_fits(*a, *c)))
                        .collect();
                    if !rest.is_empty() {
                        let t = self.cx.types.union(rest);
                        map.insert(*p, t);
                    }
                }
            }
            Ty::List(pl) => {
                if let Ty::List(al) = actual {
                    let (pe, ae) = (self.cx.types.list_elem(pl), self.cx.types.list_elem(al));
                    self.unify(pe, ae, own, map);
                }
            }
            Ty::Task(pk) => {
                if let Ty::Task(ak) = actual {
                    let (pe, ae) = (self.cx.types.task_elem(pk), self.cx.types.task_elem(ak));
                    self.unify(pe, ae, own, map);
                }
            }
            Ty::Dict(pd) => {
                if let Ty::Dict(ad) = actual {
                    let (pk, pv) = self.cx.types.dict_args(pd);
                    let (ak, av) = self.cx.types.dict_args(ad);
                    self.unify(pk, ak, own, map);
                    self.unify(pv, av, own, map);
                }
            }
            Ty::Result(pr) => {
                if let Ty::Result(ar) = actual {
                    let (po, pe) = self.cx.types.result_args(pr);
                    let (ao, ae) = self.cx.types.result_args(ar);
                    self.unify(po, ao, own, map);
                    self.unify(pe, ae, own, map);
                }
            }
            Ty::Record(r) => {
                if let Ty::Record(a) = actual {
                    let fields = self.cx.types.record_def(r).fields.clone();
                    for (n, ft) in fields {
                        if let Some((_, at)) = self.cx.types.record_def(a).field(&n) {
                            self.unify(ft, at, own, map);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// 呼び出しに明示した型引数（`f[T](x)`、ADR-0021 論点 9）を解決する。書いていなければ `None`。
    /// Generic でない関数・組み込み・レコードに書いたら E1502、個数が違えば E0402（どちらも `None` を返し、推論に任せる）。
    fn explicit_type_args(&mut self, callee: &Ident, type_args: &[TypeExpr]) -> Option<Vec<Ty>> {
        let first = type_args.first()?;
        let tys: Vec<Ty> = type_args.iter().map(|t| self.cx.resolve(t)).collect();
        let span = Span::new(
            first.span.start,
            type_args.last().expect("non-empty").span.end,
        );
        let expected = if let Some((params, _)) = self.cx.generics.get(&callee.name) {
            Some(params.len())
        } else if matches!(callee.name.as_str(), "validate" | "llm_generate") {
            // 組み込みの Generic（ADR-0022 論点 6・8）
            Some(1)
        } else if self.cx.aliases.contains_key(&callee.name) {
            None
        } else {
            self.cx
                .sig_index
                .get(&callee.name)
                .map(|&i| self.cx.sigs[i].type_params.len())
                .filter(|&n| n > 0)
        };
        let Some(n) = expected else {
            self.cx.diags.push(
                Diagnostic::error(
                    "E1502",
                    format!(
                        "`{}` is not generic, so it takes no type arguments",
                        callee.name
                    ),
                    span,
                )
                .with_hint(format!("remove the type arguments: `{}(...)`", callee.name)),
            );
            return None;
        };
        if n != tys.len() {
            self.cx.diags.push(Diagnostic::error(
                "E0402",
                format!(
                    "`{}` takes {n} type argument(s) but {} were given",
                    callee.name,
                    tys.len()
                ),
                span,
            ));
            return self.erroneous_type_args(callee, n);
        }
        if tys.contains(&Ty::Error) {
            return self.erroneous_type_args(callee, n);
        }
        Some(tys)
    }

    /// 型引数の誤りを報告した後の値。組み込みの Generic（`validate`）は推論で補えないので、`Error` の型引数にして
    /// 「型引数がない」（E0403）を重ねて出さない。利用者の Generic は従来どおり推論に任せる
    fn erroneous_type_args(&self, callee: &Ident, n: usize) -> Option<Vec<Ty>> {
        matches!(callee.name.as_str(), "validate" | "llm_generate").then(|| vec![Ty::Error; n])
    }

    /// 推論できなかった型パラメータを報告し、`Error` で埋める。
    fn require_inferred(&mut self, params: &[ParamId], map: &mut Subst, what: &str, span: Span) {
        let missing: Vec<String> = params
            .iter()
            .filter(|p| !map.contains_key(p))
            .map(|p| self.cx.name(Ty::Param(*p)))
            .collect();
        if !missing.is_empty() {
            self.cx.diags.push(
                Diagnostic::error(
                    "E0403",
                    format!(
                        "cannot infer type parameter `{}` of {what}",
                        missing.join("`, `")
                    ),
                    span,
                )
                .with_hint(
                    "pass an argument whose type determines it (a bare `None` does not), or write the type arguments, e.g. `f[int](...)`",
                ),
            );
            for p in params {
                map.entry(*p).or_insert(Ty::Error);
            }
        }
    }

    /// 呼び出しの Effect を記録し、呼び出し元に `uses` があれば検査する（ADR-0014 論点 5）。
    fn check_effects(&mut self, callee: &Ident) {
        let effects = match builtin_effects(&callee.name) {
            Some(e) => Some(e),
            None => match self.cx.sig_index.get(&callee.name) {
                Some(&i) => self.cx.sigs[i].effects,
                // 未定義の関数は E0208 で報告済み
                None => return,
            },
        };
        self.effect_sites.push(EffectSite {
            callee: callee.name.clone(),
            span: callee.span,
            effects,
        });
        let Some((declared, _)) = &self.effects else {
            return;
        };
        if callee.name == self.fn_name {
            return;
        }
        let callee_span = self
            .cx
            .sig_index
            .get(&callee.name)
            .map(|&i| self.cx.sigs[i].span);
        let Some(effects) = effects else {
            self.cx.diags.push(
                Diagnostic::error(
                    "E1103",
                    format!(
                        "`{}` declares its effects but calls `{}`, which does not",
                        self.fn_name, callee.name
                    ),
                    callee.span,
                )
                .with_cause("only built-in functions and functions with a `uses` clause can be called from a function with a `uses` clause")
                .with_hint(format!(
                    "add `uses ...` to `{}` (or `uses pure` if it has no effects)",
                    callee.name
                ))
                .with_related(RelatedKind::Function, &callee.name, callee_span),
            );
            return;
        };
        let missing = effects.missing_from(*declared);
        if missing.is_empty() {
            return;
        }
        let names: Vec<String> = missing.iter().map(|e| format!("`{}`", e.name())).collect();
        let declared_text = if declared.iter().is_empty() {
            "pure".to_string()
        } else {
            declared.names().join(", ")
        };
        let mut d = Diagnostic::error(
            "E1102",
            format!(
                "`{}` calls `{}`, which has {} {}, but declares only `uses {declared_text}`",
                self.fn_name,
                callee.name,
                if names.len() == 1 {
                    "effect"
                } else {
                    "effects"
                },
                names.join(", ")
            ),
            callee.span,
        );
        if callee_span.is_some() {
            d = d.with_related(RelatedKind::Function, &callee.name, callee_span);
        }
        // 修正案は関数の検査を終えてから、足りない Effect の和集合で付ける（attach_effect_fix）
        self.missing_effects.0.push(self.cx.diags.len());
        for e in missing {
            self.missing_effects.1.insert(e);
        }
        self.cx.diags.push(d);
    }

    /// 関数の E1102 すべてに、同じ修正案（足りない Effect の和集合を `uses` 節に足す）を付ける。
    /// どれを適用しても結果が同じになり、1回で全部の E1102 が消える。
    fn attach_effect_fix(&mut self) {
        let (indices, missing) = std::mem::take(&mut self.missing_effects);
        let Some((_, clause)) = &self.effects else {
            return;
        };
        if indices.is_empty() {
            return;
        }
        let names = missing.names().join(", ");
        let hint = match clause.names.iter().find(|n| n.name == "pure") {
            Some(p) => Hint {
                message: format!("declare `uses {names}`"),
                replacement: Some((p.span, names.clone())),
            },
            None => Hint {
                message: format!("add {names} to the `uses` clause"),
                replacement: Some((
                    Span::new(clause.span.end, clause.span.end),
                    format!(", {names}"),
                )),
            },
        };
        for i in indices {
            self.cx.diags[i].hints.push(hint.clone());
        }
    }

    /// `async def` の関数（と `sleep_ms`）の呼び出しか（ADR-0020 論点 6）。
    fn is_async_call(&self, e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Call { callee, .. } => {
                is_async_builtin(&callee.name)
                    || self
                        .cx
                        .sig_index
                        .get(&callee.name)
                        .is_some_and(|&i| self.cx.sigs[i].is_async)
            }
            _ => false,
        }
    }

    /// `await` / `spawn` を `async def` の外で使っている（E1301）。
    fn outside_async(&mut self, what: &str, span: Span) {
        let mut d = Diagnostic::error(
            "E1301",
            format!("`{what}` can only be used inside an `async def` function"),
            span,
        )
        .with_related(RelatedKind::Function, &self.fn_name, Some(self.fn_span));
        d = if self.fn_name == "main" {
            d.with_hint("declare it as `async def main() -> None:`")
        } else {
            d.with_hint(format!("declare `{}` with `async def`", self.fn_name))
        };
        self.cx.diags.push(d);
    }

    /// `await operand`（ADR-0020 論点 5）: `async def` の呼び出しならその戻り値、`Task[T]` なら `T`。
    fn await_expr(&mut self, inner: &Expr, span: Span, env: &Env) -> Ty {
        if !self.is_async {
            self.outside_async("await", span);
        }
        if self.is_async_call(inner) {
            self.allow_async_call = true;
            return self.expr(inner, env);
        }
        let t = self.expr(inner, env);
        match t {
            Ty::Task(k) => self.cx.types.task_elem(k),
            Ty::Error => Ty::Error,
            _ => {
                let name = self.cx.name(t);
                let mut d = Diagnostic::error(
                    "E1302",
                    format!(
                        "`await` needs a `Task` or a call of an `async def` function, found `{name}`"
                    ),
                    inner.span,
                );
                d = if matches!(
                    inner.kind,
                    ExprKind::Call { .. } | ExprKind::MethodCall { .. }
                ) {
                    d.with_hint("this function is not `async`; remove `await` to call it directly")
                } else {
                    d.with_hint(
                        "create a task with `spawn(f(...))`, or call an `async def` function",
                    )
                };
                self.cx.diags.push(d);
                Ty::Error
            }
        }
    }

    /// `spawn(f(args))`（ADR-0020 論点 8）: `Task[T]`（`T` は `f` の戻り値の型）。
    fn spawn(&mut self, args: &[Expr], span: Span, env: &Env) -> Ty {
        if !self.is_async {
            self.outside_async("spawn", span);
        }
        if args.len() != 1 {
            for a in args {
                self.allow_async_call = true;
                self.expr(a, env);
            }
            self.allow_async_call = false;
            self.cx.diags.push(
                Diagnostic::error(
                    "E1304",
                    format!("`spawn` takes 1 call of an `async def` function but {} argument(s) were given", args.len()),
                    span,
                )
                .with_hint("spawn each call separately: `t = spawn(f(...))`"),
            );
            return Ty::Error;
        }
        let a = &args[0];
        let is_sleep =
            matches!(&a.kind, ExprKind::Call { callee, .. } if is_async_builtin(&callee.name));
        if self.is_async_call(a) && !is_sleep {
            self.allow_async_call = true;
            let t = self.expr(a, env);
            return self.cx.types.task(t);
        }
        self.allow_async_call = true;
        let t = self.expr(a, env);
        self.allow_async_call = false;
        if t != Ty::Error {
            let what = match &a.kind {
                // `sleep_ms` は組み込みの中断点で、タスクにはできない（`await sleep_ms(n)` と書く）
                ExprKind::Call { callee, .. } if is_async_builtin(&callee.name) => {
                    format!(
                        "`{n}` cannot run as a task; write `await {n}(...)`, or spawn an `async def` function that calls it",
                        n = callee.name
                    )
                }
                ExprKind::Call { callee, .. } => {
                    format!("`{}` is not an `async def` function", callee.name)
                }
                ExprKind::Await(_) => "the argument is already awaited".into(),
                _ => format!("the argument is a value of type `{}`", self.cx.name(t)),
            };
            self.cx.diags.push(
                Diagnostic::error(
                    "E1304",
                    "the argument of `spawn` must be a call of an `async def` function",
                    a.span,
                )
                .with_cause(what)
                .with_hint("write `spawn(f(...))` where `f` is declared with `async def`"),
            );
        }
        Ty::Error
    }

    fn call(
        &mut self,
        callee: &Ident,
        explicit: Option<&[Ty]>,
        args: &[Expr],
        span: Span,
        id: ExprId,
        env: &Env,
    ) -> Ty {
        // `await` の直下・`spawn` の引数であることは、この呼び出しだけに効く（引数の呼び出しには引き継がない）
        let allowed = std::mem::take(&mut self.allow_async_call);
        if callee.name == "spawn" {
            self.check_effects(callee);
            return self.spawn(args, span, env);
        }
        let is_async = is_async_builtin(&callee.name)
            || self
                .cx
                .sig_index
                .get(&callee.name)
                .is_some_and(|&i| self.cx.sigs[i].is_async);
        if is_async && !allowed {
            self.cx.diags.push(
                Diagnostic::error(
                    "E1303",
                    format!("`{}` is an `async` function; its call must be awaited or spawned", callee.name),
                    span,
                )
                .with_hint(format!(
                    "write `await {name}(...)` to wait for it, or `spawn({name}(...))` to run it as a task",
                    name = callee.name
                )),
            );
        }
        self.check_effects(callee);
        // Generic でない関数なら、仮引数の型を手掛かりとして渡す（`f([])` のため）
        let hints: Vec<Option<Ty>> = match self.cx.sig_index.get(&callee.name) {
            Some(&i)
                if self.cx.sigs[i].type_params.is_empty()
                    && self.cx.sigs[i].params.len() == args.len() =>
            {
                self.cx.sigs[i].params.iter().map(|t| Some(*t)).collect()
            }
            // 型引数を明示した Generic 関数なら、それを入れた仮引数の型（`f[int]([])`）
            Some(&i) if explicit.is_some() && self.cx.sigs[i].params.len() == args.len() => {
                let sig = &self.cx.sigs[i];
                let map: Subst = sig
                    .type_params
                    .iter()
                    .copied()
                    .zip(explicit.unwrap_or_default().iter().copied())
                    .collect();
                let params = sig.params.clone();
                params
                    .iter()
                    .map(|t| Some(self.cx.types.subst(*t, &map)))
                    .collect()
            }
            _ => vec![None; args.len()],
        };
        let arg_tys: Vec<Ty> = args
            .iter()
            .zip(hints)
            .map(|(a, h)| self.expr_hint(a, env, h))
            .collect();
        if callee.name == "range" {
            self.cx.diags.push(
                Diagnostic::error("E0902", "`range` can only be used in a `for` loop", span)
                    .with_hint("write `for i in range(n):`"),
            );
            return Ty::Error;
        }
        if callee.name == "len"
            && args.len() == 1
            && matches!(arg_tys[0], Ty::List(_) | Ty::Dict(_))
        {
            return Ty::Int;
        }
        match callee.name.as_str() {
            // Python（ADR-0018 論点 9・10）
            "py_import" => {
                if args.len() != 1 {
                    self.cx.diags.push(Diagnostic::error(
                        "E0205",
                        format!("`py_import` takes 1 argument but {} were given", args.len()),
                        span,
                    ));
                } else {
                    self.cx.require(
                        arg_tys[0],
                        Ty::Str,
                        args[0].span,
                        "argument of `py_import`",
                        None,
                    );
                }
                let err = self.cx.types.py_error();
                return self.cx.types.result(Ty::PyObject, err);
            }
            "validate" => {
                // validate[T](u: Unknown) -> Result[T, SchemaError]（ADR-0022 論点 6）
                let err = self.cx.types.schema_error();
                let Some(&[t]) = explicit else {
                    if explicit.is_none() {
                        self.cx.diags.push(
                            Diagnostic::error(
                                "E0403",
                                "cannot infer the type parameter of `validate`",
                                span,
                            )
                            .with_hint(
                                "write the type to validate against, e.g. `validate[Plan](u)`",
                            ),
                        );
                    }
                    return Ty::Error;
                };
                if args.len() != 1 {
                    self.cx.diags.push(Diagnostic::error(
                        "E0205",
                        format!("`validate` takes 1 argument but {} were given", args.len()),
                        span,
                    ));
                } else {
                    self.cx.require(
                        arg_tys[0],
                        Ty::Unknown,
                        args[0].span,
                        "argument of `validate`",
                        None,
                    );
                }
                self.require_schema_type(t, span, "the type argument of `validate`");
                self.cx.call_type_args.insert(id, vec![t]);
                return self.cx.types.result(t, err);
            }
            "llm_generate" => {
                // llm_generate[T](req: LlmRequest) -> Result[T, LlmError]（ADR-0022 論点 8）
                let err = self.cx.types.llm_error();
                let req_t = self.cx.types.llm_request();
                let Some(&[t]) = explicit else {
                    if explicit.is_none() {
                        self.cx.diags.push(
                            Diagnostic::error(
                                "E0403",
                                "cannot infer the type parameter of `llm_generate`",
                                span,
                            )
                            .with_hint("write the type to generate, e.g. `llm_generate[Plan](req)` or `llm_generate[str](req)`"),
                        );
                    }
                    return Ty::Error;
                };
                if args.len() != 1 {
                    self.cx.diags.push(Diagnostic::error(
                        "E0205",
                        format!(
                            "`llm_generate` takes 1 argument but {} were given",
                            args.len()
                        ),
                        span,
                    ));
                } else {
                    self.cx.require(
                        arg_tys[0],
                        req_t,
                        args[0].span,
                        "argument of `llm_generate`",
                        None,
                    );
                }
                self.require_schema_type(t, span, "the type argument of `llm_generate`");
                self.cx.call_type_args.insert(id, vec![t]);
                return self.cx.types.result(t, err);
            }
            "llm_mock_push" => {
                let mr = self.cx.types.mock_response();
                if args.len() != 1 {
                    self.cx.diags.push(Diagnostic::error(
                        "E0205",
                        format!(
                            "`llm_mock_push` takes 1 argument but {} were given",
                            args.len()
                        ),
                        span,
                    ));
                } else {
                    self.cx.require(
                        arg_tys[0],
                        mr,
                        args[0].span,
                        "argument of `llm_mock_push`",
                        None,
                    );
                }
                return Ty::None;
            }
            "audit_events" => {
                if !args.is_empty() {
                    self.cx.diags.push(Diagnostic::error(
                        "E0205",
                        format!(
                            "`audit_events` takes no arguments but {} were given",
                            args.len()
                        ),
                        span,
                    ));
                }
                let ev = self.cx.types.audit_event();
                return self.cx.types.list(ev);
            }
            "llm_usage" => {
                if !args.is_empty() {
                    self.cx.diags.push(Diagnostic::error(
                        "E0205",
                        format!(
                            "`llm_usage` takes no arguments but {} were given",
                            args.len()
                        ),
                        span,
                    ));
                }
                return self.cx.types.llm_usage();
            }
            "to_json" => {
                if args.len() != 1 {
                    self.cx.diags.push(Diagnostic::error(
                        "E0205",
                        format!("`to_json` takes 1 argument but {} were given", args.len()),
                        span,
                    ));
                } else {
                    self.require_schema_type(arg_tys[0], args[0].span, "the argument of `to_json`");
                }
                return Ty::Str;
            }
            "py_value" => {
                if args.len() != 1 {
                    self.cx.diags.push(Diagnostic::error(
                        "E0205",
                        format!("`py_value` takes 1 argument but {} were given", args.len()),
                        span,
                    ));
                } else {
                    self.require_py_convertible(arg_tys[0], args[0].span);
                }
                return Ty::PyObject;
            }
            "print" => {
                if args.len() != 1 {
                    self.cx.diags.push(Diagnostic::error(
                        "E0205",
                        format!("`print` takes 1 argument but {} were given", args.len()),
                        span,
                    ));
                } else {
                    self.require_printable(arg_tys[0], args[0].span);
                }
                return Ty::None;
            }
            "arg_count" | "arg" | "parse_scalar" | "parse_json" | "len" | "float" | "int"
            | "sleep_ms" => {
                // 外部入力の入口（ADR-0011）、文字列の長さ（M3 T2b）、数値の変換（ADR-0019 論点 3）
                let (params, ret): (&[Ty], Ty) = match callee.name.as_str() {
                    "arg_count" => (&[], Ty::Int),
                    "len" => (&[Ty::Str], Ty::Int),
                    "float" => (&[Ty::Int], Ty::Float),
                    "int" => (&[Ty::Float], Ty::Int),
                    // ADR-0020 論点 10
                    "sleep_ms" => (&[Ty::Int], Ty::None),
                    "arg" => (&[Ty::Int], self.cx.types.union([Ty::Str, Ty::None])),
                    // ADR-0021 論点 5
                    "parse_json" => {
                        let e = self.cx.types.json_error();
                        (&[Ty::Str], self.cx.types.result(Ty::Unknown, e))
                    }
                    _ => (&[Ty::Str], Ty::Unknown),
                };
                if args.len() != params.len() {
                    self.cx.diags.push(Diagnostic::error(
                        "E0205",
                        format!(
                            "`{}` takes {} argument(s) but {} were given",
                            callee.name,
                            params.len(),
                            args.len()
                        ),
                        span,
                    ));
                } else {
                    for ((a, &at), &pt) in args.iter().zip(&arg_tys).zip(params) {
                        let ctx = format!("argument of `{}`", callee.name);
                        self.cx.require(at, pt, a.span, &ctx, None);
                    }
                }
                return ret;
            }
            "assert" => {
                if args.len() != 1 {
                    self.cx.diags.push(Diagnostic::error(
                        "E0205",
                        format!("`assert` takes 1 argument but {} were given", args.len()),
                        span,
                    ));
                } else {
                    self.cx.require(
                        arg_tys[0],
                        Ty::Bool,
                        args[0].span,
                        "`assert` needs a `bool` condition",
                        None,
                    );
                }
                return Ty::None;
            }
            "panic" => {
                if args.len() != 1 {
                    self.cx.diags.push(Diagnostic::error(
                        "E0205",
                        format!("`panic` takes 1 argument but {} were given", args.len()),
                        span,
                    ));
                } else {
                    self.cx.require(
                        arg_tys[0],
                        Ty::Str,
                        args[0].span,
                        "the panic message must be a `str`",
                        None,
                    );
                }
                return Ty::None;
            }
            "exit" => {
                if args.len() != 1 {
                    self.cx.diags.push(Diagnostic::error(
                        "E0205",
                        format!("`exit` takes 1 argument but {} were given", args.len()),
                        span,
                    ));
                } else {
                    self.cx.require(
                        arg_tys[0],
                        Ty::Int,
                        args[0].span,
                        "the exit status must be an `int`",
                        None,
                    );
                }
                return Ty::None;
            }
            _ => {}
        }
        let Some(&idx) = self.cx.sig_index.get(&callee.name) else {
            let msg = if self.vars.contains_key(&callee.name) {
                format!("`{}` is a variable, not a function", callee.name)
            } else {
                format!("undefined function `{}`", callee.name)
            };
            self.cx
                .diags
                .push(Diagnostic::error("E0208", msg, callee.span));
            return Ty::Error;
        };
        let sig = self.cx.sigs[idx].clone();
        if sig.params.len() != args.len() {
            self.cx.diags.push(
                Diagnostic::error(
                    "E0205",
                    format!(
                        "function `{}` takes {} argument(s) but {} were given",
                        sig.name,
                        sig.params.len(),
                        args.len()
                    ),
                    span,
                )
                .with_related(RelatedKind::Function, &sig.name, Some(sig.span)),
            );
            return if sig.type_params.is_empty() {
                sig.ret
            } else {
                Ty::Error
            };
        }
        let (params, ret) = if sig.type_params.is_empty() {
            (sig.params.clone(), sig.ret)
        } else {
            // 明示した型引数は推論より優先する（ADR-0021 論点 9）。合わない引数は下の `require` が E0201 にする
            let mut map: Subst = explicit
                .map(|ts| {
                    sig.type_params
                        .iter()
                        .copied()
                        .zip(ts.iter().copied())
                        .collect()
                })
                .unwrap_or_default();
            for (&pt, &at) in sig.params.iter().zip(&arg_tys) {
                self.unify(pt, at, &sig.type_params, &mut map);
            }
            let what = format!("function `{}`", sig.name);
            self.require_inferred(&sig.type_params, &mut map, &what, span);
            self.cx
                .call_type_args
                .insert(id, sig.type_params.iter().map(|p| map[p]).collect());
            let params = sig
                .params
                .iter()
                .map(|t| self.cx.types.subst(*t, &map))
                .collect();
            (params, self.cx.types.subst(sig.ret, &map))
        };
        for ((arg, &at), &pt) in args.iter().zip(&arg_tys).zip(&params) {
            let ctx = format!("argument of function `{}`", sig.name);
            self.cx.require(
                at,
                pt,
                arg.span,
                &ctx,
                Some((RelatedKind::Function, &sig.name, sig.span)),
            );
        }
        ret
    }
}

/// `dict` のキーに使える型（ADR-0015）。
fn valid_dict_key(k: Ty) -> bool {
    matches!(k, Ty::Int | Ty::Bool | Ty::Str | Ty::Error)
}

/// Generic な型に `None` を与えて、フィールドや `dict` の値が `None` だけになった（E0215）。
fn none_type_arg_error(slot: &str, span: Span) -> Diagnostic {
    Diagnostic::error(
        "E0215",
        format!("this type argument makes {slot} have type `None`"),
        span,
    )
    .with_cause("fields and dict values cannot have type `None`")
}

fn dict_key_error(name: &str, span: Span) -> Diagnostic {
    if name == "PyObject" {
        return py_value_use_error("a dict key", span);
    }
    Diagnostic::error(
        "E0907",
        format!("`{name}` cannot be used as a dict key"),
        span,
    )
    .with_cause("dict keys must be `int`, `bool` or `str` (ADR-0015)")
    .with_hint("use a `str` or `int` key, e.g. by building a string from the parts")
}

/// `PyObject` を、具体的な値が要る場所で使っている（E1202、ADR-0018 論点 13）。
fn py_value_use_error(place: &str, span: Span) -> Diagnostic {
    Diagnostic::error(
        "E1202",
        format!("a Python object or error cannot be used as {place}"),
        span,
    )
    .with_cause("printing, comparing or hashing it would run Python code (`__str__`, `__eq__`)")
    .with_hint("convert it first, e.g. `h.to_int()` or `h.to_str()`; read the fields of a `PyError` (`e.kind`, `e.message`)")
}

fn is_result_ctor(name: &str) -> bool {
    matches!(name, "Ok" | "Err")
}

/// 足りない構成要素に対応する `case` の書き方。
fn case_text(t: Ty) -> String {
    match t {
        Ty::Result(_) => "`case Result():`".into(),
        Ty::Int => "`case int():`".into(),
        Ty::Bool => "`case bool():`".into(),
        Ty::Str => "`case str():`".into(),
        Ty::Float => "`case float():`".into(),
        Ty::None => "`case None:`".into(),
        _ => "`case _:`".into(),
    }
}

/// 文の列の中で代入される変数名を集める（入れ子のブロックを含む）。
fn assigned_vars(stmts: &[Stmt], out: &mut HashSet<String>) {
    for s in stmts {
        match &s.kind {
            StmtKind::Assign { target, .. } => {
                out.insert(target.name.clone());
            }
            StmtKind::If {
                branches,
                else_body,
            } => {
                for (_, b) in branches {
                    assigned_vars(b, out);
                }
                if let Some(b) = else_body {
                    assigned_vars(b, out);
                }
            }
            StmtKind::While { body, .. } => assigned_vars(body, out),
            StmtKind::For { var, body, .. } => {
                out.insert(var.name.clone());
                assigned_vars(body, out);
            }
            StmtKind::IndexAssign { .. } => {}
            StmtKind::Match { arms, .. } => {
                for a in arms {
                    assigned_vars(&a.body, out);
                }
            }
            StmtKind::Expr(_) | StmtKind::Return(_) | StmtKind::Pass => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_narrows_and_checks_exhaustiveness() {
        let ok = "def f(v: int | str | None) -> int:\n    match v:\n        case int():\n            return v + 1\n        case str():\n            print(v)\n            return 0\n        case None:\n            return -1\n";
        assert_eq!(run(&with_main(ok)), Vec::<&str>::new());
        let wildcard = "def f(v: int | None) -> int:\n    match v:\n        case None:\n            return 0\n        case _:\n            return v\n";
        assert_eq!(run(&with_main(wildcard)), Vec::<&str>::new());
        let missing = "def f(v: int | str | None) -> None:\n    match v:\n        case int():\n            pass\n";
        assert_eq!(run(&with_main(missing)), vec!["E0303"]);
        let unreachable = "def f(v: int | None) -> None:\n    match v:\n        case _:\n            pass\n        case int():\n            pass\n";
        assert_eq!(run(&with_main(unreachable)), vec!["W0301"]);
        let never = "def f(v: int | None) -> None:\n    match v:\n        case str():\n            pass\n        case _:\n            pass\n";
        assert_eq!(run(&with_main(never)), vec!["E0305"]);
        let not_all_return = "def f(v: int | None) -> int:\n    match v:\n        case int():\n            return v\n        case None:\n            pass\n";
        assert_eq!(run(&with_main(not_all_return)), vec!["E0206"]);
    }

    fn run(src: &str) -> Vec<&'static str> {
        let (m, d) = wys_parser::parse_source(src);
        assert!(d.is_empty(), "parse errors: {d:?}");
        check(&m).1.into_iter().map(|d| d.code).collect()
    }

    fn main_with(body: &str) -> String {
        format!("def main() -> None:\n{body}")
    }

    #[test]
    fn builtin_effects_follow_adr_0014() {
        assert_eq!(builtin_effects("print").unwrap().names(), vec!["io"]);
        assert_eq!(builtin_effects("arg").unwrap().names(), vec!["io"]);
        assert_eq!(builtin_effects("exit").unwrap().names(), vec!["exit"]);
        assert!(builtin_effects("panic").unwrap().names().is_empty());
        assert!(builtin_effects("len").unwrap().names().is_empty());
        assert!(builtin_effects("main").is_none());
    }

    #[test]
    fn effects_are_checked_only_in_annotated_functions() {
        // 注釈のない関数は何でも呼べる。自己再帰は常に通る
        let free = "def f(n: int) -> int:\n    print(n)\n    return n\n\ndef g(n: int) -> int uses pure:\n    if n < 1:\n        return 0\n    return g(n - 1)\n";
        assert_eq!(run(&with_main(free)), Vec::<&str>::new());
        let wider = "def f() -> None uses io, exit:\n    print(1)\n    exit(1)\n";
        assert_eq!(run(&with_main(wider)), Vec::<&str>::new());
        let panics = "def f() -> None uses pure:\n    panic(\"x\")\n";
        assert_eq!(run(&with_main(panics)), Vec::<&str>::new());
        let both_missing = "def f() -> None uses pure:\n    print(1)\n    exit(1)\n";
        assert_eq!(run(&with_main(both_missing)), vec!["E1102", "E1102"]);
        // 未知の名前があれば本体は検査しない（E1102 を連鎖させない）
        let unknown = "def f() -> None uses foo:\n    print(1)\n";
        assert_eq!(run(&with_main(unknown)), vec!["E1101"]);
    }

    #[test]
    fn missing_effects_share_one_fix() {
        // どの E1102 の修正案を適用しても、足りない Effect がすべて宣言される
        let src = with_main("def f() -> None uses pure:\n    print(1)\n    exit(1)\n");
        let (m, _) = wys_parser::parse_source(&src);
        let diags = check(&m).1;
        assert_eq!(diags.len(), 2);
        for d in &diags {
            let (_, text) = d.hints[0].replacement.clone().unwrap();
            assert_eq!(text, "io, exit");
        }
        let src = with_main("def f() -> None uses io:\n    exit(1)\n    exit(2)\n");
        let (m, _) = wys_parser::parse_source(&src);
        let diags = check(&m).1;
        assert!(
            diags
                .iter()
                .all(|d| d.hints[0].replacement.as_ref().unwrap().1 == ", exit")
        );
    }

    #[test]
    fn effect_sites_are_recorded_for_every_function() {
        let src = "def f() -> None uses io:\n    print(1)\n\ndef main() -> None:\n    f()\n    g()\n\ndef g() -> None:\n    pass\n";
        let (m, d) = wys_parser::parse_source(src);
        assert!(d.is_empty());
        let (tc, diags) = check(&m);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(tc.sigs[0].effects.map(|e| e.names()), Some(vec!["io"]));
        assert_eq!(tc.sigs[2].effects, None);
        let main: Vec<(&str, Option<Vec<&str>>)> = tc.funcs[1]
            .effect_sites
            .iter()
            .map(|s| (s.callee.as_str(), s.effects.map(|e| e.names())))
            .collect();
        assert_eq!(main, vec![("f", Some(vec!["io"])), ("g", None)]);
    }

    #[test]
    fn accepts_valid_program() {
        let src = "def fib(n: int) -> int:\n    if n < 2:\n        return n\n    return fib(n - 1) + fib(n - 2)\n\ndef main() -> None:\n    x = fib(10)\n    ok: bool = x == 55 and not False\n    if ok:\n        print(\"ok\")\n    else:\n        print(x)\n";
        assert_eq!(run(src), Vec::<&str>::new());
    }

    #[test]
    fn ac001_argument_type_mismatch() {
        let src = "def f(x: int) -> int:\n    return x\n\ndef main() -> None:\n    f(\"a\")\n";
        assert_eq!(run(src), vec!["E0201"]);
    }

    #[test]
    fn no_implicit_conversions() {
        assert_eq!(run(&main_with("    x = 1 + True\n")), vec!["E0207"]);
        assert_eq!(run(&main_with("    if 1:\n        pass\n")), vec!["E0201"]);
        assert_eq!(run(&main_with("    x = 1\n    x = \"s\"\n")), vec!["E0201"]);
        assert_eq!(run(&main_with("    x = 1 / 2\n")), vec!["E0207"]);
    }

    #[test]
    fn definite_assignment() {
        let maybe = main_with("    if True:\n        x = 1\n    print(x)\n");
        assert_eq!(run(&maybe), vec!["E0204"]);
        let both =
            main_with("    if True:\n        x = 1\n    else:\n        x = 2\n    print(x)\n");
        assert_eq!(run(&both), Vec::<&str>::new());
        let loop_only = main_with("    while False:\n        y = 1\n    print(y)\n");
        assert_eq!(run(&loop_only), vec!["E0204"]);
        let returning = "def f(c: bool) -> int:\n    if c:\n        return 0\n    else:\n        x = 1\n    return x\n\ndef main() -> None:\n    pass\n";
        assert_eq!(run(returning), Vec::<&str>::new());
    }

    #[test]
    fn missing_return() {
        let src = "def f(c: bool) -> int:\n    if c:\n        return 1\n\ndef main() -> None:\n    pass\n";
        assert_eq!(run(src), vec!["E0206"]);
        let ok = "def f(c: bool) -> int:\n    if c:\n        return 1\n    else:\n        return 2\n\ndef main() -> None:\n    pass\n";
        assert_eq!(run(ok), Vec::<&str>::new());
    }

    #[test]
    fn names_and_calls() {
        assert_eq!(run(&main_with("    print(y)\n")), vec!["E0202"]);
        assert_eq!(run(&main_with("    g()\n")), vec!["E0208"]);
        assert_eq!(run(&main_with("    print(1, 2)\n")), vec!["E0205"]);
        assert_eq!(run(&main_with("    x = main\n")), vec!["E0213"]);
        assert_eq!(run(&main_with("    x = print(1)\n")), vec!["E0215"]);
    }

    #[test]
    fn main_and_definitions() {
        assert_eq!(run("def f() -> None:\n    pass\n"), vec!["E0210"]);
        assert_eq!(run("def main() -> int:\n    return 0\n"), vec!["E0210"]);
        assert_eq!(
            run("def main() -> None:\n    pass\n\ndef main() -> None:\n    pass\n"),
            vec!["E0209"]
        );
        assert_eq!(
            run("def f(x: decimal) -> None:\n    pass\n\ndef main() -> None:\n    pass\n"),
            vec!["E0203"]
        );
        assert_eq!(
            run("def f() -> int:\n    return\n\ndef main() -> None:\n    pass\n"),
            vec!["E0211"]
        );
    }

    fn with_main(defs: &str) -> String {
        format!("{defs}\ndef main() -> None:\n    pass\n")
    }

    #[test]
    fn ac002_unchecked_none_is_rejected() {
        let src = with_main("def f(s: str | None) -> None:\n    print(s)\n");
        assert_eq!(run(&src), vec!["E0301"]);
        let (m, _) = wys_parser::parse_source(&src);
        let d = check(&m).1;
        assert!(d[0].message.contains("`str | None`"), "{}", d[0].message);
    }

    #[test]
    fn narrowing_by_early_return_and_branches() {
        let ok = [
            "def f(s: str | None) -> str:\n    if s is None:\n        return \"none\"\n    return s\n",
            "def f(s: str | None) -> str:\n    if s is not None:\n        return s\n    return \"none\"\n",
            "def f(n: int | None) -> int:\n    if n is None:\n        return 0\n    elif n > 0:\n        return n\n    else:\n        return -n\n",
            "def f(n: int | None) -> int:\n    if not (n is None):\n        return n\n    return 0\n",
        ];
        for src in ok {
            assert_eq!(run(&with_main(src)), Vec::<&str>::new(), "{src}");
        }
    }

    #[test]
    fn narrowing_through_and_or() {
        assert_eq!(
            run(&with_main(
                "def f(n: int | None) -> bool:\n    return n is not None and n > 0\n"
            )),
            Vec::<&str>::new()
        );
        assert_eq!(
            run(&with_main(
                "def f(n: int | None) -> bool:\n    return n is None or n > 0\n"
            )),
            Vec::<&str>::new()
        );
        assert_eq!(
            run(&with_main(
                "def f(n: int | None) -> bool:\n    return n is None and n > 0\n"
            )),
            vec!["E0207"] // 右辺では n は確実に None なので演算子のエラーになる
        );
    }

    #[test]
    fn join_and_reassignment_reset_narrowing() {
        let join =
            "def f(n: int | None) -> int:\n    if n is not None:\n        print(n)\n    return n\n";
        assert_eq!(run(&with_main(join)), vec!["E0301"]);
        let reassign = "def f(n: int | None, m: int | None) -> int:\n    if n is None:\n        return 0\n    n = m\n    return n\n";
        assert_eq!(run(&with_main(reassign)), vec!["E0301"]);
    }

    #[test]
    fn loops_reset_narrowing_of_assigned_variables() {
        let body_assigns = "def f(n: int | None, m: int | None) -> int:\n    if n is None:\n        return 0\n    while True:\n        print(n)\n        n = m\n    return 0\n";
        assert_eq!(run(&with_main(body_assigns)), vec!["E0301"]);
        let not_assigned = "def f(n: int | None) -> int:\n    if n is None:\n        return 0\n    i = 0\n    while i < 3:\n        print(n + i)\n        i = i + 1\n    return n\n";
        assert_eq!(run(&with_main(not_assigned)), Vec::<&str>::new());
        let until_set = "def f(m: int | None) -> int:\n    x: int | None = None\n    while x is None:\n        x = m\n    return x\n";
        assert_eq!(run(&with_main(until_set)), Vec::<&str>::new());
    }

    #[test]
    fn optional_specific_errors() {
        assert_eq!(
            run(&with_main("def f(n: int) -> bool:\n    return n is None\n")),
            vec!["E0302"]
        );
        assert_eq!(
            run(&with_main(
                "def f(n: int | None) -> bool:\n    return n is 1\n"
            )),
            vec!["E0207"]
        );
        assert_eq!(
            run(&with_main(
                "def f(n: int | None) -> int:\n    return n + 1\n"
            )),
            vec!["E0301"]
        );
        assert_eq!(
            run(&with_main("def f() -> int | None:\n    return\n")),
            vec!["E0211"]
        );
        assert_eq!(
            run(&with_main(
                "def f(b: bool | None) -> None:\n    if b:\n        pass\n"
            )),
            vec!["E0301"]
        );
    }

    #[test]
    fn unions_are_flattened_and_accepted_where_they_fit() {
        assert_eq!(
            run(&with_main(
                "def f(x: int | str) -> None:\n    print(x)\n\ndef g() -> int | None | int:\n    return None\n"
            )),
            Vec::<&str>::new()
        );
        assert_eq!(
            run(&with_main("def f(x: int | str) -> int:\n    return x\n")),
            vec!["E0201"]
        );
        let mut t = Types::default();
        let a = t.union([Ty::None, Ty::Int]);
        let b = t.union([Ty::Int, a, Ty::None]);
        assert_eq!(a, b);
        assert_eq!(t.name(a), "int | None");
    }

    #[test]
    fn records_construction_fields_and_width_subtyping() {
        let defs = "type Named = {name: str}\ntype User = {name: str, age: int}\n\n";
        let ok = format!(
            "{defs}def greet(n: Named) -> str:\n    return n.name\n\ndef f() -> int:\n    u = User(name=\"a\", age=3)\n    print(greet(u))\n    m: User | None = None\n    if m is not None:\n        return m.age\n    return u.age\n"
        );
        assert_eq!(run(&with_main(&ok)), Vec::<&str>::new());
        let cases = [
            (
                "def f() -> None:\n    u = User(name=\"a\", age=1, zip=2)\n",
                "E0501",
            ),
            ("def f() -> None:\n    u = User(name=\"a\")\n", "E0502"),
            (
                "def f() -> None:\n    u = User(name=\"a\", name=\"b\", age=1)\n",
                "E0503",
            ),
            ("def f() -> None:\n    u = User(\"a\", 1)\n", "E0505"),
            ("def f(u: User) -> int:\n    return u.height\n", "E0501"),
            ("def f(u: User | None) -> int:\n    return u.age\n", "E0301"),
            (
                "def f(n: Named) -> int:\n    u: User = n\n    return 0\n",
                "E0201",
            ),
            (
                "def f(a: int) -> None:\n    print(a)\n\ndef g() -> None:\n    f(a=1)\n",
                "E0216",
            ),
            ("def f(x: User | Named) -> None:\n    pass\n", "E0508"),
        ];
        for (src, code) in cases {
            assert_eq!(
                run(&with_main(&format!("{defs}{src}"))),
                vec![code],
                "{src}"
            );
        }
    }

    #[test]
    fn recursive_type_cause_points_to_adr_0021() {
        let src = with_main("type Node = {value: int, next: Node | None}\n");
        let (m, d) = wys_parser::parse_source(&src);
        assert!(d.is_empty(), "parse errors: {d:?}");
        let diags = check(&m).1;
        let cause = diags[0].cause.clone().unwrap_or_default();
        assert!(cause.contains("ADR-0021"), "{cause}");
        assert!(!cause.contains("M5"), "{cause}");
    }

    #[test]
    fn recursive_and_duplicate_types() {
        assert_eq!(
            run(&with_main("type Node = {value: int, next: Node | None}\n")),
            vec!["E0504"]
        );
        assert_eq!(
            run(&with_main("type A = {x: int}\ntype A = {y: int}\n")),
            vec!["E0209"]
        );
        assert_eq!(
            run(&with_main("type P = {x: int, x: int}\n")),
            vec!["E0503"]
        );
        assert_eq!(
            run(&with_main(
                "type P = {x: int}\n\ndef f(v: P | None) -> None:\n    match v:\n        case P():\n            pass\n        case _:\n            pass\n"
            )),
            vec!["E0304"]
        );
        // 別名は構造的: 同じフィールドなら同じ型
        assert_eq!(
            run(&with_main(
                "type A = {x: int}\ntype B = {x: int}\n\ndef f(a: A) -> B:\n    return a\n"
            )),
            Vec::<&str>::new()
        );
    }

    #[test]
    fn generic_functions_infer_and_check() {
        let defs = "def unwrap_or[T](x: T | None, d: T) -> T:\n    if x is None:\n        return d\n    return x\n\ndef identity[T](x: T) -> T:\n    return x\n\n";
        let ok = format!(
            "{defs}def f() -> None:\n    n: int | None = None\n    print(unwrap_or(n, 5) + 1)\n    print(unwrap_or(None, \"d\"))\n    print(identity(3) + 1)\n"
        );
        assert_eq!(run(&with_main(&ok)), Vec::<&str>::new());
        let cases = [
            (
                "def same[T](a: T, b: T) -> T:\n    return a\n\ndef f() -> None:\n    print(same(1, \"a\"))\n",
                vec!["E0201"],
            ),
            ("def f[T](x: T) -> T:\n    return x + 1\n", vec!["E0207"]),
            (
                "def none[T]() -> T | None:\n    return None\n\ndef f() -> None:\n    x = none()\n",
                vec!["E0403"],
            ),
            (
                "def f[T](x: T | None) -> None:\n    match x:\n        case int():\n            pass\n        case _:\n            pass\n",
                vec!["E0401"],
            ),
            (
                "def f(x: list[int, str]) -> None:\n    pass\n",
                vec!["E0402"],
            ),
            ("def f(x: int[str]) -> None:\n    pass\n", vec!["E0402"]),
        ];
        for (src, codes) in cases {
            assert_eq!(run(&with_main(src)), codes, "{src}");
        }
    }

    #[test]
    fn generic_records() {
        let defs = "type Pair[A, B] = {first: A, second: B}\n\ndef swap[A, B](p: Pair[A, B]) -> Pair[B, A]:\n    return Pair(first=p.second, second=p.first)\n\n";
        let ok = format!(
            "{defs}def f() -> None:\n    q = swap(Pair(first=1, second=\"x\"))\n    print(q.first)\n    print(q.second + 1)\n    _r: Pair[str, int] = q\n"
        );
        assert_eq!(run(&with_main(&ok)), Vec::<&str>::new());
        assert_eq!(
            run(&with_main(&format!(
                "{defs}def f(p: Pair) -> None:\n    pass\n"
            ))),
            vec!["E0402"]
        );
        assert_eq!(
            run(&with_main(&format!(
                "{defs}def f() -> None:\n    q: Pair[int, int] = Pair(first=1, second=\"x\")\n"
            ))),
            vec!["E0201"]
        );
        let (m, _) = wys_parser::parse_source(&with_main(&format!(
            "{defs}def f() -> None:\n    q = Pair(first=1, second=\"x\")\n    q.nope\n"
        )));
        let d = check(&m).1;
        assert!(d[0].message.contains("Pair[int, str]"), "{}", d[0].message);
    }

    #[test]
    fn unknown_must_be_checked_with_match() {
        let ok = "def f() -> None:\n    v = parse_scalar(\"1\")\n    match v:\n        case int():\n            print(v + 1)\n        case str():\n            print(v)\n        case _:\n            pass\n    _u: Unknown = 1\n    a = arg(0)\n    if a is not None:\n        print(a)\n    print(arg_count())\n";
        assert_eq!(run(&with_main(ok)), Vec::<&str>::new());
        let cases = [
            (
                "def f() -> None:\n    print(parse_scalar(\"1\"))\n",
                "E0601",
            ),
            (
                "def f() -> None:\n    x = parse_scalar(\"1\") + 1\n",
                "E0601",
            ),
            (
                "def f() -> None:\n    n: int = parse_scalar(\"1\")\n",
                "E0601",
            ),
            (
                "def f() -> bool:\n    return parse_scalar(\"1\") is None\n",
                "E0601",
            ),
            (
                "def f() -> None:\n    v = parse_scalar(\"1\")\n    match v:\n        case int():\n            pass\n        case bool():\n            pass\n        case str():\n            pass\n        case None:\n            pass\n",
                "E0602",
            ),
            ("def f() -> None:\n    a = arg(\"x\")\n", "E0201"),
        ];
        for (src, code) in cases {
            assert_eq!(run(&with_main(src)), vec![code], "{src}");
        }
    }

    #[test]
    fn assert_narrows_and_tests_need_no_main() {
        let src = "def test_value() -> None:\n    x: int | None = 1\n    assert(x is not None)\n    print(x + 1)\n    assert(x == 1)\n";
        let (m, d) = wys_parser::parse_source(src);
        assert!(d.is_empty());
        assert!(check_program(&m, false).1.is_empty());
        assert_eq!(
            check(&m).1.iter().map(|d| d.code).collect::<Vec<_>>(),
            vec!["E0210"]
        );
        assert_eq!(
            run(&with_main("def f(n: int) -> None:\n    assert(n)\n")),
            vec!["E0201"]
        );
    }
}
