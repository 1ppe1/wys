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
use wys_diagnostics::{Diagnostic, RelatedKind};
use wys_span::Span;

/// 型。`Union` は `Types` に登録した構成要素の集合を指す。
/// 順序（`Ord`）は Union の構成要素の正規順として使う（int, bool, str, None）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Ty {
    Int,
    Bool,
    Str,
    /// 構造的レコード（ADR-0007、不変）。
    Record(RecordId),
    /// 型パラメータ（`def f[T]` の `T`）。中身の分からない型として扱い、単相化で具体的な型に置き換える。
    Param(ParamId),
    /// 未検証の外部値（ADR-0011）。`int` / `bool` / `str` / `None` のどれかだが、開いた型として扱い、
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
            members.retain(|m| !matches!(m, Ty::Int | Ty::Bool | Ty::Str | Ty::None));
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
        if depth == 0 && matches!(t, Ty::Union(_) | Ty::Record(_)) {
            return "…".into();
        }
        let name = |t: Ty| self.name_within(t, depth.saturating_sub(1));
        match t {
            Ty::Int => "int".into(),
            Ty::Bool => "bool".into(),
            Ty::Str => "str".into(),
            Ty::None => "None".into(),
            Ty::Error => "<error>".into(),
            Ty::Param(p) => self.params[p.0 as usize].clone(),
            Ty::Unknown => "Unknown".into(),
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
            (Ty::Int | Ty::Bool | Ty::Str | Ty::None, Ty::Unknown) => true,
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
            _ => false,
        }
    }

    /// `Unknown` を含むか。レコードは登録時に計算した値を使う。
    pub fn contains_unknown(&self, t: Ty) -> bool {
        match t {
            Ty::Unknown => true,
            Ty::Union(_) => self.members(t).iter().any(|m| self.contains_unknown(*m)),
            Ty::Record(r) => self.records[r.0 as usize].has_unknown,
            _ => false,
        }
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
            _ => 0,
        }
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

#[derive(Debug, Clone)]
pub struct FuncSig {
    pub name: String,
    /// 型パラメータ（Generic 関数でなければ空）。
    pub type_params: Vec<ParamId>,
    pub params: Vec<Ty>,
    pub ret: Ty,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct FuncInfo {
    /// 関数内のローカル変数（先頭は引数）。名前と宣言された型。
    pub locals: Vec<(String, Ty)>,
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
}

impl TypeckResult {
    pub fn ty(&self, e: &Expr) -> Ty {
        self.expr_types[e.id.0 as usize]
    }
}

const BUILTINS: &[&str] = &[
    "print",
    "exit",
    "arg_count",
    "arg",
    "parse_scalar",
    "assert",
    "len",
];

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
    };
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
        cx.sigs.push(FuncSig {
            name: f.name.name.clone(),
            type_params,
            params,
            ret,
            span: f.name.span,
        });
    }
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
    for (i, f) in module.funcs.iter().enumerate() {
        funcs.push(FnCk::run(&mut cx, f, i));
    }
    (
        TypeckResult {
            expr_types: cx.expr_types,
            sigs: cx.sigs,
            funcs,
            types: cx.types,
            call_type_args: cx.call_type_args,
        },
        cx.diags,
    )
}

