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
    None,
    Union(UnionId),
    /// エラーから回復するための型。これを含む式では追加の診断を出さない。
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UnionId(pub u32);

/// Union 型の登録表。同じ構成要素の集合には同じ ID を返す。
#[derive(Debug, Clone, Default)]
pub struct Types {
    unions: Vec<Vec<Ty>>,
    index: HashMap<Vec<Ty>, UnionId>,
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

    pub fn name(&self, t: Ty) -> String {
        match t {
            Ty::Int => "int".into(),
            Ty::Bool => "bool".into(),
            Ty::Str => "str".into(),
            Ty::None => "None".into(),
            Ty::Error => "<error>".into(),
            Ty::Union(_) => self
                .members(t)
                .into_iter()
                .map(|m| self.name(m))
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

    /// `actual` の値をすべて `expected` の場所に置けるか。
    pub fn fits(&self, actual: Ty, expected: Ty) -> bool {
        if actual == Ty::Error || expected == Ty::Error {
            return true;
        }
        let e = self.members(expected);
        self.members(actual).iter().all(|a| e.contains(a))
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
}

impl TypeckResult {
    pub fn ty(&self, e: &Expr) -> Ty {
        self.expr_types[e.id.0 as usize]
    }
}

const BUILTINS: &[&str] = &["print", "exit"];

pub fn check(module: &Module) -> (TypeckResult, Vec<Diagnostic>) {
    let mut cx = Ctx {
        diags: Vec::new(),
        expr_types: vec![Ty::Error; module.expr_count as usize],
        sigs: Vec::new(),
        sig_index: HashMap::new(),
        types: Types::default(),
    };
    for f in &module.funcs {
        let params = f.params.iter().map(|p| cx.resolve(&p.ty)).collect();
        let ret = cx.resolve(&f.ret);
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
            params,
            ret,
            span: f.name.span,
        });
    }
    check_main(&mut cx, module);

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
        },
        cx.diags,
    )
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
}

impl Ctx {
    fn resolve(&mut self, t: &TypeExpr) -> Ty {
        match &t.kind {
            TypeExprKind::Name(name) => match name.as_str() {
                "int" => Ty::Int,
                "bool" => Ty::Bool,
                "str" => Ty::Str,
                "None" => Ty::None,
                other => {
                    self.diags.push(
                        Diagnostic::error("E0203", format!("unknown type `{other}`"), t.span)
                            .with_hint("supported types are `int`, `bool`, `str`, `None` and unions of them (`A | B`)"),
                    );
                    Ty::Error
                }
            },
            TypeExprKind::Union(items) => {
                let tys: Vec<Ty> = items.iter().map(|i| self.resolve(i)).collect();
                self.types.union(tys)
            }
        }
    }

    fn name(&self, t: Ty) -> String {
        self.types.name(t)
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
            ExprKind::Call { callee, args } => self.call(callee, args, e.span, env),
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
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::FloorDiv | BinOp::Mod => {
                    (l == Ty::Int && r == Ty::Int).then_some(Ty::Int)
                }
                BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                    (l == Ty::Int && r == Ty::Int).then_some(Ty::Bool)
                }
                BinOp::Eq | BinOp::Ne => {
                    (l == r && matches!(l, Ty::Int | Ty::Bool)).then_some(Ty::Bool)
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

    fn call(&mut self, callee: &Ident, args: &[Expr], span: Span, env: &Env) -> Ty {
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
            return sig.ret;
        }
        for ((arg, &at), &pt) in args.iter().zip(&arg_tys).zip(&sig.params) {
            let ctx = format!("argument of function `{}`", sig.name);
            self.cx.require(
                at,
                pt,
                arg.span,
                &ctx,
                Some((RelatedKind::Function, &sig.name, sig.span)),
            );
        }
        sig.ret
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
            StmtKind::Expr(_) | StmtKind::Return(_) | StmtKind::Pass => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
