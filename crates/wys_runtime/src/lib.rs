//! ランタイム（ヒープオブジェクト、出力、外部入力、算術の実行時検査）
//!
//! コンパイラが生成したコードから C ABI で呼び出される。staticlib として実行ファイルに静的リンクする。
//! ここで定義する関数のシグネチャは `wys_backend` が出力する宣言と一致させる必要がある。
//! C ABI にはスカラーとポインタだけを渡す（ADR-0009）。
//!
//! ヒープオブジェクト（ADR-0012）: `{ 参照カウント: i64, 長さ: i64, 中身... }`。
//! 参照カウントが負のオブジェクトは不死で、retain / release は何もしない（文字列リテラル、コマンドライン引数）。
//!
//! 実行時エラーの終了コードは 101。生成コードの誤りでオブジェクトが解放されずに残った場合は 102。

// `#[unsafe(no_mangle)]` は unsafe 属性なので、このクレートに限り unsafe_code を許可する。
#![allow(unsafe_code)]

use std::alloc::{Layout, alloc, dealloc};
use std::ffi::{CStr, c_char};
use std::io::Write;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI64, Ordering};

/// 実行時エラー時の終了コード。
pub const RUNTIME_ERROR_EXIT_CODE: i32 = 101;
/// プログラムの終了時に、解放されていないオブジェクトがあった場合の終了コード（コンパイラのバグ）。
pub const LEAK_EXIT_CODE: i32 = 102;

fn runtime_error(msg: &str) -> ! {
    let _ = std::io::stdout().flush();
    eprintln!("runtime error: {msg}");
    std::process::exit(RUNTIME_ERROR_EXIT_CODE);
}

// ---------------------------------------------------------------- heap objects

/// オブジェクトの先頭。生成コードの LLVM 型 `{ i64, i64, [N x i8] }` と同じ配置。
#[repr(C)]
pub struct Header {
    rc: i64,
    len: i64,
}

const HEADER: usize = std::mem::size_of::<Header>();
const ALIGN: usize = 8;

/// 生存している（確保されて、まだ解放されていない）不死でないオブジェクトの数。
static LIVE: AtomicI64 = AtomicI64::new(0);

fn layout(len: usize) -> Layout {
    Layout::from_size_align(HEADER + len.max(1), ALIGN).expect("object too large")
}

/// 中身が `len` バイトのオブジェクトを確保する（参照カウント 1）。
fn new_object(len: usize) -> *mut Header {
    // SAFETY: レイアウトは大きさ 1 以上で、アラインメントは 8。
    let p = unsafe { alloc(layout(len)) } as *mut Header;
    if p.is_null() {
        let _ = std::io::stdout().flush();
        eprintln!("runtime error: out of memory");
        std::process::abort();
    }
    // SAFETY: 確保したばかりの領域に書き込む。
    unsafe {
        p.write(Header {
            rc: 1,
            len: len as i64,
        });
    }
    LIVE.fetch_add(1, Ordering::Relaxed);
    p
}

/// 不死のオブジェクトを作る（解放しない。生存数にも数えない）。
fn new_immortal(bytes: &[u8]) -> *mut Header {
    let p = new_object(bytes.len());
    LIVE.fetch_sub(1, Ordering::Relaxed);
    // SAFETY: new_object が中身の領域を確保している。
    unsafe {
        (*p).rc = -1;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), data(p), bytes.len());
    }
    p
}

/// オブジェクトの中身の先頭。
fn data(p: *mut Header) -> *mut u8 {
    // SAFETY: 中身はヘッダの直後に置かれている。
    unsafe { (p as *mut u8).add(HEADER) }
}

/// オブジェクトの中身をバイト列として読む。
///
/// # Safety
/// `p` は生成コードかこのランタイムが作った、生存しているオブジェクトでなければならない。
unsafe fn bytes<'a>(p: *const Header) -> &'a [u8] {
    // SAFETY: 関数の安全条件どおり。
    unsafe { std::slice::from_raw_parts(data(p as *mut Header), (*p).len as usize) }
}

/// # Safety
/// `p` は null か、生存しているオブジェクトでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_retain(p: *mut Header) {
    if p.is_null() {
        return;
    }
    // SAFETY: 関数の安全条件どおり。
    unsafe {
        if (*p).rc >= 0 {
            (*p).rc += 1;
        }
    }
}

