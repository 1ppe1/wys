//! Python を埋め込むランタイム（M4 T1、PoC3）
//!
//! `wys_runtime` を含む staticlib（`libwys_runtime_python.a`）。Python を使うプログラムだけが、
//! `libwys_runtime.a` の代わりにこれと `libpython3.14` をリンクする。Rust の staticlib を2つ同じ実行ファイルに
//! リンクすると標準ライブラリの記号が重なるので、こちらを `wys_runtime` の上位集合にする。
//!
//! - CPython の C API は、Stable ABI の関数だけを手書きの `extern "C"` で宣言する（外部クレートに依存しない。
//!   ユーザー決定、`docs/plans/m4.md`）。マクロ（`Py_TYPE`、`PyLong_Check` など）は使わない。
//! - Python のオブジェクトは、本言語のヒープオブジェクト（ADR-0012 のヘッダ + `PyObject*` 1語）で包んだハンドルにする。
//!   解放（`wys_py_release`）が `Py_DecRef` を呼ぶ。これで生存数の検査（終了コード 102）がハンドルにも効く。
//! - 失敗する操作はハンドル（または文字列）を返し、失敗なら null を返して、例外を「型名: メッセージ」の文字列として
//!   保存する（`wys_py_take_error` で取り出す）。例外はパニックにしない（AC-010）。
//! - GIL は初期化したスレッドが持ったまま手放さない（本言語はシングルスレッド）。
//!
//! これは試作で、名前と ABI は ADR-0018 で決め直す。

#![allow(unsafe_code)]

use std::cell::RefCell;
use std::ffi::{CString, c_char, c_int};
use std::time::Instant;
use wys_runtime::Header;
use wys_runtime::embed;

#[repr(C)]
pub struct PyObject {
    _private: [u8; 0],
}

// CPython 3.14 の Stable ABI（Limited API）にある関数だけ
#[allow(non_snake_case)]
unsafe extern "C" {
    fn Py_InitializeEx(initsigs: c_int);
    fn Py_IsInitialized() -> c_int;
    fn Py_FinalizeEx() -> c_int;
    fn Py_IncRef(o: *mut PyObject);
    fn Py_DecRef(o: *mut PyObject);
    fn PyImport_ImportModule(name: *const c_char) -> *mut PyObject;
    fn PyObject_GetAttrString(o: *mut PyObject, name: *const c_char) -> *mut PyObject;
    fn PyObject_CallObject(callable: *mut PyObject, args: *mut PyObject) -> *mut PyObject;
    fn PyObject_Str(o: *mut PyObject) -> *mut PyObject;
    fn PyObject_Type(o: *mut PyObject) -> *mut PyObject;
    fn PyType_GetName(t: *mut PyObject) -> *mut PyObject;
    fn PyTuple_New(n: isize) -> *mut PyObject;
    fn PyTuple_SetItem(t: *mut PyObject, i: isize, o: *mut PyObject) -> c_int;
    fn PyLong_FromLongLong(v: i64) -> *mut PyObject;
    fn PyLong_AsLongLong(o: *mut PyObject) -> i64;
    fn PyUnicode_FromStringAndSize(s: *const c_char, n: isize) -> *mut PyObject;
    fn PyUnicode_AsUTF8AndSize(o: *mut PyObject, size: *mut isize) -> *const c_char;
    fn PyErr_Occurred() -> *mut PyObject;
    fn PyErr_GetRaisedException() -> *mut PyObject;
}

