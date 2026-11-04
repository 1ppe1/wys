//! 共通IR（COMP-001）の定義・検証・ダンプ
//!
//! M0 時点では骨格のみ。

#[cfg(test)]
mod tests {
    #[test]
    fn crate_builds() {
        assert_eq!(env!("CARGO_PKG_NAME"), "sg_ir");
    }
}
