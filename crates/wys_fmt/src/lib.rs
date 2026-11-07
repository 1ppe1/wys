//! ソースの整形（DX-001 `fmt`）
//!
//! トークン列を出力し直す方式。コメントと空行は `lex_with_trivia` で受け取って残す。
//!
//! - インデントは空白4つ。行末の空白は取り除き、連続する空行は1行にまとめる。ファイルの先頭の空行は消す。
//! - 演算子・カンマ・コロンなどの前後の空白を正規化する（規則は `space_between`）。
//! - 括弧の中で複数行にわたる式は、いったん1行にまとめる。
//! - 1行が `MAX_WIDTH` 文字を超えたら、カンマで区切られた要素を持つ最初の括弧で折り返す（要素を1行に1つ、
//!   インデントを1段深くし、閉じ括弧は元の段に戻す）。要素がまだ長ければ、同じ規則で入れ子に折り返す。
//!   カンマを持つ括弧がない行（長い文字列だけの行など）は折り返さない。末尾のカンマは足さない（トークン列を変えない）。
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
                    let mut lines = Vec::new();
                    wrap(src, &line, level, 0, &mut lines);
                    let last = lines.len() - 1;
                    for (i, l) in lines.iter().enumerate() {
                        out.push_str(l);
                        if i == last
                            && let Some(c) = trailing
                        {
                            out.push_str("  ");
                            out.push_str(c);
                        }
                        out.push('\n');
                    }
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

/// 1行の幅の上限（インデントを含む文字数）。
pub const MAX_WIDTH: usize = 100;

fn is_open(k: &TokenKind) -> bool {
    matches!(
        k,
        TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace
    )
}

fn is_close(k: &TokenKind) -> bool {
    matches!(
        k,
        TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace
    )
}

/// `line`（括弧の深さ `depth` から始まるトークン列）を `level` 段のインデントで出力する。長すぎれば折り返す。
fn wrap(src: &str, line: &[&Token], level: usize, depth: usize, out: &mut Vec<String>) {
    let indent = "    ".repeat(level);
    let flat = format!("{indent}{}", render(src, line, depth));
    if flat.chars().count() <= MAX_WIDTH {
        out.push(flat);
        return;
    }
    // カンマで区切られた要素を持つ、この行の深さで最初の括弧を探す
    let mut d = 0usize;
    let mut open = None;
    for (i, t) in line.iter().enumerate() {
        if is_open(&t.kind) {
            if d == 0 {
                open = Some(i);
            }
            d += 1;
        } else if is_close(&t.kind) {
            d = d.saturating_sub(1);
            if d == 0
                && let Some(o) = open.take()
            {
                let items = split_items(&line[o + 1..i]);
                if items.len() > 1 || items.first().is_some_and(|it| splittable(it)) {
                    out.push(format!("{indent}{}", render(src, &line[..=o], depth)));
                    for item in items {
                        wrap(src, item, level + 1, depth + 1, out);
                    }
                    wrap(src, &line[i..], level, depth + 1, out);
                    return;
                }
            }
        }
    }
    out.push(flat);
}

/// トークン列のどこかに、カンマで区切られた要素を持つ括弧があるか（1要素の括弧の中で折り返せるか）。
fn splittable(tokens: &[&Token]) -> bool {
    let mut d = 0usize;
    for t in tokens {
        if is_open(&t.kind) {
            d += 1;
        } else if is_close(&t.kind) {
            d = d.saturating_sub(1);
        } else if d > 0 && matches!(t.kind, TokenKind::Comma) {
            return true;
        }
    }
    false
}

/// 括弧の中のトークン列を、深さ 0 のカンマで要素に分ける（カンマは前の要素の末尾に残す）。
fn split_items<'a, 'b>(inner: &'b [&'a Token]) -> Vec<&'b [&'a Token]> {
    let mut items = Vec::new();
    let mut d = 0usize;
    let mut start = 0;
    for (i, t) in inner.iter().enumerate() {
        if is_open(&t.kind) {
            d += 1;
        } else if is_close(&t.kind) {
            d = d.saturating_sub(1);
        } else if d == 0 && matches!(t.kind, TokenKind::Comma) {
            items.push(&inner[start..=i]);
            start = i + 1;
        }
    }
    if start < inner.len() {
        items.push(&inner[start..]);
    }
    items
}

