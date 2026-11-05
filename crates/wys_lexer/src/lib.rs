//! 字句解析（INDENT/DEDENT を含む）
//!
//! - インデントは空白のみ。タブはエラー（E0004）。
//! - 括弧の内側では改行とインデントを無視する。
//! - 空行とコメントだけの行はインデント判定に使わない。

use wys_diagnostics::Diagnostic;
use wys_span::Span;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenKind {
    Int(i64),
    /// 浮動小数点のリテラル（ADR-0019）。`Eq` を保つため、`f64` のビット列で持つ（`f64::from_bits`）
    Float(u64),
    Str(String),
    Ident(String),
    // keywords
    Def,
    Return,
    If,
    Elif,
    Else,
    While,
    Pass,
    True,
    False,
    None,
    And,
    Or,
    Not,
    Is,
    Match,
    Case,
    Pub,
    Import,
    From,
    /// `import a.b as c` / `from a import f as g`（ADR-0016 論点 19）
    As,
    For,
    In,
    // operators / punctuation
    Plus,
    Minus,
    Star,
    Slash,
    SlashSlash,
    Percent,
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    Assign,
    LParen,
    RParen,
    Colon,
    Comma,
    Arrow,
    Pipe,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Dot,
    /// `?`（`Result` の伝搬、ADR-0013）
    Question,
    /// コメント（`lex_with_trivia` のときだけ）。`level` は行頭のコメントのインデントの深さ、行末のコメントなら `None`。
    Comment {
        text: String,
        level: Option<usize>,
    },
    /// 空行（`lex_with_trivia` のときだけ）
    BlankLine,
    // layout
    Newline,
    Indent,
    Dedent,
    Eof,
}

impl TokenKind {
    /// 診断メッセージ用の表記。
    pub fn describe(&self) -> String {
        use TokenKind::*;
        let s = match self {
            Int(v) => return format!("integer `{v}`"),
            Float(b) => return format!("float `{:?}`", f64::from_bits(*b)),
            Str(_) => "string literal",
            Ident(n) => return format!("identifier `{n}`"),
            Def => "`def`",
            Return => "`return`",
            If => "`if`",
            Elif => "`elif`",
            Else => "`else`",
            While => "`while`",
            Pass => "`pass`",
            True => "`True`",
            False => "`False`",
            None => "`None`",
            And => "`and`",
            Or => "`or`",
            Not => "`not`",
            Is => "`is`",
            Match => "`match`",
            Case => "`case`",
            Pub => "`pub`",
            Import => "`import`",
            From => "`from`",
            As => "`as`",
            For => "`for`",
            In => "`in`",
            Plus => "`+`",
            Minus => "`-`",
            Star => "`*`",
            Slash => "`/`",
            SlashSlash => "`//`",
            Percent => "`%`",
            EqEq => "`==`",
            NotEq => "`!=`",
            Lt => "`<`",
            Le => "`<=`",
            Gt => "`>`",
            Ge => "`>=`",
            Assign => "`=`",
            LParen => "`(`",
            RParen => "`)`",
            Colon => "`:`",
            Comma => "`,`",
            Arrow => "`->`",
            Pipe => "`|`",
            LBrace => "`{`",
            RBrace => "`}`",
            LBracket => "`[`",
            RBracket => "`]`",
            Dot => "`.`",
            Question => "`?`",
            Comment { .. } => "comment",
            BlankLine => "blank line",
            Newline => "end of line",
            Indent => "indent",
            Dedent => "dedent",
            Eof => "end of file",
        };
        s.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

/// ソースをトークン列にする。エラーがあっても最後まで読み、`Eof` で終わるトークン列を返す。
pub fn lex(src: &str) -> (Vec<Token>, Vec<Diagnostic>) {
    lexer(src, false).run()
}

/// コメントと空行もトークンとして返す（`wys fmt` 用）。それ以外のトークンは `lex` と同じ。
pub fn lex_with_trivia(src: &str) -> (Vec<Token>, Vec<Diagnostic>) {
    lexer(src, true).run()
}

fn lexer(src: &str, trivia: bool) -> Lexer<'_> {
    Lexer {
        src,
        bytes: src.as_bytes(),
        pos: 0,
        tokens: Vec::new(),
        diags: Vec::new(),
        indents: vec![0],
        paren: 0,
        trivia,
    }
}

struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    tokens: Vec<Token>,
    diags: Vec<Diagnostic>,
    indents: Vec<usize>,
    paren: usize,
    trivia: bool,
}

