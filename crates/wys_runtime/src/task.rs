//! `async` のランタイム（ADR-0020、M4 T10a）: 単一スレッドの協調的なスケジューラと、スタックフル・コルーチンのタスク。
//!
//! - タスクごとに `mmap` で 1 MiB のスタックを予約し、低位にガードページを置く（論点 3）。`main` のタスク（番号 0）は
//!   メインスレッドのスタックをそのまま使う。
//! - コンテキストの切り替えは手書きのアセンブリ（naked 関数）。aarch64 と x86_64 の 2 種類（論点 2）。
//! - 実行キューは FIFO。`sleep_ms` の期限は仮想時計で決め、実行キューが空のときだけ時計を進める（論点 10）。
//!   `WYS_VIRTUAL_TIME=1` なら実時間を待たない。
//! - `main` が戻ったら、残りのタスクが終わるまでスケジューラを回す。どのタスクも進めなければ deadlock（論点 14）。
//! - Python（ADR-0018）はメインスレッドのスタックでしか正しく動かない（CPython 3.14 のスタック保護が、タスクの
//!   スタック上では効かない）。`on_main_stack` で、タスクからの呼び出しをメインスタックの上で実行する（論点 16）。
//!
//! 状態はスレッドごと（`thread_local!`）に持つ。**規律:** 状態への参照をコンテキストの切り替えをまたいで持たない
//! （切り替えの前に更新を終え、戻ったら取り直す）。
//!
//! タスクのオブジェクト（ADR-0012 のヒープオブジェクト）: `{ rc, len, 番号: i64, 中身（引数と結果）... }`。
//! 中身の配置は生成コードが決める。スケジューラはタスクが終わるまで参照を 1 つ持ち、終わったら生成コードの
//! 型ごとの release（`wys_task_new` の `release`）で手放す。最後の release は `wys_task_free` を呼ぶ。

use super::{Header, data, new_object, runtime_error};
use std::cell::Cell;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, VecDeque};
use std::ffi::c_void;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, Instant};

/// タスクのスタックの大きさ（ガードページを除く）。
const STACK_SIZE: usize = 1 << 20;
/// ガードページの大きさ。macOS arm64 のページ（16 KiB）に合わせる（Linux の 4 KiB の倍数でもある）。
const GUARD_SIZE: usize = 16 << 10;
/// タスクのオブジェクトの中身の先頭に置く番号の大きさ。
const ID_SIZE: usize = 8;

/// 確保したまま解放していないタスクのスタックの数（終了時に 0 でなければ 102。`wys_runtime_finish`）。
pub(crate) static TASK_STACKS_LIVE: AtomicI64 = AtomicI64::new(0);

// ---------------------------------------------------------------- スタック

const PROT_NONE: i32 = 0;
const PROT_READ: i32 = 1;
const PROT_WRITE: i32 = 2;
const MAP_PRIVATE: i32 = 2;
#[cfg(target_os = "macos")]
const MAP_ANON: i32 = 0x1000;
#[cfg(not(target_os = "macos"))]
const MAP_ANON: i32 = 0x20;

unsafe extern "C" {
    fn mmap(addr: *mut c_void, len: usize, prot: i32, flags: i32, fd: i32, off: i64)
    -> *mut c_void;
    fn munmap(addr: *mut c_void, len: usize) -> i32;
    fn mprotect(addr: *mut c_void, len: usize, prot: i32) -> i32;
}

struct Stack {
    base: *mut u8,
}

impl Stack {
    fn new() -> Stack {
        let len = GUARD_SIZE + STACK_SIZE;
        // SAFETY: 匿名の私的な領域を新しく確保する。
        let p = unsafe {
            mmap(
                std::ptr::null_mut(),
                len,
                PROT_READ | PROT_WRITE,
                MAP_PRIVATE | MAP_ANON,
                -1,
                0,
            )
        };
        if p as isize == -1 {
            runtime_error("out of memory (task stack)");
        }
        // SAFETY: 確保したばかりの領域の先頭（ページ境界）をガードにする。
        if unsafe { mprotect(p, GUARD_SIZE, PROT_NONE) } != 0 {
            runtime_error("failed to protect a task stack");
        }
        TASK_STACKS_LIVE.fetch_add(1, Ordering::Relaxed);
        Stack { base: p as *mut u8 }
    }