/// `type` 定義を登録し、すべて解決する（使われていない定義の誤りも報告するため）。
fn declare_types(cx: &mut Ctx, module: &Module) {
    for (i, t) in module.types.iter().enumerate() {
        let name = &t.name.name;
        if matches!(name.as_str(), "int" | "bool" | "str" | "None") {
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
            TypeExprKind::Name(name) | TypeExprKind::Apply(name, _) if name == "list" => {
                self.diags.push(
                    Diagnostic::error("E0405", "`list` is not available yet", t.span).with_cause(
                        "lists need heap memory management, which arrives in M3 (ADR-0009)",
                    ),
                );
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
                self.types.subst(body, &map)
            }
            TypeExprKind::Name(name) => match name.as_str() {
                "int" => Ty::Int,
                "bool" => Ty::Bool,
                "str" => Ty::Str,
                "None" => Ty::None,
                "Unknown" => Ty::Unknown,
                other if self.aliases.contains_key(other) => self.resolve_alias(other),
                other => {
                    self.diags.push(
                        Diagnostic::error("E0203", format!("unknown type `{other}`"), t.span)
                            .with_hint("supported types are `int`, `bool`, `str`, `None`, types defined with `type`, and unions of them (`A | B`)"),
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
                        .with_cause("recursive types are not supported yet (ADR-0009)"),
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

struct FnCk<'a> {
    cx: &'a mut Ctx,
    ret: Ty,
    vars: HashMap<String, Var>,
    locals: Vec<(String, Ty)>,
}

impl<'a> FnCk<'a> {
    fn run(cx: &'a mut Ctx, f: &FuncDef, index: usize) -> FuncInfo {
        let sig = cx.sigs[index].clone();
        let mut ck = FnCk {
            cx,
            ret: sig.ret,
            vars: HashMap::new(),
            locals: Vec::new(),
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
        FuncInfo { locals: ck.locals }
    }

    fn declare(&mut self, name: &str, ty: Ty, decl: Span) {
        self.vars.insert(name.to_string(), Var { ty, decl });
        self.locals.push((name.to_string(), ty));
    }

    /// 複数の経路の環境を合流させる: 代入済みの変数は共通部分、型は和集合。
    fn join(&mut self, envs: Vec<Env>) -> Env {
        let mut it = envs.into_iter();
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
                            args,
                            kwargs,
                        },
                    ..
                },
            ) if callee.name == "assert" && args.len() == 1 && kwargs.is_empty() => {
                let (t, t_env, _) = self.cond(&args[0], env);
                self.require_condition(t, args[0].span);
                *env = t_env;
                self.record(e, Ty::None);
                false
            }
            // exit() は戻らない: この文の後には到達しない（絞り込みと return の検査のため）
            StmtKind::Expr(
                e @ Expr {
                    kind: ExprKind::Call { callee, .. },
                    ..
                },
            ) if callee.name == "exit" => {
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
                    if !self.block(body, &mut b) {
                        live.push(b);
                    }
                    cur = f_env;
                }
                match else_body {
                    Some(body) => {
                        let mut b = cur;
                        if !self.block(body, &mut b) {
                            live.push(b);
                        }
                    }
                    None => live.push(cur),
                }
                if live.is_empty() {
                    return true; // すべての分岐が return する
                }
                *env = self.join(live);
                false
            }
            StmtKind::Match { subject, arms } => self.match_stmt(subject, arms, env),
            StmtKind::While { cond, body } => {
                // ループ本体で代入される変数は、2回目以降の条件評価で型が変わり得るので、絞り込みを解除する。
                let mut assigned = HashSet::new();
                assigned_vars(body, &mut assigned);
                for name in assigned {
                    if let (Some(slot), Some(var)) = (env.get_mut(&name), self.vars.get(&name)) {
                        *slot = var.ty;
                    }
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
        let subject_var = match &subject.kind {
            ExprKind::Name(n) if self.vars.contains_key(n) => Some(n.clone()),
            _ => None,
        };
        // Unknown は int / bool / str / None に加えて「将来増える種類」を持つ開いた型。
        // 最後の Unknown は `case _:` だけが網羅できる（ADR-0011）。
        let members = (st != Ty::Error).then(|| {
            if st == Ty::Unknown {
                vec![Ty::Int, Ty::Bool, Ty::Str, Ty::None, Ty::Unknown]
            } else {
                self.cx.types.members(st)
            }
        });
        let mut remaining = members.clone().unwrap_or_default();
        let mut live = Vec::new();
        for arm in arms {
            let mut pattern_failed = false;
            let covered: Vec<Ty> = match &arm.pattern.kind {
                PatternKind::Wildcard => remaining.clone(),
                kind => {
                    let pt = self.pattern_type(kind, arm.pattern.span);
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
                let narrowed = self.cx.types.union(covered.iter().copied());
                b.insert(n.clone(), narrowed);
            }
            if !self.block(&arm.body, &mut b) {
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

    fn pattern_type(&mut self, kind: &PatternKind, span: Span) -> Ty {
        match kind {
            PatternKind::None => Ty::None,
            PatternKind::Wildcard => Ty::Error,
            PatternKind::Type(name) if self.cx.aliases.contains_key(name) => {
                match self.cx.resolve_alias(name) {
                    t @ (Ty::Int | Ty::Bool | Ty::Str | Ty::Error) => t,
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
                other => {
                    self.cx.diags.push(
                        Diagnostic::error("E0203", format!("unknown type `{other}` in pattern"), span)
                            .with_hint("type patterns are `int()`, `bool()` and `str()`; use `None` or `_` for the others"),
                    );
                    Ty::Error
                }
            },
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
        let vt = self.expr(value, env);
        let ann_ty = ann.map(|a| self.cx.resolve(a));
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
        // 再代入で絞り込みは解除する（宣言された型に戻す）。
        env.insert(target.name.clone(), declared);
    }

    fn ret_stmt(&mut self, span: Span, value: Option<&Expr>, env: &Env) {
        match (self.ret, value) {
            (Ty::Error, Some(e)) => {
                self.expr(e, env);
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
                let t = self.expr(e, env);
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
                    if !self.cx.types.contains_none(lt) {
                        let msg = format!(
                            "this value can never be `None` (its type is `{}`), so the check is always {}",
                            self.cx.name(lt),
                            if *negated { "True" } else { "False" }
                        );
                        self.cx.diags.push(
                            Diagnostic::error("E0302", msg, lhs.span)
                                .with_hint("remove the check, or give the value an optional type such as `T | None`"),
                        );
                    } else if let ExprKind::Name(n) = &lhs.kind
                        && self.vars.contains_key(n)
                    {
                        let some = self.cx.types.without_none(lt);
                        let (on_true, on_false) = if *negated {
                            (some, Ty::None)
                        } else {
                            (Ty::None, some)
                        };
                        t_env.insert(n.clone(), on_true);
                        f_env.insert(n.clone(), on_false);
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
            ExprKind::Bool(_) => Ty::Bool,
            ExprKind::Str(_) => Ty::Str,
            ExprKind::None => Ty::None,
            ExprKind::Name(name) => {
                if let Some(&t) = env.get(name) {
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
            ExprKind::Binary { op, lhs, rhs } => {
                let lt = self.expr(lhs, env);
                let rt = self.expr(rhs, env);
                self.binary(*op, lt, rt, e.span)
            }
            ExprKind::Call {
                callee,
                args,
                kwargs,
            } => {
                if self.cx.aliases.contains_key(&callee.name) {
                    self.construct(callee, args, kwargs, e.span, env)
                } else if !kwargs.is_empty() {
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
                    self.call(callee, args, e.span, e.id, env)
                }
            }
            ExprKind::Field { base, name } => self.field(base, name, env),
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
        if t == want {
            return want;
        }
        let desc = format!(
            "operator `{sym}` cannot be applied to `{}`",
            self.cx.name(t)
        );
        self.operator_error(desc, &[t], |ts| ts[0] == want, span);
        Ty::Error
    }

    fn binary(&mut self, op: BinOp, lt: Ty, rt: Ty, span: Span) -> Ty {
        if lt == Ty::Error || rt == Ty::Error {
            return Ty::Error;
        }
        if op == BinOp::Div {
            self.cx.diags.push(
                Diagnostic::error("E0207", "operator `/` is not supported yet", span)
                    .with_cause("`/` produces a float, and floats are not supported yet")
                    .with_hint("use `//` for integer (floor) division"),
            );
            return Ty::Error;
        }
        let rule = move |ts: &[Ty]| -> Option<Ty> {
            let (l, r) = (ts[0], ts[1]);
            match op {
                // 文字列の連結（M3 T2b）
                BinOp::Add if l == Ty::Str && r == Ty::Str => Some(Ty::Str),
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::FloorDiv | BinOp::Mod => {
                    (l == Ty::Int && r == Ty::Int).then_some(Ty::Int)
                }
                BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                    (l == Ty::Int && r == Ty::Int).then_some(Ty::Bool)
                }
                BinOp::Eq | BinOp::Ne => {
                    (l == r && matches!(l, Ty::Int | Ty::Bool | Ty::Str)).then_some(Ty::Bool)
                }
                BinOp::And | BinOp::Or => (l == Ty::Bool && r == Ty::Bool).then_some(Ty::Bool),
                BinOp::Div => None,
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

    /// `print` の引数は `int` / `bool` / `str` とその Union。
    /// 検査のために Union 型を登録しない（使われない Union が IR に入らないようにする）。
    fn require_printable(&mut self, t: Ty, span: Span) {
        if t == Ty::Error {
            return;
        }
        if self.cx.types.members(t).contains(&Ty::Unknown) {
            self.cx.diags.push(Diagnostic::error("E0601", "this `Unknown` value has not been checked", span)
                    .with_cause("values from outside the program must be checked before use (LANG-006, ADR-0011)")
                    .with_hint("inspect it with `match`: `case int():`, `case str():`, ..., `case _:`"));
            return;
        }
        let printable = |m: &Ty| matches!(m, Ty::Int | Ty::Bool | Ty::Str);
        let members = self.cx.types.members(t);
        if members.iter().all(printable) {
            return;
        }
        let found = self.cx.name(t);
        let d = if members.iter().all(|m| printable(m) || *m == Ty::None) {
            Diagnostic::error(
                "E0301",
                format!("this value may be `None`: `print` cannot print `{found}`"),
                span,
            )
            .with_cause("`print` accepts `int`, `bool` or `str`")
            .with_hint("check it first, e.g. `if x is not None:`")
        } else {
            Diagnostic::error(
                "E0201",
                format!("mismatched types: `print` cannot print `{found}`"),
                span,
            )
            .with_cause("`print` accepts `int`, `bool` or `str`")
        };
        self.cx.diags.push(d);
    }

    /// `Point(x=1, y=2)`: レコードの作成。フィールドはすべてキーワード引数でちょうど1回ずつ指定する。
    /// Generic な `type`（`Pair(first=1, second="a")`）なら、キーワード引数の型から型引数を推論する。
    fn construct(
        &mut self,
        callee: &Ident,
        args: &[Expr],
        kwargs: &[(Ident, Expr)],
        span: Span,
        env: &Env,
    ) -> Ty {
        for a in args {
            self.expr(a, env);
        }
        let kw_tys: Vec<Ty> = kwargs.iter().map(|(_, v)| self.expr(v, env)).collect();
        let ty = match self.cx.generics.get(&callee.name).cloned() {
            Some((_, Ty::Error)) => Ty::Error,
            Some((params, body)) => {
                let mut map = Subst::new();
                if let Ty::Record(r) = body {
                    for ((k, _), &kt) in kwargs.iter().zip(&kw_tys) {
                        if let Some((_, ft)) = self.cx.types.record_def(r).field(&k.name) {
                            self.unify(ft, kt, &params, &mut map);
                        }
                    }
                }
                let what = format!("record `{}`", callee.name);
                self.require_inferred(&params, &mut map, &what, span);
                self.cx.types.subst(body, &map)
            }
            None => self.cx.resolve_alias(&callee.name),
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
    fn field(&mut self, base: &Expr, name: &Ident, env: &Env) -> Ty {
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
                Some((_, t)) => t,
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
                .with_hint("pass an argument whose type determines it (a bare `None` does not)"),
            );
            for p in params {
                map.entry(*p).or_insert(Ty::Error);
            }
        }
    }

    fn call(&mut self, callee: &Ident, args: &[Expr], span: Span, id: ExprId, env: &Env) -> Ty {
        let arg_tys: Vec<Ty> = args.iter().map(|a| self.expr(a, env)).collect();
        match callee.name.as_str() {
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
            "arg_count" | "arg" | "parse_scalar" | "len" => {
                // 外部入力の入口（ADR-0011）と、文字列の長さ（M3 T2b）
                let (params, ret): (&[Ty], Ty) = match callee.name.as_str() {
                    "arg_count" => (&[], Ty::Int),
                    "len" => (&[Ty::Str], Ty::Int),
                    "arg" => (&[Ty::Int], self.cx.types.union([Ty::Str, Ty::None])),
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
            let mut map = Subst::new();
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

/// 足りない構成要素に対応する `case` の書き方。
fn case_text(t: Ty) -> String {
    match t {
        Ty::Int => "`case int():`".into(),
        Ty::Bool => "`case bool():`".into(),
        Ty::Str => "`case str():`".into(),
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
            run("def f(x: float) -> None:\n    pass\n\ndef main() -> None:\n    pass\n"),
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
            ("def f(x: list[int]) -> None:\n    pass\n", vec!["E0405"]),
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
            "{defs}def f() -> None:\n    q = swap(Pair(first=1, second=\"x\"))\n    print(q.first)\n    print(q.second + 1)\n    r: Pair[str, int] = q\n"
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
        let ok = "def f() -> None:\n    v = parse_scalar(\"1\")\n    match v:\n        case int():\n            print(v + 1)\n        case str():\n            print(v)\n        case _:\n            pass\n    u: Unknown = 1\n    a = arg(0)\n    if a is not None:\n        print(a)\n    print(arg_count())\n";
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