/// 参照カウントを減らし、0 になったら解放する。
///
/// # Safety
/// `p` は null か、生存しているオブジェクトでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_release(p: *mut Header) {
    if p.is_null() {
        return;
    }
    // SAFETY: 関数の安全条件どおり。
    unsafe {
        if (*p).rc < 0 {
            return; // 不死
        }
        (*p).rc -= 1;
        if (*p).rc == 0 {
            let len = (*p).len as usize;
            dealloc(p as *mut u8, layout(len));
            LIVE.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// 生成コードの `main` が最後に呼ぶ。解放されていないオブジェクトが残っていれば、コンパイラのバグとして報告する。
#[unsafe(no_mangle)]
pub extern "C" fn wys_runtime_finish() {
    let _ = std::io::stdout().flush();
    let live = LIVE.load(Ordering::Relaxed);
    if live != 0 && std::env::var_os("WYS_ALLOW_LEAKS").is_none() {
        eprintln!("runtime error: {live} object(s) were not freed (this is a compiler bug)");
        std::process::exit(LEAK_EXIT_CODE);
    }
}

// ---------------------------------------------------------------- strings (M3 T2b)

/// 連結した新しい文字列オブジェクト（参照カウント 1）を返す。
///
/// # Safety
/// `a` と `b` は生存している文字列オブジェクトでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_str_concat(a: *const Header, b: *const Header) -> *mut Header {
    // SAFETY: 関数の安全条件どおり。
    let (a, b) = unsafe { (bytes(a), bytes(b)) };
    let p = new_object(a.len() + b.len());
    // SAFETY: new_object が a.len() + b.len() バイトの中身を確保している。
    unsafe {
        std::ptr::copy_nonoverlapping(a.as_ptr(), data(p), a.len());
        std::ptr::copy_nonoverlapping(b.as_ptr(), data(p).add(a.len()), b.len());
    }
    p
}

/// # Safety
/// `a` と `b` は生存している文字列オブジェクトでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_str_eq(a: *const Header, b: *const Header) -> bool {
    // SAFETY: 関数の安全条件どおり。
    unsafe { bytes(a) == bytes(b) }
}

/// 文字数（Unicode のコードポイント数。Python の `len` と同じ）。
///
/// # Safety
/// `s` は生存している文字列オブジェクトでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_str_len(s: *const Header) -> i64 {
    // SAFETY: 関数の安全条件どおり。
    let b = unsafe { bytes(s) };
    std::str::from_utf8(b).map_or(b.len(), |s| s.chars().count()) as i64
}

// ---------------------------------------------------------------- command line

/// プログラムに渡されたコマンドライン引数（プログラム名を除く）。不死のオブジェクトとして持つ。
static ARGS: OnceLock<Vec<usize>> = OnceLock::new();

/// 生成コードの `main` が最初に呼ぶ。
///
/// # Safety
/// `argv` は C の `main` が受け取った、`argc` 個の NUL 終端文字列へのポインタの配列でなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_runtime_init(argc: i32, argv: *const *const c_char) {
    let mut args = Vec::new();
    for i in 1..argc.max(0) as usize {
        // SAFETY: C の main から渡された argv は argc 個の有効な NUL 終端文字列を指す。
        let s: &CStr = unsafe { CStr::from_ptr(*argv.add(i)) };
        args.push(new_immortal(s.to_bytes()) as usize);
    }
    let _ = ARGS.set(args);
}

fn arg_at(i: i64) -> Option<*mut Header> {
    let args = ARGS.get().map_or(&[][..], |v| v.as_slice());
    usize::try_from(i)
        .ok()
        .and_then(|i| args.get(i).map(|p| *p as *mut Header))
}

#[unsafe(no_mangle)]
pub extern "C" fn wys_arg_count() -> i64 {
    ARGS.get().map_or(0, |v| v.len() as i64)
}

#[unsafe(no_mangle)]
pub extern "C" fn wys_arg_exists(i: i64) -> bool {
    arg_at(i).is_some()
}

/// `i` 番目の引数（不死の文字列オブジェクト）。`wys_arg_exists` が真の番号にだけ使う。
#[unsafe(no_mangle)]
pub extern "C" fn wys_arg(i: i64) -> *mut Header {
    arg_at(i).unwrap_or_else(|| runtime_error("argument index out of range"))
}