    /// 使える領域の低位の端（ガードの直後）。
    fn bottom(&self) -> *mut u8 {
        // SAFETY: 確保した領域の中。
        unsafe { self.base.add(GUARD_SIZE) }
    }

    /// 高位の端（スタックはここから下へ伸びる。16 バイト境界）。
    fn top(&self) -> *mut u8 {
        // SAFETY: 確保した領域の終わり。
        unsafe { self.base.add(GUARD_SIZE + STACK_SIZE) }
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        if let Some(h) = hooks() {
            // SAFETY: このスタックの上のフレームはもうない。ASan の shadow を消す。
            unsafe { (h.unpoison)(self.bottom() as *const c_void, STACK_SIZE) };
        }
        // SAFETY: Stack::new が確保した領域を、使い終わってから一度だけ解放する。
        unsafe { munmap(self.base as *mut c_void, GUARD_SIZE + STACK_SIZE) };
        TASK_STACKS_LIVE.fetch_sub(1, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------- コンテキストの切り替え

// `wys_ctx_switch(save, to)`: 呼び出し先保存のレジスタを今のスタックに積み、スタックポインタを `*save` に書き、
// `to` に切り替えて、そちらに積んであったレジスタを戻して `ret` する。
// `trampoline`: 新しいスタックの最初の `ret` の行き先。保存してあった 2 つのレジスタから `entry(arg)` を呼ぶ（戻らない）。
// `call_on_stack(sp, f, arg)`: スタックポインタを `sp` にして `f(arg)` を呼び、元に戻る。

#[cfg(target_arch = "aarch64")]
mod arch {
    /// 切り替えで積む大きさ（x19〜x28、x29、x30、d8〜d15）。
    pub const FRAME: usize = 160;

    #[unsafe(naked)]
    pub unsafe extern "C" fn ctx_switch(_save: *mut *mut u8, _to: *mut u8) {
        core::arch::naked_asm!(
            "sub sp, sp, #160",
            "stp x19, x20, [sp, #0]",
            "stp x21, x22, [sp, #16]",
            "stp x23, x24, [sp, #32]",
            "stp x25, x26, [sp, #48]",
            "stp x27, x28, [sp, #64]",
            "stp x29, x30, [sp, #80]",
            "stp d8, d9, [sp, #96]",
            "stp d10, d11, [sp, #112]",
            "stp d12, d13, [sp, #128]",
            "stp d14, d15, [sp, #144]",
            "mov x9, sp",
            "str x9, [x0]",
            "mov sp, x1",
            "ldp x19, x20, [sp, #0]",
            "ldp x21, x22, [sp, #16]",
            "ldp x23, x24, [sp, #32]",
            "ldp x25, x26, [sp, #48]",
            "ldp x27, x28, [sp, #64]",
            "ldp x29, x30, [sp, #80]",
            "ldp d8, d9, [sp, #96]",
            "ldp d10, d11, [sp, #112]",
            "ldp d12, d13, [sp, #128]",
            "ldp d14, d15, [sp, #144]",
            "add sp, sp, #160",
            "ret",
        )
    }

    #[unsafe(naked)]
    pub unsafe extern "C" fn trampoline() {
        core::arch::naked_asm!("mov x0, x19", "blr x20", "brk #1")
    }

    #[unsafe(naked)]
    pub unsafe extern "C" fn call_on_stack(
        _sp: *mut u8,
        _f: unsafe extern "C" fn(*mut u8),
        _arg: *mut u8,
    ) {
        core::arch::naked_asm!(
            "stp x29, x30, [sp, #-16]!",
            "mov x29, sp",
            "mov sp, x0",
            "mov x0, x2",
            "blr x1",
            "mov sp, x29",
            "ldp x29, x30, [sp], #16",
            "ret",
        )
    }

    /// 新しいスタックの最初のフレームを作り、`ctx_switch` の `to` に渡すスタックポインタを返す。
    ///
    /// # Safety
    /// `top` は 16 バイト境界で、その下に `FRAME` バイト以上の書き込める領域があること。
    pub unsafe fn init_frame(top: *mut u8, entry: usize, arg: usize) -> *mut u8 {
        // SAFETY: 関数の安全条件どおり。
        unsafe {
            let sp = top.sub(FRAME);
            std::ptr::write_bytes(sp, 0, FRAME);
            let w = sp as *mut usize;
            w.write(arg); // x19
            w.add(1).write(entry); // x20
            w.add(11).write(trampoline as *const () as usize); // x30
            sp
        }
    }
}

#[cfg(target_arch = "x86_64")]
mod arch {
    /// 切り替えで積む大きさ（MXCSR と x87 の制御語、r15、r14、r13、r12、rbx、rbp と戻り先）。
    pub const FRAME: usize = 80;

    #[unsafe(naked)]
    pub unsafe extern "C" fn ctx_switch(_save: *mut *mut u8, _to: *mut u8) {
        core::arch::naked_asm!(
            "push rbp",
            "push rbx",
            "push r12",
            "push r13",
            "push r14",
            "push r15",
            // MXCSR の制御ビットと x87 の制御語も呼び出し先保存（SysV ABI）
            "sub rsp, 8",
            "stmxcsr [rsp]",
            "fnstcw [rsp + 4]",
            "mov [rdi], rsp",
            "mov rsp, rsi",
            "ldmxcsr [rsp]",
            "fldcw [rsp + 4]",
            "add rsp, 8",
            "pop r15",
            "pop r14",
            "pop r13",
            "pop r12",
            "pop rbx",
            "pop rbp",
            "ret",
        )
    }

    #[unsafe(naked)]
    pub unsafe extern "C" fn trampoline() {
        core::arch::naked_asm!("mov rdi, r12", "call r13", "ud2")
    }

    #[unsafe(naked)]
    pub unsafe extern "C" fn call_on_stack(
        _sp: *mut u8,
        _f: unsafe extern "C" fn(*mut u8),
        _arg: *mut u8,
    ) {
        core::arch::naked_asm!(
            "push rbp",
            "mov rbp, rsp",
            "mov rsp, rdi",
            "mov rdi, rdx",
            "call rsi",
            "mov rsp, rbp",
            "pop rbp",
            "ret",
        )
    }

    /// 新しいスタックの最初のフレームを作り、`ctx_switch` の `to` に渡すスタックポインタを返す。
    ///
    /// # Safety
    /// `top` は 16 バイト境界で、その下に `FRAME` バイト以上の書き込める領域があること。
    pub unsafe fn init_frame(top: *mut u8, entry: usize, arg: usize) -> *mut u8 {
        // SAFETY: 関数の安全条件どおり。`ret` の後のスタックポインタ（sp + 64 = top - 16）が 16 バイト境界になる。
        unsafe {
            let sp = top.sub(FRAME);
            std::ptr::write_bytes(sp, 0, FRAME);
            // MXCSR と x87 の制御語の既定値（すべての例外を隠し、最近接への丸め）
            (sp as *mut u32).write(0x1F80);
            (sp.add(4) as *mut u16).write(0x037F);
            let w = sp as *mut usize;
            w.add(3).write(entry); // r13
            w.add(4).write(arg); // r12
            w.add(7).write(trampoline as *const () as usize); // 戻り先
            sp
        }
    }
}

#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
compile_error!("async の切り替えは aarch64 と x86_64 だけに対応している（ADR-0020 論点 2）");

// ---------------------------------------------------------------- AddressSanitizer の hook（論点 18）

type StartSwitch = unsafe extern "C" fn(*mut *mut c_void, *const c_void, usize);
type FinishSwitch = unsafe extern "C" fn(*mut c_void, *mut *const c_void, *mut usize);
type Unpoison = unsafe extern "C" fn(*const c_void, usize);

#[derive(Clone, Copy)]
struct Hooks {
    start: StartSwitch,
    finish: FinishSwitch,
    unpoison: Unpoison,
}

static HOOKS: std::sync::OnceLock<Hooks> = std::sync::OnceLock::new();

fn hooks() -> Option<Hooks> {
    HOOKS.get().copied()
}

/// AddressSanitizer の fiber API を登録する。ASan でビルドした生成コードの `main` だけが呼ぶ
/// （`__sanitizer_start_switch_fiber`、`__sanitizer_finish_switch_fiber`、`__asan_unpoison_memory_region`）。
#[unsafe(no_mangle)]
pub extern "C" fn wys_set_fiber_hooks(
    start: StartSwitch,
    finish: FinishSwitch,
    unpoison: Unpoison,
) {
    let _ = HOOKS.set(Hooks {
        start,
        finish,
        unpoison,
    });
}

// ---------------------------------------------------------------- スケジューラ

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    /// 実行キューにいる、または実行中
    Runnable,
    /// ほかのタスクの終わりを待っている
    Waiting,
    /// タイマーを待っている
    Sleeping,
    /// `main` が戻り、残りのタスクの終わりを待っている（タスク 0 だけ）
    Draining,
    Done,
}

type Entry = unsafe extern "C" fn(*mut Header);

struct Cb {
    /// 中断中のスタックポインタ
    sp: *mut u8,
    /// タスク 0 は `None`（メインスレッドのスタック）
    stack: Option<Stack>,
    state: State,
    waiters: Vec<usize>,
    entry: Option<Entry>,
    release: Option<Entry>,
    obj: *mut Header,
    /// 終わって切り替える途中に、オブジェクトが解放された（番号は次のタスクの after_switch で空ける）
    obj_freed: bool,
    /// ASan の fiber の情報
    fake_stack: *mut c_void,
    bottom: *const c_void,
    size: usize,
}

impl Cb {
    fn new(entry: Option<Entry>, release: Option<Entry>, obj: *mut Header) -> Cb {
        Cb {
            sp: std::ptr::null_mut(),
            stack: None,
            state: State::Runnable,
            waiters: Vec::new(),
            entry,
            release,
            obj,
            obj_freed: false,
            fake_stack: std::ptr::null_mut(),
            bottom: std::ptr::null(),
            size: 0,
        }
    }
}

struct Sched {
    tasks: Vec<Option<Cb>>,
    free_ids: Vec<usize>,
    cur: usize,
    prev: usize,
    run: VecDeque<usize>,
    timers: BinaryHeap<Reverse<(u64, u64, usize)>>,
    seq: u64,
    /// 仮想時計（ミリ秒）
    now_ms: u64,
    start: Instant,
    virtual_time: bool,
    /// 始めて、まだ終わっていないタスクの数（タスク 0 を除く）
    unfinished: usize,
    /// 終わったタスクのスタック。次に動くタスクが解放する（自分のスタックの上では解放できない）
    dead_stack: Option<Stack>,
    /// 終わったタスクの番号（`dead_stack` と一緒に、次に動くタスクが片付ける）
    dead_id: Option<usize>,
    /// 終わったタスクが切り替えるときに、スタックポインタを書き捨てる場所
    dead_sp: *mut u8,
    /// `on_main_stack` の実行中
    in_service: bool,
    /// 切り替えの回数（検査用）
    switches: u64,
}

// 状態は `Box` の生ポインタで持つ。スレッドローカルにデストラクタを持たせない: タスクのスタックの上で `exit` すると
// （実行時エラー）、macOS はそのスレッドのデストラクタを走らせるので、状態を drop すると今いるスタックを解放してしまう。
thread_local! {
    static SCHED: Cell<*mut Sched> = const { Cell::new(std::ptr::null_mut()) };
}

/// スケジューラの状態。参照は短く持つこと（モジュールの説明の規律）。
fn sched() -> &'static mut Sched {
    let p = SCHED.with(|s| s.get());
    if p.is_null() {
        runtime_error("async is used outside of an async main");
    }
    // SAFETY: run_with が作り、終わるまで解放しない。単一スレッドで、呼び出し側は参照を切り替えをまたいで持たない。
    unsafe { &mut *p }
}

fn running() -> bool {
    !SCHED.with(|s| s.get()).is_null()
}

fn cb(s: &mut Sched, id: usize) -> &mut Cb {
    s.tasks[id].as_mut().expect("live task")
}

/// `main`（`async def main`）をタスク 0 として動かし、残りのタスクがすべて終わるまでスケジューラを回す。
#[unsafe(no_mangle)]
pub extern "C" fn wys_sched_run(entry: extern "C" fn()) {
    let virtual_time = std::env::var("WYS_VIRTUAL_TIME").is_ok_and(|v| !v.is_empty() && v != "0");
    run_with(virtual_time, entry);
}

fn run_with(virtual_time: bool, entry: extern "C" fn()) {
    if running() {
        runtime_error("the scheduler is already running");
    }
    let state = Box::new(Sched {
        tasks: vec![Some(Cb::new(None, None, std::ptr::null_mut()))],
        free_ids: Vec::new(),
        cur: 0,
        prev: 0,
        run: VecDeque::new(),
        timers: BinaryHeap::new(),
        seq: 0,
        now_ms: 0,
        start: Instant::now(),
        virtual_time,
        unfinished: 0,
        dead_stack: None,
        dead_id: None,
        dead_sp: std::ptr::null_mut(),
        in_service: false,
        switches: 0,
    });
    SCHED.with(|s| s.set(Box::into_raw(state)));
    entry();
    {
        let s = sched();
        if s.unfinished > 0 {
            cb(s, 0).state = State::Draining;
        }
    }
    if sched().unfinished > 0 {
        schedule();
    }
    let p = SCHED.with(|s| s.replace(std::ptr::null_mut()));
    // SAFETY: もうどのタスクも動いていない。run_with が Box::into_raw で作った状態を一度だけ解放する。
    drop(unsafe { Box::from_raw(p) });
}

/// 現在のタスクが自分の状態を更新した後に呼ぶ。次に動くタスクへ切り替える（自分が次なら戻る）。
fn schedule() {
    loop {
        let s = sched();
        if let Some(next) = s.run.pop_front() {
            if next != s.cur {
                switch_to(next);
            }
            return;
        }
        // タイマーは 1 つずつ起こす（期限が同じでも、起きたタスクが動いてから次を起こす。順序は決定的）
        if let Some(Reverse((deadline, _, id))) = s.timers.pop() {
            if !s.virtual_time {
                let target = s.start + Duration::from_millis(deadline);
                let now = Instant::now();
                if target > now {
                    std::thread::sleep(target - now);
                }
            }
            s.now_ms = s.now_ms.max(deadline);
            cb(s, id).state = State::Runnable;
            s.run.push_back(id);
            continue;
        }
        let waiting = s
            .tasks
            .iter()
            .flatten()
            .filter(|c| matches!(c.state, State::Waiting))
            .count();
        runtime_error(&format!(
            "deadlock: {waiting} task(s) are waiting but none can run"
        ));
    }
}

/// `next` へ切り替える。戻ってくるのは、ほかのタスクがこのタスクへ切り替えたとき。
fn switch_to(next: usize) {
    let s = sched();
    let cur = s.cur;
    let dying = cb(s, cur).state == State::Done;
    // 状態（cur）を書き換える前にスタックを用意する（書き換えた後は「タスク 0 は中断中」に見えるので、
    // この間に on_main_stack を呼ぶとメインスタックの使用中のフレームを踏む）
    if cb(s, next).sp.is_null() {
        // 初めて動くタスク: スタックを確保して最初のフレームを作る
        let stack = Stack::new();
        let (bottom, top) = (stack.bottom(), stack.top());
        let c = cb(s, next);
        c.bottom = bottom as *const c_void;
        c.size = STACK_SIZE;
        // SAFETY: top は 16 バイト境界のスタックの端。
        c.sp = unsafe { arch::init_frame(top, task_main as *const () as usize, next) };
        c.stack = Some(stack);
    }
    s.prev = cur;
    s.cur = next;
    s.switches += 1;
    let to = cb(s, next).sp;
    let (to_bottom, to_size) = (cb(s, next).bottom, cb(s, next).size);
    let save: *mut *mut u8 = if dying {
        &mut s.dead_sp
    } else {
        &mut cb(s, cur).sp
    };
    if let Some(h) = hooks() {
        let fake: *mut *mut c_void = if dying {
            std::ptr::null_mut()
        } else {
            &mut cb(s, cur).fake_stack
        };
        // SAFETY: ASan の fiber API の約束どおり、切り替えの直前に呼ぶ。
        unsafe { (h.start)(fake, to_bottom, to_size) };
    }
    // SAFETY: to は中断中のタスクか、init_frame で作ったスタックポインタ。参照は持っていない。
    unsafe { arch::ctx_switch(save, to) };
    after_switch();
}

/// 切り替えで戻ってきた直後（新しいタスクの最初を含む）の処理。
fn after_switch() {
    let s = sched();
    if let Some(h) = hooks() {
        let cur = s.cur;
        let fake = cb(s, cur).fake_stack;
        let mut bottom: *const c_void = std::ptr::null();
        let mut size = 0usize;
        // SAFETY: ASan の fiber API の約束どおり、切り替えの直後に呼ぶ。
        unsafe { (h.finish)(fake, &mut bottom, &mut size) };
        // 切り替え元のスタックの範囲（タスク 0 のものはここで初めて分かる）
        if s.prev == 0 {
            let main = cb(s, 0);
            main.bottom = bottom;
            main.size = size;
        }
    }
    // 終わったタスクのスタックを解放する（いまはそのスタックの上にいない）
    s.dead_stack = None;
    if let Some(d) = s.dead_id.take()
        && cb(s, d).obj_freed
    {
        s.tasks[d] = None;
        s.free_ids.push(d);
    }
}

/// 新しいタスクのスタックの最初のフレーム（trampoline から呼ばれる。戻らない）。
extern "C" fn task_main(id: usize) {
    after_switch();
    let (entry, obj) = {
        let c = cb(sched(), id);
        (c.entry.expect("task entry"), c.obj)
    };
    // SAFETY: 生成コードの入口。obj はスケジューラが参照を持つタスクのオブジェクト。
    unsafe { entry(obj) };
    crate::ai::task_finished(id);
    let release = {
        let s = sched();
        let c = cb(s, id);
        c.state = State::Done;
        let waiters = std::mem::take(&mut c.waiters);
        let release = c.release.expect("task release");
        let stack = c.stack.take();
        for w in waiters {
            cb(s, w).state = State::Runnable;
            s.run.push_back(w);
        }
        s.unfinished -= 1;
        if s.unfinished == 0 && cb(s, 0).state == State::Draining {
            cb(s, 0).state = State::Runnable;
            s.run.push_back(0);
        }
        s.dead_stack = stack;
        s.dead_id = Some(id);
        release
    };
    // スケジューラの参照を手放す（最後の参照なら wys_task_free が obj_freed を立てる）
    // SAFETY: スケジューラが持っていた +1。
    unsafe { release(obj) };
    schedule();
    // 終わったタスクには誰も切り替えない
    std::process::abort();
}

fn task_id(t: *const Header) -> usize {
    // SAFETY: t はタスクのオブジェクトで、中身の先頭に番号がある。
    unsafe { (data(t as *mut Header) as *const i64).read() as usize }
}

/// タスクのオブジェクトを作る（参照カウント 1。まだ動かさない）。中身は番号の後に `payload_len` バイトで、
/// 0 で埋める（結果を書く前に最後の参照が消えても、型ごとの release は null の結果を release するだけで済む）。
/// `entry` はタスクの本体（引数を中身から読んで関数を呼び、結果を書く）、`release` は型ごとの release。
///
/// **ABI の約束:** タスクのオブジェクトの参照は、汎用の `wys_release` ではなく必ず `release`（参照カウントを減らし、
/// 0 になったら結果を release してから `wys_task_free` を呼ぶ）で手放す。`wys_release` で解放すると制御ブロックが残る。
#[unsafe(no_mangle)]
pub extern "C" fn wys_task_new(payload_len: i64, entry: Entry, release: Entry) -> *mut Header {
    let s = sched();
    let obj = new_object(ID_SIZE + payload_len.max(0) as usize);
    let id = match s.free_ids.pop() {
        Some(id) => id,
        None => {
            s.tasks.push(None);
            s.tasks.len() - 1
        }
    };
    s.tasks[id] = Some(Cb::new(Some(entry), Some(release), obj));
    // SAFETY: 確保したばかりの中身。先頭に番号を書き、残りを 0 で埋める。
    unsafe {
        (data(obj) as *mut i64).write(id as i64);
        std::ptr::write_bytes(data(obj).add(ID_SIZE), 0, payload_len.max(0) as usize);
    }
    obj
}

/// タスクのオブジェクトの、引数と結果の領域（番号の直後）。
///
/// # Safety
/// `t` は生存しているタスクのオブジェクトでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_task_payload(t: *mut Header) -> *mut u8 {
    // SAFETY: 関数の安全条件どおり。
    unsafe { data(t).add(ID_SIZE) }
}

