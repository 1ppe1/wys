// PoC3（M4 T1）: 生成コードの代わりに、ランタイムの C ABI を直接呼ぶ。生成コードと同じく、文字列は
// 本言語のオブジェクト `{ 参照カウント, 長さ, バイト列 }`（不死は参照カウント -1）で渡す。
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <time.h>

typedef struct Obj Obj;
#define LIT(name, text) \
    static struct { int64_t rc; int64_t len; char b[sizeof(text)]; } name = {-1, sizeof(text) - 1, text}
#define S(x) ((const Obj *)&(x))

Obj *wys_py_import(const Obj *name);
Obj *wys_py_getattr(const Obj *h, const Obj *name);
Obj *wys_py_call(const Obj *h, const Obj *const *args, int64_t n);
Obj *wys_py_from_int(int64_t v);
Obj *wys_py_from_str(const Obj *s);
_Bool wys_py_to_int(const Obj *h, int64_t *out);
Obj *wys_py_to_str(const Obj *h);
Obj *wys_py_take_error(void);
void wys_py_release(Obj *h);
void wys_py_finish(void);
int64_t wys_py_init_micros(void);
void wys_print_int(int64_t v);
void wys_print_str(const Obj *s);
void wys_rc_release(Obj *s);
void wys_release(Obj *s);
void wys_runtime_init(int argc, char **argv);
void wys_runtime_finish(void);

LIT(s_math, "math");
LIT(s_json, "json");
LIT(s_builtins, "builtins");
LIT(s_operator, "operator");
LIT(s_factorial, "factorial");
LIT(s_dumps, "dumps");
LIT(s_int, "int");
LIT(s_gcd, "gcd");
LIT(s_floordiv, "floordiv");
LIT(s_nope, "nope");
LIT(s_missing, "wys_no_such_module");
LIT(s_hi, "hi");
LIT(s_x, "x");
LIT(s_exec, "exec");
LIT(s_dict, "dict");
LIT(s_bad_str, "class E(Exception):\n    def __str__(self):\n        raise RuntimeError('bad')\nraise E()\n");
LIT(s_nul_name, "ma\0th");

// 失敗を期待する操作: 理由を1行出す
static void report_error(void) {
    Obj *e = wys_py_take_error();
    wys_print_str(e);
    wys_release(e);
}

static Obj *call1(Obj *f, Obj *arg) {
    const Obj *args[1] = {arg};
    return wys_py_call(f, args, 1);
}

static double now(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec + t.tv_nsec / 1e9;
}

int main(int argc, char **argv) {
    wys_runtime_init(argc, argv);
    Obj *math = wys_py_import(S(s_math));
    Obj *json = wys_py_import(S(s_json));
    Obj *builtins = wys_py_import(S(s_builtins));
    Obj *op = wys_py_import(S(s_operator));

    // 1. int の往復: math.factorial(10)
    Obj *fact = wys_py_getattr(math, S(s_factorial));
    Obj *ten = wys_py_from_int(10);
    Obj *r = call1(fact, ten);
    int64_t v = 0;
    if (wys_py_to_int(r, &v)) wys_print_int(v);
    wys_py_release(r);

    // 2. str の往復: json.dumps("hi")
    Obj *dumps = wys_py_getattr(json, S(s_dumps));
    Obj *hi = wys_py_from_str(S(s_hi));
    r = call1(dumps, hi);
    Obj *text = wys_py_to_str(r);
    wys_print_str(text);
    wys_release(text);
    wys_py_release(r);

    // 3〜8. 例外は値として返る（プロセスは落ちない）
    Obj *int_ = wys_py_getattr(builtins, S(s_int));
    Obj *x = wys_py_from_str(S(s_x));
    if (!call1(int_, x)) report_error();                   // ValueError
    Obj *minus = wys_py_from_int(-1);
    if (!call1(fact, minus)) report_error();               // ValueError
    if (!wys_py_import(S(s_missing))) report_error();      // ModuleNotFoundError（ImportError）
    if (!wys_py_getattr(math, S(s_nope))) report_error();  // AttributeError
    Obj *floordiv = wys_py_getattr(op, S(s_floordiv));
    Obj *one = wys_py_from_int(1);
    Obj *zero = wys_py_from_int(0);
    const Obj *args2[2] = {one, zero};
    if (!wys_py_call(floordiv, args2, 2)) report_error();  // ZeroDivisionError
    if (!wys_py_to_int(hi, &v)) report_error();            // TypeError（変換の型不一致）
    // 9. 例外の整形そのものが失敗する（`__str__` が例外を出す）。落ちずに値として返り、後の操作も続けられる
    Obj *exec_ = wys_py_getattr(builtins, S(s_exec));
    Obj *dict = wys_py_getattr(builtins, S(s_dict));
    Obj *globals = wys_py_call(dict, 0, 0);
    Obj *code = wys_py_from_str(S(s_bad_str));
    const Obj *eargs[2] = {code, globals};
    if (!wys_py_call(exec_, eargs, 2)) report_error();
    // 10. NUL を含む名前は、プロセスを止めずに失敗として返る
    if (!wys_py_import(S(s_nul_name))) report_error();
    if (!wys_py_getattr(math, S(s_nul_name))) report_error();

    // 計測（絶対値だけ記録する）: math.gcd(12, 18) の往復
    Obj *gcd = wys_py_getattr(math, S(s_gcd));
    Obj *a = wys_py_from_int(12), *b = wys_py_from_int(18);
    const Obj *gargs[2] = {a, b};
    const int N = 200000;
    double t0 = now();
    for (int i = 0; i < N; i++) {
        Obj *g = wys_py_call(gcd, gargs, 2);
        wys_py_to_int(g, &v);
        wys_py_release(g);
    }
    double t1 = now();
    fprintf(stderr, "poc3: init_us=%lld roundtrip_ns=%.0f gcd=%lld\n", (long long)wys_py_init_micros(),
            (t1 - t0) / N * 1e9, (long long)v);

    Obj *handles[] = {math, json, builtins, op, fact, ten, dumps, hi, int_, x, minus,
                      floordiv, one, zero, gcd, a, b, exec_, dict, globals, code};
    for (size_t i = 0; i < sizeof handles / sizeof *handles; i++) wys_py_release(handles[i]);
    wys_py_finish();
    wys_runtime_finish();  // 生存しているハンドルや文字列があれば終了コード 102
    return 0;
}
