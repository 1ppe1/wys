// Python のランタイム（ADR-0018）の C ABI を直接呼ぶ。生成コードと同じく、文字列は本言語のオブジェクト
// `{ 参照カウント, 長さ, バイト列 }`（不死は参照カウント -1）、引数は Python のハンドルの `list` で渡す。
// 失敗し得る操作の後は wys_py_failed で確かめ、理由は wys_py_err_kind / wys_py_err_message で読む。
#include <stdint.h>
#include <stdio.h>

typedef struct Obj Obj;
typedef struct List List;
#define LIT(name, text) \
    static struct { int64_t rc; int64_t len; char b[sizeof(text)]; } name = {-1, sizeof(text) - 1, text}
#define S(x) ((const Obj *)&(x))

Obj *wys_py_import(const Obj *name);
Obj *wys_py_getattr(const Obj *h, const Obj *name);
Obj *wys_py_call(const Obj *h, const List *args);
Obj *wys_py_from_int(int64_t v);
Obj *wys_py_from_bool(_Bool v);
Obj *wys_py_from_str(const Obj *s);
Obj *wys_py_none(void);
int64_t wys_py_to_int(const Obj *h);
_Bool wys_py_to_bool(const Obj *h);
Obj *wys_py_to_str(const Obj *h);
_Bool wys_py_is_none(const Obj *h);
_Bool wys_py_failed(void);
Obj *wys_py_err_kind(void);
Obj *wys_py_err_message(void);
Obj *wys_py_err_traceback(void);
void wys_py_release(Obj *h);
void wys_py_finish(void);
List *wys_list_new(int64_t elem_size, int64_t cap);
void *wys_list_push_slot(List *l, int64_t elem_size);
void wys_list_free(List *l, int64_t elem_size);
void wys_print_int(int64_t v);
void wys_print_bool(_Bool v);
void wys_print_str(const Obj *s);
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
LIT(s_floordiv, "floordiv");
LIT(s_nope, "nope");
LIT(s_missing, "wys_no_such_module");
LIT(s_hi, "hi");
LIT(s_x, "x");
LIT(s_exec, "exec");
LIT(s_dict, "dict");
LIT(s_bad_str, "class E(Exception):\n    def __str__(self):\n        raise RuntimeError('bad')\nraise E()\n");
LIT(s_nul_name, "ma\0th");
LIT(s_raise, "def boom():\n    raise ValueError('deep')\nboom()\n");
LIT(s_eval, "eval");
LIT(s_index_obj, "type('I', (), {'__index__': lambda self: 42})()");
LIT(s_bad_tb, "import traceback\ntraceback.format_exception = lambda *a, **k: ('x',)\nraise ValueError('v')\n");

// C の stdout と、ランタイム（Rust）の stdout は別々にバッファするので、C で書いたら必ず flush する
static void say(const char *line) {
    puts(line);
    fflush(stdout);
}

static void print_and_release(Obj *s) {
    wys_print_str(s);
    wys_release(s);
}

// 直前の操作の失敗を「型名」「メッセージ」の2行で出す。失敗していなければ ok
static void report(void) {
    if (!wys_py_failed()) {
        say("ok");
        return;
    }
    print_and_release(wys_py_err_kind());
    print_and_release(wys_py_err_message());
}

static List *args_of(int n, Obj **items) {
    List *l = wys_list_new(8, n);
    for (int i = 0; i < n; i++) *(Obj **)wys_list_push_slot(l, 8) = items[i];
    return l;
}