/// タスクを実行キューの末尾に入れる。スケジューラが参照を 1 つ持つ（`spawn`）。
///
/// # Safety
/// `t` は `wys_task_new` が作った、まだ始めていないタスクのオブジェクトでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_task_start(t: *mut Header) {
    // SAFETY: 関数の安全条件どおり。
    let s = sched();
    let id = task_id(t);
    let c = cb(s, id);
    if c.state != State::Runnable || !c.sp.is_null() || s.run.contains(&id) {
        runtime_error("a task was started twice (this is a compiler bug)");
    }
    // SAFETY: 関数の安全条件どおり。
    unsafe { super::wys_retain(t) };
    s.unfinished += 1;
    s.run.push_back(id);
    // 子タスクは親の agent コンテキストを引き継ぐ（ADR-0023 の訂正）
    crate::ai::task_spawned(s.cur, id);
}

/// タスクが終わるまで現在のタスクを止める（`await t`）。終わっていれば切り替えない。
///
/// # Safety
/// `t` は生存しているタスクのオブジェクトでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_task_wait(t: *mut Header) {
    let s = sched();
    let id = task_id(t);
    if id == s.cur {
        runtime_error("a task cannot await itself");
    }
    if cb(s, id).state == State::Done {
        return;
    }
    let cur = s.cur;
    cb(s, id).waiters.push(cur);
    cb(s, cur).state = State::Waiting;
    schedule();
}

