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

/// ソースマップの基準位置 `base` に置かれたファイルを解析する（位置は通し番号になる）。
/// 式の ID は `first_expr` から振る（複数のモジュールを1つにまとめても重ならないように）。
pub fn parse_source_at(src: &str, base: u32, first_expr: u32) -> (Module, Vec<Diagnostic>) {
    let shift = |s: Span| Span::new(s.start + base, s.end + base);
    let (mut tokens, mut diags) = wys_lexer::lex(src);
    for t in &mut tokens {
        t.span = shift(t.span);
    }
    for d in &mut diags {
        d.span = shift(d.span);
    }
    let (module, pdiags) = parse_from(&tokens, first_expr);
    diags.extend(pdiags);
    (module, diags)
}

/// トークン列を構文解析する。`tokens` は `Eof` で終わっていなければならない。
pub fn parse(tokens: &[Token]) -> (Module, Vec<Diagnostic>) {
    parse_from(tokens, 0)
}

fn parse_from(tokens: &[Token], first_expr: u32) -> (Module, Vec<Diagnostic>) {
    let mut p = Parser {
        toks: tokens,
        pos: 0,
        diags: Vec::new(),
        next_expr: first_expr,
    };
    let (imports, types, funcs) = p.module();
    (
        Module {
            imports,
            types,
            funcs,
            expr_count: p.next_expr,
        },
        p.diags,
    )
}

