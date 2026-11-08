//! ホバーと定義ジャンプ（ADR-0026 論点 10・11、`docs/design/lsp.md` 4・5 章）。
//!
//! どちらも `wys_driver::frontend_for_editor` の解析の結果（構文木と型検査の結果）から、エントリのファイルの
//! バイト位置 `pos`（`SourceMap` の通しの位置。エントリのファイルは基準位置 0）について答える純粋な関数。

use wys_ast::{
    AttrElem, AttrValue, Expr, ExprKind, FuncDef, Ident, Pattern, PatternKind, Stmt, StmtKind,
    TypeDefValue, TypeExpr, TypeExprKind,
};
use wys_driver::{Analysis, Frontend};
use wys_span::Span;
use wys_typeck::{FuncInfo, FuncSig, Ty, TypeckResult};

fn contains(span: Span, pos: u32) -> bool {
    span.start <= pos && pos < span.end
}

/// ホバーの中身: コードブロックの 1 行と、短い説明
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hover {
    pub code: String,
    pub note: Option<String>,
    pub span: Span,
}

impl Hover {
    /// Markdown（1 つのコードブロックと説明の行）
    pub fn markdown(&self) -> String {
        match &self.note {
            Some(n) => format!("```wys\n{}\n```\n{n}", self.code),
            None => format!("```wys\n{}\n```", self.code),
        }
    }
}

/// 位置 `pos` のホバー。何もなければ `None`（キーワード・空白・コメント・文字列の中・型注釈の上など）
pub fn hover_at(fe: &Frontend, pos: u32) -> Option<Hover> {
    let a = fe.analysis.as_ref()?;
    // 空白（式の中の、カンマの後など）の上は出さない（Codex のレビュー）
    let file = fe.file.file_at(pos);
    let local = fe.file.position(pos).byte as usize;
    if file.text[local..]
        .chars()
        .next()
        .is_none_or(char::is_whitespace)
    {
        return None;
    }
    let tc = &a.typeck;
    let (fi, f) = a
        .module
        .funcs
        .iter()
        .enumerate()
        .find(|(_, f)| contains(f.span, pos))?;
    let info = &tc.funcs[fi];
    let sig = &tc.sigs[fi];
    if contains(f.name.span, pos) {
        return Some(signature_hover(a, fi, f.name.span));
    }
    for (pi, p) in f.params.iter().enumerate() {
        if contains(p.name.span, pos) {
            return Some(Hover {
                code: format!("{}: {}", p.name.name, ty(tc, sig.params[pi])?),
                note: Some("parameter".into()),
                span: p.name.span,
            });
        }
    }
    let mut found = Found::None;
    stmts(&f.body, pos, &mut found);
    match found {
        Found::None => None,
        Found::Binder(id) => local_hover(tc, info, &id),
        Found::Expr(e) => expr_hover(a, info, e, pos),
    }
}

enum Found<'a> {
    None,
    /// 代入の左辺・`for` の変数・`case` の束縛（宣言の位置）
    Binder(Ident),
    Expr(&'a Expr),
}

fn stmts<'a>(body: &'a [Stmt], pos: u32, out: &mut Found<'a>) {
    for s in body {
        if contains(s.span, pos) {
            stmt(s, pos, out);
            return;
        }
    }
}

fn stmt<'a>(s: &'a Stmt, pos: u32, out: &mut Found<'a>) {
    match &s.kind {
        StmtKind::Assign { target, value, .. } => {
            if contains(target.span, pos) {
                *out = Found::Binder(target.clone());
            } else {
                expr(value, pos, out);
            }
        }
        StmtKind::Expr(e) | StmtKind::Return(Some(e)) => expr(e, pos, out),
        StmtKind::Return(None) | StmtKind::Pass => {}
        StmtKind::If {
            branches,
            else_body,
        } => {
            for (c, b) in branches {
                expr(c, pos, out);
                stmts(b, pos, out);
            }
            if let Some(b) = else_body {
                stmts(b, pos, out);
            }
        }
        StmtKind::While { cond, body } => {
            expr(cond, pos, out);
            stmts(body, pos, out);
        }
        StmtKind::For { var, iter, body } => {
            if contains(var.span, pos) {
                *out = Found::Binder(var.clone());
            }
            expr(iter, pos, out);
            stmts(body, pos, out);
        }
        StmtKind::IndexAssign { target, value } => {
            expr(target, pos, out);
            expr(value, pos, out);
        }
        StmtKind::Match { subject, arms } => {
            expr(subject, pos, out);
            for arm in arms {
                if let Some(b) = binding(&arm.pattern)
                    && contains(b.span, pos)
                {
                    *out = Found::Binder(b.clone());
                }
                stmts(&arm.body, pos, out);
            }
        }
    }
}

