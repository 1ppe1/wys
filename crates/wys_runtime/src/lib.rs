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
            wys_free_object(p);
        }
    }
}

/// 参照カウントが 0 になったオブジェクトを解放する。参照カウントの増減は生成コードの中で行う（ADR-0012、
/// AddressSanitizer で release のしすぎを検出するため）。
///
/// # Safety
/// `p` は参照カウントが 0 になった、生存しているオブジェクトでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_free_object(p: *mut Header) {
    // SAFETY: 関数の安全条件どおり。
    unsafe {
        let len = (*p).len as usize;
        dealloc(p as *mut u8, layout(len));
    }
    LIVE.fetch_sub(1, Ordering::Relaxed);
}

/// 生成コードの `main` が最後に呼ぶ。解放されていないオブジェクトが残っていれば、コンパイラのバグとして報告する。
/// 参照カウントの操作の回数（`WYS_RC_STATS` 付きでビルドした生成コードだけが加算する。ベンチ計測用、M3 T7）。
#[unsafe(no_mangle)]
#[allow(non_upper_case_globals)]
pub static wys_stat_retains: AtomicI64 = AtomicI64::new(0);
#[unsafe(no_mangle)]
#[allow(non_upper_case_globals)]
pub static wys_stat_releases: AtomicI64 = AtomicI64::new(0);

#[unsafe(no_mangle)]
pub extern "C" fn wys_runtime_finish() {
    let _ = std::io::stdout().flush();
    if std::env::var("WYS_RC_STATS").is_ok_and(|v| !v.is_empty() && v != "0") {
        eprintln!(
            "rc stats: retain {}, release {}",
            wys_stat_retains.load(Ordering::Relaxed),
            wys_stat_releases.load(Ordering::Relaxed)
        );
    }
    let live = LIVE.load(Ordering::Relaxed);
    if live != 0 && std::env::var_os("WYS_ALLOW_LEAKS").is_none() {
        eprintln!("runtime error: {live} object(s) were not freed (this is a compiler bug)");
        std::process::exit(LEAK_EXIT_CODE);
    }
    let stacks = task::TASK_STACKS_LIVE.load(Ordering::Relaxed);
    if stacks != 0 {
        eprintln!("runtime error: {stacks} task stack(s) were not freed (this is a compiler bug)");
        std::process::exit(LEAK_EXIT_CODE);
    }
}

pub mod task;

// ---------------------------------------------------------------- 別のランタイム（Python、M4 PoC3）向け

/// ほかのランタイムのクレート（`wys_runtime_python`）が、このランタイムのヒープオブジェクトを作り・読むための関数。
/// C ABI ではなく Rust の関数として公開する（同じ staticlib にまとめてリンクするため）。
pub mod embed {
    use super::Header;

