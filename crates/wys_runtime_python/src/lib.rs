//! Python を埋め込むランタイム（ADR-0018、M4 T5）
//!
//! `wys_runtime` を含む staticlib（`libwys_runtime_python.a`）。Python を使うプログラムだけが、
//! `libwys_runtime.a` の代わりにこれと `libpython3.14` をリンクする（ADR-0018 論点 4）。Rust の staticlib を2つ同じ
//! 実行ファイルにリンクすると標準ライブラリの記号が重なるので、こちらを `wys_runtime` の上位集合にする。
//!
//! - CPython の C API は、Stable ABI の関数だけを手書きの `extern "C"` で宣言する（論点 2）。マクロは使わない。
//! - Python のオブジェクトは、本言語のヒープオブジェクト（ADR-0012 のヘッダ + `PyObject*` 1語）で包んだハンドル
//!   （IR の型 `py`、論点 6）。最後の release（`wys_py_release`）が `Py_DecRef` してから解放する。
//! - 失敗し得る操作は、失敗したら理由（例外の型名・メッセージ・トレースバック）を保存し、成功したら消す。
//!   生成コードは操作の直後に `wys_py_failed` で確かめて、`Result` の `Err` を作る（論点 8）。失敗したときの
//!   戻り値は null のハンドル、`0`、`false`、空の文字列のどれか（使われない）。例外はパニックにしない。
//! - 引数の値が原因の失敗（NUL を含む名前など）も、例外と同じく失敗として返す（PoC3 のレビュー）。
//! - GIL は初期化したスレッドが持ったまま手放さない（論点 3）。

#![allow(unsafe_code)]

use std::cell::RefCell;
use std::ffi::{CString, c_char, c_int, c_long};
use wys_runtime::embed;
use wys_runtime::{DictObj, Header, ListObj};

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
    fn Py_IsNone(o: *mut PyObject) -> c_int;
    fn Py_IsTrue(o: *mut PyObject) -> c_int;
    fn Py_IsFalse(o: *mut PyObject) -> c_int;
    fn Py_GetConstantBorrowed(id: u32) -> *mut PyObject;
    fn PyImport_ImportModule(name: *const c_char) -> *mut PyObject;
    fn PyObject_GetAttrString(o: *mut PyObject, name: *const c_char) -> *mut PyObject;
    fn PyObject_CallObject(callable: *mut PyObject, args: *mut PyObject) -> *mut PyObject;
    fn PyObject_Str(o: *mut PyObject) -> *mut PyObject;
    fn PyObject_Type(o: *mut PyObject) -> *mut PyObject;
    fn PyType_GetName(t: *mut PyObject) -> *mut PyObject;
    fn PyTuple_New(n: isize) -> *mut PyObject;
    fn PyTuple_SetItem(t: *mut PyObject, i: isize, o: *mut PyObject) -> c_int;
    fn PyList_Size(l: *mut PyObject) -> isize;
    fn PyList_GetItem(l: *mut PyObject, i: isize) -> *mut PyObject;
    fn PyLong_FromLongLong(v: i64) -> *mut PyObject;
    fn PyLong_AsLongLong(o: *mut PyObject) -> i64;
    fn PyBool_FromLong(v: c_long) -> *mut PyObject;
    fn PyFloat_FromDouble(v: f64) -> *mut PyObject;
    fn PyUnicode_FromStringAndSize(s: *const c_char, n: isize) -> *mut PyObject;
    fn PyUnicode_AsUTF8AndSize(o: *mut PyObject, size: *mut isize) -> *const c_char;
    fn PyErr_Occurred() -> *mut PyObject;
    fn PyErr_GetRaisedException() -> *mut PyObject;
    fn PyErr_Clear();
    fn PyObject_IsInstance(o: *mut PyObject, cls: *mut PyObject) -> c_int;
    fn PyObject_Call(
        callable: *mut PyObject,
        args: *mut PyObject,
        kwargs: *mut PyObject,
    ) -> *mut PyObject;
    fn PyFloat_AsDouble(o: *mut PyObject) -> f64;
    fn PyList_New(n: isize) -> *mut PyObject;
    fn PyList_Append(l: *mut PyObject, o: *mut PyObject) -> c_int;
    fn PySequence_Size(o: *mut PyObject) -> isize;
    fn PySequence_GetItem(o: *mut PyObject, i: isize) -> *mut PyObject;
    fn PyDict_New() -> *mut PyObject;
    fn PyDict_SetItem(d: *mut PyObject, k: *mut PyObject, v: *mut PyObject) -> c_int;
    fn PyDict_Next(
        d: *mut PyObject,
        pos: *mut isize,
        k: *mut *mut PyObject,
        v: *mut *mut PyObject,
    ) -> c_int;
    // 型オブジェクト（Stable ABI のデータ記号）
    static mut PyLong_Type: u8;
    static mut PyFloat_Type: u8;
    static mut PyList_Type: u8;
    static mut PyTuple_Type: u8;
    static mut PyDict_Type: u8;
    static mut PyUnicode_Type: u8;
}

