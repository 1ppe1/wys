//! ソースの整形（DX-001 `fmt`）
//!
//! トークン列を出力し直す方式。コメントと空行は `lex_with_trivia` で受け取って残す。
//!
//! - インデントは空白4つ。行末の空白は取り除き、連続する空行は1行にまとめる。ファイルの先頭の空行は消す。
//! - 演算子・カンマ・コロンなどの前後の空白を正規化する（規則は `space_between`）。
//! - 長い行の折り返しはしない。括弧の中で複数行にわたる式は1行にまとめる（v0 の制限）。
//! - 字句エラーのあるファイルは整形しない。
//!
//! 整形しても意味が変わらないこと（コメントと空行を除いたトークン列が同じ）と、
//! 2回整形しても結果が変わらないこと（冪等性）を、全 Conformance ケースで確かめる。

use wys_diagnostics::Diagnostic;
use wys_lexer::{Token, TokenKind, lex_with_trivia};

/// ソースを整形する。字句エラーがあれば、整形せずに診断を返す。
pub fn format_source(src: &str) -> Result<String, Vec<Diagnostic>> {
    let (tokens, diags) = lex_with_trivia(src);
    if diags.iter().any(Diagnostic::is_error) {
        return Err(diags);
    }
    let mut out = String::new();
    let mut level = 0usize;
    let mut line: Vec<&Token> = Vec::new();
    let mut trailing: Option<&str> = None;
    let mut blank = false;
    let flush_blank = |out: &mut String, blank: &mut bool| {
        if *blank && !out.is_empty() {
            out.push('\n');
        }
        *blank = false;
    };
    for t in &tokens {
        match &t.kind {
            TokenKind::Indent => level += 1,
            TokenKind::Dedent => level = level.saturating_sub(1),
            TokenKind::BlankLine => blank = true,
            TokenKind::Comment {
                text,
                level: Some(l),
            } => {
                flush_blank(&mut out, &mut blank);
                out.push_str(&"    ".repeat(*l));
                out.push_str(text);
                out.push('\n');
            }
            TokenKind::Comment { text, level: None } => trailing = Some(text),
            TokenKind::Newline => {
                if !line.is_empty() {
                    flush_blank(&mut out, &mut blank);
                    out.push_str(&"    ".repeat(level));
                    out.push_str(&render(src, &line));
                    if let Some(c) = trailing {
                        out.push_str("  ");
                        out.push_str(c);
                    }
                    out.push('\n');
                }
                line.clear();
                trailing = None;
            }
            TokenKind::Eof => {}
            _ => line.push(t),
        }
    }
    Ok(out)
}

/// 1つの論理行のトークンを、空白の規則に従ってつなげる。トークンの表記はソースのまま使う。
fn render(src: &str, line: &[&Token]) -> String {
    let mut out = String::new();
    let mut prev: Option<&TokenKind> = None;
    let mut prev_unary_minus = false;
    let mut depth = 0usize;
    for t in line {
        let k = &t.kind;
        if let Some(p) = prev
            && space_between(p, k, depth, prev_unary_minus)
        {
            out.push(' ');
        }
        out.push_str(&src[t.span.start as usize..t.span.end as usize]);
        prev_unary_minus = matches!(k, TokenKind::Minus) && starts_operand(prev);
        match k {
            TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
            TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                depth = depth.saturating_sub(1)
            }
            _ => {}
        }
        prev = Some(k);
    }
    out
}

/// 直前のトークンが `prev` のとき、次に来るのはオペランド（`-` は単項）か。
fn starts_operand(prev: Option<&TokenKind>) -> bool {
    use TokenKind::*;
    match prev {
        Option::None => true,
        Some(p) => matches!(
            p,
            LParen
                | LBracket
                | LBrace
                | Comma
                | Colon
                | Assign
                | Arrow
                | Plus
                | Minus
                | Star
                | Slash
                | SlashSlash
                | Percent
                | EqEq
                | NotEq
                | Lt
                | Le
                | Gt
                | Ge
                | Pipe
                | Return
                | If
                | Elif
                | While
                | And
                | Or
                | Not
                | Is
                | Match
                | Case
        ),
    }
}

/// `prev` と `cur` の間に空白を入れるか。`depth` は括弧の深さ（括弧の中の `=` はキーワード引数）。
fn space_between(prev: &TokenKind, cur: &TokenKind, depth: usize, prev_unary_minus: bool) -> bool {
    use TokenKind::*;
    if matches!(
        cur,
        RParen | RBracket | RBrace | Comma | Colon | Dot | Question
    ) {
        return false;
    }
    if matches!(prev, LParen | LBracket | LBrace | Dot) {
        return false;
    }
    if matches!(cur, LParen | LBracket) {
        // 呼び出し `f(` / 型引数 `f[T]` / `def f[T](`
        return !matches!(prev, Ident(_) | RParen | RBracket);
    }
    if prev_unary_minus {
        return false;
    }
    if depth > 0 && (matches!(cur, Assign) || matches!(prev, Assign)) {
        return false; // キーワード引数 `x=1`
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn fmt(src: &str) -> String {
        format_source(src).expect("no lexical errors")
    }

    #[test]
    fn normalizes_spacing_indentation_and_blank_lines() {
        let src = "\n\ndef  f( x:int,y : int )->int :\n  if x>-y and not(x==1):\n        return x+y   # sum\n  return  Point( x = 1 ).x\n\n\n\n# end\n";
        assert_eq!(
            fmt(src),
            "def f(x: int, y: int) -> int:\n    if x > -y and not (x == 1):\n        return x + y  # sum\n    return Point(x=1).x\n\n# end\n"
        );
    }

    #[test]
    fn keeps_comments_at_their_depth() {
        let src = "def f() -> None:\n  # first\n  pass\n  # after pass\n\n# top level\ndef g[T](x: T | None) -> T | None:\n  return x\n";
        assert_eq!(
            fmt(src),
            "def f() -> None:\n    # first\n    pass\n    # after pass\n\n# top level\ndef g[T](x: T | None) -> T | None:\n    return x\n"
        );
    }

    #[test]
    fn joins_multiline_expressions_in_parens() {
        assert_eq!(
            fmt("def main() -> None:\n    print(add(1,\n              2))\n"),
            "def main() -> None:\n    print(add(1, 2))\n"
        );
    }

    #[test]
    fn refuses_files_with_lexical_errors() {
        assert!(format_source("x = \"unterminated\n").is_err());
    }

    fn significant(src: &str) -> Vec<TokenKind> {
        wys_lexer::lex(src).0.into_iter().map(|t| t.kind).collect()
    }

    fn corpus() -> Vec<PathBuf> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut out = Vec::new();
        let mut stack = vec![root.join("tests/conformance"), root.join("examples")];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "wys") {
                    out.push(p);
                }
            }
        }
        out
    }

    /// 全 Conformance ケースと例で: 整形してもトークン列（＝意味）が変わらず、2回整形しても同じ結果になる。
    #[test]
    fn formatting_preserves_tokens_and_is_idempotent() {
        let mut checked = 0;
        for path in corpus() {
            let src = std::fs::read_to_string(&path).unwrap();
            let Ok(once) = format_source(&src) else {
                continue; // 字句エラーのケース（E000x）は整形しない
            };
            assert_eq!(
                significant(&once),
                significant(&src),
                "tokens changed: {}",
                path.display()
            );
            assert_eq!(fmt(&once), once, "not idempotent: {}", path.display());
            checked += 1;
        }
        assert!(checked > 90, "too few files checked: {checked}");
    }
}