    /// 中身が `bytes` のオブジェクト（参照カウント 1、生存数に数える）を作る。
    pub fn new_object(bytes: &[u8]) -> *mut Header {
        let p = super::new_object(bytes.len());
        // SAFETY: new_object が bytes.len() バイトの中身を確保している。
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), super::data(p), bytes.len()) };
        p
    }

    /// オブジェクトの中身。
    ///
    /// # Safety
    /// `p` は生存しているオブジェクトでなければならない。
    pub unsafe fn bytes<'a>(p: *const Header) -> &'a [u8] {
        // SAFETY: 関数の安全条件どおり。
        unsafe { super::bytes(p) }
    }

    /// 参照カウントを1減らし、0 になったら `true` を返す（解放は呼び出し側が中身を片付けてから行う）。
    ///
    /// # Safety
    /// `p` は生存している、不死でないオブジェクトでなければならない。
    pub unsafe fn release_last(p: *mut Header) -> bool {
        // SAFETY: 関数の安全条件どおり。
        unsafe {
            (*p).rc -= 1;
            (*p).rc == 0
        }
    }

    /// 実行時エラーで終了する（終了コード 101）。
    pub fn runtime_error(msg: &str) -> ! {
        super::runtime_error(msg)
    }

    /// 要素が1語（ポインタ）のリストの要素を、先頭から順に読む。
    ///
    /// # Safety
    /// `l` は生存している、要素の大きさが 8 バイトのリスト。
    pub unsafe fn list_words(l: *const super::ListObj) -> Vec<usize> {
        // SAFETY: 関数の安全条件どおり。
        unsafe {
            let n = (*l).len as usize;
            let data = (*l).data as *const usize;
            (0..n).map(|i| *data.add(i)).collect()
        }
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

// ---------------------------------------------------------------- lists (M3 T4a)

/// リストのオブジェクト。先頭は `Header` と同じ配置（参照カウントの増減は生成コードが行う）。
/// 要素は `data` が指す別のバッファに、要素の大きさ（生成コードが渡す）ごとに並べる。
#[repr(C)]
pub struct ListObj {
    rc: i64,
    len: i64,
    cap: i64,
    data: *mut u8,
}

fn buffer_layout(cap: usize, elem_size: usize) -> Layout {
    Layout::from_size_align((cap * elem_size).max(1), ALIGN).expect("list too large")
}

/// 空のリスト（参照カウント 1、容量 `cap`）を作る。
#[unsafe(no_mangle)]
pub extern "C" fn wys_list_new(elem_size: i64, cap: i64) -> *mut ListObj {
    let cap = cap.max(4) as usize;
    // SAFETY: レイアウトは大きさ 1 以上で、アラインメントは 8。
    let (obj, data) = unsafe {
        (
            alloc(Layout::new::<ListObj>()) as *mut ListObj,
            alloc(buffer_layout(cap, elem_size as usize)),
        )
    };
    if obj.is_null() || data.is_null() {
        let _ = std::io::stdout().flush();
        eprintln!("runtime error: out of memory");
        std::process::abort();
    }
    // SAFETY: 確保したばかりの領域に書き込む。
    unsafe {
        obj.write(ListObj {
            rc: 1,
            len: 0,
            cap: cap as i64,
            data,
        });
    }
    LIVE.fetch_add(1, Ordering::Relaxed);
    obj
}

/// 末尾に1つ分の領域を足し、その場所を返す（生成コードが要素を書き込む）。
///
/// # Safety
/// `l` は生存しているリストで、`elem_size` は作成時と同じでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_list_push_slot(l: *mut ListObj, elem_size: i64) -> *mut u8 {
    let size = elem_size as usize;
    // SAFETY: 関数の安全条件どおり。
    unsafe {
        if (*l).len == (*l).cap {
            let old_cap = (*l).cap as usize;
            let new_cap = old_cap * 2;
            let data = std::alloc::realloc(
                (*l).data,
                buffer_layout(old_cap, size),
                (new_cap * size).max(1),
            );
            if data.is_null() {
                let _ = std::io::stdout().flush();
                eprintln!("runtime error: out of memory");
                std::process::abort();
            }
            (*l).data = data;
            (*l).cap = new_cap as i64;
        }
        let slot = (*l).data.add((*l).len as usize * size);
        (*l).len += 1;
        slot
    }
}

/// `i` 番目の要素の場所。負の添字は末尾から数える（Python と同じ）。範囲外は実行時エラー。
///
/// # Safety
/// `l` は生存しているリストで、`elem_size` は作成時と同じでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_list_slot(l: *mut ListObj, i: i64, elem_size: i64) -> *mut u8 {
    // SAFETY: 関数の安全条件どおり。
    unsafe {
        let len = (*l).len;
        let j = if i < 0 { i + len } else { i };
        if j < 0 || j >= len {
            runtime_error(&format!("index out of range: index {i}, length {len}"));
        }
        (*l).data.add(j as usize * elem_size as usize)
    }
}

/// # Safety
/// `l` は生存しているリストでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_list_len(l: *const ListObj) -> i64 {
    // SAFETY: 関数の安全条件どおり。
    unsafe { (*l).len }
}

/// 参照カウントが 0 になったリストを解放する（要素の release は生成コードが先に行う）。
///
/// # Safety
/// `l` は参照カウントが 0 になったリストで、`elem_size` は作成時と同じでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_list_free(l: *mut ListObj, elem_size: i64) {
    // SAFETY: 関数の安全条件どおり。
    unsafe {
        dealloc(
            (*l).data,
            buffer_layout((*l).cap as usize, elem_size as usize),
        );
        dealloc(l as *mut u8, Layout::new::<ListObj>());
    }
    LIVE.fetch_sub(1, Ordering::Relaxed);
}

// ---------------------------------------------------------------- dicts (M3 T6a, ADR-0015)