/// `Py_GetConstantBorrowed` の `Py_CONSTANT_NONE`
const PY_CONSTANT_NONE: u32 = 0;

/// 失敗の理由（`PyError` のフィールド、ADR-0018 論点 8）
struct Failure {
    kind: String,
    message: String,
    traceback: String,
}

thread_local! {
    /// 直前の Python の操作の失敗（成功したら `None`）
    static LAST: RefCell<Option<Failure>> = const { RefCell::new(None) };
}

/// Python を初期化する。`Py_InitializeEx` は失敗すると CPython の致命的なエラーで終了し、戻ってこない
/// （`Py_InitializeFromConfig` は Stable ABI にない）。下の検査は念のため。
fn ensure_init() {
    // SAFETY: CPython の初期化。GIL は初期化したスレッドが持つ（論点 3）。
    unsafe {
        if Py_IsInitialized() == 0 {
            Py_InitializeEx(0);
            if Py_IsInitialized() == 0 {
                embed::runtime_error("cannot initialize Python");
            }
        }
    }
}

fn succeed() {
    LAST.with(|l| *l.borrow_mut() = None);
}

/// Python の例外を経由しない失敗（引数の値が原因のもの）。
fn fail(kind: &str, message: &str) {
    let f = Failure {
        kind: kind.into(),
        message: message.into(),
        traceback: String::new(),
    };
    LAST.with(|l| *l.borrow_mut() = Some(f));
}