/// タスクのオブジェクトを解放する（型ごとの release が、参照カウントが 0 になって結果を release した後に呼ぶ）。
///
/// # Safety
/// `t` は参照カウントが 0 になったタスクのオブジェクトでなければならない。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wys_task_free(t: *mut Header) {
    let id = task_id(t);
    let s = sched();
    let c = cb(s, id);
    let started = c.state != State::Runnable || !c.sp.is_null() || s.run.contains(&id);
    if started && cb(s, id).state != State::Done {
        runtime_error("a running task was freed (this is a compiler bug)");
    }
    if s.dead_id == Some(id) {
        // 終わったばかりのタスクが自分のスタックの上で手放した。切り替えた後で番号を空ける
        cb(s, id).obj_freed = true;
    } else {
        s.tasks[id] = None;
        s.free_ids.push(id);
    }
    // SAFETY: 関数の安全条件どおり。
    unsafe { super::wys_free_object(t) };
}

/// `sleep_ms(n)`: 仮想時計で `n` ミリ秒後まで現在のタスクを止める。負の `n` は 0。
#[unsafe(no_mangle)]
pub extern "C" fn wys_sleep_ms(n: i64) {
    let s = sched();
    let deadline = s.now_ms.saturating_add(n.max(0) as u64);
    let seq = s.seq;
    s.seq += 1;
    let cur = s.cur;
    s.timers.push(Reverse((deadline, seq, cur)));
    cb(s, cur).state = State::Sleeping;
    schedule();
}