/// キーの種類（生成コードが `wys_dict_new` に渡す）。キーは i64 1語で受け渡す（`bool` は 0 / 1、`str` はポインタ）。
const KEY_INT: i64 = 0;
const KEY_BOOL: i64 = 1;
const KEY_STR: i64 = 2;

/// 辞書のオブジェクト。先頭は `Header` と同じ配置（参照カウントの増減は生成コードが行う）。`len` は生存しているキーの数。
/// 中身は CPython と同じく、挿入順のエントリ配列と、開番地法（線形探索）の索引表で持つ。
/// キーと値の参照カウント（retain / release）は生成コードが行う。ランタイムはキーの値（`str` ならポインタ）をそのまま保持する。
#[repr(C)]
pub struct DictObj {
    rc: i64,
    len: i64,
    inner: *mut DictInner,
}

struct DictInner {
    kind: i64,
    /// 値の大きさ（8 バイトの語数）
    val_words: usize,
    hashes: Vec<u64>,
    keys: Vec<i64>,
    /// 値はエントリごとに `val_words` 語。箱に入れない
    vals: Vec<u64>,
    /// エントリが生きているか（`pop` で削除したエントリは偽。表の伸長で詰め直す）
    live: Vec<bool>,
    /// 索引表: エントリの番号、空きは `EMPTY`、削除した場所は `DELETED`（探索を続ける）。大きさは 2 の累乗
    index: Vec<i64>,
}

const EMPTY: i64 = -1;
const DELETED: i64 = -2;

fn hash_key(kind: i64, key: i64) -> u64 {
    if kind == KEY_STR {
        // FNV-1a（乱数化しない: 反復は挿入順なので、ハッシュは順序に影響しない）
        // SAFETY: str のキーは生存している文字列オブジェクト（生成コードが +1 で保持させる）。
        let b = unsafe { bytes(key as *const Header) };
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &x in b {
            h ^= x as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    } else {
        // 整数は乗算で混ぜる（`i * 8` のような並びが同じ位置に固まらないように）
        let h = (key as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        h ^ (h >> 29)
    }
}

fn keys_equal(kind: i64, a: i64, b: i64) -> bool {
    if a == b {
        return true;
    }
    // SAFETY: str のキーは生存している文字列オブジェクト。
    kind == KEY_STR && unsafe { bytes(a as *const Header) == bytes(b as *const Header) }
}

impl DictInner {
    fn find(&self, key: i64, h: u64) -> Result<usize, usize> {
        let mask = self.index.len() - 1;
        let mut i = (h as usize) & mask;
        loop {
            let e = self.index[i];
            if e == EMPTY {
                return Err(i);
            }
            if e != DELETED {
                let e = e as usize;
                if self.hashes[e] == h && keys_equal(self.kind, self.keys[e], key) {
                    return Ok(e);
                }
            }
            i = (i + 1) & mask;
        }
    }

    /// 削除したエントリを詰めてから、索引表を大きさ `cap` で作り直す。エントリの番号は変わる。
    fn rebuild(&mut self, cap: usize) {
        if self.live.iter().any(|l| !l) {
            let w = self.val_words;
            let mut j = 0;
            for e in 0..self.hashes.len() {
                if self.live[e] {
                    self.hashes[j] = self.hashes[e];
                    self.keys[j] = self.keys[e];
                    self.vals.copy_within(e * w..(e + 1) * w, j * w);
                    j += 1;
                }
            }
            self.hashes.truncate(j);
            self.keys.truncate(j);
            self.vals.truncate(j * w);
            self.live = vec![true; j];
        }
        self.index = vec![EMPTY; cap];
        let mask = cap - 1;
        for (e, &h) in self.hashes.iter().enumerate() {
            let mut i = (h as usize) & mask;
            while self.index[i] != EMPTY {
                i = (i + 1) & mask;
            }
            self.index[i] = e as i64;
        }
    }
}

/// 空の辞書（参照カウント 1）を作る。`val_size` は値の大きさ（バイト、8 の倍数）。
#[unsafe(no_mangle)]
pub extern "C" fn wys_dict_new(kind: i64, val_size: i64) -> *mut DictObj {
    debug_assert!(matches!(kind, KEY_INT | KEY_BOOL | KEY_STR));
    let inner = Box::new(DictInner {
        kind,
        val_words: (val_size as usize).div_ceil(8).max(1),
        hashes: Vec::new(),
        keys: Vec::new(),
        vals: Vec::new(),
        live: Vec::new(),
        index: vec![EMPTY; 8],
    });
    let obj = Box::new(DictObj {
        rc: 1,
        len: 0,
        inner: Box::into_raw(inner),
    });
    LIVE.fetch_add(1, Ordering::Relaxed);
    Box::into_raw(obj)
}

/// # Safety
/// `d` は生存している辞書でなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_dict_len(d: *const DictObj) -> i64 {
    // SAFETY: 関数の安全条件どおり。
    unsafe { (*d).len }
}

/// キーのエントリの番号。なければ -1。
///
/// # Safety
/// `d` は生存している辞書で、`key` はその種類のキーでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_dict_find(d: *const DictObj, key: i64) -> i64 {
    // SAFETY: 関数の安全条件どおり。
    let inner = unsafe { &*(*d).inner };
    match inner.find(key, hash_key(inner.kind, key)) {
        Ok(e) => e as i64,
        Err(_) => -1,
    }
}