fn binding(p: &Pattern) -> Option<&Ident> {
    match &p.kind {
        PatternKind::Bind(_, Some(b)) => Some(b),
        _ => None,
    }
}

/// `pos` を含む最も小さい式を探す（`span.start <= pos < span.end`）
fn expr<'a>(e: &'a Expr, pos: u32, out: &mut Found<'a>) {
    if !contains(e.span, pos) {
        return;
    }
    *out = Found::Expr(e);
    let mut sub = |c: &'a Expr| expr(c, pos, out);
    match &e.kind {
        ExprKind::Unary { operand, .. } => sub(operand),
        ExprKind::Binary { lhs, rhs, .. } | ExprKind::Is { lhs, rhs, .. } => {
            sub(lhs);
            sub(rhs);
        }
        ExprKind::Call { args, kwargs, .. } => {
            for a in args {
                sub(a);
            }
            for (_, v) in kwargs {
                sub(v);
            }
        }
        ExprKind::MethodCall {
            receiver,
            args,
            kwargs,
            ..
        } => {
            sub(receiver);
            for a in args {
                sub(a);
            }
            for (_, v) in kwargs {
                sub(v);
            }
        }
        ExprKind::List(items) => {
            for i in items {
                sub(i);
            }
        }
        ExprKind::Dict(items) => {
            for (k, v) in items {
                sub(k);
                sub(v);
            }
        }
        ExprKind::Index { base, index } => {
            sub(base);
            sub(index);
        }
        ExprKind::Try(i) | ExprKind::Await(i) => sub(i),
        ExprKind::Field { base, .. } => sub(base),
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Bool(_)
        | ExprKind::Str(_)
        | ExprKind::None
        | ExprKind::Name(_) => {}
    }
}

fn ty(tc: &TypeckResult, t: Ty) -> Option<String> {
    (t != Ty::Error).then(|| tc.types.name(t))
}

fn expr_ty(tc: &TypeckResult, e: &Expr) -> Option<String> {
    ty(tc, *tc.expr_types.get(e.id.0 as usize)?)
}

/// 宣言の位置のローカル変数・束縛
fn local_hover(tc: &TypeckResult, info: &FuncInfo, id: &Ident) -> Option<Hover> {
    let declared = info
        .locals
        .iter()
        .zip(&info.local_spans)
        .find(|(_, s)| **s == id.span)
        .map(|((_, t), _)| *t)
        .or_else(|| {
            info.bindings
                .iter()
                .find(|(_, _, s)| *s == id.span)
                .map(|(_, t, _)| *t)
        })
        // 再代入の左辺（宣言ではない）は、最初の宣言の型
        .or_else(|| {
            info.locals
                .iter()
                .find(|(n, _)| *n == id.name)
                .map(|(_, t)| *t)
        })?;
    Some(Hover {
        code: format!("{}: {}", id.name, ty(tc, declared)?),
        note: Some("local variable".into()),
        span: id.span,
    })
}

fn expr_hover(a: &Analysis, info: &FuncInfo, e: &Expr, pos: u32) -> Option<Hover> {
    let tc = &a.typeck;
    match &e.kind {
        ExprKind::Call { callee, .. } if contains(callee.span, pos) => {
            if let Some(i) = tc.sigs.iter().position(|s| s.name == callee.name) {
                return Some(signature_hover(a, i, callee.span));
            }
            if a.module.types.iter().any(|t| t.name.name == callee.name) {
                return Some(Hover {
                    code: format!("{}(...) -> {}", callee.name, expr_ty(tc, e)?),
                    note: Some("record".into()),
                    span: callee.span,
                });
            }
            // 組み込みの関数: 呼び出しの型と Effect
            let effects = wys_typeck::builtin_effects(&callee.name)
                .map(|e| e.names())
                .unwrap_or_default();
            let uses = if effects.is_empty() {
                String::new()
            } else {
                format!(" uses {}", effects.join(", "))
            };
            Some(Hover {
                code: format!("{}(...) -> {}{uses}", callee.name, expr_ty(tc, e)?),
                note: Some("built-in function".into()),
                span: callee.span,
            })
        }
        ExprKind::MethodCall { method, .. } if contains(method.span, pos) => Some(Hover {
            code: format!(".{}(...) -> {}", method.name, expr_ty(tc, e)?),
            note: Some("method".into()),
            span: method.span,
        }),
        ExprKind::Field { name, .. } if contains(name.span, pos) => Some(Hover {
            code: format!(".{}: {}", name.name, expr_ty(tc, e)?),
            note: Some("field".into()),
            span: name.span,
        }),
        ExprKind::Name(n) => {
            let now = expr_ty(tc, e)?;
            let declared = info
                .narrowings
                .iter()
                .find(|x| x.span == e.span)
                .map(|x| x.declared);
            let note = match declared.and_then(|d| ty(tc, d)) {
                Some(d) if d != now => Some(format!("declared as `{d}`, narrowed here")),
                _ => Some("variable".into()),
            };
            Some(Hover {
                code: format!("{n}: {now}"),
                note,
                span: e.span,
            })
        }
        // 文字列の中は出さない
        ExprKind::Str(_) => None,
        _ => Some(Hover {
            code: expr_ty(tc, e)?,
            note: None,
            span: e.span,
        }),
    }
}