/// Python の文字列（`str`）を Rust の文字列にする。失敗したら `None`（例外は残す）。
///
/// # Safety
/// GIL を持っていること。`o` は null でない、生存している Python のオブジェクト。
unsafe fn py_str(o: *mut PyObject) -> Option<String> {
    debug_assert!(!o.is_null());
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

/// 整形の結果（新しい参照か null）を文字列にして手放す。失敗したら、送出中の例外を消して `None`。
///
/// # Safety
/// GIL を持っていること。`o` は null か、呼び出し側が持つ新しい参照。
unsafe fn take_text(o: *mut PyObject) -> Option<String> {
    // SAFETY: 関数の安全条件どおり。null なら py_str を呼ばない。Py_DecRef は null を無視する。
    unsafe {
        let s = if o.is_null() { None } else { py_str(o) };
        Py_DecRef(o);
        if s.is_none() {
            PyErr_Clear();
        }
        s
    }
}

/// 例外の整形したトレースバック（`traceback.format_exception(exc)` を連結したもの）。取れなければ空。
///
/// # Safety
/// GIL を持っていること。`exc` は生存している例外。送出中の例外はない。
unsafe fn traceback_of(exc: *mut PyObject) -> String {
    // SAFETY: 関数の安全条件どおり。途中で失敗したら例外を消して、そこまでの文字列にする。
    unsafe {
        let m = PyImport_ImportModule(c"traceback".as_ptr());
        if m.is_null() {
            PyErr_Clear();
            return String::new();
        }
        let f = PyObject_GetAttrString(m, c"format_exception".as_ptr());
        Py_DecRef(m);
        if f.is_null() {
            PyErr_Clear();
            return String::new();
        }
        let args = PyTuple_New(1);
        if args.is_null() {
            Py_DecRef(f);
            PyErr_Clear();
            return String::new();
        }
        Py_IncRef(exc);
        PyTuple_SetItem(args, 0, exc);
        let lines = PyObject_CallObject(f, args);
        Py_DecRef(args);
        Py_DecRef(f);
        if lines.is_null() {
            PyErr_Clear();
            return String::new();
        }
        let mut out = String::new();
        let n = PyList_Size(lines);
        if n < 0 {
            // `format_exception` が置き換えられて list 以外を返した。送出中の例外を残さない
            PyErr_Clear();
        }
        for i in 0..n.max(0) {
            let line = PyList_GetItem(lines, i);
            match (!line.is_null()).then(|| py_str(line)).flatten() {
                Some(s) => out.push_str(&s),
                None => {
                    PyErr_Clear();
                    break;
                }
            }
        }
        Py_DecRef(lines);
        out
    }
}

/// 送出中の例外を取り出して、失敗として保存する。整形そのものが失敗しても代わりの文字列にする（論点 8）。
fn capture_error() {
    // SAFETY: GIL を持っている。返されたオブジェクトはここで手放す。
    let f = unsafe {
        if PyErr_Occurred().is_null() {
            Failure {
                kind: "SystemError".into(),
                message: "an operation failed without an exception".into(),
                traceback: String::new(),
            }
        } else {
            let exc = PyErr_GetRaisedException();
            let ty = PyObject_Type(exc);
            let kind = take_text(PyType_GetName(ty)).unwrap_or_else(|| "Exception".into());
            Py_DecRef(ty);
            let message = take_text(PyObject_Str(exc))
                .unwrap_or_else(|| "<str() of the exception failed>".into());
            let traceback = traceback_of(exc);
            Py_DecRef(exc);
            Failure {
                kind,
                message,
                traceback,
            }
        }
    };
    LAST.with(|l| *l.borrow_mut() = Some(f));
}

/// `py_value` の変換（失敗しない操作）の結果を包む。CPython が新しいオブジェクトを作れなかった（メモリ不足）なら、
/// 失敗のハンドルを返さずに実行時エラーにする（`py_value` は `Result` を返さない。ADR-0018 論点 8）。
fn wrap_value(o: *mut PyObject) -> *mut Header {
    let h = wrap(o);
    if h.is_null() {
        embed::runtime_error("cannot create a Python object");
    }
    h
}

/// Python の参照（+1）をハンドルで包む。null なら例外を保存して null を返す。
fn wrap(o: *mut PyObject) -> *mut Header {
    if o.is_null() {
        capture_error();
        return std::ptr::null_mut();
    }
    succeed();
    embed::new_object(&(o as usize).to_ne_bytes())
}

/// ハンドルが持つ Python の参照（借用）。
///
/// # Safety
/// `h` は `wrap` が作った、生存しているハンドル。
unsafe fn inner(h: *const Header) -> *mut PyObject {
    if h.is_null() {
        // 失敗した操作のハンドル（null）を、`wys_py_failed` を確かめずに使った: 生成コードの誤り
        embed::runtime_error(
            "a failed Python operation's handle was used (this is a compiler bug)",
        );
    }
    // SAFETY: 関数の安全条件どおり。中身は PyObject* 1語。
    let b = unsafe { embed::bytes(h) };
    usize::from_ne_bytes(b.try_into().expect("a handle holds one pointer")) as *mut PyObject
}

/// 名前を C の文字列にする。NUL を含むなら失敗を保存して `None`（プロセスは止めない）。
fn c_name(s: *const Header) -> Option<CString> {
    // SAFETY: 呼び出し側の安全条件（生存している文字列オブジェクト）。
    let b = unsafe { embed::bytes(s) };
    let c = CString::new(b).ok();
    if c.is_none() {
        fail("ValueError", "embedded null character in a Python name");
    }
    c
}

/// 直前の Python の操作が失敗したか。
#[unsafe(no_mangle)]
pub extern "C" fn wys_py_failed() -> bool {
    LAST.with(|l| l.borrow().is_some())
}

fn failure_text(pick: fn(&Failure) -> &str) -> *mut Header {
    let s = LAST.with(|l| l.borrow().as_ref().map(|f| pick(f).to_string()));
    embed::new_object(s.unwrap_or_default().as_bytes())
}

/// 直前の失敗の例外の型名（参照カウント 1 の文字列）。失敗していなければ空。
#[unsafe(no_mangle)]
pub extern "C" fn wys_py_err_kind() -> *mut Header {
    failure_text(|f| &f.kind)
}

/// 直前の失敗のメッセージ（`str(e)`）。
#[unsafe(no_mangle)]
pub extern "C" fn wys_py_err_message() -> *mut Header {
    failure_text(|f| &f.message)
}

/// 直前の失敗の整形したトレースバック。なければ空。
#[unsafe(no_mangle)]
pub extern "C" fn wys_py_err_traceback() -> *mut Header {
    failure_text(|f| &f.traceback)
}

/// `import name`。
///
/// # Safety
/// `name` は生存している文字列オブジェクト。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_py_import(name: *const Header) -> *mut Header {
    ensure_init();
    let Some(n) = c_name(name) else {
        return std::ptr::null_mut();
    };
    // SAFETY: 初期化済みで GIL を持っている。
    wrap(unsafe { PyImport_ImportModule(n.as_ptr()) })
}