/// まだないキーのエントリを末尾に追加し、その番号を返す（値は生成コードが `wys_dict_val` の場所に書き込む）。
/// キーの所有（`str` の +1）は辞書に移る。値の領域の場所は、追加で変わり得るので、追加の後に取り直すこと。
///
/// # Safety
/// `d` は生存している辞書で、`key` はその種類の、まだ辞書にないキーでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_dict_push(d: *mut DictObj, key: i64) -> i64 {
    // SAFETY: 関数の安全条件どおり。
    let (obj, inner) = unsafe { (&mut *d, &mut *(*d).inner) };
    let h = hash_key(inner.kind, key);
    // 負荷率 2/3 を超えないように索引表を作り直す（削除したエントリを詰め、足りなければ広げる）
    if (inner.hashes.len() + 1) * 3 > inner.index.len() * 2 {
        let live = obj.len as usize;
        let mut cap = inner.index.len();
        while (live + 1) * 3 > cap * 2 {
            cap *= 2;
        }
        inner.rebuild(cap);
    }
    let slot = match inner.find(key, h) {
        Err(slot) => slot,
        Ok(_) => panic!("wys_dict_push: the key already exists"),
    };
    let e = inner.hashes.len();
    inner.hashes.push(h);
    inner.keys.push(key);
    inner.live.push(true);
    inner.vals.resize(inner.vals.len() + inner.val_words, 0);
    inner.index[slot] = e as i64;
    obj.len += 1;
    e as i64
}

/// エントリ `e` の値の場所。
///
/// # Safety
/// `d` は生存している辞書で、`e` は有効なエントリの番号でなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_dict_val(d: *mut DictObj, e: i64) -> *mut u8 {
    // SAFETY: 関数の安全条件どおり。
    let inner = unsafe { &mut *(*d).inner };
    let w = inner.val_words;
    inner.vals[e as usize * w..].as_mut_ptr() as *mut u8
}

/// エントリ `e` のキー（`str` ならポインタ）。
///
/// # Safety
/// `d` は生存している辞書で、`e` は有効なエントリの番号でなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_dict_key(d: *const DictObj, e: i64) -> i64 {
    // SAFETY: 関数の安全条件どおり。
    unsafe { (&(*(*d).inner).keys)[e as usize] }
}

/// 反復: `pos` 以降の最初のエントリの番号。なければ -1。
/// 反復の開始時のキーの数 `len0` と今の数が違えば、実行時エラー（ADR-0015）。
///
/// # Safety
/// `d` は生存している辞書でなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_dict_iter_next(d: *const DictObj, pos: i64, len0: i64) -> i64 {
    // SAFETY: 関数の安全条件どおり。
    let (len, used) = unsafe { ((*d).len, (*(*d).inner).hashes.len() as i64) };
    if len != len0 {
        runtime_error("dict changed size during iteration");
    }
    // SAFETY: 関数の安全条件どおり。
    let live = unsafe { &(*(*d).inner).live };
    let mut p = pos;
    while p < used {
        if live[p as usize] {
            return p;
        }
        p += 1;
    }
    -1
}

