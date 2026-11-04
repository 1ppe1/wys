//! 構文解析（トークン列から AST へ）
//!
//! 再帰下降パーサ。エラーが起きた文は読み飛ばして次の文から再開する（同期）。

use wys_ast::*;
use wys_diagnostics::Diagnostic;
use wys_lexer::{Token, TokenKind};
use wys_span::Span;

/// ソース文字列を字句解析・構文解析する。
pub fn parse_source(src: &str) -> (Module, Vec<Diagnostic>) {
    let (tokens, mut diags) = wys_lexer::lex(src);
    let (module, pdiags) = parse(&tokens);
    diags.extend(pdiags);
    (module, diags)
}

/// トークン列を構文解析する。`tokens` は `Eof` で終わっていなければならない。
pub fn parse(tokens: &[Token]) -> (Module, Vec<Diagnostic>) {
    let mut p = Parser {
        toks: tokens,
        pos: 0,
        diags: Vec::new(),
        next_expr: 0,
    };
    let funcs = p.module();
    (
        Module {
            funcs,
            expr_count: p.next_expr,
        },
        p.diags,
    )
}

/// エラー時の値。診断は発生時点で `diags` に積まれている。
struct Reported;
type PResult<T> = Result<T, Reported>;

struct Parser<'t> {
    toks: &'t [Token],
    pos: usize,
    diags: Vec<Diagnostic>,
    next_expr: u32,
}

