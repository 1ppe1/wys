//! ランタイム（print、exit、算術の実行時検査）
//!
//! コンパイラが生成したコードから C ABI で呼び出される。staticlib として実行ファイルに静的リンクする。
//! ここで定義する関数のシグネチャは `wys_backend` が出力する宣言と一致させる必要がある。
//!
//! 実行時エラーの終了コードは 101 とする（PoC1 時点の暫定）。

// `#[unsafe(no_mangle)]` は unsafe 属性なので、このクレートに限り unsafe_code を許可する。
#![allow(unsafe_code)]

use std::io::Write;

/// 実行時エラー時の終了コード。
pub const RUNTIME_ERROR_EXIT_CODE: i32 = 101;

fn runtime_error(msg: &str) -> ! {
    let _ = std::io::stdout().flush();
    eprintln!("runtime error: {msg}");
    std::process::exit(RUNTIME_ERROR_EXIT_CODE);
}

#[unsafe(no_mangle)]
pub extern "C" fn wys_print_int(v: i64) {
    println!("{v}");
}

#[unsafe(no_mangle)]
pub extern "C" fn wys_print_bool(v: bool) {
    println!("{}", if v { "True" } else { "False" });
}

/// `ptr` はコンパイラが生成した UTF-8 の定数文字列を指し、`len` バイトの長さを持つ。
///
/// # Safety
/// `ptr` は `len` バイト読み取り可能な有効な UTF-8 データを指していなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_print_str(ptr: *const u8, len: i64) {
    // SAFETY: コンパイラは定数文字列のアドレスと正確な長さだけを渡す。
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(bytes);
    let _ = out.write_all(b"\n");
}

#[unsafe(no_mangle)]
pub extern "C" fn wys_exit(code: i64) -> ! {
    let _ = std::io::stdout().flush();
    std::process::exit(code as i32);
}

#[unsafe(no_mangle)]
pub extern "C" fn wys_panic_overflow() -> ! {
    runtime_error("integer overflow");
}

/// Python と同じ床除算（結果は負の無限大方向に丸める）。
#[unsafe(no_mangle)]
pub extern "C" fn wys_floordiv(a: i64, b: i64) -> i64 {
    if b == 0 {
        runtime_error("integer division by zero");
    }
    let Some(q) = a.checked_div(b) else {
        runtime_error("integer overflow");
    };
    if (a % b != 0) && ((a < 0) != (b < 0)) {
        q - 1
    } else {
        q
    }
}

/// Python と同じ剰余（結果の符号は除数と同じ）。
#[unsafe(no_mangle)]
pub extern "C" fn wys_mod(a: i64, b: i64) -> i64 {
    if b == 0 {
        runtime_error("integer modulo by zero");
    }
    let r = a.wrapping_rem(b);
    if r != 0 && ((r < 0) != (b < 0)) {
        r + b
    } else {
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floordiv_matches_python() {
        assert_eq!(wys_floordiv(7, 2), 3);
        assert_eq!(wys_floordiv(-7, 2), -4);
        assert_eq!(wys_floordiv(7, -2), -4);
        assert_eq!(wys_floordiv(-7, -2), 3);
    }

    #[test]
    fn mod_matches_python() {
        assert_eq!(wys_mod(7, 3), 1);
        assert_eq!(wys_mod(-7, 3), 2);
        assert_eq!(wys_mod(7, -3), -2);
        assert_eq!(wys_mod(i64::MIN, -1), 0);
    }
}