/// エントリ `e` を削除する（`pop`）。キーと値は、生成コードが先に取り出して所有を引き継ぐ。
///
/// # Safety
/// `d` は生存している辞書で、`e` は生きているエントリの番号でなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_dict_erase(d: *mut DictObj, e: i64) {
    // SAFETY: 関数の安全条件どおり。
    let (obj, inner) = unsafe { (&mut *d, &mut *(*d).inner) };
    let e = e as usize;
    let mask = inner.index.len() - 1;
    let mut i = (inner.hashes[e] as usize) & mask;
    while inner.index[i] != e as i64 {
        i = (i + 1) & mask;
    }
    inner.index[i] = DELETED;
    inner.live[e] = false;
    obj.len -= 1;
}

/// `d[k]` でキーがなかった（実行時エラー）。
///
/// # Safety
/// `d` は生存している辞書で、`key` はその種類のキーでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_dict_key_missing(d: *const DictObj, key: i64) -> ! {
    // SAFETY: 関数の安全条件どおり。
    let kind = unsafe { (*(*d).inner).kind };
    let shown = match kind {
        KEY_STR => {
            // SAFETY: str のキーは生存している文字列オブジェクト。
            let b = unsafe { bytes(key as *const Header) };
            format!("{:?}", String::from_utf8_lossy(b))
        }
        KEY_BOOL => (if key != 0 { "True" } else { "False" }).to_string(),
        _ => key.to_string(),
    };
    runtime_error(&format!("key not found: {shown}"));
}

/// 参照カウントが 0 になった辞書を解放する（キーと値の release は生成コードが先に行う）。
///
/// # Safety
/// `d` は参照カウントが 0 になった辞書でなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_dict_free(d: *mut DictObj) {
    // SAFETY: 関数の安全条件どおり。`wys_dict_new` が Box で確保した。
    unsafe {
        drop(Box::from_raw((*d).inner));
        drop(Box::from_raw(d));
    }
    LIVE.fetch_sub(1, Ordering::Relaxed);
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

/// `parse_scalar` の分類: 0 = int、1 = bool、2 = str、3 = null、4 = float（ADR-0019 論点 7）。
#[derive(Debug, PartialEq)]
enum Scalar {
    Int(i64),
    Bool(bool),
    Str,
    Null,
    Float(f64),
}

/// JSON の数値の文法で、小数点か指数を含むもの（`1.5`、`-0.5`、`1e3`）。無限大になるものは含まない。
fn json_float(bytes: &[u8]) -> Option<f64> {
    let mut i = usize::from(bytes.first() == Some(&b'-'));
    // 整数部は `0` か、0 でない数字で始まる（`01.5` は JSON の数値ではない）
    if bytes.get(i) == Some(&b'0') && bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
        return None;
    }
    let digits = |i: &mut usize| {
        let start = *i;
        while *i < bytes.len() && bytes[*i].is_ascii_digit() {
            *i += 1;
        }
        *i > start
    };
    if !digits(&mut i) {
        return None;
    }
    let mut fractional = false;
    if bytes.get(i) == Some(&b'.') {
        i += 1;
        if !digits(&mut i) {
            return None;
        }
        fractional = true;
    }
    if matches!(bytes.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(bytes.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        if !digits(&mut i) {
            return None;
        }
        fractional = true;
    }
    if i != bytes.len() || !fractional {
        return None;
    }
    let v: f64 = std::str::from_utf8(bytes).ok()?.parse().ok()?;
    v.is_finite().then_some(v)
}