/// `h.name`。
///
/// # Safety
/// `h` は生存しているハンドル、`name` は生存している文字列オブジェクト。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_py_getattr(h: *const Header, name: *const Header) -> *mut Header {
    let Some(n) = c_name(name) else {
        return std::ptr::null_mut();
    };
    // SAFETY: 関数の安全条件どおり。
    wrap(unsafe { PyObject_GetAttrString(inner(h), n.as_ptr()) })
}

/// `h(*args)`。`args` はハンドルの `list`（要素は借用で読む）。
///
/// # Safety
/// `h` は生存しているハンドル、`args` は要素がすべて生存しているハンドルのリスト。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_py_call(h: *const Header, args: *const ListObj) -> *mut Header {
    // SAFETY: 関数の安全条件どおり。PyTuple_SetItem は参照を盗むので、先に Py_IncRef する。
    unsafe {
        let items = embed::list_words(args);
        let t = PyTuple_New(items.len() as isize);
        if t.is_null() {
            return wrap(t);
        }
        for (i, &a) in items.iter().enumerate() {
            let o = inner(a as *const Header);
            Py_IncRef(o);
            PyTuple_SetItem(t, i as isize, o);
        }
        let r = PyObject_CallObject(inner(h), t);
        Py_DecRef(t);
        wrap(r)
    }
}

/// `int` から Python の `int`。
#[unsafe(no_mangle)]
pub extern "C" fn wys_py_from_int(v: i64) -> *mut Header {
    ensure_init();
    // SAFETY: 初期化済み。
    wrap_value(unsafe { PyLong_FromLongLong(v) })
}

/// `bool` から Python の `bool`。
#[unsafe(no_mangle)]
pub extern "C" fn wys_py_from_bool(v: bool) -> *mut Header {
    ensure_init();
    // SAFETY: 初期化済み。
    wrap_value(unsafe { PyBool_FromLong(v as c_long) })
}

/// `float` から Python の `float`（ADR-0019）。
#[unsafe(no_mangle)]
pub extern "C" fn wys_py_from_float(v: f64) -> *mut Header {
    ensure_init();
    // SAFETY: 初期化済み。
    wrap_value(unsafe { PyFloat_FromDouble(v) })
}

/// 文字列から Python の `str`。
///
/// # Safety
/// `s` は生存している文字列オブジェクト。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_py_from_str(s: *const Header) -> *mut Header {
    ensure_init();
    // SAFETY: 関数の安全条件どおり。
    let b = unsafe { embed::bytes(s) };
    // SAFETY: 初期化済み。b は b.len() バイトの UTF-8。
    wrap_value(unsafe {
        PyUnicode_FromStringAndSize(b.as_ptr() as *const c_char, b.len() as isize)
    })
}

/// Python の `None`。
#[unsafe(no_mangle)]
pub extern "C" fn wys_py_none() -> *mut Header {
    ensure_init();
    // SAFETY: 初期化済み。借用の参照なので、包む前に Py_IncRef する。
    unsafe {
        let n = Py_GetConstantBorrowed(PY_CONSTANT_NONE);
        Py_IncRef(n);
        wrap_value(n)
    }
}