impl Parser<'_> {
    // ---- token helpers ----

    fn peek(&self) -> &TokenKind {
        &self.toks[self.pos].kind
    }

    fn peek_at(&self, n: usize) -> &TokenKind {
        let i = (self.pos + n).min(self.toks.len() - 1);
        &self.toks[i].kind
    }

    fn span(&self) -> Span {
        self.toks[self.pos].span
    }

    fn prev_end(&self) -> u32 {
        if self.pos == 0 {
            0
        } else {
            self.toks[self.pos - 1].span.end
        }
    }

    fn bump(&mut self) -> &Token {
        let t = &self.toks[self.pos];
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }

    fn at(&self, k: &TokenKind) -> bool {
        self.peek() == k
    }

    fn eat(&mut self, k: &TokenKind) -> bool {
        if self.at(k) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn error_here(&mut self, expected: &str) -> Reported {
        let found = self.peek().describe();
        let d = Diagnostic::error(
            "E0101",
            format!("expected {expected}, found {found}"),
            self.span(),
        );
        self.diags.push(d);
        Reported
    }

    fn expect(&mut self, k: &TokenKind, expected: &str) -> PResult<Span> {
        if self.at(k) {
            Ok(self.bump().span)
        } else {
            Err(self.error_here(expected))
        }
    }

    fn ident(&mut self, what: &str) -> PResult<Ident> {
        if let TokenKind::Ident(name) = self.peek() {
            let name = name.clone();
            let span = self.bump().span;
            Ok(Ident { name, span })
        } else {
            Err(self.error_here(what))
        }
    }

    /// 文の途中でエラーになったとき、次の文の先頭まで読み飛ばす。
    /// 行末の後ろにインデントされたブロックが続く場合は、そのブロックも読み飛ばす。
    fn sync(&mut self) {
        let mut depth = 0usize;
        loop {
            match self.peek() {
                TokenKind::Eof => return,
                TokenKind::Indent => depth += 1,
                TokenKind::Dedent => {
                    if depth == 0 {
                        return;
                    }
                    depth -= 1;
                }
                TokenKind::Newline if depth == 0 => {
                    self.bump();
                    if self.at(&TokenKind::Indent) {
                        self.skip_block();
                    }
                    return;
                }
                _ => {}
            }
            self.bump();
        }
    }

    /// 現在位置の `Indent` から対応する `Dedent` までを読み飛ばす。
    fn skip_block(&mut self) {
        let mut depth = 0usize;
        loop {
            match self.peek() {
                TokenKind::Eof => return,
                TokenKind::Indent => depth += 1,
                TokenKind::Dedent => {
                    depth -= 1;
                    if depth == 0 {
                        self.bump();
                        return;
                    }
                }
                _ => {}
            }
            self.bump();
        }
    }

    // ---- module / definitions ----

    fn module(&mut self) -> Vec<FuncDef> {
        let mut funcs = Vec::new();
        loop {
            match self.peek() {
                TokenKind::Eof => break,
                TokenKind::Newline => {
                    self.bump();
                }
                TokenKind::Def => match self.func_def() {
                    Ok(f) => funcs.push(f),
                    Err(Reported) => self.sync(),
                },
                TokenKind::Indent => {
                    self.diags
                        .push(Diagnostic::error("E0101", "unexpected indent", self.span()));
                    self.skip_block();
                }
                TokenKind::Dedent => {
                    self.bump();
                }
                _ => {
                    self.diags.push(
                        Diagnostic::error(
                            "E0106",
                            "statements are not allowed at the top level",
                            self.span(),
                        )
                        .with_hint("move this statement into `def main() -> None:`"),
                    );
                    self.sync();
                }
            }
        }
        funcs
    }

    fn type_expr(&mut self) -> PResult<TypeExpr> {
        let first = self.type_atom()?;
        if !self.at(&TokenKind::Pipe) {
            return Ok(first);
        }
        let start = first.span.start;
        let mut items = vec![first];
        while self.eat(&TokenKind::Pipe) {
            items.push(self.type_atom()?);
        }
        Ok(TypeExpr {
            kind: TypeExprKind::Union(items),
            span: Span::new(start, self.prev_end()),
        })
    }

    fn type_atom(&mut self) -> PResult<TypeExpr> {
        let name = match self.peek().clone() {
            TokenKind::Ident(name) => name,
            TokenKind::None => "None".to_string(),
            _ => return Err(self.error_here("a type")),
        };
        let span = self.bump().span;
        Ok(TypeExpr {
            kind: TypeExprKind::Name(name),
            span,
        })
    }

    fn func_def(&mut self) -> PResult<FuncDef> {
        let start = self.expect(&TokenKind::Def, "`def`")?.start;
        let name = self.ident("a function name")?;
        self.expect(&TokenKind::LParen, "`(`")?;
        let mut params = Vec::new();
        if !self.at(&TokenKind::RParen) {
            loop {
                let pname = self.ident("a parameter name")?;
                if !self.eat(&TokenKind::Colon) {
                    self.diags.push(
                        Diagnostic::error(
                            "E0103",
                            format!("parameter `{}` needs a type annotation", pname.name),
                            pname.span,
                        )
                        .with_fix(
                            "add a type annotation",
                            Span::new(pname.span.end, pname.span.end),
                            ": int",
                        ),
                    );
                    return Err(Reported);
                }
                let ty = self.type_expr()?;
                params.push(Param { name: pname, ty });
                if self.eat(&TokenKind::Comma) {
                    if self.at(&TokenKind::RParen) {
                        break;
                    }
                    continue;
                }
                break;
            }
        }
        let rparen = self.expect(&TokenKind::RParen, "`,` or `)`")?;
        if !self.eat(&TokenKind::Arrow) {
            self.diags.push(
                Diagnostic::error(
                    "E0104",
                    format!("function `{}` needs a return type annotation", name.name),
                    name.span,
                )
                .with_fix(
                    "add `-> None` if the function returns nothing",
                    Span::new(rparen.end, rparen.end),
                    " -> None",
                ),
            );
            return Err(Reported);
        }
        let ret = self.type_expr()?;
        self.expect(&TokenKind::Colon, "`:`")?;
        let body = self.block()?;
        Ok(FuncDef {
            name,
            params,
            ret,
            body,
            span: Span::new(start, self.prev_end()),
        })
    }

    fn block(&mut self) -> PResult<Vec<Stmt>> {
        self.expect(&TokenKind::Newline, "a new line after `:`")?;
        if !self.at(&TokenKind::Indent) {
            return Err(self.error_here("an indented block"));
        }
        self.bump();
        let mut stmts = Vec::new();
        while !matches!(self.peek(), TokenKind::Dedent | TokenKind::Eof) {
            match self.stmt() {
                Ok(s) => stmts.push(s),
                Err(Reported) => self.sync(),
            }
        }
        self.eat(&TokenKind::Dedent);
        Ok(stmts)
    }

    // ---- statements ----

    fn stmt(&mut self) -> PResult<Stmt> {
        let start = self.span().start;
        let kind = match self.peek() {
            TokenKind::Def => {
                self.diags.push(Diagnostic::error(
                    "E0105",
                    "nested function definitions are not supported yet",
                    self.span(),
                ));
                return Err(Reported);
            }
            TokenKind::Indent => {
                self.diags
                    .push(Diagnostic::error("E0101", "unexpected indent", self.span()));
                self.skip_block();
                return Err(Reported);
            }
            TokenKind::If => return self.if_stmt(),
            TokenKind::Match => return self.match_stmt(),
            TokenKind::While => {
                self.bump();
                let cond = self.expr()?;
                self.expect(&TokenKind::Colon, "`:`")?;
                let body = self.block()?;
                return Ok(Stmt {
                    kind: StmtKind::While { cond, body },
                    span: Span::new(start, self.prev_end()),
                });
            }
            TokenKind::Pass => {
                self.bump();
                StmtKind::Pass
            }
            TokenKind::Return => {
                self.bump();
                if self.at(&TokenKind::Newline) {
                    StmtKind::Return(None)
                } else {
                    StmtKind::Return(Some(self.expr()?))
                }
            }
            TokenKind::Ident(_)
                if matches!(self.peek_at(1), TokenKind::Assign | TokenKind::Colon) =>
            {
                let target = self.ident("a variable name")?;
                let ann = if self.eat(&TokenKind::Colon) {
                    Some(self.type_expr()?)
                } else {
                    None
                };
                self.expect(&TokenKind::Assign, "`=`")?;
                let value = self.expr()?;
                StmtKind::Assign { target, ann, value }
            }
            _ => StmtKind::Expr(self.expr()?),
        };
        let end = self.prev_end();
        self.expect(&TokenKind::Newline, "end of line")?;
        Ok(Stmt {
            kind,
            span: Span::new(start, end),
        })
    }

    fn if_stmt(&mut self) -> PResult<Stmt> {
        let start = self.expect(&TokenKind::If, "`if`")?.start;
        let mut branches = Vec::new();
        let cond = self.expr()?;
        self.expect(&TokenKind::Colon, "`:`")?;
        branches.push((cond, self.block()?));
        let mut else_body = None;
        loop {
            if self.eat(&TokenKind::Elif) {
                let cond = self.expr()?;
                self.expect(&TokenKind::Colon, "`:`")?;
                branches.push((cond, self.block()?));
            } else if self.eat(&TokenKind::Else) {
                self.expect(&TokenKind::Colon, "`:`")?;
                else_body = Some(self.block()?);
                break;
            } else {
                break;
            }
        }
        Ok(Stmt {
            kind: StmtKind::If {
                branches,
                else_body,
            },
            span: Span::new(start, self.prev_end()),
        })
    }

    fn match_stmt(&mut self) -> PResult<Stmt> {
        let start = self.expect(&TokenKind::Match, "`match`")?.start;
        let subject = self.expr()?;
        self.expect(&TokenKind::Colon, "`:`")?;
        self.expect(&TokenKind::Newline, "a new line after `:`")?;
        if !self.at(&TokenKind::Indent) {
            return Err(self.error_here("an indented block of `case` clauses"));
        }
        self.bump();
        let mut arms = Vec::new();
        while !matches!(self.peek(), TokenKind::Dedent | TokenKind::Eof) {
            match self.match_arm() {
                Ok(arm) => arms.push(arm),
                // 1つの case の誤りで match 全体を読み飛ばさない
                Err(Reported) => self.sync(),
            }
        }
        self.eat(&TokenKind::Dedent);
        Ok(Stmt {
            kind: StmtKind::Match { subject, arms },
            span: Span::new(start, self.prev_end()),
        })
    }

    fn match_arm(&mut self) -> PResult<MatchArm> {
        let start = self.expect(&TokenKind::Case, "`case`")?.start;
        let pattern = self.pattern()?;
        self.expect(&TokenKind::Colon, "`:`")?;
        let body = self.block()?;
        Ok(MatchArm {
            pattern,
            body,
            span: Span::new(start, self.prev_end()),
        })
    }

    fn pattern(&mut self) -> PResult<Pattern> {
        let start = self.span().start;
        let kind = match self.peek().clone() {
            TokenKind::None => {
                self.bump();
                PatternKind::None
            }
            TokenKind::Ident(name) if name == "_" => {
                self.bump();
                PatternKind::Wildcard
            }
            TokenKind::Ident(name) => {
                self.bump();
                self.expect(
                    &TokenKind::LParen,
                    "`(` (type patterns are written like `int()`)",
                )?;
                self.expect(&TokenKind::RParen, "`)`")?;
                PatternKind::Type(name)
            }
            _ => {
                return Err(
                    self.error_here("a pattern (`int()`, `bool()`, `str()`, `None` or `_`)")
                );
            }
        };
        Ok(Pattern {
            kind,
            span: Span::new(start, self.prev_end()),
        })
    }

    // ---- expressions ----

    fn mk(&mut self, kind: ExprKind, start: u32) -> Expr {
        let id = ExprId(self.next_expr);
        self.next_expr += 1;
        Expr {
            id,
            kind,
            span: Span::new(start, self.prev_end()),
        }
    }

    fn binary(&mut self, op: BinOp, lhs: Expr, rhs: Expr) -> Expr {
        let start = lhs.span.start;
        self.mk(
            ExprKind::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
            start,
        )
    }

    fn expr(&mut self) -> PResult<Expr> {
        self.or_expr()
    }

    fn or_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.and_expr()?;
        while self.eat(&TokenKind::Or) {
            let rhs = self.and_expr()?;
            lhs = self.binary(BinOp::Or, lhs, rhs);
        }
        Ok(lhs)
    }

    fn and_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.not_expr()?;
        while self.eat(&TokenKind::And) {
            let rhs = self.not_expr()?;
            lhs = self.binary(BinOp::And, lhs, rhs);
        }
        Ok(lhs)
    }

    fn not_expr(&mut self) -> PResult<Expr> {
        if self.at(&TokenKind::Not) {
            let start = self.bump().span.start;
            let operand = self.not_expr()?;
            return Ok(self.mk(
                ExprKind::Unary {
                    op: UnaryOp::Not,
                    operand: Box::new(operand),
                },
                start,
            ));
        }
        self.comparison()
    }

    fn cmp_op(&self) -> Option<BinOp> {
        Some(match self.peek() {
            TokenKind::EqEq => BinOp::Eq,
            TokenKind::NotEq => BinOp::Ne,
            TokenKind::Lt => BinOp::Lt,
            TokenKind::Le => BinOp::Le,
            TokenKind::Gt => BinOp::Gt,
            TokenKind::Ge => BinOp::Ge,
            _ => return None,
        })
    }

    fn comparison(&mut self) -> PResult<Expr> {
        let lhs = self.additive()?;
        if self.eat(&TokenKind::Is) {
            let negated = self.eat(&TokenKind::Not);
            let rhs = self.additive()?;
            if self.cmp_op().is_some() || self.at(&TokenKind::Is) {
                self.diags.push(
                    Diagnostic::error(
                        "E0102",
                        "chained comparisons are not supported",
                        Span::new(lhs.span.start, self.span().end),
                    )
                    .with_hint("split it with `and`"),
                );
                return Err(Reported);
            }
            let start = lhs.span.start;
            return Ok(self.mk(
                ExprKind::Is {
                    lhs: Box::new(lhs),
                    negated,
                    rhs: Box::new(rhs),
                },
                start,
            ));
        }
        let Some(op) = self.cmp_op() else {
            return Ok(lhs);
        };
        self.bump();
        let rhs = self.additive()?;
        if self.cmp_op().is_some() || self.at(&TokenKind::Is) {
            self.diags.push(
                Diagnostic::error(
                    "E0102",
                    "chained comparisons are not supported",
                    Span::new(lhs.span.start, self.span().end),
                )
                .with_hint("split it with `and`, e.g. `a < b and b < c`"),
            );
            return Err(Reported);
        }
        Ok(self.binary(op, lhs, rhs))
    }

    fn additive(&mut self) -> PResult<Expr> {
        let mut lhs = self.term()?;
        loop {
            let op = match self.peek() {
                TokenKind::Plus => BinOp::Add,
                TokenKind::Minus => BinOp::Sub,
                _ => return Ok(lhs),
            };
            self.bump();
            let rhs = self.term()?;
            lhs = self.binary(op, lhs, rhs);
        }
    }

    fn term(&mut self) -> PResult<Expr> {
        let mut lhs = self.unary()?;
        loop {
            let op = match self.peek() {
                TokenKind::Star => BinOp::Mul,
                TokenKind::Slash => BinOp::Div,
                TokenKind::SlashSlash => BinOp::FloorDiv,
                TokenKind::Percent => BinOp::Mod,
                _ => return Ok(lhs),
            };
            self.bump();
            let rhs = self.unary()?;
            lhs = self.binary(op, lhs, rhs);
        }
    }

    fn unary(&mut self) -> PResult<Expr> {
        if self.at(&TokenKind::Minus) {
            let start = self.bump().span.start;
            let operand = self.unary()?;
            return Ok(self.mk(
                ExprKind::Unary {
                    op: UnaryOp::Neg,
                    operand: Box::new(operand),
                },
                start,
            ));
        }
        self.primary()
    }

    fn primary(&mut self) -> PResult<Expr> {
        let start = self.span().start;
        let kind = match self.peek().clone() {
            TokenKind::Int(v) => {
                self.bump();
                ExprKind::Int(v)
            }
            TokenKind::Str(s) => {
                self.bump();
                ExprKind::Str(s)
            }
            TokenKind::True => {
                self.bump();
                ExprKind::Bool(true)
            }
            TokenKind::False => {
                self.bump();
                ExprKind::Bool(false)
            }
            TokenKind::None => {
                self.bump();
                ExprKind::None
            }
            TokenKind::Ident(name) => {
                let span = self.bump().span;
                if self.eat(&TokenKind::LParen) {
                    let mut args = Vec::new();
                    while !self.at(&TokenKind::RParen) {
                        args.push(self.expr()?);
                        if !self.eat(&TokenKind::Comma) {
                            break;
                        }
                    }
                    self.expect(&TokenKind::RParen, "`,` or `)`")?;
                    ExprKind::Call {
                        callee: Ident { name, span },
                        args,
                    }
                } else {
                    ExprKind::Name(name)
                }
            }
            TokenKind::LParen => {
                self.bump();
                let inner = self.expr()?;
                self.expect(&TokenKind::RParen, "`)`")?;
                // 括弧は範囲だけ広げ、式そのものは中身を使う
                return Ok(Expr {
                    span: Span::new(start, self.prev_end()),
                    ..inner
                });
            }
            _ => return Err(self.error_here("an expression")),
        };
        Ok(self.mk(kind, start))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(src: &str) -> Module {
        let (m, d) = parse_source(src);
        assert!(d.is_empty(), "unexpected diagnostics: {d:?}");
        m
    }

    fn codes(src: &str) -> Vec<&'static str> {
        parse_source(src).1.into_iter().map(|d| d.code).collect()
    }

    #[test]
    fn parses_function_with_control_flow() {
        let m = ok(
            "def f(n: int) -> int:\n    i = 0\n    while i < n:\n        i = i + 1\n    if i == n:\n        return i\n    elif i > n:\n        return 0\n    else:\n        return -1\n",
        );
        assert_eq!(m.funcs.len(), 1);
        let f = &m.funcs[0];
        assert_eq!(f.name.name, "f");
        assert_eq!(f.params.len(), 1);
        assert_eq!(f.body.len(), 3);
        match &f.body[2].kind {
            StmtKind::If {
                branches,
                else_body,
            } => {
                assert_eq!(branches.len(), 2);
                assert!(else_body.is_some());
            }
            other => panic!("expected if, got {other:?}"),
        }
    }

    #[test]
    fn precedence() {
        let m = ok("def f() -> int:\n    return 1 + 2 * -3\n");
        let StmtKind::Return(Some(e)) = &m.funcs[0].body[0].kind else {
            panic!()
        };
        let ExprKind::Binary { op, rhs, .. } = &e.kind else {
            panic!()
        };
        assert_eq!(*op, BinOp::Add);
        assert!(matches!(rhs.kind, ExprKind::Binary { op: BinOp::Mul, .. }));
    }

    #[test]
    fn union_types_and_is_none() {
        let m = ok(
            "def f(x: int | None) -> str | None:\n    if x is not None:\n        return None\n    return None\n",
        );
        let f = &m.funcs[0];
        assert!(matches!(&f.params[0].ty.kind, TypeExprKind::Union(items) if items.len() == 2));
        let StmtKind::If { branches, .. } = &f.body[0].kind else {
            panic!()
        };
        assert!(matches!(
            branches[0].0.kind,
            ExprKind::Is { negated: true, .. }
        ));
    }

    #[test]
    fn match_statement() {
        let m = ok(
            "def f(x: int | None) -> None:\n    match x:\n        case int():\n            pass\n        case None:\n            pass\n        case _:\n            pass\n",
        );
        let StmtKind::Match { arms, .. } = &m.funcs[0].body[0].kind else {
            panic!()
        };
        let kinds: Vec<_> = arms.iter().map(|a| a.pattern.kind.clone()).collect();
        assert_eq!(
            kinds,
            vec![
                PatternKind::Type("int".into()),
                PatternKind::None,
                PatternKind::Wildcard
            ]
        );
    }

    #[test]
    fn match_recovers_per_case() {
        let src = "def f(x: int) -> None:\n    match x:\n        case 1:\n            pass\n        case int():\n            pass\n    pass\n";
        let (m, d) = parse_source(src);
        assert_eq!(d.iter().map(|d| d.code).collect::<Vec<_>>(), vec!["E0101"]);
        let StmtKind::Match { arms, .. } = &m.funcs[0].body[0].kind else {
            panic!()
        };
        assert_eq!(arms.len(), 1);
        assert_eq!(m.funcs[0].body.len(), 2);
    }

    #[test]
    fn expr_ids_are_unique() {
        let m = ok("def f() -> None:\n    print(1 + 2)\n");
        assert_eq!(m.expr_count, 4);
    }

    #[test]
    fn error_codes() {
        assert_eq!(codes("x = 1\n"), vec!["E0106"]);
        assert_eq!(codes("def f(x) -> None:\n    pass\n"), vec!["E0103"]);
        assert_eq!(codes("def f():\n    pass\n"), vec!["E0104"]);
        assert_eq!(
            codes("def f() -> None:\n    def g() -> None:\n        pass\n"),
            vec!["E0105"]
        );
        assert_eq!(
            codes("def f() -> bool:\n    return 1 < 2 < 3\n"),
            vec!["E0102"]
        );
        assert_eq!(codes("def f() -> None:\n    x = \n"), vec!["E0101"]);
    }

    #[test]
    fn recovers_and_reports_multiple_errors() {
        let src = "def f() -> None:\n    x = )\n    y = +\n\ndef g(a) -> None:\n    pass\n";
        let c = codes(src);
        assert_eq!(c, vec!["E0101", "E0101", "E0103"]);
    }
}
