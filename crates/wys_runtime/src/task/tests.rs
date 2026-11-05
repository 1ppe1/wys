//! スケジューラの単体テスト（ADR-0020、M4 T10a）。生成コードの代わりに、タスクの本体と型ごとの release を Rust で書く。
//!
//! タスクの中身の配置（このテストでの約束）: `{ 引数: i64, 結果: i64 }`。

use super::*;
use crate::LIVE;
use std::cell::RefCell;
use std::process::Command;

thread_local! {
    static LOG: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn log(s: impl Into<String>) {
    LOG.with(|l| l.borrow_mut().push(s.into()));
}

fn take_log() -> Vec<String> {
    LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
}

fn arg(t: *mut Header) -> i64 {
    // SAFETY: テストのタスクは中身の先頭に引数を置く。
    unsafe { (wys_task_payload(t) as *const i64).read() }
}

fn set_result(t: *mut Header, v: i64) {
    // SAFETY: テストのタスクは中身の 2 語目に結果を置く。
    unsafe { (wys_task_payload(t) as *mut i64).add(1).write(v) }
}

fn result(t: *mut Header) -> i64 {
    // SAFETY: 同上。
    unsafe { (wys_task_payload(t) as *const i64).add(1).read() }
}

/// 型ごとの release の代わり（結果は i64 なので release するものはない）。
unsafe extern "C" fn release_task(t: *mut Header) {
    // SAFETY: 生成コードの release glue と同じ手順。
    unsafe {
        (*t).rc -= 1;
        if (*t).rc == 0 {
            wys_task_free(t);
        }
    }
}

fn spawn(entry: Entry, a: i64) -> *mut Header {
    let t = wys_task_new(16, entry, release_task);
    // SAFETY: 作ったばかりのタスク。
    unsafe {
        (wys_task_payload(t) as *mut i64).write(a);
        wys_task_start(t);
    }
    t
}

fn wait(t: *mut Header) {
    // SAFETY: 生存しているタスク。
    unsafe { wys_task_wait(t) }
}

fn drop_task(t: *mut Header) {
    // SAFETY: 呼び出し側の参照を手放す。
    unsafe { release_task(t) }
}

/// 引数の名前を記録して、引数 * 10 を返す。
unsafe extern "C" fn named(t: *mut Header) {
    log(format!("task {}", arg(t)));
    set_result(t, arg(t) * 10);
}

/// 引数のミリ秒だけ眠ってから記録する。
unsafe extern "C" fn sleeper(t: *mut Header) {
    wys_sleep_ms(arg(t));
    log(format!("woke {}", arg(t)));
}

/// 状態が空であることを確かめてから、テストの本体（`main`）を動かす。
fn run(virtual_time: bool, main: extern "C" fn()) -> Vec<String> {
    let _guard = crate::tests::LIVE_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let live = LIVE.load(Ordering::Relaxed);
    let stacks = TASK_STACKS_LIVE.load(Ordering::Relaxed);
    take_log();
    run_with(virtual_time, main);
    assert_eq!(LIVE.load(Ordering::Relaxed), live, "objects leaked");
    assert_eq!(
        TASK_STACKS_LIVE.load(Ordering::Relaxed),
        stacks,
        "task stacks leaked"
    );
    assert!(!running());
    take_log()
}

#[test]
fn spawn_runs_in_fifo_order() {
    extern "C" fn main() {
        let a = spawn(named, 1);
        let b = spawn(named, 2);
        log("main");
        wait(b);
        log("main after b");
        assert_eq!(result(a), 10);
        assert_eq!(result(b), 20);
        drop_task(a);
        drop_task(b);
    }
    assert_eq!(
        run(true, main),
        ["main", "task 1", "task 2", "main after b"]
    );
}

#[test]
fn sleep_orders_by_virtual_deadline() {
    extern "C" fn main() {
        let ts = [spawn(sleeper, 30), spawn(sleeper, 10), spawn(sleeper, 20)];
        for t in ts {
            wait(t);
            drop_task(t);
        }
        log(format!("now {}", sched().now_ms));
    }
    assert_eq!(run(true, main), ["woke 10", "woke 20", "woke 30", "now 30"]);
}

#[test]
fn same_deadline_keeps_spawn_order() {
    extern "C" fn main() {
        let ts: Vec<_> = (0..5).map(|_| spawn(sleeper, 7)).collect();
        // 引数で見分けられないので、番号を書き換えてから眠らせる
        for t in ts {
            wait(t);
            drop_task(t);
        }
    }
    assert_eq!(run(true, main), ["woke 7"; 5]);

    extern "C" fn main2() {
        let a = spawn(order, 1);
        let b = spawn(order, 2);
        let c = spawn(order, 3);
        for t in [a, b, c] {
            wait(t);
            drop_task(t);
        }
    }
    unsafe extern "C" fn order(t: *mut Header) {
        wys_sleep_ms(5);
        log(format!("woke {}", arg(t)));
    }
    assert_eq!(run(true, main2), ["woke 1", "woke 2", "woke 3"]);
}

#[test]
fn await_finished_task_does_not_switch() {
    extern "C" fn main() {
        let t = spawn(named, 4);
        wait(t);
        let before = sched().switches;
        wait(t);
        wait(t);
        assert_eq!(sched().switches, before);
        assert_eq!(result(t), 40);
        drop_task(t);
    }
    assert_eq!(run(true, main), ["task 4"]);
}

#[test]
fn main_return_drains_remaining_tasks() {
    extern "C" fn main() {
        // ハンドルをすぐに捨てても、スケジューラの参照でタスクは最後まで動く
        drop_task(spawn(sleeper, 5));
        drop_task(spawn(named, 1));
        log("main done");
    }
    assert_eq!(run(true, main), ["main done", "task 1", "woke 5"]);
}

#[test]
fn tasks_can_spawn_and_await_tasks() {
    unsafe extern "C" fn parent(t: *mut Header) {
        let child = spawn(named, arg(t) + 1);
        wait(child);
        set_result(t, result(child) + 1);
        drop_task(child);
    }
    extern "C" fn main() {
        let p = spawn(parent, 1);
        wait(p);
        log(format!("result {}", result(p)));
        drop_task(p);
    }
    assert_eq!(run(true, main), ["task 2", "result 21"]);
}

#[test]
fn many_waiters_wake_in_order() {
    unsafe extern "C" fn waiter(t: *mut Header) {
        let target = arg(t) as *mut Header;
        wait(target);
        log("waiter woke");
    }
    extern "C" fn main() {
        let s = spawn(sleeper, 3);
        let w1 = spawn(waiter, s as i64);
        let w2 = spawn(waiter, s as i64);
        wait(w2);
        wait(w1);
        for t in [s, w1, w2] {
            drop_task(t);
        }
    }
    assert_eq!(run(true, main), ["woke 3", "waiter woke", "waiter woke"]);
}

#[test]
fn thousand_tasks_fit() {
    extern "C" fn main() {
        let ts: Vec<_> = (0..1000).map(|i| spawn(named, i)).collect();
        let mut sum = 0;
        for t in ts {
            wait(t);
            sum += result(t);
            drop_task(t);
        }
        log(format!("sum {sum}"));
    }
    let out = run(true, main);
    assert_eq!(
        out.last().unwrap(),
        &format!("sum {}", (0..1000).sum::<i64>() * 10)
    );
}

#[test]
fn task_ids_are_reused() {
    extern "C" fn main() {
        for i in 0..10 {
            let t = spawn(named, i);
            wait(t);
            drop_task(t);
        }
        log(format!("slots {}", sched().tasks.len()));
    }
    let out = run(true, main);
    // タスク 0 と、使い回す 1 つ（解放は次の切り替えの後なので、多くても 2 つ）
    let slots: usize = out.last().unwrap()[6..].parse().unwrap();
    assert!(slots <= 3, "{out:?}");
}

#[test]
fn deep_recursion_fits_in_a_task() {
    fn depth(n: u64) -> u64 {
        let pad = std::hint::black_box([0u8; 64]);
        if n == 0 {
            pad[0] as u64
        } else {
            1 + depth(n - 1)
        }
    }
    unsafe extern "C" fn deep(t: *mut Header) {
        set_result(t, depth(arg(t) as u64) as i64);
    }
    extern "C" fn main() {
        let t = spawn(deep, 2000);
        wait(t);
        assert_eq!(result(t), 2000);
        drop_task(t);
    }
    run(true, main);
}

#[test]
fn on_main_stack_runs_on_the_main_stack() {
    unsafe extern "C" fn probe(t: *mut Header) {
        let main_sp = cb(sched(), 0).sp as usize;
        let here = approx_sp();
        let there = on_main_stack(approx_sp);
        // タスクのスタックは別の領域、サービス呼び出しはメインスタックの中断点より下
        assert!(here.abs_diff(main_sp) > STACK_SIZE);
        assert!(there < main_sp && main_sp - there < 64 << 10);
        // 入れ子でもそのまま
        assert_eq!(on_main_stack(|| on_main_stack(|| 7)), 7);
        set_result(t, on_main_stack(|| arg(t) + 1));
    }
    extern "C" fn main() {
        // タスク 0 の上ではそのまま呼ぶ
        assert_eq!(on_main_stack(|| 3), 3);
        let t = spawn(probe, 41);
        wait(t);
        assert_eq!(result(t), 42);
        drop_task(t);
    }
    run(true, main);
    // スケジューラがないときもそのまま呼ぶ
    assert_eq!(on_main_stack(|| 5), 5);
}

#[test]
fn sleep_waits_real_time_without_virtual_flag() {
    extern "C" fn main() {
        let start = Instant::now();
        let t = spawn(sleeper, 20);
        wait(t);
        drop_task(t);
        assert!(start.elapsed() >= Duration::from_millis(20));
    }
    assert_eq!(run(false, main), ["woke 20"]);
}

/// 実行時エラー（プロセスを終える）のケースは、このテストのバイナリを子プロセスで動かして確かめる。
fn child(name: &str, body: impl FnOnce()) -> Option<std::process::Output> {
    if std::env::var_os("WYS_TASK_CHILD").is_some() {
        body();
        return None;
    }
    let out = Command::new(std::env::current_exe().unwrap())
        .args([name, "--exact", "--test-threads=1", "--nocapture"])
        .env("WYS_TASK_CHILD", "1")
        .output()
        .unwrap();
    Some(out)
}

#[test]
fn deadlock_is_detected() {
    unsafe extern "C" fn waits_on_main_task(t: *mut Header) {
        // 自分を待つ兄弟を待つ（a → b → a）
        let other = arg(t) as *mut Header;
        wait(other);
    }
    extern "C" fn main() {
        let a = wys_task_new(16, waits_on_main_task, release_task);
        let b = wys_task_new(16, waits_on_main_task, release_task);
        // SAFETY: 作ったばかりのタスク。
        unsafe {
            (wys_task_payload(a) as *mut i64).write(b as i64);
            (wys_task_payload(b) as *mut i64).write(a as i64);
            wys_task_start(a);
            wys_task_start(b);
        }
        wait(a);
    }
    if let Some(out) = child("task::tests::deadlock_is_detected", || run_with(true, main)) {
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(101), "{stderr}");
        assert!(
            stderr.contains("runtime error: deadlock: 3 task(s) are waiting but none can run"),
            "{stderr}"
        );
    }
}

