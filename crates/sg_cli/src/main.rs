//! コマンドラインインターフェース（build/run/check）
//!
//! M0 時点では骨格のみ。実行ファイル名は M1 で決める（`sg` は既存コマンドと衝突するため避ける）。

fn main() {
    println!("sg_cli {}", env!("CARGO_PKG_VERSION"));
}