/// 呼び出しの位置引数とキーワード引数。
type CallArgs = (Vec<Expr>, Vec<(Ident, Expr)>);

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

    fn module(&mut self) -> (Vec<Import>, Vec<TypeDef>, Vec<FuncDef>) {
        let mut imports = Vec::new();
        let mut types = Vec::new();
        let mut funcs = Vec::new();
        loop {
            let public = matches!(self.peek(), TokenKind::Pub)
                && matches!(self.peek_at(1), TokenKind::Def | TokenKind::Ident(_));
            if public {
                self.bump();
            }
            match self.peek() {
                TokenKind::Eof => break,
                TokenKind::Import | TokenKind::From if !public => match self.import() {
                    Ok(i) => imports.push(i),
                    Err(Reported) => self.sync(),
                },
                // `type` は予約語ではない。トップレベルの `type Name =` のときだけ型定義として扱う
                TokenKind::Ident(kw)
                    if kw == "type"
                        && matches!(self.peek_at(1), TokenKind::Ident(_))
                        && matches!(self.peek_at(2), TokenKind::Assign | TokenKind::LBracket) =>
                {
                    match self.type_def() {
                        Ok(mut t) => {
                            t.public = public;
                            types.push(t);
                        }
                        Err(Reported) => self.sync(),
                    }
                }
                TokenKind::Newline => {
                    self.bump();
                }
                TokenKind::Def => match self.func_def() {
                    Ok(mut f) => {
                        f.public = public;
                        funcs.push(f);
                    }
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
        (imports, types, funcs)
    }

    /// `import a.b [as c]` / `from a.b import f [as g], ...`
    fn import(&mut self) -> PResult<Import> {
        let start = self.span().start;
        let from = self.eat(&TokenKind::From);
        if !from {
            self.expect(&TokenKind::Import, "`import`")?;
        }
        let mut module = vec![self.ident("a module name")?];
        while self.eat(&TokenKind::Dot) {
            module.push(self.ident("a module name")?);
        }
        let (alias, names) = if from {
            self.expect(&TokenKind::Import, "`import`")?;
            let mut names = vec![self.import_name()?];
            while self.eat(&TokenKind::Comma) {
                names.push(self.import_name()?);
            }
            (None, Some(names))
        } else if self.eat(&TokenKind::As) {
            (Some(self.ident("a module alias")?), None)
        } else {
            (None, None)
        };
        let end = self.prev_end();
        self.expect(&TokenKind::Newline, "end of line")?;
        Ok(Import {
            module,
            alias,
            names,
            span: Span::new(start, end),
        })
    }

    fn import_name(&mut self) -> PResult<ImportName> {
        let name = self.ident("a name to import")?;
        let alias = if self.eat(&TokenKind::As) {
            Some(self.ident("an alias")?)
        } else {
            None
        };
        Ok(ImportName { name, alias })
    }

    fn type_def(&mut self) -> PResult<TypeDef> {
        let start = self.bump().span.start; // `type`
        let name = self.ident("a type name")?;
        let params = self.type_params()?;
        self.expect(&TokenKind::Assign, "`=`")?;
        let value = if self.eat(&TokenKind::LBrace) {
            let mut fields = Vec::new();
            while !self.at(&TokenKind::RBrace) {
                let fname = self.ident("a field name")?;
                self.expect(&TokenKind::Colon, "`:` and the field type")?;
                let ty = self.type_expr()?;
                fields.push((fname, ty));
                if !self.eat(&TokenKind::Comma) {
                    break;
                }
            }
            self.expect(&TokenKind::RBrace, "`,` or `}`")?;
            TypeDefValue::Record(fields)
        } else {
            TypeDefValue::Alias(self.type_expr()?)
        };
        let end = self.prev_end();
        self.expect(&TokenKind::Newline, "end of line")?;
        Ok(TypeDef {
            public: false,
            name,
            params,
            value,
            span: Span::new(start, end),
        })
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

    /// `[T, U]`（省略可）。
    fn type_params(&mut self) -> PResult<Vec<Ident>> {
        let mut params = Vec::new();
        if self.eat(&TokenKind::LBracket) {
            loop {
                params.push(self.ident("a type parameter name")?);
                if !self.eat(&TokenKind::Comma) || self.at(&TokenKind::RBracket) {
                    break;
                }
            }
            self.expect(&TokenKind::RBracket, "`,` or `]`")?;
        }
        Ok(params)
    }

    fn type_atom(&mut self) -> PResult<TypeExpr> {
        let name = match self.peek().clone() {
            TokenKind::Ident(name) => name,
            TokenKind::None => "None".to_string(),
            _ => return Err(self.error_here("a type")),
        };
        let mut span = self.bump().span;
        let mut name = name;
        // `m.Point` のような修飾付きの型名（ADR-0010）
        while matches!(self.peek(), TokenKind::Dot)
            && matches!(self.peek_at(1), TokenKind::Ident(_))
        {
            self.bump();
            let part = self.ident("a type name")?;
            name = format!("{name}.{}", part.name);
            span = Span::new(span.start, part.span.end);
        }
        if self.eat(&TokenKind::LBracket) {
            let mut args = Vec::new();
            loop {
                args.push(self.type_expr()?);
                if !self.eat(&TokenKind::Comma) || self.at(&TokenKind::RBracket) {
                    break;
                }
            }
            self.expect(&TokenKind::RBracket, "`,` or `]`")?;
            return Ok(TypeExpr {
                kind: TypeExprKind::Apply(name, args),
                span: Span::new(span.start, self.prev_end()),
            });
        }
        Ok(TypeExpr {
            kind: TypeExprKind::Name(name),
            span,
        })
    }

    fn func_def(&mut self) -> PResult<FuncDef> {
        let start = self.expect(&TokenKind::Def, "`def`")?.start;
        let name = self.ident("a function name")?;
        let type_params = self.type_params()?;
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
        // `uses` 節（ADR-0014）。`uses` はこの位置でだけ意味を持つソフトキーワード
        let effects = if matches!(self.peek(), TokenKind::Ident(n) if n == "uses") {
            let uses = self.bump().span;
            let mut names = vec![self.ident("an effect name")?];
            while self.eat(&TokenKind::Comma) {
                names.push(self.ident("an effect name")?);
            }
            Some(EffectClause {
                names,
                span: Span::new(uses.start, self.prev_end()),
            })
        } else {
            None
        };
        self.expect(&TokenKind::Colon, "`:`")?;
        let body = self.block()?;
        Ok(FuncDef {
            public: false,
            name,
            type_params,
            params,
            ret,
            effects,
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
            TokenKind::For => {
                self.bump();
                let var = self.ident("a loop variable name")?;
                self.expect(&TokenKind::In, "`in`")?;
                let iter = self.expr()?;
                self.expect(&TokenKind::Colon, "`:`")?;
                let body = self.block()?;
                return Ok(Stmt {
                    kind: StmtKind::For { var, iter, body },
                    span: Span::new(start, self.prev_end()),
                });
            }
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
            _ => {
                let e = self.expr()?;
                if matches!(e.kind, ExprKind::Index { .. }) && self.eat(&TokenKind::Assign) {
                    StmtKind::IndexAssign {
                        target: e,
                        value: self.expr()?,
                    }
                } else {
                    StmtKind::Expr(e)
                }
            }
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
                // `Ok(x)` / `Ok(_)`: 束縛パターン（ADR-0013）
                let bind = match self.peek().clone() {
                    TokenKind::Ident(b) => {
                        let span = self.span();
                        self.bump();
                        Some((b != "_").then_some(Ident { name: b, span }))
                    }
                    _ => None,
                };
                self.expect(&TokenKind::RParen, "`)`")?;
                match bind {
                    Some(b) => PatternKind::Bind(name, b),
                    None => PatternKind::Type(name),
                }
            }
            _ => {
                return Err(self.error_here(
                    "a pattern (`int()`, `bool()`, `str()`, `Ok(x)`, `Err(e)`, `None` or `_`)",
                ));
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
            TokenKind::In => BinOp::In,
            TokenKind::Not if matches!(self.peek_at(1), TokenKind::In) => BinOp::NotIn,
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
        if op == BinOp::NotIn {
            self.bump(); // `not in` は2語
        }
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
        self.postfix()
    }

    /// `primary { "." IDENT }`
    fn postfix(&mut self) -> PResult<Expr> {
        let mut e = self.primary()?;
        loop {
            if self.at(&TokenKind::Question) {
                self.bump();
                let start = e.span.start;
                e = self.mk(ExprKind::Try(Box::new(e)), start);
                // `?.` / `?[` は書けない（ADR-0006: `?.` を導入しない）。括弧で囲む
                if matches!(self.peek(), TokenKind::Dot | TokenKind::LBracket) {
                    let found = self.peek().describe();
                    self.diags.push(
                        Diagnostic::error(
                            "E0101",
                            format!("expected the end of the expression after `?`, found {found}"),
                            self.span(),
                        )
                        .with_hint("wrap the propagated value in parentheses, e.g. `(f()?)[0]`"),
                    );
                    return Err(Reported);
                }
                continue;
            }
            if self.eat(&TokenKind::LBracket) {
                let index = self.expr()?;
                self.expect(&TokenKind::RBracket, "`]`")?;
                let start = e.span.start;
                e = self.mk(
                    ExprKind::Index {
                        base: Box::new(e),
                        index: Box::new(index),
                    },
                    start,
                );
                continue;
            }
            if !self.eat(&TokenKind::Dot) {
                break;
            }
            let name = self.ident("a field name")?;
            let start = e.span.start;
            // `x.f(...)` / `a.b.f(...)` / `f().g(...)`: メソッド呼び出し。受け手が名前の並び（`m`、`a.b`）なら、
            // 名前解決が、先頭がローカル変数でなければモジュールの修飾付きの呼び出しに直す（M4 T4・T6）。
            // フィールドの値の受け手（`box.handle.to_int()`）はメソッド呼び出しのまま
            if self.at(&TokenKind::LParen) {
                self.bump();
                let (args, kwargs) = self.call_args()?;
                e = self.mk(
                    ExprKind::MethodCall {
                        receiver: Box::new(e),
                        method: name,
                        args,
                        kwargs,
                    },
                    start,
                );
                continue;
            }
            e = self.mk(
                ExprKind::Field {
                    base: Box::new(e),
                    name,
                },
                start,
            );
        }
        Ok(e)
    }

    /// `(` の後ろから `)` まで: 位置引数とキーワード引数。
    fn call_args(&mut self) -> PResult<CallArgs> {
        let mut args = Vec::new();
        let mut kwargs = Vec::new();
        while !self.at(&TokenKind::RParen) {
            if matches!(self.peek(), TokenKind::Ident(_))
                && matches!(self.peek_at(1), TokenKind::Assign)
            {
                let k = self.ident("an argument name")?;
                self.bump(); // `=`
                kwargs.push((k, self.expr()?));
            } else if !kwargs.is_empty() {
                self.diags.push(Diagnostic::error(
                    "E0101",
                    "positional arguments must come before keyword arguments",
                    self.span(),
                ));
                return Err(Reported);
            } else {
                args.push(self.expr()?);
            }
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&TokenKind::RParen, "`,` or `)`")?;
        Ok((args, kwargs))
    }

    fn primary(&mut self) -> PResult<Expr> {
        let start = self.span().start;
        let kind = match self.peek().clone() {
            TokenKind::Int(v) => {
                self.bump();
                ExprKind::Int(v)
            }
            TokenKind::Float(b) => {
                self.bump();
                ExprKind::Float(f64::from_bits(b))
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
                    let (args, kwargs) = self.call_args()?;
                    ExprKind::Call {
                        callee: Ident { name, span },
                        args,
                        kwargs,
                    }
                } else {
                    ExprKind::Name(name)
                }
            }
            TokenKind::LBracket => {
                self.bump();
                let mut items = Vec::new();
                while !self.at(&TokenKind::RBracket) {
                    items.push(self.expr()?);
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(&TokenKind::RBracket, "`,` or `]`")?;
                ExprKind::List(items)
            }
            TokenKind::LBrace => {
                self.bump();
                let mut entries = Vec::new();
                while !self.at(&TokenKind::RBrace) {
                    let k = self.expr()?;
                    self.expect(&TokenKind::Colon, "`:` between a key and its value")?;
                    let v = self.expr()?;
                    entries.push((k, v));
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(&TokenKind::RBrace, "`,` or `}`")?;
                ExprKind::Dict(entries)
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
    fn method_calls_and_qualified_calls() {
        // `x.f(...)` / `a.b.f(...)` はメソッド呼び出し（名前解決がモジュールの参照と区別する）
        let m = ok("def main() -> None:\n    xs.append(1)\n    a.b.f(2)\n    f(1).g()\n");
        let kinds: Vec<&ExprKind> = m.funcs[0]
            .body
            .iter()
            .map(|s| match &s.kind {
                StmtKind::Expr(e) => &e.kind,
                other => panic!("expected an expression statement, got {other:?}"),
            })
            .collect();
        assert!(matches!(
            kinds[0],
            ExprKind::MethodCall { receiver, method, args, .. }
                if receiver.as_name() == Some("xs") && method.name == "append" && args.len() == 1
        ));
        // `a.b.f(...)` もパーサではメソッド呼び出し。名前解決が、`a` がローカル変数でなければ修飾付きの呼び出しに直す
        assert!(matches!(
            kinds[1],
            ExprKind::MethodCall { receiver, method, .. }
                if matches!(receiver.kind, ExprKind::Field { .. }) && method.name == "f"
        ));
        // 受け手は式でもよい（M4 T6）
        assert!(matches!(
            kinds[2],
            ExprKind::MethodCall { receiver, method, .. }
                if matches!(receiver.kind, ExprKind::Call { .. }) && method.name == "g"
        ));
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
    fn record_definitions_construction_and_fields() {
        let m = ok(
            "type Point = {x: int, y: int}\ntype MaybeInt = int | None\n\ndef f(p: Point) -> int:\n    q = Point(x=1, y=p.y)\n    return q.x\n",
        );
        assert_eq!(m.types.len(), 2);
        assert!(matches!(&m.types[0].value, TypeDefValue::Record(f) if f.len() == 2));
        assert!(matches!(&m.types[1].value, TypeDefValue::Alias(_)));
        let StmtKind::Assign { value, .. } = &m.funcs[0].body[0].kind else {
            panic!()
        };
        let ExprKind::Call { kwargs, args, .. } = &value.kind else {
            panic!()
        };
        assert!(args.is_empty());
        assert_eq!(kwargs.len(), 2);
        assert!(matches!(kwargs[1].1.kind, ExprKind::Field { .. }));
        // `type` は予約語ではない
        ok("def f(type: int) -> int:\n    return type\n");
    }

    #[test]
    fn generic_functions_and_types() {
        let m = ok(
            "type Pair[A, B] = {first: A, second: B}\n\ndef swap[A, B](p: Pair[A, B]) -> Pair[B, A]:\n    return Pair(first=p.second, second=p.first)\n",
        );
        assert_eq!(m.types[0].params.len(), 2);
        let f = &m.funcs[0];
        assert_eq!(f.type_params.len(), 2);
        assert!(
            matches!(&f.params[0].ty.kind, TypeExprKind::Apply(n, a) if n == "Pair" && a.len() == 2)
        );
    }

    #[test]
    fn imports_visibility_and_qualified_names() {
        let m = ok(
            "import util.text\nfrom shapes import Point, area\n\npub type P = {x: int}\n\npub def f(p: shapes.Point) -> int:\n    return util.text.width(p.x)\n\ndef g() -> None:\n    pass\n",
        );
        assert_eq!(m.imports.len(), 2);
        assert_eq!(m.imports[0].module_name(), "util.text");
        assert!(m.imports[0].names.is_none());
        assert_eq!(m.imports[1].names.as_ref().unwrap().len(), 2);
        assert!(m.types[0].public);
        assert!(m.funcs[0].public && !m.funcs[1].public);
        assert!(
            matches!(&m.funcs[0].params[0].ty.kind, TypeExprKind::Name(n) if n == "shapes.Point")
        );
        let StmtKind::Return(Some(e)) = &m.funcs[0].body[0].kind else {
            panic!()
        };
        // パーサではメソッド呼び出し（受け手 `util.text`）。修飾付きの呼び出しにするのは名前解決（M4 T6）
        let ExprKind::MethodCall {
            receiver,
            method,
            args,
            ..
        } = &e.kind
        else {
            panic!()
        };
        assert!(matches!(receiver.kind, ExprKind::Field { .. }));
        assert_eq!(method.name, "width");
        assert!(matches!(args[0].kind, ExprKind::Field { .. }));
    }

    #[test]
    fn parse_at_offsets_spans_and_expr_ids() {
        let (m, d) = parse_source_at("def f() -> int:\n    return 1\n", 100, 7);
        assert!(d.is_empty());
        assert_eq!(m.funcs[0].name.span.start, 104);
        assert_eq!(m.expr_count, 8);
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