/// 1つの論理行のトークンを、空白の規則に従ってつなげる。トークンの表記はソースのまま使う。
/// `depth` は先頭のトークンの前の括弧の深さ（折り返した要素の中の `=` をキーワード引数として扱うため）。
fn render(src: &str, line: &[&Token], depth: usize) -> String {
    let mut out = String::new();
    let mut prev: Option<&TokenKind> = None;
    let mut prev_unary_minus = false;
    let mut depth = depth;
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
    // デコレータ `@tool(...)`（ADR-0022 論点 11）
    if matches!(prev, LParen | LBracket | LBrace | Dot | At) {
        return false;
    }
    if depth > 0 && (matches!(cur, Assign) || matches!(prev, Assign)) {
        return false; // キーワード引数 `x=1`、`x=[]`
    }
    if matches!(cur, LParen | LBracket) {
        // 呼び出し `f(` / 型引数 `f[T]` / `def f[T](`
        return !matches!(prev, Ident(_) | RParen | RBracket);
    }
    if prev_unary_minus {
        return false;
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
    fn decorators_have_no_space_after_the_at_sign() {
        assert_eq!(
            fmt("@ tool( description = \"x\" )\ndef f() -> int uses pure:\n    return 1\n"),
            "@tool(description=\"x\")\ndef f() -> int uses pure:\n    return 1\n"
        );
    }

    #[test]
    fn keyword_arguments_starting_with_a_bracket_have_no_space() {
        assert_eq!(
            fmt("def main() -> None:\n    b = Box[list[int]](value = [], other = (1))\n"),
            "def main() -> None:\n    b = Box[list[int]](value=[], other=(1))\n"
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
    fn wraps_long_lines_at_the_first_bracket() {
        let args = (0..12)
            .map(|i| format!("argument_{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let src = format!("def main() -> None:\n    total = compute({args})  # note\n");
        let items = (0..12)
            .map(|i| format!("        argument_{i}"))
            .collect::<Vec<_>>()
            .join(",\n");
        assert_eq!(
            fmt(&src),
            format!("def main() -> None:\n    total = compute(\n{items}\n    )  # note\n")
        );
    }

    #[test]
    fn wraps_nested_brackets_that_are_still_too_long() {
        let inner = (0..8)
            .map(|i| format!("field_{i}=value_{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let src = format!("def main() -> None:\n    push(Response(text=\"\", {inner}), [1, 2])\n");
        let fields = (0..8)
            .map(|i| format!("            field_{i}=value_{i}"))
            .collect::<Vec<_>>()
            .join(",\n");
        assert_eq!(
            fmt(&src),
            format!(
                "def main() -> None:\n    push(\n        Response(\n            text=\"\",\n{fields}\n        ),\n        [1, 2]\n    )\n"
            )
        );
    }

    #[test]
    fn wraps_inside_a_bracket_with_one_element() {
        let fields = (0..8)
            .map(|i| format!("field_{i}=value_{i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let src = format!("def main() -> None:\n    push(Response({fields}))\n");
        let lines = (0..8)
            .map(|i| format!("            field_{i}=value_{i}"))
            .collect::<Vec<_>>()
            .join(",\n");
        assert_eq!(
            fmt(&src),
            format!(
                "def main() -> None:\n    push(\n        Response(\n{lines}\n        )\n    )\n"
            )
        );
    }

    #[test]
    fn keeps_long_lines_without_brackets() {
        let long = "x".repeat(120);
        let src = format!("def main() -> None:\n    print(\"{long}\")\n    y = \"{long}\"\n");
        assert_eq!(fmt(&src), src);
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