/// Python の `int`（`bool` を含む）を読む。`int` でない、または i64 に収まらなければ失敗（論点 11）。
/// `PyLong_AsLongLong` は `int` でないオブジェクトにも `__index__` で暗黙に変換するので、先に型を確かめる。
///
/// # Safety
/// `h` は生存しているハンドル。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_py_to_int(h: *const Header) -> i64 {
    // SAFETY: 関数の安全条件どおり。-1 は値か失敗のどちらかなので、例外の有無で見分ける。
    unsafe {
        let o = inner(h);
        match PyObject_IsInstance(o, &raw mut PyLong_Type as *mut PyObject) {
            1 => {}
            0 => {
                fail("TypeError", "expected an int");
                return 0;
            }
            _ => {
                capture_error();
                return 0;
            }
        }
        let v = PyLong_AsLongLong(o);
        if v == -1 && !PyErr_Occurred().is_null() {
            capture_error();
            return 0;
        }
        succeed();
        v
    }
}

/// `o` が型 `t` のインスタンスか（例外は消す）。
///
/// # Safety
/// GIL を持っていること。`o` は生存しているオブジェクト、`t` は型オブジェクト。
unsafe fn is_instance(o: *mut PyObject, t: *mut u8) -> bool {
    // SAFETY: 関数の安全条件どおり。
    unsafe {
        match PyObject_IsInstance(o, t as *mut PyObject) {
            1 => true,
            0 => false,
            _ => {
                PyErr_Clear();
                false
            }
        }
    }
}

/// Python の `float` を読む。`float` でなければ失敗（`int` も暗黙に変換しない、ADR-0018 論点 11）。
///
/// # Safety
/// `h` は生存しているハンドル。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_py_to_float(h: *const Header) -> f64 {
    // SAFETY: 関数の安全条件どおり。
    unsafe {
        let o = inner(h);
        if !is_instance(o, &raw mut PyFloat_Type) {
            fail("TypeError", "expected a float");
            return 0.0;
        }
        let v = PyFloat_AsDouble(o);
        if v == -1.0 && !PyErr_Occurred().is_null() {
            capture_error();
            return 0.0;
        }
        succeed();
        v
    }
}

/// Python の `list` / `tuple` を、要素のハンドルの `list[PyObject]`（参照カウント 1）にする。それ以外は失敗
/// （空のリストを返す）。要素はコピーせず、ハンドルで包む。
///
/// # Safety
/// `h` は生存しているハンドル。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_py_to_list(h: *const Header) -> *mut ListObj {
    // SAFETY: 関数の安全条件どおり。要素は新しい参照（PySequence_GetItem）をハンドルで包む。
    unsafe {
        let o = inner(h);
        if !(is_instance(o, &raw mut PyList_Type) || is_instance(o, &raw mut PyTuple_Type)) {
            fail("TypeError", "expected a list or tuple");
            return wys_runtime::wys_list_new(8, 0);
        }
        let n = PySequence_Size(o);
        if n < 0 {
            // `__len__` が例外を出した（list のサブクラス）: 例外を残さず、失敗にする（PR #85 のレビュー）
            capture_error();
            return wys_runtime::wys_list_new(8, 0);
        }
        let l = wys_runtime::wys_list_new(8, n as i64);
        for i in 0..n {
            let item = PySequence_GetItem(o, i);
            if item.is_null() {
                // 要素の取得に失敗した（並びが途中で変わった）: 取り出した分は解放し、失敗にする
                capture_error();
                for w in embed::list_words(l) {
                    wys_py_release(w as *mut Header);
                }
                wys_runtime::wys_list_free(l, 8);
                return wys_runtime::wys_list_new(8, 0);
            }
            *(wys_runtime::wys_list_push_slot(l, 8) as *mut *mut Header) =
                embed::new_object(&(item as usize).to_ne_bytes());
        }
        succeed();
        l
    }
}