thread_local! {
    /// 直前の失敗の「型名: メッセージ」
    static LAST_ERROR: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// 初期化にかかった時間（PoC の計測用。マイクロ秒）
static INIT_MICROS: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(-1);

fn ensure_init() {
    // SAFETY: CPython の初期化。GIL は初期化したスレッドが持つ。
    unsafe {
        if Py_IsInitialized() == 0 {
            let t = Instant::now();
            Py_InitializeEx(0);
            if Py_IsInitialized() == 0 {
                embed::runtime_error("cannot initialize Python");
            }
            INIT_MICROS.store(
                t.elapsed().as_micros() as i64,
                std::sync::atomic::Ordering::Relaxed,
            );
        }
    }
}

/// Python の文字列（`str`）を Rust の文字列にする。失敗したら `None`（例外は残す）。
///
/// # Safety
/// GIL を持っていること。`o` は生存している Python のオブジェクト。
unsafe fn py_str(o: *mut PyObject) -> Option<String> {
    let mut n: isize = 0;
    // SAFETY: 関数の安全条件どおり。
    let p = unsafe { PyUnicode_AsUTF8AndSize(o, &mut n) };
    if p.is_null() {
        return None;
    }
    // SAFETY: CPython が n バイトの UTF-8 を返した。
    let b = unsafe { std::slice::from_raw_parts(p as *const u8, n as usize) };
    Some(String::from_utf8_lossy(b).into_owned())
}

/// 送出中の例外を取り出し、「型名: メッセージ」として保存する。
fn capture_error() {
    // SAFETY: GIL を持っている。返されたオブジェクトはここで手放す。
    let text = unsafe {
        if PyErr_Occurred().is_null() {
            "SystemError: an operation failed without an exception".to_string()
        } else {
            let exc = PyErr_GetRaisedException();
            let ty = PyObject_Type(exc);
            let name = PyType_GetName(ty);
            let msg = PyObject_Str(exc);
            let name_s = py_str(name).unwrap_or_else(|| "Exception".into());
            let msg_s = py_str(msg).unwrap_or_default();
            for o in [msg, name, ty, exc] {
                Py_DecRef(o);
            }
            if msg_s.is_empty() {
                name_s
            } else {
                format!("{name_s}: {msg_s}")
            }
        }
    };
    LAST_ERROR.with(|e| *e.borrow_mut() = Some(text));
}

/// Python の参照（+1）をハンドルで包む。null なら例外を保存して null を返す。
fn wrap(o: *mut PyObject) -> *mut Header {
    if o.is_null() {
        capture_error();
        return std::ptr::null_mut();
    }
    embed::new_object(&(o as usize).to_ne_bytes())
}

/// ハンドルが持つ Python の参照（借用）。
///
/// # Safety
/// `h` は `wrap` が作った、生存しているハンドル。
unsafe fn inner(h: *const Header) -> *mut PyObject {
    // SAFETY: 関数の安全条件どおり。中身は PyObject* 1語。
    let b = unsafe { embed::bytes(h) };
    usize::from_ne_bytes(b.try_into().expect("a handle holds one pointer")) as *mut PyObject
}

fn c_name(s: *const Header) -> CString {
    // SAFETY: 呼び出し側の安全条件（生存している文字列オブジェクト）。
    let b = unsafe { embed::bytes(s) };
    CString::new(b).unwrap_or_else(|_| embed::runtime_error("a Python name contains a NUL byte"))
}

/// `import name`。失敗なら null（`wys_py_take_error` で理由を取り出す）。
///
/// # Safety
/// `name` は生存している文字列オブジェクト。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_py_import(name: *const Header) -> *mut Header {
    ensure_init();
    let n = c_name(name);
    // SAFETY: 初期化済みで GIL を持っている。
    wrap(unsafe { PyImport_ImportModule(n.as_ptr()) })
}

/// `h.name`。失敗なら null。
///
/// # Safety
/// `h` は生存しているハンドル、`name` は生存している文字列オブジェクト。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_py_getattr(h: *const Header, name: *const Header) -> *mut Header {
    let n = c_name(name);
    // SAFETY: 関数の安全条件どおり。
    wrap(unsafe { PyObject_GetAttrString(inner(h), n.as_ptr()) })
}