fn classify(bytes: &[u8]) -> Scalar {
    match bytes {
        b"true" => Scalar::Bool(true),
        b"false" => Scalar::Bool(false),
        b"null" => Scalar::Null,
        _ => {
            let digits = bytes.strip_prefix(b"-").unwrap_or(bytes);
            if let Some(f) = json_float(bytes) {
                Scalar::Float(f)
            } else if !digits.is_empty() && digits.iter().all(u8::is_ascii_digit) {
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
        Scalar::Float(_) => 4,
    }
}

/// `wys_scalar_kind` が 4（float）の文字列の値。
///
/// # Safety
/// `s` は生存している文字列オブジェクトでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_scalar_float(s: *const Header) -> f64 {
    // SAFETY: 関数の安全条件どおり。
    match classify(unsafe { bytes(s) }) {
        Scalar::Float(f) => f,
        _ => 0.0,
    }
}

// ---------------------------------------------------------------- float (ADR-0019)

/// Python の `repr(float)` と同じ表記（ADR-0019 論点 6）: 元の値に戻せる最短の 10 進表記。指数が 16 以上か
/// -5 以下なら指数表記（`1e+16`、`1e-05`）、それ以外は小数点の表記で、整数の値なら `.0` を付ける。
pub fn float_repr(v: f64) -> String {
    if v.is_nan() {
        return "nan".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "inf" } else { "-inf" }.into();
    }
    if v == 0.0 {
        return if v.is_sign_negative() { "-0.0" } else { "0.0" }.into();
    }
    // `{:e}` で元の値に戻せる最短の桁数を知り、その桁数で正確に丸め直す。同じ長さの候補が2つあるとき、
    // `{:e}` は Python と違う方を選ぶことがある（`1000000000000000.25` → `...0.3`、Python は `...0.2`）。
    // 精度付きの書式は正確な値を偶数丸めするので、Python の `repr`（正確な値に最も近い最短の表記）と一致する
    let shortest = format!("{:e}", v.abs());
    let n = shortest
        .split('e')
        .next()
        .expect("{:e} has a mantissa")
        .chars()
        .filter(char::is_ascii_digit)
        .count();
    // ただし丸め直した表記が元の値に戻らないことがある（2 のべき乗は下側の間隔が半分。`2**-24` など）。
    // そのときは `{:e}` の最短の表記を使う
    let rounded = format!("{:.*e}", n - 1, v.abs());
    let sci = if rounded.parse::<f64>().ok() == Some(v.abs()) {
        rounded
    } else {
        shortest
    };
    let (mantissa, exp) = sci.split_once('e').expect("{:e} has an exponent");
    let exp: i32 = exp.parse().expect("the exponent is an integer");
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    let sign = if v < 0.0 { "-" } else { "" };
    if !(-5 < exp && exp < 16) {
        let (first, rest) = digits.split_at(1);
        let frac = if rest.is_empty() {
            String::new()
        } else {
            format!(".{rest}")
        };
        let esign = if exp < 0 { '-' } else { '+' };
        return format!("{sign}{first}{frac}e{esign}{:02}", exp.abs());
    }
    // 小数点の位置: 先頭の桁が 10^exp の位
    let point = exp + 1;
    let body = if point <= 0 {
        format!("0.{}{digits}", "0".repeat((-point) as usize))
    } else if (point as usize) >= digits.len() {
        format!("{digits}{}.0", "0".repeat(point as usize - digits.len()))
    } else {
        let (int, frac) = digits.split_at(point as usize);
        format!("{int}.{frac}")
    };
    format!("{sign}{body}")
}

#[unsafe(no_mangle)]
pub extern "C" fn wys_print_float(v: f64) {
    println!("{}", float_repr(v));
}

#[unsafe(no_mangle)]
pub extern "C" fn wys_write_float(v: f64) {
    print!("{}", float_repr(v));
}

/// `a / b`。0 での割り算は実行時エラー（ADR-0019 論点 4、Python の `ZeroDivisionError` と同じ）。
#[unsafe(no_mangle)]
pub extern "C" fn wys_float_div(a: f64, b: f64) -> f64 {
    if b == 0.0 {
        runtime_error("float division by zero");
    }
    a / b
}

