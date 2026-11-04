//! 字句解析（INDENT/DEDENT を含む）
//!
//! M0 時点では骨格のみ。

#[cfg(test)]
mod tests {
    #[test]
    fn crate_builds() {
        assert_eq!(env!("CARGO_PKG_NAME"), "wys_lexer");
    }
}