/// 関数の署名（内部名。推論した Effect は `uses(inferred)`）
fn signature_hover(a: &Analysis, i: usize, span: Span) -> Hover {
    let tc = &a.typeck;
    let sig: &FuncSig = &tc.sigs[i];
    let f: &FuncDef = &a.module.funcs[i];
    let params: Vec<String> = f
        .params
        .iter()
        .zip(&sig.params)
        .map(|(p, t)| format!("{}: {}", p.name.name, tc.types.name(*t)))
        .collect();
    let type_params = if f.type_params.is_empty() {
        String::new()
    } else {
        let names: Vec<&str> = f.type_params.iter().map(|p| p.name.as_str()).collect();
        format!("[{}]", names.join(", "))
    };
    let uses = match (sig.effects, sig.inferred_effects) {
        (Some(e), _) => format!(" uses {}", effect_list(e)),
        (None, Some(e)) => format!(" uses(inferred) {}", effect_list(e)),
        (None, None) => String::new(),
    };
    let prefix = format!(
        "{}{}",
        if f.public { "pub " } else { "" },
        if f.is_async { "async " } else { "" }
    );
    let mut notes = Vec::new();
    if sig.tool.is_some() {
        notes.push("`@tool`".to_string());
    }
    if let Some(ag) = &sig.agent {
        notes.push(format!("`@agent` (tools: {})", ag.tools.join(", ")));
    }
    Hover {
        code: format!(
            "{prefix}def {}{type_params}({}) -> {}{uses}",
            sig.name,
            params.join(", "),
            tc.types.name(sig.ret)
        ),
        note: (!notes.is_empty()).then(|| notes.join(", ")),
        span,
    }
}

fn effect_list(e: wys_typeck::EffectSet) -> String {
    let names = e.names();
    if names.is_empty() {
        "pure".into()
    } else {
        names.join(", ")
    }
}

/// 位置 `pos` の名前の定義の位置（通しの位置。ほかのファイルのこともある）。組み込みなど定義がなければ `None`
pub fn definition_at(fe: &Frontend, pos: u32) -> Option<Span> {
    let a = fe.analysis.as_ref()?;
    let tc = &a.typeck;
    // import 文: モジュール名 → そのファイルの先頭、`from m import f` の名前 → `f` の定義
    for imp in &a.module.imports {
        if !contains(imp.span, pos) {
            continue;
        }
        let module = imp.module_name();
        if let Some(names) = &imp.names {
            for n in names {
                if contains(n.name.span, pos)
                    || n.alias.as_ref().is_some_and(|a| contains(a.span, pos))
                {
                    return named_def(a, &format!("{module}.{}", n.name.name));
                }
            }
        }
        return module_start(fe, &module);
    }
    for t in &a.module.types {
        if let Some(s) = type_ref(a, &t.value, pos) {
            return s;
        }
    }
    let (fi, f) = a.module.funcs.iter().enumerate().find(|(_, f)| {
        let start = f.decorators.first().map_or(f.span.start, |d| d.span.start);
        contains(Span::new(start.min(f.span.start), f.span.end), pos)
    })?;
    let info = &tc.funcs[fi];
    // デコレータの関数名（`@agent(tools=[add, m.f])`。デコレータは関数の範囲の前にある）
    for d in &f.decorators {
        for arg in &d.args {
            if let AttrValue::List(elems) = &arg.value {
                for el in elems {
                    if let AttrElem::Name(n, span) = el
                        && contains(*span, pos)
                    {
                        return named_def(a, n);
                    }
                }
            }
        }
    }
    if contains(f.name.span, pos) {
        return Some(f.name.span);
    }
    for p in &f.params {
        if contains(p.name.span, pos) {
            return Some(p.name.span);
        }
        if let Some(s) = type_expr_ref(a, &p.ty, pos) {
            return s;
        }
    }
    if let Some(s) = type_expr_ref(a, &f.ret, pos) {
        return s;
    }
    let mut found = Found::None;
    stmts(&f.body, pos, &mut found);
    if let Some(s) = body_type_ref(a, &f.body, pos) {
        return s;
    }
    match found {
        Found::None => None,
        Found::Binder(id) => local_def(info, &id.name, id.span.start).or(Some(id.span)),
        Found::Expr(e) => match &e.kind {
            ExprKind::Call { callee, .. } if contains(callee.span, pos) => {
                named_def(a, &callee.name)
            }
            ExprKind::Name(n) => local_def(info, n, e.span.start),
            // 呼び出しの型引数（`validate[Plan](u)`、`f[Plan](x)`。Codex のレビュー）
            ExprKind::Call { type_args, .. } | ExprKind::MethodCall { type_args, .. } => type_args
                .iter()
                .find_map(|t| type_expr_ref(a, t, pos))
                .flatten(),
            _ => None,
        },
    }
}

