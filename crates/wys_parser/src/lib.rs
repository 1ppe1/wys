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
        depth: 0,
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
    /// いまの入れ子の深さ（式・型・パターン・ブロック）。`MAX_NESTING` を超えたら E0107
    depth: u32,
}

/// 入れ子の深さの上限（ADR-0026 論点 14）。構文木の再帰（型検査・lowering も同じ深さで再帰する）でスタックが
/// 溢れるとプロセスごと止まる（LSP のサーバーも）ので、パーサで止めて診断にする
pub const MAX_NESTING: u32 = 100;

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
            // `@name(...)` は `[pub] [async] def` の直前にだけ書ける（ADR-0022 論点 11）
            let decorators = if self.at(&TokenKind::At) {
                match self.decorators() {
                    Ok(d) => d,
                    Err(Reported) => {
                        self.sync();
                        continue;
                    }
                }
            } else {
                Vec::new()
            };
            let public = matches!(self.peek(), TokenKind::Pub)
                && matches!(
                    self.peek_at(1),
                    TokenKind::Def | TokenKind::Async | TokenKind::Ident(_)
                );
            if public {
                self.bump();
            }
            if !decorators.is_empty() && !matches!(self.peek(), TokenKind::Def | TokenKind::Async) {
                let found = self.peek().describe();
                self.diags.push(
                    Diagnostic::error(
                        "E0101",
                        format!("expected `def` after a decorator, found {found}"),
                        self.span(),
                    )
                    .with_hint("decorators can only be written before a function definition"),
                );
                self.sync();
                continue;
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
                TokenKind::Def | TokenKind::Async => match self.func_def() {
                    Ok(mut f) => {
                        f.public = public;
                        f.decorators = decorators;
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

    /// `@name` / `@name(key=value, ...)` を 1 行に 1 つずつ（ADR-0022 論点 11）。
    fn decorators(&mut self) -> PResult<Vec<Decorator>> {
        let mut out = Vec::new();
        while self.at(&TokenKind::At) {
            let start = self.bump().span.start;
            let name = self.ident("a decorator name")?;
            let mut args = Vec::new();
            if self.eat(&TokenKind::LParen) {
                while !self.at(&TokenKind::RParen) {
                    let astart = self.span().start;
                    let key = if matches!(self.peek(), TokenKind::Ident(_))
                        && matches!(self.peek_at(1), TokenKind::Assign)
                    {
                        let k = self.ident("an argument name")?;
                        self.bump(); // `=`
                        Some(k)
                    } else {
                        None
                    };
                    let value = self.attr_value()?;
                    args.push(AttrArg {
                        key,
                        value,
                        span: Span::new(astart, self.prev_end()),
                    });
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(&TokenKind::RParen, "`,` or `)`")?;
            }
            let span = Span::new(start, self.prev_end());
            self.expect(&TokenKind::Newline, "a new line after a decorator")?;
            while self.eat(&TokenKind::BlankLine) {}
            out.push(Decorator { name, args, span });
        }
        Ok(out)
    }

    /// デコレータの引数の値: 整数（負を含む）、文字列、`True` / `False`、文字列と名前の `[...]`。
    fn attr_value(&mut self) -> PResult<AttrValue> {
        let bad = |p: &mut Self| {
            let found = p.peek().describe();
            p.diags.push(
                Diagnostic::error(
                    "E1402",
                    format!("expected a literal or a list of names as a decorator argument, found {found}"),
                    p.span(),
                )
                .with_hint("decorator arguments are numbers, strings, `True` / `False`, or lists of strings and function names"),
            );
            Reported
        };
        let v = match self.peek().clone() {
            TokenKind::Int(v) => {
                self.bump();
                AttrValue::Int(v)
            }
            TokenKind::Minus if matches!(self.peek_at(1), TokenKind::Int(_)) => {
                self.bump();
                let TokenKind::Int(v) = self.bump().kind.clone() else {
                    unreachable!("checked above")
                };
                AttrValue::Int(-v)
            }
            TokenKind::Float(bits) => {
                self.bump();
                AttrValue::Float(f64::from_bits(bits))
            }
            TokenKind::Minus if matches!(self.peek_at(1), TokenKind::Float(_)) => {
                self.bump();
                let TokenKind::Float(bits) = self.bump().kind.clone() else {
                    unreachable!("checked above")
                };
                AttrValue::Float(-f64::from_bits(bits))
            }
            TokenKind::Str(s) => {
                self.bump();
                AttrValue::Str(s)
            }
            TokenKind::True => {
                self.bump();
                AttrValue::Bool(true)
            }
            TokenKind::False => {
                self.bump();
                AttrValue::Bool(false)
            }
            TokenKind::LBracket => {
                self.bump();
                let mut elems = Vec::new();
                while !self.at(&TokenKind::RBracket) {
                    match self.peek().clone() {
                        TokenKind::Str(s) => {
                            let sp = self.bump().span;
                            elems.push(AttrElem::Str(s, sp));
                        }
                        TokenKind::Ident(_) => {
                            let first = self.ident("a function name")?;
                            let mut name = first.name;
                            let mut span = first.span;
                            while self.eat(&TokenKind::Dot) {
                                let part = self.ident("a function name")?;
                                name = format!("{name}.{}", part.name);
                                span = Span::new(span.start, part.span.end);
                            }
                            elems.push(AttrElem::Name(name, span));
                        }
                        _ => return Err(bad(self)),
                    }
                    if !self.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                self.expect(&TokenKind::RBracket, "`,` or `]`")?;
                AttrValue::List(elems)
            }
            _ => return Err(bad(self)),
        };
        Ok(v)
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
        self.nested(Self::type_expr_inner)
    }

    fn type_expr_inner(&mut self) -> PResult<TypeExpr> {
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
        let start = self.span().start;
        let is_async = self.eat(&TokenKind::Async);
        self.expect(&TokenKind::Def, "`def`")?;
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
            decorators: Vec::new(),
            public: false,
            is_async,
            name,
            type_params,
            params,
            ret,
            effects,
            body,
            span: Span::new(start, self.prev_end()),
        })
    }

    /// 入れ子を 1 段深くして `f` を呼ぶ。深すぎれば E0107（1 回だけ報告し、その構文は読み飛ばす）
    fn nested<T>(&mut self, f: impl FnOnce(&mut Self) -> PResult<T>) -> PResult<T> {
        let saved = self.depth;
        self.deeper()?;
        let r = f(self);
        // 二項演算子の連なり（`deeper`）で増えた分も、この構文を出るときに戻す
        self.depth = saved;
        r
    }

    /// 入れ子を 1 段深くする。二項演算子の連なり（`a + b + c ...`）は左に深い構文木になり、型検査と lowering が
    /// その深さで再帰するので、演算子ごとに 1 段数える（`nested` を出るときに戻る）
    fn deeper(&mut self) -> PResult<()> {
        if self.depth >= MAX_NESTING {
            if !self.diags.iter().any(|d| d.code == "E0107") {
                self.diags.push(
                    Diagnostic::error(
                        "E0107",
                        format!("the code is nested too deeply (more than {MAX_NESTING} levels)"),
                        self.span(),
                    )
                    .with_hint("move the inner part into a variable or a function"),
                );
            }
            return Err(Reported);
        }
        self.depth += 1;
        Ok(())
    }

    fn block(&mut self) -> PResult<Vec<Stmt>> {
        self.nested(Self::block_inner)
    }

    fn block_inner(&mut self) -> PResult<Vec<Stmt>> {
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
            TokenKind::Def | TokenKind::Async => {
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
        self.nested(Self::pattern_inner)
    }

    fn pattern_inner(&mut self) -> PResult<Pattern> {
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
            TokenKind::Ident(first) => {
                self.bump();
                // `case m.Id():`: ほかのモジュールの型名（修飾名）。名前解決が内部名に直す（M4 T0）
                let mut name = first;
                while self.eat(&TokenKind::Dot) {
                    name = format!("{name}.{}", self.ident("a type name")?.name);
                }
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
        self.nested(Self::or_expr)
    }

    fn or_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.and_expr()?;
        while self.eat(&TokenKind::Or) {
            self.deeper()?;
            let rhs = self.and_expr()?;
            lhs = self.binary(BinOp::Or, lhs, rhs);
        }
        Ok(lhs)
    }

    fn and_expr(&mut self) -> PResult<Expr> {
        let mut lhs = self.not_expr()?;
        while self.eat(&TokenKind::And) {
            self.deeper()?;
            let rhs = self.not_expr()?;
            lhs = self.binary(BinOp::And, lhs, rhs);
        }
        Ok(lhs)
    }

    fn not_expr(&mut self) -> PResult<Expr> {
        if self.at(&TokenKind::Not) {
            let start = self.bump().span.start;
            let operand = self.nested(Self::not_expr)?;
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
            self.deeper()?;
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
            self.deeper()?;
            let rhs = self.unary()?;
            lhs = self.binary(op, lhs, rhs);
        }
    }

    /// 単項演算子（`-` / `not` の繰り返しは自分を呼ぶたびに 1 段深くする）
    fn unary(&mut self) -> PResult<Expr> {
        if self.at(&TokenKind::Minus) {
            let start = self.bump().span.start;
            let operand = self.nested(Self::unary)?;
            return Ok(self.mk(
                ExprKind::Unary {
                    op: UnaryOp::Neg,
                    operand: Box::new(operand),
                },
                start,
            ));
        }
        if self.at(&TokenKind::Await) {
            // `await` の被演算子は `?` を含まない後置の並びで、`?` は `await` の外側に付く（ADR-0020 論点 4・13）
            let start = self.bump().span.start;
            let operand = self.postfix_chain(false)?;
            let e = self.mk(ExprKind::Await(Box::new(operand)), start);
            return self.postfix_after(e, true);
        }
        self.postfix()
    }

    /// `primary { "." IDENT }`
    fn postfix(&mut self) -> PResult<Expr> {
        self.postfix_chain(true)
    }

    /// 後置の並び。`allow_try` が偽なら `?` の手前で止まる（`await` の被演算子）。
    fn postfix_chain(&mut self, allow_try: bool) -> PResult<Expr> {
        let e = self.primary()?;
        self.postfix_after(e, allow_try)
    }

    fn postfix_after(&mut self, mut e: Expr, allow_try: bool) -> PResult<Expr> {
        loop {
            if self.at(&TokenKind::Question) {
                if !allow_try {
                    return Ok(e);
                }
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
            let type_args = self.call_type_args();
            if self.at(&TokenKind::LParen) {
                self.bump();
                let (args, kwargs) = self.call_args()?;
                e = self.mk(
                    ExprKind::MethodCall {
                        receiver: Box::new(e),
                        method: name,
                        type_args,
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

    /// 呼び出しの型引数 `[T, U]`（ADR-0021 論点 9）。`[` の後を型の並びとして読み、`]` の直後に `(` が
    /// 続くときだけ型引数とする。そうでなければ読んだ分を巻き戻して空を返す（`xs[i]` は添字のまま）。
    /// 関数値がない（E0213）ので、`a[b](c)` はこれまで合法な形ではなく、この先読みで一意に決まる。
    fn call_type_args(&mut self) -> Vec<TypeExpr> {
        if !self.at(&TokenKind::LBracket) {
            return Vec::new();
        }
        let (pos, ndiags) = (self.pos, self.diags.len());
        self.bump();
        let mut args = Vec::new();
        let ok = loop {
            match self.type_expr() {
                Ok(t) => args.push(t),
                Err(_) => break false,
            }
            if !self.eat(&TokenKind::Comma) || self.at(&TokenKind::RBracket) {
                break self.eat(&TokenKind::RBracket) && self.at(&TokenKind::LParen);
            }
        };
        if ok {
            return args;
        }
        self.pos = pos;
        self.diags.truncate(ndiags);
        Vec::new()
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
                let type_args = self.call_type_args();
                if self.eat(&TokenKind::LParen) {
                    let (args, kwargs) = self.call_args()?;
                    ExprKind::Call {
                        callee: Ident { name, span },
                        type_args,
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

    #[test]
    fn nesting_is_limited_before_the_stack_overflows() {
        let deep = |n: usize| {
            format!(
                "def main() -> None:\n    x = {}1{}\n",
                "(".repeat(n),
                ")".repeat(n)
            )
        };
        // 関数の本体（1 段）と式（1 段）に、括弧 98 段で 100 段
        assert!(parse_source(&deep(98)).1.is_empty());
        let codes: Vec<String> = parse_source(&deep(99))
            .1
            .iter()
            .map(|d| d.code.to_string())
            .collect();
        assert_eq!(codes, vec!["E0107"]);
        // 極端に深い入力も、1 つの E0107 で止まる（スタックを溢れさせない）
        let codes: Vec<String> = parse_source(&deep(200_000))
            .1
            .iter()
            .map(|d| d.code.to_string())
            .collect();
        assert_eq!(codes, vec!["E0107"]);
        let chain = format!(
            "def main() -> None:\n    x = {}\n",
            vec!["1"; 200_000].join(" + ")
        );
        let codes: Vec<String> = parse_source(&chain)
            .1
            .iter()
            .map(|d| d.code.to_string())
            .collect();
        assert_eq!(codes, vec!["E0107"]);
    }

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
    fn explicit_type_arguments_on_calls() {
        // `f[T](x)` は型引数付きの呼び出し、`xs[i]` / `xs[i] = v` / `xs[f(1)]` は添字のまま（ADR-0021 論点 9）
        let m = ok(
            "def main() -> None:\n    f[int, list[str]](1)\n    m.g[Pair[int]]()\n    xs[i]\n    xs[i] = 2\n    xs[f(1)]\n    Pair[int](first=1)\n",
        );
        let kinds: Vec<&ExprKind> = m.funcs[0]
            .body
            .iter()
            .filter_map(|s| match &s.kind {
                StmtKind::Expr(e) => Some(&e.kind),
                _ => None,
            })
            .collect();
        assert!(matches!(
            kinds[0],
            ExprKind::Call { callee, type_args, args, .. }
                if callee.name == "f" && type_args.len() == 2 && args.len() == 1
        ));
        assert!(matches!(
            kinds[1],
            ExprKind::MethodCall { method, type_args, .. }
                if method.name == "g" && type_args.len() == 1
        ));
        assert!(matches!(kinds[2], ExprKind::Index { .. }));
        assert!(
            matches!(kinds[3], ExprKind::Index { index, .. } if matches!(index.kind, ExprKind::Call { .. }))
        );
        assert!(matches!(
            kinds[4],
            ExprKind::Call { callee, type_args, kwargs, .. }
                if callee.name == "Pair" && type_args.len() == 1 && kwargs.len() == 1
        ));
        assert!(matches!(
            m.funcs[0].body[3].kind,
            StmtKind::IndexAssign { .. }
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