fn sp(start: usize, end: usize) -> Span {
    Span::new(start as u32, end as u32)
}

fn keyword(s: &str) -> Option<TokenKind> {
    use TokenKind::*;
    Some(match s {
        "def" => Def,
        "return" => Return,
        "if" => If,
        "elif" => Elif,
        "else" => Else,
        "while" => While,
        "pass" => Pass,
        "True" => True,
        "False" => False,
        "None" => None,
        "and" => And,
        "or" => Or,
        "not" => Not,
        "is" => Is,
        "match" => Match,
        "case" => Case,
        "pub" => Pub,
        "import" => Import,
        "from" => From,
        "as" => As,
        "for" => For,
        "in" => In,
        _ => return Option::None,
    })
}

impl Lexer<'_> {
    fn run(mut self) -> (Vec<Token>, Vec<Diagnostic>) {
        let mut at_line_start = true;
        while self.pos < self.bytes.len() {
            if at_line_start && self.paren == 0 {
                if self.skip_blank_line() {
                    continue;
                }
                self.indentation();
                at_line_start = false;
                continue;
            }
            let start = self.pos;
            let c = self.bytes[self.pos];
            match c {
                b'\n' => {
                    self.pos += 1;
                    if self.paren == 0 {
                        self.newline(start);
                        at_line_start = true;
                    }
                }
                b' ' | b'\t' | b'\r' => self.pos += 1,
                b'#' => {
                    while self.pos < self.bytes.len() && self.bytes[self.pos] != b'\n' {
                        self.pos += 1;
                    }
                    if self.trivia {
                        let text = self.src[start..self.pos].trim_end().to_string();
                        self.push(TokenKind::Comment { text, level: None }, start, self.pos);
                    }
                }
                b'0'..=b'9' => self.number(),
                b'"' | b'\'' => self.string(c),
                b'a'..=b'z' | b'A'..=b'Z' | b'_' => self.ident(),
                _ => self.operator(),
            }
        }
        let end = self.bytes.len();
        if self.paren > 0 {
            self.diags.push(Diagnostic::error(
                "E0008",
                "unclosed `(` at end of file",
                sp(end, end),
            ));
        }
        self.newline(end);
        while self.indents.len() > 1 {
            self.indents.pop();
            self.push(TokenKind::Dedent, end, end);
        }
        self.push(TokenKind::Eof, end, end);
        (self.tokens, self.diags)
    }

    fn push(&mut self, kind: TokenKind, start: usize, end: usize) {
        self.tokens.push(Token {
            kind,
            span: sp(start, end),
        });
    }

    fn newline(&mut self, at: usize) {
        match self.tokens.last() {
            Some(t) if t.kind != TokenKind::Newline => {
                let end = (at + 1).min(self.bytes.len());
                self.push(TokenKind::Newline, at, end);
            }
            _ => {}
        }
    }

    /// 空行・コメントだけの行なら行末まで読み飛ばして true を返す。
    fn skip_blank_line(&mut self) -> bool {
        let mut i = self.pos;
        while i < self.bytes.len() && matches!(self.bytes[i], b' ' | b'\t' | b'\r') {
            i += 1;
        }
        if i >= self.bytes.len() {
            self.pos = i;
            return true;
        }
        match self.bytes[i] {
            b'\n' => {
                if self.trivia {
                    self.push(TokenKind::BlankLine, i, i);
                }
                self.pos = i + 1;
                true
            }
            b'#' => {
                let (width, start) = (i - self.pos, i);
                while i < self.bytes.len() && self.bytes[i] != b'\n' {
                    i += 1;
                }
                if self.trivia {
                    // コメントの字下げより浅いか同じ深さのブロックの数が、そのコメントのインデントの深さ。
                    // 今のブロックより深ければ、これから始まるブロック（`def f():` の直後など）に属する
                    let top = *self.indents.last().expect("indent stack is never empty");
                    let level = self
                        .indents
                        .iter()
                        .filter(|w| **w > 0 && **w <= width)
                        .count()
                        + usize::from(width > top);
                    let text = self.src[start..i].trim_end().to_string();
                    self.push(
                        TokenKind::Comment {
                            text,
                            level: Some(level),
                        },
                        start,
                        i,
                    );
                }
                self.pos = (i + 1).min(self.bytes.len());
                true
            }
            _ => false,
        }
    }

    fn indentation(&mut self) {
        let start = self.pos;
        let mut width = 0;
        while self.pos < self.bytes.len() {
            match self.bytes[self.pos] {
                b' ' => width += 1,
                b'\t' => {
                    self.diags.push(
                        Diagnostic::error(
                            "E0004",
                            "tab character in indentation",
                            sp(self.pos, self.pos + 1),
                        )
                        .with_hint("indent with spaces only"),
                    );
                    width += 1;
                }
                _ => break,
            }
            self.pos += 1;
        }
        let top = *self.indents.last().expect("indent stack is never empty");
        if width > top {
            self.indents.push(width);
            self.push(TokenKind::Indent, start, self.pos);
        } else if width < top {
            while width < *self.indents.last().expect("indent stack is never empty") {
                self.indents.pop();
                self.push(TokenKind::Dedent, self.pos, self.pos);
            }
            if width != *self.indents.last().expect("indent stack is never empty") {
                self.diags.push(Diagnostic::error(
                    "E0003",
                    "dedent does not match any outer indentation level",
                    sp(start, self.pos),
                ));
                // 回復: この幅を新しいレベルとして扱う
                self.indents.push(width);
            }
        }
    }

    fn number(&mut self) {
        let start = self.pos;
        while self.pos < self.bytes.len() && self.bytes[self.pos].is_ascii_digit() {
            self.pos += 1;
        }
        // `1.5` / `1e10` / `1.5e-3`（ADR-0019 論点 2）。小数点の後と、指数の後には数字が要る
        let digits_at = |pos: usize| pos < self.bytes.len() && self.bytes[pos].is_ascii_digit();
        let mut is_float = false;
        if self.pos < self.bytes.len() && self.bytes[self.pos] == b'.' && digits_at(self.pos + 1) {
            is_float = true;
            self.pos += 1;
            while digits_at(self.pos) {
                self.pos += 1;
            }
        }
        if self.pos < self.bytes.len() && matches!(self.bytes[self.pos], b'e' | b'E') {
            let sign = usize::from(
                self.pos + 1 < self.bytes.len() && matches!(self.bytes[self.pos + 1], b'+' | b'-'),
            );
            if digits_at(self.pos + 1 + sign) {
                is_float = true;
                self.pos += 1 + sign;
                while digits_at(self.pos) {
                    self.pos += 1;
                }
            }
        }
        if is_float {
            let text = &self.src[start..self.pos];
            let v: f64 = text
                .parse()
                .expect("the lexer accepted a valid float literal");
            if v.is_infinite() {
                self.diags.push(
                    Diagnostic::error("E0009", "float literal is too large", sp(start, self.pos))
                        .with_cause("it rounds to infinity (`float` is an IEEE 754 double)"),
                );
            }
            self.push(TokenKind::Float(v.to_bits()), start, self.pos);
            return;
        }
        let text = &self.src[start..self.pos];
        let value = match text.parse::<i64>() {
            Ok(v) => v,
            Err(_) => {
                self.diags.push(
                    Diagnostic::error("E0005", "integer literal is too large", sp(start, self.pos))
                        .with_cause("`int` is a signed 64-bit integer"),
                );
                0
            }
        };
        self.push(TokenKind::Int(value), start, self.pos);
    }

    fn string(&mut self, quote: u8) {
        let start = self.pos;
        self.pos += 1;
        let mut value = String::new();
        loop {
            if self.pos >= self.bytes.len() || self.bytes[self.pos] == b'\n' {
                self.diags.push(Diagnostic::error(
                    "E0002",
                    "unterminated string literal",
                    sp(start, self.pos),
                ));
                break;
            }
            let ch = self.src[self.pos..].chars().next().expect("in bounds");
            let ch_len = ch.len_utf8();
            if ch as u32 == quote as u32 {
                self.pos += 1;
                break;
            }
            if ch == '\\' {
                let esc_start = self.pos;
                self.pos += 1;
                let Some(e) = self.src[self.pos..].chars().next() else {
                    continue;
                };
                if e == '\n' {
                    continue;
                }
                self.pos += e.len_utf8();
                match e {
                    'n' => value.push('\n'),
                    't' => value.push('\t'),
                    'r' => value.push('\r'),
                    '0' => value.push('\0'),
                    '\\' => value.push('\\'),
                    '"' => value.push('"'),
                    '\'' => value.push('\''),
                    other => {
                        self.diags.push(Diagnostic::error(
                            "E0006",
                            format!("unknown escape sequence `\\{other}`"),
                            sp(esc_start, self.pos),
                        ));
                        value.push(other);
                    }
                }
                continue;
            }
            value.push(ch);
            self.pos += ch_len;
        }
        self.push(TokenKind::Str(value), start, self.pos);
    }

    fn ident(&mut self) {
        let start = self.pos;
        while self.pos < self.bytes.len()
            && (self.bytes[self.pos].is_ascii_alphanumeric() || self.bytes[self.pos] == b'_')
        {
            self.pos += 1;
        }
        let text = &self.src[start..self.pos];
        let kind = keyword(text).unwrap_or_else(|| TokenKind::Ident(text.to_string()));
        self.push(kind, start, self.pos);
    }

    fn operator(&mut self) {
        use TokenKind::*;
        let start = self.pos;
        let c = self.bytes[self.pos];
        let next = self.bytes.get(self.pos + 1).copied();
        let (kind, len) = match (c, next) {
            (b'-', Some(b'>')) => (Arrow, 2),
            (b'/', Some(b'/')) => (SlashSlash, 2),
            (b'=', Some(b'=')) => (EqEq, 2),
            (b'!', Some(b'=')) => (NotEq, 2),
            (b'<', Some(b'=')) => (Le, 2),
            (b'>', Some(b'=')) => (Ge, 2),
            (b'+', _) => (Plus, 1),
            (b'-', _) => (Minus, 1),
            (b'*', _) => (Star, 1),
            (b'/', _) => (Slash, 1),
            (b'%', _) => (Percent, 1),
            (b'<', _) => (Lt, 1),
            (b'>', _) => (Gt, 1),
            (b'=', _) => (Assign, 1),
            (b'(', _) => (LParen, 1),
            (b')', _) => (RParen, 1),
            (b':', _) => (Colon, 1),
            (b',', _) => (Comma, 1),
            (b'|', _) => (Pipe, 1),
            (b'{', _) => (LBrace, 1),
            (b'}', _) => (RBrace, 1),
            (b'.', _) => (Dot, 1),
            (b'?', _) => (Question, 1),
            (b'[', _) => (LBracket, 1),
            (b']', _) => (RBracket, 1),
            _ => {
                let ch = self.src[self.pos..].chars().next().expect("in bounds");
                self.pos += ch.len_utf8();
                self.diags.push(Diagnostic::error(
                    "E0001",
                    format!("unexpected character `{ch}`"),
                    sp(start, self.pos),
                ));
                return;
            }
        };
        match kind {
            // 括弧の内側では改行とインデントを無視する（`{ }` も同じ）
            LParen | LBrace | LBracket => self.paren += 1,
            RParen | RBrace | RBracket => self.paren = self.paren.saturating_sub(1),
            _ => {}
        }
        self.pos += len;
        self.push(kind, start, self.pos);
    }
}

