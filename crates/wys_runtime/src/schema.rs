//! スキーマ型（ADR-0022 論点 6・7）のランタイムの補助。
//!
//! - `validate[T]` の glue（バックエンドが型ごとに生成する）は、失敗した葉で `wys_schema_fail` を呼び、戻りながら
//!   各段が自分の区切り（`[i]`、フィールド名、`dict` のキー）を先頭に足す。経路は失敗したときだけ組み立てる。
//!   呼び出し地点は glue の戻り値を `wys_schema_finish` に渡し、`wys_schema_failed` はそれを読む（Union の構成要素の
//!   試行で途中の失敗が記録されていても、成功なら失敗と判定しない）。
//! - `to_json` の glue は、スレッドローカルのバッファに `wys_json_*` で書き、`wys_json_end` で文字列にする。glue の中で
//!   利用者のコードは動かず、タスクも切り替わらないので、バッファは 1 本でよい。

use std::cell::{Cell, RefCell};

use crate::{Header, embed};

/// JSON の種類の名前（`wys_ir::UNKNOWN_KINDS` の順。`None` は JSON の `null`）と、フィールドがないときの `missing`。
const FOUND: [&str; 8] = [
    "int", "bool", "str", "float", "null", "list", "dict", "missing",
];

enum Seg {
    Index(i64),
    Name(String),
}

#[derive(Default)]
struct Failure {
    /// 根から遠い順に積む（戻りながら足すので、描くときに逆にする）
    segs: Vec<Seg>,
    expected: String,
    found: String,
}

thread_local! {
    static LAST: RefCell<Failure> = RefCell::new(Failure::default());
    static FAILED: Cell<bool> = const { Cell::new(false) };
    static BUF: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// # Safety
/// `p` は `len` バイト読める場所でなければならない。
unsafe fn bytes_at(p: *const u8, len: i64) -> String {
    // SAFETY: 関数の安全条件どおり。
    String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(p, len as usize) }).into_owned()
}

/// 葉の不一致を記録する（それまでの経路は捨てる）。`found` は `FOUND` の番号。
///
/// # Safety
/// `expected` は `len` バイト読める場所でなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_schema_fail(expected: *const u8, len: i64, found: i64) {
    // SAFETY: 関数の安全条件どおり。
    let expected = unsafe { bytes_at(expected, len) };
    LAST.with(|l| {
        *l.borrow_mut() = Failure {
            segs: Vec::new(),
            expected,
            found: FOUND
                .get(found as usize)
                .copied()
                .unwrap_or("?")
                .to_string(),
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn wys_schema_path_index(i: i64) {
    LAST.with(|l| l.borrow_mut().segs.push(Seg::Index(i)));
}

/// フィールド名（静的なバイト列）。
///
/// # Safety
/// `name` は `len` バイト読める場所でなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_schema_path_field(name: *const u8, len: i64) {
    // SAFETY: 関数の安全条件どおり。
    let n = unsafe { bytes_at(name, len) };
    LAST.with(|l| l.borrow_mut().segs.push(Seg::Name(n)));
}

/// `dict` のキー（文字列オブジェクト）。
///
/// # Safety
/// `key` は生存している文字列オブジェクトでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_schema_path_key(key: *const Header) {
    // SAFETY: 関数の安全条件どおり。
    let n = String::from_utf8_lossy(unsafe { embed::bytes(key) }).into_owned();
    LAST.with(|l| l.borrow_mut().segs.push(Seg::Name(n)));
}

#[unsafe(no_mangle)]
pub extern "C" fn wys_schema_finish(ok: bool) {
    FAILED.with(|f| f.set(!ok));
}

#[unsafe(no_mangle)]
pub extern "C" fn wys_schema_failed() -> bool {
    FAILED.with(Cell::get)
}

/// 経路の文字列（先頭のフィールド・キーは `name`、続きは `.name`、添字は `[i]`。ADR-0022 の訂正）。
fn render(segs: &[Seg]) -> String {
    let mut out = String::new();
    for s in segs.iter().rev() {
        match s {
            Seg::Index(i) => out.push_str(&format!("[{i}]")),
            Seg::Name(n) => {
                if !out.is_empty() {
                    out.push('.');
                }
                out.push_str(n);
            }
        }
    }
    out
}

#[unsafe(no_mangle)]
pub extern "C" fn wys_schema_err_path() -> *mut Header {
    LAST.with(|l| embed::new_object(render(&l.borrow().segs).as_bytes()))
}

#[unsafe(no_mangle)]
pub extern "C" fn wys_schema_err_expected() -> *mut Header {
    LAST.with(|l| embed::new_object(l.borrow().expected.as_bytes()))
}

#[unsafe(no_mangle)]
pub extern "C" fn wys_schema_err_found() -> *mut Header {
    LAST.with(|l| embed::new_object(l.borrow().found.as_bytes()))
}

// ---------------------------------------------------------------- to_json

fn push(b: &[u8]) {
    BUF.with(|buf| buf.borrow_mut().extend_from_slice(b));
}

#[unsafe(no_mangle)]
pub extern "C" fn wys_json_begin() {
    BUF.with(|b| b.borrow_mut().clear());
}

/// 静的なバイト列（`[`、`,`、`"name":`、`null` など）。
///
/// # Safety
/// `p` は `len` バイト読める場所でなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_json_bytes(p: *const u8, len: i64) {
    // SAFETY: 関数の安全条件どおり。
    push(unsafe { std::slice::from_raw_parts(p, len as usize) });
}

