//! 構文解析（トークン列から AST へ）
//!
//! M0 時点では骨格のみ。

#[cfg(test)]
mod tests {
    #[test]
    fn crate_builds() {
        assert_eq!(env!("CARGO_PKG_NAME"), "wys_parser");
    }
}