/// `int(x)`: 0 の方向に切り捨てる。NaN・無限大・`int` の範囲外は実行時エラー（ADR-0019 論点 3）。
#[unsafe(no_mangle)]
pub extern "C" fn wys_float_to_int(v: f64) -> i64 {
    let t = v.trunc();
    // i64 の範囲は [-2^63, 2^63)。2^63 は f64 で正確に表せる
    if !t.is_finite() || t < -9_223_372_036_854_775_808.0 || t >= 9_223_372_036_854_775_808.0 {
        runtime_error(&format!("cannot convert {} to int", float_repr(v)));
    }
    t as i64
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

// 構造を持つ値の表示（ADR-0016 論点 16・17）。改行せずに書き出す。型ごとの表示関数は生成コードが作り、
// 区切りや名前（`[`、`P(`、`x=` など）は `wys_write_bytes` で書く。文字列を組み立てないので確保しない。

#[unsafe(no_mangle)]
pub extern "C" fn wys_write_int(v: i64) {
    print!("{v}");
}

#[unsafe(no_mangle)]
pub extern "C" fn wys_write_bool(v: bool) {
    print!("{}", if v { "True" } else { "False" });
}

/// 構造の中の文字列: `"` で囲み、`\"` `\\` `\n` `\t` `\r` `\0` をエスケープする（本言語のリテラルと同じ表記）。
///
/// # Safety
/// `s` は生存している文字列オブジェクトでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_write_str(s: *const Header) {
    let mut out = std::io::stdout().lock();
    let mut buf = Vec::with_capacity(2);
    buf.push(b'"');
    // SAFETY: 関数の安全条件どおり。
    for &c in unsafe { bytes(s) } {
        match c {
            b'"' => buf.extend_from_slice(b"\\\""),
            b'\\' => buf.extend_from_slice(b"\\\\"),
            b'\n' => buf.extend_from_slice(b"\\n"),
            b'\t' => buf.extend_from_slice(b"\\t"),
            b'\r' => buf.extend_from_slice(b"\\r"),
            0 => buf.extend_from_slice(b"\\0"),
            c => buf.push(c),
        }
    }
    buf.push(b'"');
    let _ = out.write_all(&buf);
}

/// 生成コードが持つバイト列（区切りや名前）をそのまま書く。
///
/// # Safety
/// `p` は `len` バイトの読める領域を指していなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_write_bytes(p: *const u8, len: i64) {
    // SAFETY: 関数の安全条件どおり。
    let b = unsafe { std::slice::from_raw_parts(p, len as usize) };
    let _ = std::io::stdout().lock().write_all(b);
}