#[unsafe(no_mangle)]
pub extern "C" fn wys_json_int(v: i64) {
    push(v.to_string().as_bytes());
}

#[unsafe(no_mangle)]
pub extern "C" fn wys_json_bool(v: bool) {
    push(if v { b"true" } else { b"false" });
}

/// `float` は ADR-0019 の表示（JSON の数値の文法に収まる）。NaN / 無限大は実行時エラー。
#[unsafe(no_mangle)]
pub extern "C" fn wys_json_float(v: f64) {
    if !v.is_finite() {
        crate::embed::runtime_error("cannot encode NaN or infinity as JSON");
    }
    push(crate::float_repr(v).as_bytes());
}

fn push_str(s: &[u8]) {
    let s = String::from_utf8_lossy(s);
    push(serde_json::to_string(&*s).unwrap_or_default().as_bytes());
}

/// 文字列（引用符で囲み、エスケープする）。
///
/// # Safety
/// `s` は生存している文字列オブジェクトでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_json_str(s: *const Header) {
    // SAFETY: 関数の安全条件どおり。
    push_str(unsafe { embed::bytes(s) });
}

/// `Unknown` の木（`%unknown` の場所を受け取る）。
///
/// # Safety
/// `v` は有効な `Unknown` の値を指さなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_json_unknown(v: *const crate::json::UnknownVal) {
    // SAFETY: 関数の安全条件どおり。
    unsafe { crate::json::write_unknown(*v, &mut |b| push(b), &mut |s| push_str(s)) };
}

#[unsafe(no_mangle)]
pub extern "C" fn wys_json_end() -> *mut Header {
    BUF.with(|b| {
        let out = embed::new_object(&b.borrow());
        b.borrow_mut().clear();
        out
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_render_fields_keys_and_indexes() {
        // 戻りながら足すので、葉に近い順に積む
        let segs = vec![
            Seg::Name("name".into()),
            Seg::Index(2),
            Seg::Name("steps".into()),
        ];
        assert_eq!(render(&segs), "steps[2].name");
        assert_eq!(render(&[Seg::Index(1)]), "[1]");
        assert_eq!(
            render(&[Seg::Name("k".into()), Seg::Name("meta".into())]),
            "meta.k"
        );
        assert_eq!(render(&[]), "");
    }
}