#[cfg(test)]
mod tests {
    use super::TokenKind::*;
    use super::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        let (toks, diags) = lex(src);
        assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
        toks.into_iter().map(|t| t.kind).collect()
    }

    fn codes(src: &str) -> Vec<&'static str> {
        lex(src).1.into_iter().map(|d| d.code).collect()
    }

    #[test]
    fn indent_and_dedent() {
        let src = "def f() -> None:\n    if x:\n        pass\n    pass\n";
        let ks = kinds(src);
        let layout: Vec<_> = ks
            .iter()
            .filter(|k| matches!(k, Indent | Dedent | Newline))
            .collect();
        assert_eq!(
            layout,
            vec![
                &Newline, &Indent, &Newline, &Indent, &Newline, &Dedent, &Newline, &Dedent
            ]
        );
        assert_eq!(ks.last(), Some(&Eof));
    }

    #[test]
    fn blank_and_comment_lines_are_ignored() {
        let a = kinds("x = 1\n\n   # comment\nx = 2\n");
        let b = kinds("x = 1\nx = 2\n");
        assert_eq!(a, b);
    }

    #[test]
    fn newlines_inside_parens_are_ignored() {
        let ks = kinds("f(1,\n  2)\n");
        assert_eq!(
            ks,
            vec![
                Ident("f".into()),
                LParen,
                Int(1),
                Comma,
                Int(2),
                RParen,
                Newline,
                Eof
            ]
        );
    }

    #[test]
    fn missing_trailing_newline_still_ends_statement() {
        assert_eq!(kinds("pass"), vec![Pass, Newline, Eof]);
    }

    #[test]
    fn operators() {
        assert_eq!(
            kinds("a // b -> c != d <= e"),
            vec![
                Ident("a".into()),
                SlashSlash,
                Ident("b".into()),
                Arrow,
                Ident("c".into()),
                NotEq,
                Ident("d".into()),
                Le,
                Ident("e".into()),
                Newline,
                Eof
            ]
        );
    }

    #[test]
    fn string_escapes_and_unicode() {
        assert_eq!(kinds(r#"print("あ\n\"")"#)[2], Str("あ\n\"".to_string()));
    }

    #[test]
    fn trivia_mode_keeps_comments_and_blank_lines() {
        let src = "# top\ndef f() -> None:\n    # inner\n    pass  # trailing\n\npass\n";
        let (toks, d) = lex_with_trivia(src);
        assert!(d.is_empty());
        let trivia: Vec<_> = toks
            .iter()
            .filter(|t| matches!(t.kind, Comment { .. } | BlankLine))
            .map(|t| t.kind.clone())
            .collect();
        assert_eq!(
            trivia,
            vec![
                Comment {
                    text: "# top".into(),
                    level: Some(0)
                },
                Comment {
                    text: "# inner".into(),
                    level: Some(1)
                },
                Comment {
                    text: "# trailing".into(),
                    level: Option::None
                },
                BlankLine,
            ]
        );
        // 通常のトークンは lex と同じ
        let plain = |ts: Vec<Token>| -> Vec<TokenKind> {
            ts.into_iter()
                .map(|t| t.kind)
                .filter(|k| !matches!(k, Comment { .. } | BlankLine))
                .collect()
        };
        assert_eq!(plain(toks), plain(lex(src).0));
    }

    #[test]
    fn errors_have_stable_codes() {
        assert_eq!(codes("x = \"abc\n"), vec!["E0002"]);
        assert_eq!(codes("if x:\n\tpass\n"), vec!["E0004"]);
        assert_eq!(codes("if x:\n    a\n  b\n"), vec!["E0003"]);
        assert_eq!(codes("x = 99999999999999999999\n"), vec!["E0005"]);
        assert_eq!(codes("x = \"\\q\"\n"), vec!["E0006"]);
        assert_eq!(codes("x = 1e400\n"), vec!["E0009"]);
        assert_eq!(
            kinds("1.5 2e3 1.")[..3],
            [Float(1.5f64.to_bits()), Float(2e3f64.to_bits()), Int(1)]
        );
        assert_eq!(codes("x = $\n"), vec!["E0001"]);
        assert_eq!(kinds("a is not b | c")[1..4], [Is, Not, Ident("b".into())]);
        assert_eq!(codes("f(1\n"), vec!["E0008"]);
    }
}