// ---------------------------------------------------------------- メインスタックでの実行（論点 16）

/// `f` をメインスレッドのスタックの上で実行する。タスク 0 で動いているとき（スケジューラがないときを含む）は
/// そのまま呼ぶ。`f` の中でタスクを切り替えてはならない（Python の呼び出しは切り替えない）。
pub fn on_main_stack<R, F: FnOnce() -> R>(f: F) -> R {
    if !running() {
        return f();
    }
    let (main_sp, main_bottom, main_size) = {
        let s = sched();
        if s.cur == 0 || s.in_service {
            return f();
        }
        let m = cb(s, 0);
        (m.sp, m.bottom, m.size)
    };
    struct Call<F, R> {
        f: Option<F>,
        r: Option<R>,
        cur_bottom: *const c_void,
        cur_size: usize,
    }
    unsafe extern "C" fn thunk<F: FnOnce() -> R, R>(arg: *mut u8) {
        // SAFETY: arg は on_main_stack の Call。
        let call = unsafe { &mut *(arg as *mut Call<F, R>) };
        let h = hooks();
        if let Some(h) = h {
            // SAFETY: メインスタックに移った直後。
            unsafe {
                (h.finish)(
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
        }
        let f = call.f.take().expect("service call");
        call.r = Some(f());
        if let Some(h) = h {
            // SAFETY: タスクのスタックへ戻る直前。
            unsafe { (h.start)(std::ptr::null_mut(), call.cur_bottom, call.cur_size) };
        }
    }
    let (cur_bottom, cur_size) = {
        let s = sched();
        let cur = s.cur;
        (cb(s, cur).bottom, cb(s, cur).size)
    };
    let mut call: Call<F, R> = Call {
        f: Some(f),
        r: None,
        cur_bottom,
        cur_size,
    };
    // 中断中のタスク 0 のフレームの下（レッドゾーンを避けて 16 バイト境界）
    let sp = ((main_sp as usize - 1024) & !15) as *mut u8;
    sched().in_service = true;
    let mut fake: *mut c_void = std::ptr::null_mut();
    if let Some(h) = hooks() {
        // SAFETY: メインスタックへ移る直前。
        unsafe { (h.start)(&mut fake, main_bottom, main_size) };
    }
    // SAFETY: sp はメインスタックの使われていない領域。thunk は call を読み書きして戻る。
    unsafe { arch::call_on_stack(sp, thunk::<F, R>, &mut call as *mut Call<F, R> as *mut u8) };
    if let Some(h) = hooks() {
        // SAFETY: タスクのスタックに戻った直後。
        unsafe { (h.finish)(fake, std::ptr::null_mut(), std::ptr::null_mut()) };
    }
    sched().in_service = false;
    call.r.take().expect("service call result")
}

/// 現在のスタックポインタの近く（検査用）。
#[cfg(test)]
fn approx_sp() -> usize {
    let x = 0u8;
    std::hint::black_box(&x) as *const u8 as usize
}

#[cfg(test)]
mod tests;

/// `WYS_VIRTUAL_TIME=1` か（PoC4: agent のタイムアウトもモックの遅延だけで進める）。
pub fn virtual_time_enabled() -> bool {
    std::env::var("WYS_VIRTUAL_TIME").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// いま動いているタスクの番号（スケジューラの外なら 0 = `main`）。agent コンテキストのスタックを引く（ADR-0023 の訂正）。
pub fn current_task() -> usize {
    if running() { sched().cur } else { 0 }
}