/// Python の `dict`（キーがすべて `str`）を `dict[str, PyObject]`（参照カウント 1）にする。それ以外は失敗
/// （空の辞書を返す）。値はコピーせず、ハンドルで包む。
///
/// # Safety
/// `h` は生存しているハンドル。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_py_to_dict(h: *const Header) -> *mut DictObj {
    const KEY_STR: i64 = 2;
    // SAFETY: 関数の安全条件どおり。PyDict_Next は借用の参照を返すので、包む前に Py_IncRef する。
    unsafe {
        let o = inner(h);
        if !is_instance(o, &raw mut PyDict_Type) {
            fail("TypeError", "expected a dict");
            return wys_runtime::wys_dict_new(KEY_STR, 8);
        }
        // 先にキーがすべて str か確かめる（途中で失敗して、作りかけの辞書を片付けずに済むように）
        let mut entries = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut pos: isize = 0;
        let (mut k, mut v) = (std::ptr::null_mut(), std::ptr::null_mut());
        while PyDict_Next(o, &mut pos, &mut k, &mut v) != 0 {
            match is_instance(k, &raw mut PyUnicode_Type)
                .then(|| py_str(k))
                .flatten()
            {
                // 別々のキー（str のサブクラス）が同じ文字列になることがある。本言語の dict では区別できない
                // （PR #85 のレビュー）
                Some(key) if !seen.insert(key.clone()) => {
                    fail(
                        "ValueError",
                        &format!("the dict has more than one key equal to {key:?} as a str"),
                    );
                    return wys_runtime::wys_dict_new(KEY_STR, 8);
                }
                Some(key) => entries.push((key, v)),
                None => {
                    PyErr_Clear();
                    fail("TypeError", "dict keys must be str");
                    return wys_runtime::wys_dict_new(KEY_STR, 8);
                }
            }
        }
        let d = wys_runtime::wys_dict_new(KEY_STR, 8);
        for (key, v) in entries {
            // 辞書がキーの文字列（+1）を持つ
            let ks = embed::new_object(key.as_bytes());
            let e = wys_runtime::wys_dict_push(d, ks as i64);
            Py_IncRef(v);
            *(wys_runtime::wys_dict_val(d, e) as *mut *mut Header) =
                embed::new_object(&(v as usize).to_ne_bytes());
        }
        succeed();
        d
    }
}

/// 新しい空の Python の `list`（`py_value` の変換用。失敗しない）。
#[unsafe(no_mangle)]
pub extern "C" fn wys_py_new_list() -> *mut Header {
    ensure_init();
    // SAFETY: 初期化済み。
    wrap_value(unsafe { PyList_New(0) })
}

/// `l.append(item)`。`item` のハンドルの所有は、この関数に移る（`py_value` の変換用）。
///
/// # Safety
/// `l` は Python の `list` のハンドル、`item` は生存しているハンドル。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_py_list_append(l: *const Header, item: *mut Header) {
    // SAFETY: 関数の安全条件どおり。PyList_Append は参照を盗まない。
    unsafe {
        if PyList_Append(inner(l), inner(item)) < 0 {
            embed::runtime_error("cannot create a Python object");
        }
        wys_py_release(item);
    }
}

/// 新しい空の Python の `dict`（`py_value` の変換用。失敗しない）。
#[unsafe(no_mangle)]
pub extern "C" fn wys_py_new_dict() -> *mut Header {
    ensure_init();
    // SAFETY: 初期化済み。
    wrap_value(unsafe { PyDict_New() })
}

/// `d[k] = v`。`k` と `v` のハンドルの所有は、この関数に移る（`py_value` の変換用）。
///
/// # Safety
/// `d` は Python の `dict` のハンドル、`k` と `v` は生存しているハンドル。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_py_dict_set(d: *const Header, k: *mut Header, v: *mut Header) {
    // SAFETY: 関数の安全条件どおり。PyDict_SetItem は参照を盗まない。
    unsafe {
        if PyDict_SetItem(inner(d), inner(k), inner(v)) < 0 {
            embed::runtime_error("cannot create a Python object");
        }
        wys_py_release(k);
        wys_py_release(v);
    }
}