#[test]
fn self_await_is_runtime_error() {
    unsafe extern "C" fn awaits_self(t: *mut Header) {
        wait(t);
    }
    extern "C" fn main() {
        let t = spawn(awaits_self, 0);
        wait(t);
    }
    if let Some(out) = child("task::tests::self_await_is_runtime_error", || {
        run_with(true, main)
    }) {
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(101), "{stderr}");
        assert!(
            stderr.contains("runtime error: a task cannot await itself"),
            "{stderr}"
        );
    }
}

#[test]
fn stack_overflow_in_a_task_hits_the_guard() {
    fn forever(n: u64) -> u64 {
        let pad = std::hint::black_box([0u8; 256]);
        if std::hint::black_box(n) == u64::MAX {
            return 0;
        }
        pad[0] as u64 + forever(n + 1) + 1
    }
    unsafe extern "C" fn overflow(t: *mut Header) {
        set_result(t, forever(0) as i64);
    }
    extern "C" fn main() {
        let t = spawn(overflow, 0);
        wait(t);
    }
    if let Some(out) = child(
        "task::tests::stack_overflow_in_a_task_hits_the_guard",
        || run_with(true, main),
    ) {
        // ガードページに当たってシグナルで終わる（静かにメモリを壊さない）
        assert_eq!(out.status.code(), None, "{out:?}");
    }
}

#[test]
fn async_without_scheduler_is_runtime_error() {
    if let Some(out) = child(
        "task::tests::async_without_scheduler_is_runtime_error",
        || {
            wys_sleep_ms(1);
        },
    ) {
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(101), "{stderr}");
        assert!(stderr.contains("outside of an async main"), "{stderr}");
    }
}
