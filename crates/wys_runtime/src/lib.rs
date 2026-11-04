//! ランタイム（print、exit、算術の実行時検査）
//!
//! コンパイラが生成したコードから C ABI で呼び出される。staticlib として実行ファイルに静的リンクする。
//! ここで定義する関数のシグネチャは `wys_backend` が出力する宣言と一致させる必要がある。
//!
//! 実行時エラーの終了コードは 101 とする（PoC1 時点の暫定）。

// `#[unsafe(no_mangle)]` は unsafe 属性なので、このクレートに限り unsafe_code を許可する。
#![allow(unsafe_code)]

use std::ffi::{CStr, c_char};
use std::io::Write;
use std::sync::OnceLock;

/// プログラムに渡されたコマンドライン引数（プログラム名を除く）。`argv` の文字列はプロセスの終了まで有効（不死）。
static ARGS: OnceLock<Vec<&'static [u8]>> = OnceLock::new();

/// 生成コードの `main` が最初に呼ぶ。
///
/// # Safety
/// `argv` は C の `main` が受け取った、`argc` 個の NUL 終端文字列へのポインタの配列でなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_runtime_init(argc: i32, argv: *const *const c_char) {
    let mut args = Vec::new();
    for i in 1..argc.max(0) as usize {
        // SAFETY: C の main から渡された argv は argc 個の有効な NUL 終端文字列を指し、プロセスの終了まで有効。
        let s: &'static CStr = unsafe { CStr::from_ptr(*argv.add(i)) };
        args.push(s.to_bytes());
    }
    let _ = ARGS.set(args);
}

fn args() -> &'static [&'static [u8]] {
    ARGS.get().map_or(&[], |v| v.as_slice())
}

fn arg_at(i: i64) -> Option<&'static [u8]> {
    usize::try_from(i).ok().and_then(|i| args().get(i).copied())
}

#[unsafe(no_mangle)]
pub extern "C" fn wys_arg_count() -> i64 {
    args().len() as i64
}

#[unsafe(no_mangle)]
pub extern "C" fn wys_arg_exists(i: i64) -> bool {
    arg_at(i).is_some()
}

#[unsafe(no_mangle)]
pub extern "C" fn wys_arg_ptr(i: i64) -> *const u8 {
    arg_at(i).map_or(std::ptr::null(), |a| a.as_ptr())
}

#[unsafe(no_mangle)]
pub extern "C" fn wys_arg_len(i: i64) -> i64 {
    arg_at(i).map_or(0, |a| a.len() as i64)
}

/// `parse_scalar` の分類（ADR-0011）: 0 = int、1 = bool、2 = str、3 = null。
#[derive(Debug, PartialEq, Eq)]
enum Scalar {
    Int(i64),
    Bool(bool),
    Str,
    Null,
}

fn classify(bytes: &[u8]) -> Scalar {
    match bytes {
        b"true" => Scalar::Bool(true),
        b"false" => Scalar::Bool(false),
        b"null" => Scalar::Null,
        _ => {
            let digits = bytes.strip_prefix(b"-").unwrap_or(bytes);
            if !digits.is_empty() && digits.iter().all(u8::is_ascii_digit) {
                // 64 ビットに収まらない整数は文字列のまま扱う
                std::str::from_utf8(bytes)
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .map_or(Scalar::Str, Scalar::Int)
            } else {
                Scalar::Str
            }
        }
    }
}

/// # Safety
/// `ptr` は `len` バイト読み取り可能でなければならない。
unsafe fn bytes<'a>(ptr: *const u8, len: i64) -> &'a [u8] {
    // SAFETY: 呼び出し側（生成コード）が有効な文字列のポインタと長さを渡す。
    unsafe { std::slice::from_raw_parts(ptr, len as usize) }
}

/// # Safety
/// `ptr` は `len` バイト読み取り可能でなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_scalar_kind(ptr: *const u8, len: i64) -> i64 {
    // SAFETY: 関数の安全条件どおり。
    match classify(unsafe { bytes(ptr, len) }) {
        Scalar::Int(_) => 0,
        Scalar::Bool(_) => 1,
        Scalar::Str => 2,
        Scalar::Null => 3,
    }
}

/// # Safety
/// `ptr` は `len` バイト読み取り可能でなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_scalar_int(ptr: *const u8, len: i64) -> i64 {
    // SAFETY: 関数の安全条件どおり。
    match classify(unsafe { bytes(ptr, len) }) {
        Scalar::Int(v) => v,
        _ => 0,
    }
}

/// # Safety
/// `ptr` は `len` バイト読み取り可能でなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_scalar_bool(ptr: *const u8, len: i64) -> bool {
    // SAFETY: 関数の安全条件どおり。
    matches!(classify(unsafe { bytes(ptr, len) }), Scalar::Bool(true))
}

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
    fn classify_matches_adr_0011() {
        assert_eq!(classify(b"42"), Scalar::Int(42));
        assert_eq!(classify(b"-7"), Scalar::Int(-7));
        assert_eq!(classify(b"true"), Scalar::Bool(true));
        assert_eq!(classify(b"false"), Scalar::Bool(false));
        assert_eq!(classify(b"null"), Scalar::Null);
        assert_eq!(classify(b"True"), Scalar::Str);
        assert_eq!(classify(b"-"), Scalar::Str);
        assert_eq!(classify(b"1.5"), Scalar::Str);
        assert_eq!(classify(b"99999999999999999999"), Scalar::Str);
        assert_eq!(classify(b""), Scalar::Str);
    }

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