/// `h(args[0], ..., args[n-1])`。引数のハンドルは借用で読む。失敗なら null。
///
/// # Safety
/// `h` と `args` の各要素は生存しているハンドル。`args` は `n` 個の要素を持つ。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_py_call(
    h: *const Header,
    args: *const *const Header,
    n: i64,
) -> *mut Header {
    // SAFETY: 関数の安全条件どおり。PyTuple_SetItem は参照を盗むので、先に Py_IncRef する。
    unsafe {
        let t = PyTuple_New(n as isize);
        if t.is_null() {
            return wrap(t);
        }
        for i in 0..n as usize {
            let a = inner(*args.add(i));
            Py_IncRef(a);
            PyTuple_SetItem(t, i as isize, a);
        }
        let r = PyObject_CallObject(inner(h), t);
        Py_DecRef(t);
        wrap(r)
    }
}

/// `int` の値から Python の `int` を作る。
#[unsafe(no_mangle)]
pub extern "C" fn wys_py_from_int(v: i64) -> *mut Header {
    ensure_init();
    // SAFETY: 初期化済み。
    wrap(unsafe { PyLong_FromLongLong(v) })
}

/// 文字列から Python の `str` を作る。
///
/// # Safety
/// `s` は生存している文字列オブジェクト。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_py_from_str(s: *const Header) -> *mut Header {
    ensure_init();
    // SAFETY: 関数の安全条件どおり。
    let b = unsafe { embed::bytes(s) };
    // SAFETY: 初期化済み。b は b.len() バイトの UTF-8。
    wrap(unsafe { PyUnicode_FromStringAndSize(b.as_ptr() as *const c_char, b.len() as isize) })
}

/// Python の `int` を読む。`int` でなければ（あふれも含む）`false` を返し、例外を保存する。
///
/// # Safety
/// `h` は生存しているハンドル、`out` は書き込める。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_py_to_int(h: *const Header, out: *mut i64) -> bool {
    // SAFETY: 関数の安全条件どおり。-1 は値か失敗のどちらかなので、例外の有無で見分ける。
    unsafe {
        let v = PyLong_AsLongLong(inner(h));
        if v == -1 && !PyErr_Occurred().is_null() {
            capture_error();
            return false;
        }
        *out = v;
    }
    true
}

/// Python の `str` を本言語の文字列（参照カウント 1）にする。`str` でなければ null。
///
/// # Safety
/// `h` は生存しているハンドル。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_py_to_str(h: *const Header) -> *mut Header {
    // SAFETY: 関数の安全条件どおり。
    match unsafe { py_str(inner(h)) } {
        Some(s) => embed::new_object(s.as_bytes()),
        None => {
            capture_error();
            std::ptr::null_mut()
        }
    }
}

/// 直前の失敗の理由（「型名: メッセージ」、参照カウント 1 の文字列）。なければ空文字列。
#[unsafe(no_mangle)]
pub extern "C" fn wys_py_take_error() -> *mut Header {
    let e = LAST_ERROR
        .with(|e| e.borrow_mut().take())
        .unwrap_or_default();
    embed::new_object(e.as_bytes())
}

/// ハンドルの release。参照カウントが 0 になったら Python の参照を手放してから解放する。
///
/// # Safety
/// `h` は null か、生存しているハンドル。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_py_release(h: *mut Header) {
    if h.is_null() {
        return;
    }
    // SAFETY: 関数の安全条件どおり。
    unsafe {
        if embed::release_last(h) {
            Py_DecRef(inner(h));
            wys_runtime::wys_free_object(h);
        }
    }
}

/// Python の終了処理（`Py_FinalizeEx`）。初期化していなければ何もしない。
#[unsafe(no_mangle)]
pub extern "C" fn wys_py_finish() {
    // SAFETY: GIL を持っているスレッドから呼ぶ。
    unsafe {
        if Py_IsInitialized() != 0 && Py_FinalizeEx() < 0 {
            eprintln!("warning: Python finalization reported an error");
        }
    }
}

/// PoC の計測: 初期化にかかった時間（マイクロ秒）。初期化していなければ -1。
#[unsafe(no_mangle)]
pub extern "C" fn wys_py_init_micros() -> i64 {
    INIT_MICROS.load(std::sync::atomic::Ordering::Relaxed)
}
