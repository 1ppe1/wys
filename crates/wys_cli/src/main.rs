//! コマンドラインインターフェース（build/run/check）
//!
//! M0 時点では骨格のみ。実行ファイル名は M1 で決める。

fn main() {
    println!("wys_cli {}", env!("CARGO_PKG_VERSION"));
}