static Obj *call(Obj *f, int n, Obj **items) {
    List *l = args_of(n, items);
    Obj *r = wys_py_call(f, l);
    wys_list_free(l, 8);  // 要素のハンドルは借用で渡しただけ
    return r;
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
    Obj *r = call(fact, 1, &ten);
    wys_print_int(wys_py_to_int(r));
    wys_py_release(r);

    // 2. str の往復: json.dumps("hi")、bool と None
    Obj *dumps = wys_py_getattr(json, S(s_dumps));
    Obj *hi = wys_py_from_str(S(s_hi));
    r = call(dumps, 1, &hi);
    print_and_release(wys_py_to_str(r));
    wys_py_release(r);
    Obj *t = wys_py_from_bool(1);
    wys_print_bool(wys_py_to_bool(t));
    Obj *none = wys_py_none();
    wys_print_bool(wys_py_is_none(none));
    wys_print_bool(wys_py_is_none(t));

    // 3〜8. 例外は値として返る（プロセスは落ちない）
    Obj *int_ = wys_py_getattr(builtins, S(s_int));
    Obj *x = wys_py_from_str(S(s_x));
    call(int_, 1, &x);
    report();  // ValueError
    Obj *minus = wys_py_from_int(-1);
    call(fact, 1, &minus);
    report();  // ValueError
    wys_py_import(S(s_missing));
    report();  // ModuleNotFoundError（ImportError）
    wys_py_getattr(math, S(s_nope));
    report();  // AttributeError
    Obj *floordiv = wys_py_getattr(op, S(s_floordiv));
    Obj *pair[2] = {wys_py_from_int(1), wys_py_from_int(0)};
    call(floordiv, 2, pair);
    report();  // ZeroDivisionError
    wys_py_to_int(hi);
    report();  // TypeError（変換の型不一致）
    wys_py_to_bool(ten);
    report();  // TypeError（真偽値への暗黙の変換をしない）
    // 9. 例外の整形そのものが失敗する（`__str__` が例外を出す）。落ちずに値として返り、後の操作も続けられる
    Obj *exec_ = wys_py_getattr(builtins, S(s_exec));
    Obj *dict = wys_py_getattr(builtins, S(s_dict));
    Obj *globals = call(dict, 0, 0);
    Obj *code = wys_py_from_str(S(s_bad_str));
    Obj *eargs[2] = {code, globals};
    call(exec_, 2, eargs);
    report();
    // 10. NUL を含む名前は、プロセスを止めずに失敗として返る
    wys_py_import(S(s_nul_name));
    report();
    wys_py_getattr(math, S(s_nul_name));
    report();
    // 11. 成功すると失敗は消える。トレースバックには、例外を出した関数の名前が入る
    wys_py_to_int(ten);
    report();
    Obj *code2 = wys_py_from_str(S(s_raise));
    Obj *eargs2[2] = {code2, globals};
    call(exec_, 2, eargs2);
    Obj *tb = wys_py_err_traceback();
    const char *bytes = (const char *)tb + 16;
    int64_t len = ((const int64_t *)tb)[1];
    int found = 0;
    for (int64_t i = 0; i + 4 <= len; i++)
        if (bytes[i] == 'b' && bytes[i + 1] == 'o' && bytes[i + 2] == 'o' && bytes[i + 3] == 'm') found = 1;
    say(found ? "traceback mentions boom" : "traceback is missing boom");
    wys_release(tb);
    // 12. `__index__` を持つだけのオブジェクトは int ではない（暗黙に変換しない）
    Obj *eval_ = wys_py_getattr(builtins, S(s_eval));
    Obj *src = wys_py_from_str(S(s_index_obj));
    Obj *iargs[2] = {src, globals};
    Obj *iobj = call(eval_, 2, iargs);
    wys_py_to_int(iobj);
    report();
    // 13. traceback.format_exception が list 以外を返しても、送出中の例外を残さない
    Obj *code3 = wys_py_from_str(S(s_bad_tb));
    Obj *eargs3[2] = {code3, globals};
    call(exec_, 2, eargs3);
    report();
    wys_py_to_int(ten);
    report();

    Obj *handles[] = {math, json, builtins, op, fact, ten, dumps, hi, t, none, int_, x, minus,
                      floordiv, pair[0], pair[1], exec_, dict, globals, code, code2,
                      eval_, src, iobj, code3};
    for (size_t i = 0; i < sizeof handles / sizeof *handles; i++) wys_py_release(handles[i]);
    wys_py_finish();
    wys_runtime_finish();  // 生存しているハンドルや文字列があれば終了コード 102
    return 0;
}
