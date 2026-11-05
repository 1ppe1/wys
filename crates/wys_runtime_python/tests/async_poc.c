// タスク（ADR-0020）の中から Python を呼ぶ（論点 16、M4 T10a）。CPython 3.14 のスタックの保護はメインスレッドの
// スタックの範囲で働くので、タスクのスタックの上でそのまま呼ぶと深い再帰でガードページに当たって落ちる。
// ランタイムは Python の呼び出しをメインスタックの上で実行する（サービス呼び出し）ので、メインスタックと同じく
// 深さ 5000 は通り、深さ 100000 は RecursionError（値としての失敗）になる。
#include <stdint.h>
#include <stdio.h>
#if defined(__has_feature)
#if __has_feature(address_sanitizer)
#include <sanitizer/asan_interface.h>
#include <sanitizer/common_interface_defs.h>
#define WYS_ASAN 1
#endif
#endif

typedef struct Obj Obj;
typedef struct List List;
#define LIT(name, text) \
    static struct { int64_t rc; int64_t len; char b[sizeof(text)]; } name = {-1, sizeof(text) - 1, text}
#define S(x) ((const Obj *)&(x))

Obj *wys_py_import(const Obj *name);
Obj *wys_py_getattr(const Obj *h, const Obj *name);
Obj *wys_py_call(const Obj *h, const List *args);
Obj *wys_py_from_str(const Obj *s);
Obj *wys_py_new_dict(void);
int64_t wys_py_to_int(const Obj *h);
_Bool wys_py_failed(void);
Obj *wys_py_err_kind(void);
void wys_py_release(Obj *h);
List *wys_list_new(int64_t elem_size, int64_t cap);
void *wys_list_push_slot(List *l, int64_t elem_size);
void wys_list_free(List *l, int64_t elem_size);
void wys_print_int(int64_t v);
void wys_print_str(const Obj *s);
void wys_release(Obj *s);
void wys_runtime_init(int argc, char **argv);
void wys_runtime_finish(void);
void wys_sched_run(void (*entry)(void));
Obj *wys_task_new(int64_t payload_len, void (*entry)(Obj *), void (*release)(Obj *));
void wys_task_start(Obj *t);
void wys_task_wait(Obj *t);
void wys_task_free(Obj *t);
void wys_sleep_ms(int64_t n);
void wys_set_fiber_hooks(void *start, void *finish, void *unpoison);

LIT(s_builtins, "builtins");
LIT(s_eval, "eval");
LIT(s_shallow, "len(__import__('json').dumps(__import__('functools').reduce(lambda x, _: [x], range(5000), [])))");
LIT(s_deep, "len(__import__('json').dumps(__import__('functools').reduce(lambda x, _: [x], range(100000), [])))");

static Obj *eval_(const Obj *code) {
    Obj *b = wys_py_import(S(s_builtins));
    Obj *ev = wys_py_getattr(b, S(s_eval));
    Obj *src = wys_py_from_str(code);
    Obj *globals = wys_py_new_dict();  // フレームの外の eval には globals が要る
    List *l = wys_list_new(8, 2);
    *(Obj **)wys_list_push_slot(l, 8) = src;
    *(Obj **)wys_list_push_slot(l, 8) = globals;
    Obj *r = wys_py_call(ev, l);
    wys_list_free(l, 8);
    wys_py_release(globals);
    wys_py_release(src);
    wys_py_release(ev);
    wys_py_release(b);
    return r;
}

static void eval_and_print(const Obj *code) {
    Obj *r = eval_(code);
    if (wys_py_failed()) {
        Obj *k = wys_py_err_kind();
        wys_print_str(k);
        wys_release(k);
        return;
    }
    wys_print_int(wys_py_to_int(r));
    wys_py_release(r);
}

// 生成コードの、型ごとの release の代わり（結果を持たない）
static void release_task(Obj *t) {
    if (--*(int64_t *)t == 0) wys_task_free(t);
}

static void body(Obj *t) {
    (void)t;
    wys_sleep_ms(1);  // 一度中断してから（タスク 0 は待ちの中）
    eval_and_print(S(s_shallow));
    eval_and_print(S(s_deep));
}

static void async_main(void) {
    Obj *t = wys_task_new(0, body, release_task);
    wys_task_start(t);
    wys_task_wait(t);
    release_task(t);
}

int main(int argc, char **argv) {
    wys_runtime_init(argc, argv);
#ifdef WYS_ASAN
    // 生成コードの main と同じく、ASan のときだけ fiber の API を登録する（ADR-0020 論点 18）
    wys_set_fiber_hooks((void *)__sanitizer_start_switch_fiber, (void *)__sanitizer_finish_switch_fiber,
                        (void *)__asan_unpoison_memory_region);
#endif
    wys_sched_run(async_main);
    wys_runtime_finish();
    return 0;
}
