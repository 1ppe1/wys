//! 型検査・型推論
//!
//! - 関数シグネチャは注釈必須、ローカル変数は最初の代入から推論する（LANG-002）。
//! - 暗黙の型変換はしない。条件式は `bool` でなければならない（LANG-007）。
//! - 代入されていない可能性のある変数の使用を拒否する（確定代入解析）。
//! - 値を返す関数は、すべての経路で `return` しなければならない。

use std::collections::{HashMap, HashSet};
use wys_ast::*;
use wys_diagnostics::{Diagnostic, RelatedKind};
use wys_span::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ty {
    Int,
    Bool,
    Str,
    None,
    /// エラーから回復するための型。これを含む式では追加の診断を出さない。
    Error,
}

impl Ty {
    pub fn name(self) -> &'static str {
        match self {
            Ty::Int => "int",
            Ty::Bool => "bool",
            Ty::Str => "str",
            Ty::None => "None",
            Ty::Error => "<error>",
        }
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
    /// 関数内のローカル変数（先頭は引数）。名前と型。
    pub locals: Vec<(String, Ty)>,
}

#[derive(Debug, Clone)]
pub struct TypeckResult {
    /// `ExprId` ごとの型。
    pub expr_types: Vec<Ty>,
    /// `module.funcs` と同じ順序。
    pub sigs: Vec<FuncSig>,
    pub funcs: Vec<FuncInfo>,
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
}

impl Ctx {
    fn resolve(&mut self, t: &TypeExpr) -> Ty {
        match t.name.as_str() {
            "int" => Ty::Int,
            "bool" => Ty::Bool,
            "str" => Ty::Str,
            "None" => Ty::None,
            other => {
                self.diags.push(
                    Diagnostic::error("E0203", format!("unknown type `{other}`"), t.span)
                        .with_hint("supported types are `int`, `bool`, `str` and `None`"),
                );
                Ty::Error
            }
        }
    }

    fn mismatch(&mut self, expected: Ty, found: Ty, span: Span, context: &str) -> Diagnostic {
        Diagnostic::error(
            "E0201",
            format!(
                "mismatched types: expected `{}`, found `{}`",
                expected.name(),
                found.name()
            ),
            span,
        )
        .with_cause(context.to_string())
    }
}

