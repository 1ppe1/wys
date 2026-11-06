//! `parse_json`（ADR-0021 論点 5）: JSON の文字列を `Unknown` の木にする。
//!
//! `serde_json` の `Deserializer` を独自の visitor で駆動する（`serde_json::Value` は使わない: 重複したキーを
//! 後勝ちで通し、`-0` を `float` にするため）。いったん Rust の `Node` の木を作り、成功したときだけ本言語の
//! ヒープオブジェクトに写す（失敗したときに作りかけの木を解放する処理が要らない）。
//!
//! `Unknown` の値は `{ i8 タグ, [1 x i64] 中身 }`（生成コードの `%unknown`）。タグは `wys_ir::UNKNOWN_KINDS` の順。
//! `list` / `dict` の種類は、要素（値）の大きさ 16 バイトの `list[Unknown]` / `dict[str, Unknown]` で、
//! 生成コードの `@wys_release_list{l}` / `@wys_release_dict{d}` がそのまま解放できる形にする。

use std::cell::{Cell, RefCell};
use std::fmt;

use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};

use crate::{
    Header, KEY_STR, ListObj, embed, wys_dict_new, wys_dict_push, wys_dict_val, wys_list_new,
    wys_list_push_slot,
};

// タグ（`wys_ir::UNKNOWN_KINDS` と同じ順）
pub(crate) const UNK_INT: u8 = 0;
pub(crate) const UNK_BOOL: u8 = 1;
pub(crate) const UNK_STR: u8 = 2;
pub(crate) const UNK_FLOAT: u8 = 3;
pub(crate) const UNK_NONE: u8 = 4;
pub(crate) const UNK_LIST: u8 = 5;
pub(crate) const UNK_DICT: u8 = 6;

/// 要素の大きさ（バイト）: タグ 1 語 + 中身 1 語。
const UNKNOWN_SIZE: i64 = 16;

/// 入れ子の深さの上限（ADR-0021 論点 5。タスクのスタック 1 MiB と、同じ深さで再帰する解放処理を守る）。
const MAX_DEPTH: u32 = 128;

/// 生成コードの `%unknown` と同じ配置。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct UnknownVal {
    pub tag: u8,
    pub word: i64,
}

#[derive(Debug)]
enum Node {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Node>),
    Dict(Vec<(String, Node)>),
}

struct Failure {
    message: String,
    offset: i64,
}

thread_local! {
    static LAST: RefCell<Option<Failure>> = const { RefCell::new(None) };
}

/// 解析の状態。数値の表記（`-0` は `int`）は入力を先に走査して、出現の順に引く。
struct State {
    depth: Cell<u32>,
    /// 数値ごとに、小数点も指数もない表記か
    int_forms: Vec<bool>,
    next_number: Cell<usize>,
}

impl State {
    fn take_number(&self) -> bool {
        let i = self.next_number.get();
        self.next_number.set(i + 1);
        self.int_forms.get(i).copied().unwrap_or(false)
    }
}

/// 文字列の外にある数値のトークンを順に調べ、小数点も指数もない表記かを返す。
fn number_forms(s: &str) -> Vec<bool> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            b'-' | b'0'..=b'9' => {
                let start = i;
                while i < b.len() && matches!(b[i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9')
                {
                    i += 1;
                }
                out.push(!b[start..i].iter().any(|c| matches!(c, b'.' | b'e' | b'E')));
            }
            _ => i += 1,
        }
    }
    out
}

struct Seed<'a>(&'a State);

impl<'de> DeserializeSeed<'de> for Seed<'_> {
    type Value = Node;
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Node, D::Error> {
        d.deserialize_any(NodeVisitor(self.0))
    }
}

struct NodeVisitor<'a>(&'a State);

impl NodeVisitor<'_> {
    fn enter<E: de::Error>(&self) -> Result<(), E> {
        let d = self.0.depth.get() + 1;
        if d > MAX_DEPTH {
            return Err(E::custom("nesting too deep"));
        }
        self.0.depth.set(d);
        Ok(())
    }

    fn leave(&self) {
        self.0.depth.set(self.0.depth.get() - 1);
    }
}