// ---------------------------------------------------------------- parse_scalar (ADR-0011)

/// `parse_scalar` の分類: 0 = int、1 = bool、2 = str、3 = null。
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
/// `s` は生存している文字列オブジェクトでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_scalar_kind(s: *const Header) -> i64 {
    // SAFETY: 関数の安全条件どおり。
    match classify(unsafe { bytes(s) }) {
        Scalar::Int(_) => 0,
        Scalar::Bool(_) => 1,
        Scalar::Str => 2,
        Scalar::Null => 3,
    }
}

/// # Safety
/// `s` は生存している文字列オブジェクトでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_scalar_int(s: *const Header) -> i64 {
    // SAFETY: 関数の安全条件どおり。
    match classify(unsafe { bytes(s) }) {
        Scalar::Int(v) => v,
        _ => 0,
    }
}

/// # Safety
/// `s` は生存している文字列オブジェクトでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_scalar_bool(s: *const Header) -> bool {
    // SAFETY: 関数の安全条件どおり。
    matches!(classify(unsafe { bytes(s) }), Scalar::Bool(true))
}

// ---------------------------------------------------------------- output and control

#[unsafe(no_mangle)]
pub extern "C" fn wys_print_int(v: i64) {
    println!("{v}");
}

#[unsafe(no_mangle)]
pub extern "C" fn wys_print_bool(v: bool) {
    println!("{}", if v { "True" } else { "False" });
}

/// # Safety
/// `s` は生存している文字列オブジェクトでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_print_str(s: *const Header) {
    let mut out = std::io::stdout().lock();
    // SAFETY: 関数の安全条件どおり。
    let _ = out.write_all(unsafe { bytes(s) });
    let _ = out.write_all(b"\n");
}

#[unsafe(no_mangle)]
pub extern "C" fn wys_exit(code: i64) -> ! {
    let _ = std::io::stdout().flush();
    std::process::exit(code as i32);
}

/// `wys test` が実行するテストの番号（環境変数 `WYS_TEST_INDEX`）。なければ -1。
#[unsafe(no_mangle)]
pub extern "C" fn wys_test_index() -> i64 {
    std::env::var("WYS_TEST_INDEX")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(-1)
}

/// `assert` の失敗。`loc` は「ファイル:行:列」の不死の文字列オブジェクト。
///
/// # Safety
/// `loc` は生存している文字列オブジェクトでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_assert_fail(loc: *const Header) -> ! {
    // SAFETY: 関数の安全条件どおり。
    let loc = String::from_utf8_lossy(unsafe { bytes(loc) }).into_owned();
    runtime_error(&format!("assertion failed at {loc}"));
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
    fn objects_are_counted_and_freed() {
        let before = LIVE.load(Ordering::Relaxed);
        let p = new_object(5);
        assert_eq!(LIVE.load(Ordering::Relaxed), before + 1);
        // SAFETY: p は生存しているオブジェクト。
        unsafe {
            wys_retain(p);
            assert_eq!((*p).rc, 2);
            wys_release(p);
            wys_release(p);
        }
        assert_eq!(LIVE.load(Ordering::Relaxed), before);
        // null と不死は何もしない
        // SAFETY: null は許される。不死のオブジェクトは解放されない。
        unsafe {
            wys_retain(std::ptr::null_mut());
            wys_release(std::ptr::null_mut());
            let i = new_immortal(b"abc");
            wys_release(i);
            assert_eq!((*i).rc, -1);
            assert_eq!(bytes(i), b"abc");
        }
        assert_eq!(LIVE.load(Ordering::Relaxed), before);
    }

    #[test]
    fn string_operations() {
        let before = LIVE.load(Ordering::Relaxed);
        let a = new_immortal("あい".as_bytes());
        let b = new_immortal(b"u");
        // SAFETY: a と b は不死のオブジェクト、c は連結で作ったオブジェクト。
        unsafe {
            let c = wys_str_concat(a, b);
            assert_eq!(bytes(c), "あいu".as_bytes());
            assert_eq!(wys_str_len(c), 3);
            assert!(wys_str_eq(c, c));
            assert!(!wys_str_eq(c, a));
            assert_eq!(LIVE.load(Ordering::Relaxed), before + 1);
            wys_release(c);
        }
        assert_eq!(LIVE.load(Ordering::Relaxed), before);
    }

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