struct Var {
    ty: Ty,
    decl: Span,
}

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
        let mut assigned = HashSet::new();
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
            assigned.insert(p.name.name.clone());
        }
        let returns = ck.block(&f.body, &mut assigned);
        if !returns && sig.ret != Ty::None && sig.ret != Ty::Error {
            ck.cx.diags.push(
                Diagnostic::error(
                    "E0206",
                    format!(
                        "function `{}` may reach the end without returning a value",
                        f.name.name
                    ),
                    f.name.span,
                )
                .with_cause(format!("the return type is `{}`", sig.ret.name()))
                .with_hint("add a `return` at the end of the function"),
            );
        }
        FuncInfo { locals: ck.locals }
    }

    fn declare(&mut self, name: &str, ty: Ty, decl: Span) {
        self.vars.insert(name.to_string(), Var { ty, decl });
        self.locals.push((name.to_string(), ty));
    }

    /// 文の列を検査する。必ず `return` するなら true。
    fn block(&mut self, stmts: &[Stmt], assigned: &mut HashSet<String>) -> bool {
        let mut returns = false;
        for s in stmts {
            if self.stmt(s, assigned) {
                returns = true;
            }
        }
        returns
    }

    fn stmt(&mut self, s: &Stmt, assigned: &mut HashSet<String>) -> bool {
        match &s.kind {
            StmtKind::Pass => false,
            StmtKind::Expr(e) => {
                self.expr(e, assigned);
                false
            }
            StmtKind::Assign { target, ann, value } => {
                self.assign(target, ann.as_ref(), value, assigned);
                false
            }
            StmtKind::Return(value) => {
                self.ret_stmt(s.span, value.as_ref(), assigned);
                true
            }
            StmtKind::If {
                branches,
                else_body,
            } => {
                let mut outcomes: Vec<(bool, HashSet<String>)> = Vec::new();
                for (cond, body) in branches {
                    self.condition(cond, assigned);
                    let mut a = assigned.clone();
                    let r = self.block(body, &mut a);
                    outcomes.push((r, a));
                }
                match else_body {
                    Some(body) => {
                        let mut a = assigned.clone();
                        let r = self.block(body, &mut a);
                        outcomes.push((r, a));
                    }
                    None => outcomes.push((false, assigned.clone())),
                }
                let mut live = outcomes.into_iter().filter(|(r, _)| !r).map(|(_, a)| a);
                match live.next() {
                    None => true, // すべての分岐が return する
                    Some(first) => {
                        *assigned = live.fold(first, |acc, a| &acc & &a);
                        false
                    }
                }
            }
            StmtKind::While { cond, body } => {
                self.condition(cond, assigned);
                let mut a = assigned.clone();
                self.block(body, &mut a);
                false
            }
        }
    }

    fn condition(&mut self, cond: &Expr, assigned: &HashSet<String>) {
        let t = self.expr(cond, assigned);
        if t != Ty::Bool && t != Ty::Error {
            let d = self
                .cx
                .mismatch(Ty::Bool, t, cond.span, "a condition must be a `bool`")
                .with_hint(
                    "there is no implicit conversion to `bool`; compare explicitly, e.g. `x != 0`",
                );
            self.cx.diags.push(d);
        }
    }

    fn assign(
        &mut self,
        target: &Ident,
        ann: Option<&TypeExpr>,
        value: &Expr,
        assigned: &mut HashSet<String>,
    ) {
        let vt = self.expr(value, assigned);
        let ann_ty = ann.map(|a| self.cx.resolve(a));
        if let Some(var) = self.vars.get(&target.name) {
            let (decl_ty, decl) = (var.ty, var.decl);
            if let Some(at) = ann_ty
                && at != decl_ty
                && at != Ty::Error
                && decl_ty != Ty::Error
            {
                let d = self
                    .cx
                    .mismatch(
                        decl_ty,
                        at,
                        ann.expect("ann_ty implies ann").span,
                        "a variable keeps the type of its first assignment",
                    )
                    .with_related(RelatedKind::Variable, &target.name, Some(decl));
                self.cx.diags.push(d);
            } else if vt != decl_ty && vt != Ty::Error && decl_ty != Ty::Error {
                let d = self
                    .cx
                    .mismatch(
                        decl_ty,
                        vt,
                        value.span,
                        &format!(
                            "variable `{}` was declared as `{}`",
                            target.name,
                            decl_ty.name()
                        ),
                    )
                    .with_related(RelatedKind::Variable, &target.name, Some(decl));
                self.cx.diags.push(d);
            }
        } else {
            let ty = match ann_ty {
                Some(at) => {
                    if vt != at && vt != Ty::Error && at != Ty::Error {
                        let d = self.cx.mismatch(
                            at,
                            vt,
                            value.span,
                            &format!("variable `{}` is annotated as `{}`", target.name, at.name()),
                        );
                        self.cx.diags.push(d);
                    }
                    at
                }
                None => vt,
            };
            if ty == Ty::None {
                self.cx.diags.push(
                    Diagnostic::error(
                        "E0215",
                        format!("variable `{}` cannot hold `None`", target.name),
                        value.span,
                    )
                    .with_cause("`None` values cannot be stored in variables yet"),
                );
            }
            self.declare(&target.name, ty, target.span);
        }
        assigned.insert(target.name.clone());
    }

    fn ret_stmt(&mut self, span: Span, value: Option<&Expr>, assigned: &HashSet<String>) {
        match (self.ret, value) {
            (Ty::Error, Some(e)) => {
                self.expr(e, assigned);
            }
            (Ty::Error, None) => {}
            (Ty::None, None) => {}
            (Ty::None, Some(e)) => {
                let t = self.expr(e, assigned);
                if t != Ty::None && t != Ty::Error {
                    self.cx.diags.push(
                        Diagnostic::error(
                            "E0211",
                            format!(
                                "this function returns `None`, but a `{}` value is returned",
                                t.name()
                            ),
                            e.span,
                        )
                        .with_hint("change the return type annotation or remove the value"),
                    );
                }
            }
            (r, None) => {
                self.cx.diags.push(Diagnostic::error(
                    "E0211",
                    format!("this function must return a `{}` value", r.name()),
                    span,
                ));
            }
            (r, Some(e)) => {
                let t = self.expr(e, assigned);
                if t != r && t != Ty::Error {
                    let d = self.cx.mismatch(
                        r,
                        t,
                        e.span,
                        &format!("the function's return type is `{}`", r.name()),
                    );
                    self.cx.diags.push(d);
                }
            }
        }
    }

    fn expr(&mut self, e: &Expr, assigned: &HashSet<String>) -> Ty {
        let t = self.expr_inner(e, assigned);
        self.cx.expr_types[e.id.0 as usize] = t;
        t
    }

    fn expr_inner(&mut self, e: &Expr, assigned: &HashSet<String>) -> Ty {
        match &e.kind {
            ExprKind::Int(_) => Ty::Int,
            ExprKind::Bool(_) => Ty::Bool,
            ExprKind::Str(_) => Ty::Str,
            ExprKind::None => Ty::None,
            ExprKind::Name(name) => {
                if let Some(var) = self.vars.get(name) {
                    if !assigned.contains(name) {
                        let decl = var.decl;
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
                    }
                    var.ty
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
                let t = self.expr(operand, assigned);
                let (want, sym) = match op {
                    UnaryOp::Neg => (Ty::Int, "-"),
                    UnaryOp::Not => (Ty::Bool, "not"),
                };
                if t == Ty::Error {
                    return Ty::Error;
                }
                if t != want {
                    self.cx.diags.push(Diagnostic::error(
                        "E0207",
                        format!("operator `{sym}` cannot be applied to `{}`", t.name()),
                        e.span,
                    ));
                    return Ty::Error;
                }
                want
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let lt = self.expr(lhs, assigned);
                let rt = self.expr(rhs, assigned);
                self.binary(*op, lt, rt, e.span)
            }
            ExprKind::Call { callee, args } => self.call(callee, args, e.span, assigned),
        }
    }

    fn binary(&mut self, op: BinOp, lt: Ty, rt: Ty, span: Span) -> Ty {
        if lt == Ty::Error || rt == Ty::Error {
            return Ty::Error;
        }
        let result = match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::FloorDiv | BinOp::Mod => {
                (lt == Ty::Int && rt == Ty::Int).then_some(Ty::Int)
            }
            BinOp::Div => {
                self.cx.diags.push(
                    Diagnostic::error("E0207", "operator `/` is not supported yet", span)
                        .with_cause("`/` produces a float, and floats are not supported yet")
                        .with_hint("use `//` for integer (floor) division"),
                );
                return Ty::Error;
            }
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                (lt == Ty::Int && rt == Ty::Int).then_some(Ty::Bool)
            }
            BinOp::Eq | BinOp::Ne => {
                (lt == rt && matches!(lt, Ty::Int | Ty::Bool)).then_some(Ty::Bool)
            }
            BinOp::And | BinOp::Or => (lt == Ty::Bool && rt == Ty::Bool).then_some(Ty::Bool),
        };
        match result {
            Some(t) => t,
            None => {
                self.cx.diags.push(
                    Diagnostic::error(
                        "E0207",
                        format!(
                            "operator `{}` cannot be applied to `{}` and `{}`",
                            op.symbol(),
                            lt.name(),
                            rt.name()
                        ),
                        span,
                    )
                    .with_cause("there are no implicit conversions between types"),
                );
                Ty::Error
            }
        }
    }

    fn call(
        &mut self,
        callee: &Ident,
        args: &[Expr],
        span: Span,
        assigned: &HashSet<String>,
    ) -> Ty {
        let arg_tys: Vec<Ty> = args.iter().map(|a| self.expr(a, assigned)).collect();
        match callee.name.as_str() {
            "print" => {
                if args.len() != 1 {
                    self.cx.diags.push(Diagnostic::error(
                        "E0205",
                        format!("`print` takes 1 argument but {} were given", args.len()),
                        span,
                    ));
                } else if !matches!(arg_tys[0], Ty::Int | Ty::Bool | Ty::Str | Ty::Error) {
                    self.cx.diags.push(
                        Diagnostic::error(
                            "E0201",
                            format!("`print` cannot print a `{}` value", arg_tys[0].name()),
                            args[0].span,
                        )
                        .with_cause("`print` accepts `int`, `bool` or `str`"),
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
                } else if arg_tys[0] != Ty::Int && arg_tys[0] != Ty::Error {
                    let d = self.cx.mismatch(
                        Ty::Int,
                        arg_tys[0],
                        args[0].span,
                        "the exit status must be an `int`",
                    );
                    self.cx.diags.push(d);
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
            if at != pt && at != Ty::Error && pt != Ty::Error {
                let d = self
                    .cx
                    .mismatch(
                        pt,
                        at,
                        arg.span,
                        &format!("argument of function `{}`", sig.name),
                    )
                    .with_related(RelatedKind::Function, &sig.name, Some(sig.span));
                self.cx.diags.push(d);
            }
        }
        sig.ret
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
}