/// `h(*args, **kwargs)`。`kwargs` は `dict[str, PyObject]`（要素は借用で読む）。
///
/// # Safety
/// `h` は生存しているハンドル、`args` はハンドルのリスト、`kwargs` はキーが `str` で値がハンドルの辞書。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_py_call_kw(
    h: *const Header,
    args: *const ListObj,
    kwargs: *const DictObj,
) -> *mut Header {
    // SAFETY: 関数の安全条件どおり。
    unsafe {
        let items = embed::list_words(args);
        let t = PyTuple_New(items.len() as isize);
        if t.is_null() {
            return wrap(t);
        }
        for (i, &a) in items.iter().enumerate() {
            let o = inner(a as *const Header);
            Py_IncRef(o);
            PyTuple_SetItem(t, i as isize, o);
        }
        let kw = PyDict_New();
        if kw.is_null() {
            Py_DecRef(t);
            return wrap(kw);
        }
        let len = wys_runtime::wys_dict_len(kwargs);
        let mut pos = 0;
        loop {
            let e = wys_runtime::wys_dict_iter_next(kwargs, pos, len);
            if e < 0 {
                break;
            }
            let key = wys_runtime::wys_dict_key(kwargs, e) as *const Header;
            let b = embed::bytes(key);
            let k = PyUnicode_FromStringAndSize(b.as_ptr() as *const c_char, b.len() as isize);
            let v = *(wys_runtime::wys_dict_val(kwargs as *mut DictObj, e) as *const *const Header);
            if k.is_null() || PyDict_SetItem(kw, k, inner(v)) < 0 {
                Py_DecRef(k);
                Py_DecRef(kw);
                Py_DecRef(t);
                return wrap(std::ptr::null_mut());
            }
            Py_DecRef(k);
            pos = e + 1;
        }
        let r = PyObject_Call(inner(h), t, kw);
        Py_DecRef(kw);
        Py_DecRef(t);
        wrap(r)
    }
}

/// Python の `bool` を読む。`bool` でなければ失敗（真偽値への暗黙の変換をしない、論点 11）。
///
/// # Safety
/// `h` は生存しているハンドル。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_py_to_bool(h: *const Header) -> bool {
    // SAFETY: 関数の安全条件どおり。
    let (t, f) = unsafe {
        let o = inner(h);
        (Py_IsTrue(o) != 0, Py_IsFalse(o) != 0)
    };
    if !(t || f) {
        fail("TypeError", "expected a bool");
        return false;
    }
    succeed();
    t
}

/// Python の `str` を本言語の文字列（参照カウント 1）にする。`str` でなければ失敗（空の文字列を返す）。
///
/// # Safety
/// `h` は生存しているハンドル。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_py_to_str(h: *const Header) -> *mut Header {
    // SAFETY: 関数の安全条件どおり。
    match unsafe { py_str(inner(h)) } {
        Some(s) => {
            succeed();
            embed::new_object(s.as_bytes())
        }
        None => {
            capture_error();
            embed::new_object(b"")
        }
    }
}

/// Python の `None` か。失敗しない。
///
/// # Safety
/// `h` は生存しているハンドル。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_py_is_none(h: *const Header) -> bool {
    // SAFETY: 関数の安全条件どおり。
    unsafe { Py_IsNone(inner(h)) != 0 }
}

/// ハンドルの release。参照カウントが 0 になったら Python の参照を手放してから解放する（論点 6）。
///
/// # Safety
/// `h` は null か、生存しているハンドル。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_py_release(h: *mut Header) {
    if h.is_null() {
        return;
    }
    // SAFETY: 関数の安全条件どおり。終了処理の後（生成コードからは起きない）は Python の参照に触れない。
    unsafe {
        if embed::release_last(h) {
            if Py_IsInitialized() != 0 {
                Py_DecRef(inner(h));
            }
            wys_runtime::wys_free_object(h);
        }
    }
}

/// Python の終了処理（`Py_FinalizeEx`、論点 7）。初期化していなければ何もしない。失敗しても終了コードは変えない。
#[unsafe(no_mangle)]
pub extern "C" fn wys_py_finish() {
    // SAFETY: GIL を持っているスレッドから呼ぶ。
    unsafe {
        if Py_IsInitialized() != 0 && Py_FinalizeEx() < 0 {
            eprintln!("warning: Python finalization reported an error");
        }
    }
}