#[unsafe(no_mangle)]
pub extern "C" fn wys_write_newline() {
    println!();
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

/// `panic(msg)`（ADR-0013）: `runtime error: <msg> at <場所>` を出して終了コード 101 で終わる。
///
/// # Safety
/// `msg` と `loc` は有効な文字列オブジェクトでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_panic(msg: *const Header, loc: *const Header) -> ! {
    // SAFETY: 関数の安全条件どおり。
    let msg = String::from_utf8_lossy(unsafe { bytes(msg) }).into_owned();
    // SAFETY: 関数の安全条件どおり。
    let loc = String::from_utf8_lossy(unsafe { bytes(loc) }).into_owned();
    runtime_error(&format!("{msg} at {loc}"));
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
    use std::sync::Mutex;

    /// 生存オブジェクト数（プロセス全体で共有）を使うテストは、並列に動かすと互いの確保を数えてしまうので順番に実行する。
    pub(crate) static LIVE_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn dicts_keep_insertion_order_and_grow() {
        let _guard = LIVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let before = LIVE.load(Ordering::Relaxed);
        let d = wys_dict_new(KEY_INT, 8);
        // SAFETY: d は生存している辞書。値の場所には i64 を書く。
        unsafe {
            for i in 0..1000i64 {
                assert_eq!(wys_dict_find(d, i * 8), -1);
                let e = wys_dict_push(d, i * 8);
                *(wys_dict_val(d, e) as *mut i64) = i;
            }
            assert_eq!(wys_dict_len(d), 1000);
            for i in 0..1000i64 {
                let e = wys_dict_find(d, i * 8);
                assert_eq!(e, i, "insertion order");
                assert_eq!(*(wys_dict_val(d, e) as *mut i64), i);
                assert_eq!(wys_dict_key(d, e), i * 8);
            }
            assert_eq!(wys_dict_find(d, 3), -1);
            assert_eq!(wys_dict_iter_next(d, 999, 1000), 999);
            assert_eq!(wys_dict_iter_next(d, 1000, 1000), -1);
            wys_dict_free(d);
        }
        assert_eq!(LIVE.load(Ordering::Relaxed), before);
        // 削除と再挿入: 削除したキーは見つからず、反復で飛ばされ、伸長で詰め直される
        let d = wys_dict_new(KEY_INT, 8);
        // SAFETY: d は生存している辞書。
        unsafe {
            for i in 0..100i64 {
                wys_dict_push(d, i);
            }
            for i in 0..90i64 {
                wys_dict_erase(d, wys_dict_find(d, i));
            }
            assert_eq!(wys_dict_len(d), 10);
            assert_eq!(wys_dict_find(d, 5), -1);
            assert_eq!(wys_dict_iter_next(d, 0, 10), 90);
            for i in 100..1000i64 {
                wys_dict_push(d, i);
            }
            assert_eq!(wys_dict_len(d), 910);
            assert_eq!(wys_dict_key(d, wys_dict_iter_next(d, 0, 910)), 90);
            for i in 90..1000i64 {
                assert!(wys_dict_find(d, i) >= 0);
            }
            wys_dict_free(d);
        }
        assert_eq!(LIVE.load(Ordering::Relaxed), before);
        // str のキーは中身で比べる
        let s = wys_dict_new(KEY_STR, 8);
        let a1 = new_immortal(b"key");
        let a2 = new_immortal(b"key");
        let b = new_immortal(b"other");
        // SAFETY: s は生存している辞書、キーは不死の文字列。
        unsafe {
            wys_dict_push(s, a1 as i64);
            assert_eq!(wys_dict_find(s, a2 as i64), 0);
            assert_eq!(wys_dict_find(s, b as i64), -1);
            wys_dict_free(s);
        }
        assert_eq!(LIVE.load(Ordering::Relaxed), before);
    }

    #[test]
    fn objects_are_counted_and_freed() {
        let _guard = LIVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
        let _guard = LIVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    fn lists_grow_index_and_free() {
        let _guard = LIVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let before = LIVE.load(Ordering::Relaxed);
        let l = wys_list_new(8, 1);
        // SAFETY: l は生存しているリストで、要素の大きさは 8。
        unsafe {
            for v in 0..10i64 {
                let s = wys_list_push_slot(l, 8) as *mut i64;
                s.write(v * v);
            }
            assert_eq!(wys_list_len(l), 10);
            assert_eq!(*(wys_list_slot(l, 3, 8) as *mut i64), 9);
            assert_eq!(*(wys_list_slot(l, -1, 8) as *mut i64), 81);
            assert_eq!(LIVE.load(Ordering::Relaxed), before + 1);
            (*l).rc = 0;
            wys_list_free(l, 8);
        }
        assert_eq!(LIVE.load(Ordering::Relaxed), before);
    }

    #[test]
    fn float_repr_matches_python() {
        // Python 3.14 の repr() で確かめた値
        for (v, want) in [
            (1.5, "1.5"),
            (4.0, "4.0"),
            (0.1 + 0.2, "0.30000000000000004"),
            (1e10, "10000000000.0"),
            (1e15, "1000000000000000.0"),
            (1e16, "1e+16"),
            (1.5e16, "1.5e+16"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1.5e-3, "0.0015"),
            (123.456, "123.456"),
            (-2.5, "-2.5"),
            (-0.0, "-0.0"),
            (5e-324, "5e-324"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            // 同じ長さの候補が2つある（PR #83 のレビュー）: Python は正確な値に最も近い方、同点なら偶数
            (1e15 + 0.25, "1000000000000000.2"),
            (1e15 + 0.75, "1000000000000000.8"),
            // 丸め直すと元の値に戻らない 2 のべき乗（同上のレビュー）
            (5.960464477539063e-8, "5.960464477539063e-08"),
            (5.684341886080802e-14, "5.684341886080802e-14"),
            (7.120236347223045e-307, "7.120236347223045e-307"),
            (f64::INFINITY, "inf"),
            (f64::NEG_INFINITY, "-inf"),
            (f64::NAN, "nan"),
        ] {
            assert_eq!(float_repr(v), want, "{v:?}");
        }
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
        assert_eq!(classify(b"1.5"), Scalar::Float(1.5));
        assert_eq!(classify(b"-0.5"), Scalar::Float(-0.5));
        assert_eq!(classify(b"1e3"), Scalar::Float(1000.0));
        assert_eq!(classify(b"1."), Scalar::Str);
        assert_eq!(classify(b".5"), Scalar::Str);
        assert_eq!(classify(b"inf"), Scalar::Str);
        assert_eq!(classify(b"1e400"), Scalar::Str);
        // JSON の数値ではない（整数部の先頭の 0。PR #83 のレビュー）
        assert_eq!(classify(b"01.5"), Scalar::Str);
        assert_eq!(classify(b"-01.5"), Scalar::Str);
        assert_eq!(classify(b"00e3"), Scalar::Str);
        assert_eq!(classify(b"0.5"), Scalar::Float(0.5));
        assert_eq!(classify(b"-0e1"), Scalar::Float(-0.0));
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