/// 名前 `n`（内部名）の関数か型の定義
fn named_def(a: &Analysis, n: &str) -> Option<Span> {
    if let Some(s) = a.typeck.sigs.iter().find(|s| s.name == n) {
        return Some(s.span);
    }
    a.module
        .types
        .iter()
        .find(|t| t.name.name == n)
        .map(|t| t.name.span)
}

/// ローカル変数・引数・束縛 `name` の宣言（`at` より前で最後のもの）
fn local_def(info: &FuncInfo, name: &str, at: u32) -> Option<Span> {
    info.locals
        .iter()
        .map(|(n, _)| n.as_str())
        .zip(info.local_spans.iter().copied())
        .chain(info.bindings.iter().map(|(n, _, s)| (n.as_str(), *s)))
        .filter(|(n, s)| *n == name && s.start <= at)
        .max_by_key(|(_, s)| s.start)
        .map(|(_, s)| s)
}

/// モジュール `a.b` のファイルの先頭（エントリと同じディレクトリの `a/b.wys`）
fn module_start(fe: &Frontend, module: &str) -> Option<Span> {
    let suffix = format!("/{}.wys", module.replace('.', "/"));
    let base = fe
        .file
        .base_of_file(|name| name.ends_with(&suffix) || name == &suffix[1..])?;
    Some(Span::new(base, base))
}

fn type_ref(a: &Analysis, v: &TypeDefValue, pos: u32) -> Option<Option<Span>> {
    match v {
        TypeDefValue::Alias(t) => type_expr_ref(a, t, pos),
        TypeDefValue::Record(fields) => fields.iter().find_map(|(_, t)| type_expr_ref(a, t, pos)),
    }
}

/// 型の注釈の中の名前 → `type` の定義（組み込みの型と型パラメータは `Some(None)`: 定義がない）
fn type_expr_ref(a: &Analysis, t: &TypeExpr, pos: u32) -> Option<Option<Span>> {
    if !contains(t.span, pos) {
        return None;
    }
    match &t.kind {
        TypeExprKind::Name(n) => Some(named_def(a, n)),
        TypeExprKind::Apply(n, args) => args
            .iter()
            .find_map(|x| type_expr_ref(a, x, pos))
            .or_else(|| Some(named_def(a, n))),
        TypeExprKind::Union(items) => items.iter().find_map(|x| type_expr_ref(a, x, pos)),
    }
}

/// 本体の中の型の注釈（`x: Plan = ...`、`f[Plan](...)`）
fn body_type_ref(a: &Analysis, body: &[Stmt], pos: u32) -> Option<Option<Span>> {
    for s in body {
        if !contains(s.span, pos) {
            continue;
        }
        return match &s.kind {
            StmtKind::Assign { ann: Some(t), .. } => type_expr_ref(a, t, pos),
            StmtKind::If {
                branches,
                else_body,
            } => branches
                .iter()
                .find_map(|(_, b)| body_type_ref(a, b, pos))
                .or_else(|| else_body.as_ref().and_then(|b| body_type_ref(a, b, pos))),
            StmtKind::While { body, .. } | StmtKind::For { body, .. } => {
                body_type_ref(a, body, pos)
            }
            StmtKind::Match { arms, .. } => {
                arms.iter().find_map(|arm| body_type_ref(a, &arm.body, pos))
            }
            _ => None,
        };
    }
    None
}