impl<'de> Visitor<'de> for NodeVisitor<'_> {
    type Value = Node;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_unit<E>(self) -> Result<Node, E> {
        Ok(Node::Null)
    }

    fn visit_bool<E>(self, v: bool) -> Result<Node, E> {
        Ok(Node::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Node, E> {
        self.0.take_number();
        Ok(Node::Int(v))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Node, E> {
        self.0.take_number();
        // `i64` に収まらない整数は `float`
        Ok(i64::try_from(v).map_or(Node::Float(v as f64), Node::Int))
    }

    fn visit_f64<E>(self, v: f64) -> Result<Node, E> {
        // `serde_json` は `-0` を `-0.0` として渡す。表記が整数なら `int` の 0（ADR-0021 論点 5）
        if self.0.take_number() && v == 0.0 {
            return Ok(Node::Int(0));
        }
        Ok(Node::Float(v))
    }

    fn visit_str<E>(self, v: &str) -> Result<Node, E> {
        Ok(Node::Str(v.to_string()))
    }

    fn visit_string<E>(self, v: String) -> Result<Node, E> {
        Ok(Node::Str(v))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Node, A::Error> {
        self.enter()?;
        let mut items = Vec::new();
        while let Some(n) = seq.next_element_seed(Seed(self.0))? {
            items.push(n);
        }
        self.leave();
        Ok(Node::List(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Node, A::Error> {
        self.enter()?;
        let mut entries: Vec<(String, Node)> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        while let Some(k) = map.next_key::<String>()? {
            if !seen.insert(k.clone()) {
                return Err(de::Error::custom(format!("duplicate key {k:?}")));
            }
            let v = map.next_value_seed(Seed(self.0))?;
            entries.push((k, v));
        }
        self.leave();
        Ok(Node::Dict(entries))
    }
}

/// `serde_json` のエラーを、メッセージと UTF-8 のバイト位置にする。位置は誤りを見つけた地点。
fn failure(s: &str, e: &serde_json::Error) -> Failure {
    let text = e.to_string();
    let message = match text.rsplit_once(" at line ") {
        Some((m, _)) => m.to_string(),
        None => text,
    };
    let message = if message == "recursion limit exceeded" {
        "nesting too deep".to_string()
    } else {
        message
    };
    // 行・列（1 始まり、列はバイト数）をバイト位置にする
    let (line, col) = (e.line(), e.column());
    let line_start: usize = s
        .split_inclusive('\n')
        .take(line.saturating_sub(1))
        .map(str::len)
        .sum();
    // 入力の途中で終わったら、位置は入力の長さ（`serde_json` の列は最後に読んだバイトを指す）
    let offset = if e.classify() == serde_json::error::Category::Eof {
        s.len()
    } else {
        (line_start + col.saturating_sub(1)).min(s.len())
    };
    Failure {
        message,
        offset: offset as i64,
    }
}

fn parse(s: &str) -> Result<Node, Failure> {
    let state = State {
        depth: Cell::new(0),
        int_forms: number_forms(s),
        next_number: Cell::new(0),
    };
    let mut de = serde_json::Deserializer::from_str(s);
    // 深さは visitor が数える（上限 128 を自分で決めるため）
    de.disable_recursion_limit();
    let node = Seed(&state)
        .deserialize(&mut de)
        .map_err(|e| failure(s, &e))?;
    de.end().map_err(|e| failure(s, &e))?;
    Ok(node)
}

/// `Node` を本言語の値にする（参照型の種類は参照カウント 1 のオブジェクト）。
fn build(n: Node) -> UnknownVal {
    match n {
        Node::Null => UnknownVal {
            tag: UNK_NONE,
            word: 0,
        },
        Node::Bool(b) => UnknownVal {
            tag: UNK_BOOL,
            word: b as i64,
        },
        Node::Int(v) => UnknownVal {
            tag: UNK_INT,
            word: v,
        },
        Node::Float(v) => UnknownVal {
            tag: UNK_FLOAT,
            word: v.to_bits() as i64,
        },
        Node::Str(s) => UnknownVal {
            tag: UNK_STR,
            word: embed::new_object(s.as_bytes()) as i64,
        },
        Node::List(items) => {
            let l: *mut ListObj = wys_list_new(UNKNOWN_SIZE, items.len() as i64);
            for it in items {
                let v = build(it);
                // SAFETY: l は作ったばかりのリストで、要素の大きさは作成時と同じ 16 バイト。
                unsafe { (wys_list_push_slot(l, UNKNOWN_SIZE) as *mut UnknownVal).write(v) };
            }
            UnknownVal {
                tag: UNK_LIST,
                word: l as i64,
            }
        }
        Node::Dict(entries) => {
            let d = wys_dict_new(KEY_STR, UNKNOWN_SIZE);
            for (k, v) in entries {
                let key = embed::new_object(k.as_bytes()) as i64;
                let v = build(v);
                // SAFETY: d は作ったばかりの辞書で、キーは重複しない（解析で確かめた）。値の場所は追加の後に取る。
                unsafe {
                    let e = wys_dict_push(d, key);
                    (wys_dict_val(d, e) as *mut UnknownVal).write(v);
                }
            }
            UnknownVal {
                tag: UNK_DICT,
                word: d as i64,
            }
        }
    }
}

/// `parse_json(s)`: 成功なら木を `out` に書く（+1 所有）。失敗なら `None` の種類を書き、理由を覚える。
///
/// # Safety
/// `s` は生存している文字列オブジェクト、`out` は `%unknown` を書ける場所でなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_json_parse(s: *const Header, out: *mut UnknownVal) {
    // SAFETY: 関数の安全条件どおり。本言語の `str` は UTF-8。
    let text = unsafe { embed::bytes(s) };
    let text = std::str::from_utf8(text).unwrap_or_default();
    let v = match parse(text) {
        Ok(n) => {
            LAST.with(|l| *l.borrow_mut() = None);
            build(n)
        }
        Err(f) => {
            LAST.with(|l| *l.borrow_mut() = Some(f));
            UnknownVal {
                tag: UNK_NONE,
                word: 0,
            }
        }
    };
    // SAFETY: 関数の安全条件どおり。
    unsafe { out.write(v) };
}

/// 直前の `wys_json_parse` が失敗したか。
#[unsafe(no_mangle)]
pub extern "C" fn wys_json_failed() -> bool {
    LAST.with(|l| l.borrow().is_some())
}

/// 直前の失敗のメッセージ（参照カウント 1 の文字列）。
#[unsafe(no_mangle)]
pub extern "C" fn wys_json_err_message() -> *mut Header {
    LAST.with(|l| {
        let m = l
            .borrow()
            .as_ref()
            .map(|f| f.message.clone())
            .unwrap_or_default();
        embed::new_object(m.as_bytes())
    })
}

/// 直前の失敗の位置（UTF-8 のバイト位置）。
#[unsafe(no_mangle)]
pub extern "C" fn wys_json_err_offset() -> i64 {
    LAST.with(|l| l.borrow().as_ref().map_or(0, |f| f.offset))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::LIVE_LOCK;
    use crate::{
        DictObj, LIVE, wys_dict_free, wys_dict_iter_next, wys_dict_key, wys_dict_len,
        wys_free_object, wys_list_free, wys_list_len, wys_list_slot,
    };
    use std::sync::atomic::Ordering;

    fn ok(s: &str) -> Node {
        match parse(s) {
            Ok(n) => n,
            Err(f) => panic!("{s}: {} at {}", f.message, f.offset),
        }
    }

    fn err(s: &str) -> (String, i64) {
        match parse(s) {
            Ok(n) => panic!("{s}: expected an error, got {n:?}"),
            Err(f) => (f.message, f.offset),
        }
    }

    fn nested(n: usize) -> String {
        "[".repeat(n) + &"]".repeat(n)
    }

    #[test]
    fn numbers_follow_adr_0021() {
        assert!(matches!(ok("-0"), Node::Int(0)));
        assert!(matches!(ok("[-0, 1]"), Node::List(v) if matches!(v[0], Node::Int(0))));
        assert!(matches!(ok("-0.0"), Node::Float(v) if v == 0.0 && v.is_sign_negative()));
        assert!(matches!(ok("1.0"), Node::Float(v) if v == 1.0));
        assert!(matches!(ok("1e2"), Node::Float(v) if v == 100.0));
        assert!(matches!(ok("9223372036854775807"), Node::Int(i64::MAX)));
        assert!(matches!(ok("-9223372036854775808"), Node::Int(i64::MIN)));
        assert!(matches!(ok("9223372036854775808"), Node::Float(_)));
        assert!(matches!(ok("18446744073709551616"), Node::Float(_)));
        // 文字列の中の数字は数えない
        assert!(
            matches!(ok(r#"{"a-1": "2.5", "b": -0}"#), Node::Dict(e) if matches!(e[1].1, Node::Int(0)))
        );
        assert_eq!(err("1e400").0, "number out of range");
    }

    #[test]
    fn failures_have_messages_and_offsets() {
        assert_eq!(err(r#"{"a":1,"a":2}"#).0, r#"duplicate key "a""#);
        assert!(matches!(ok(&nested(128)), Node::List(_)));
        assert_eq!(err(&nested(129)).0, "nesting too deep");
        assert_eq!(err("\u{feff}1"), ("expected value".to_string(), 0));
        assert_eq!(err("1 2"), ("trailing characters".to_string(), 2));
        assert_eq!(err(""), ("EOF while parsing a value".to_string(), 0));
        assert_eq!(err("[1,"), ("EOF while parsing a value".to_string(), 3));
        assert_eq!(err(r#"{"a":}"#), ("expected value".to_string(), 5));
        assert_eq!(err("[1,\n  x]"), ("expected value".to_string(), 6));
    }

    /// 生成コードの解放処理と同じことをする（テストで生存数を確かめるため）。
    unsafe fn release(v: UnknownVal) {
        // SAFETY: v は build が作った値。
        unsafe {
            match v.tag {
                UNK_STR => drop_obj(v.word as *mut Header),
                UNK_LIST => {
                    let l = v.word as *mut ListObj;
                    for i in 0..wys_list_len(l) {
                        release(*(wys_list_slot(l, i, UNKNOWN_SIZE) as *const UnknownVal));
                    }
                    wys_list_free(l, UNKNOWN_SIZE);
                }
                UNK_DICT => {
                    let d = v.word as *mut DictObj;
                    let len = wys_dict_len(d);
                    let mut e = wys_dict_iter_next(d, 0, len);
                    while e >= 0 {
                        drop_obj(wys_dict_key(d, e) as *mut Header);
                        release(*(wys_dict_val(d, e) as *const UnknownVal));
                        e = wys_dict_iter_next(d, e + 1, len);
                    }
                    wys_dict_free(d);
                }
                _ => {}
            }
        }
    }

    unsafe fn drop_obj(p: *mut Header) {
        // SAFETY: p は参照カウント 1 の文字列。
        unsafe {
            if embed::release_last(p) {
                wys_free_object(p);
            }
        }
    }

    #[test]
    fn trees_are_built_as_heap_objects_and_freed() {
        let _guard = LIVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let before = LIVE.load(Ordering::Relaxed);
        let s = embed::new_object(br#"{"k": [1, 2.5, "s", true, null, {"k": []}], "z": "y"}"#);
        let mut out = UnknownVal { tag: 0, word: 0 };
        // SAFETY: s は生存している文字列、out は書ける場所。
        unsafe { wys_json_parse(s, &mut out) };
        assert!(!wys_json_failed());
        assert_eq!(out.tag, UNK_DICT);
        // SAFETY: out は dict の木。
        unsafe {
            let d = out.word as *mut DictObj;
            assert_eq!(wys_dict_len(d), 2);
            let k = *(wys_dict_val(d, 0) as *const UnknownVal);
            assert_eq!(k.tag, UNK_LIST);
            assert_eq!(wys_list_len(k.word as *const ListObj), 6);
            release(out);
            drop_obj(s);
        }
        assert_eq!(LIVE.load(Ordering::Relaxed), before);
        // 失敗: None の種類と、理由
        let bad = embed::new_object(b"[1,");
        // SAFETY: 同上。
        unsafe { wys_json_parse(bad, &mut out) };
        assert!(wys_json_failed());
        assert_eq!(out.tag, UNK_NONE);
        let m = wys_json_err_message();
        // SAFETY: m は参照カウント 1 の文字列。
        unsafe {
            assert_eq!(embed::bytes(m), b"EOF while parsing a value");
            drop_obj(m);
            drop_obj(bad);
        }
        assert_eq!(LIVE.load(Ordering::Relaxed), before);
    }
}
