// PoC4（M5 T1）: 生成コードの代わりに、`@tool` 3 つと `@agent` 1 つの相当物を手で書く。
// コンパイラが生成する予定のもの: ツール表（名前・説明・引数の JSON Schema・Effect・承認の要否・トランポリン）、
// トランポリン（JSON の引数を `parse_json` → 型ごとの検証の glue（`validate[T]`）→ 関数 → 戻り値を JSON に）、
// agent の本体を囲む `wys_agent_enter` / `wys_agent_exit`、出力の検証の glue。
// ランタイム（`wys_runtime::ai`）が、LLM のツールの要求を実行の前に検査する（一覧・Capability・予算）。
#include <stdint.h>
#include <stdio.h>
#include <string.h>

typedef struct Obj Obj;
typedef struct List List;
typedef struct Dict Dict;
typedef struct { uint8_t tag; int64_t word; } Unknown;  // %unknown = { i8, [1 x i64] }
enum { U_INT, U_BOOL, U_STR, U_FLOAT, U_NONE, U_LIST, U_DICT };

#define LIT(name, text) \
    static struct { int64_t rc; int64_t len; char b[sizeof(text)]; } name = {-1, sizeof(text) - 1, text}
#define S(x) ((const Obj *)&(x))

void wys_runtime_init(int argc, char **argv);
void wys_runtime_finish(void);
void wys_print_str(const Obj *s);
void wys_print_int(int64_t v);
void wys_release(Obj *s);
Obj *wys_str_concat(const Obj *a, const Obj *b);
void wys_json_parse(const Obj *s, Unknown *out);
_Bool wys_json_failed(void);
int64_t wys_list_len(const List *l);
void *wys_list_slot(List *l, int64_t i, int64_t size);
void wys_list_free(List *l, int64_t size);
int64_t wys_dict_len(const Dict *d);
int64_t wys_dict_find(const Dict *d, int64_t key);
void *wys_dict_val(Dict *d, int64_t e);
int64_t wys_dict_key(const Dict *d, int64_t e);
int64_t wys_dict_iter_next(const Dict *d, int64_t pos, int64_t len0);
void wys_dict_free(Dict *d);
void wys_free_object(Obj *p);

typedef struct {
    const char *name, *description, *schema;
    uint64_t effects;
    int64_t approval;
    Obj *(*entry)(const Obj *args);
} ToolDesc;
typedef struct {
    const char *name;
    const int64_t *tools;
    int64_t n_tools;
    uint64_t caps;
    int64_t max_llm_calls, max_tool_calls, max_tokens, timeout_ms;
} AgentCfg;
enum { CAP_IO = 1, CAP_EXIT = 2, CAP_PYTHON = 4, CAP_LLM = 8 };
void wys_tools_register(const ToolDesc *t, int64_t n);
void wys_agent_enter(const AgentCfg *c);
void wys_agent_exit(void);
void wys_llm_mock_push(const Obj *text, const Obj *tool, const Obj *args, int64_t in, int64_t out, int64_t latency);
Obj *wys_llm_generate(const Obj *prompt, const char *schema);
_Bool wys_llm_failed(void);
Obj *wys_llm_err_kind(void);
Obj *wys_llm_err_message(void);
int64_t wys_audit_count(void);
Obj *wys_audit_entry(int64_t i);
void wys_tool_fail(const Obj *msg);
void wys_ai_finish(void);
Obj *wys_new_str(const char *s);  // テスト用（下で定義）

// ---- 生成コードの解放処理の相当物（`@wys_release_unknown`）
static void rc_release(Obj *p) {
    int64_t *rc = (int64_t *)p;
    if (*rc < 0) return;
    if (--*rc == 0) wys_free_object(p);
}
static void release_unknown(Unknown v) {
    if (v.tag == U_STR) {
        rc_release((Obj *)v.word);
    } else if (v.tag == U_LIST) {
        List *l = (List *)v.word;
        if (--*(int64_t *)l != 0) return;
        for (int64_t i = 0; i < wys_list_len(l); i++) release_unknown(*(Unknown *)wys_list_slot(l, i, 16));
        wys_list_free(l, 16);
    } else if (v.tag == U_DICT) {
        Dict *d = (Dict *)v.word;
        if (--*(int64_t *)d != 0) return;
        int64_t len = wys_dict_len(d);
        for (int64_t e = wys_dict_iter_next(d, 0, len); e >= 0; e = wys_dict_iter_next(d, e + 1, len)) {
            rc_release((Obj *)wys_dict_key(d, e));
            release_unknown(*(Unknown *)wys_dict_val(d, e));
        }
        wys_dict_free(d);
    }
}

// ---- 検証の glue（`validate[T]` が型ごとに生成する予定のもの）。失敗は経路付き
static const char *kind_name(uint8_t t) {
    static const char *n[] = {"int", "bool", "str", "float", "null", "list", "dict"};
    return n[t];
}
static char why[256];
static int field(Unknown obj, const Obj *key, const char *path, uint8_t want, Unknown *out) {
    if (obj.tag != U_DICT) {
        snprintf(why, sizeof why, "%s: expected object, found %s", path[0] ? path : "$", kind_name(obj.tag));
        return 0;
    }
    int64_t e = wys_dict_find((Dict *)obj.word, (int64_t)key);
    if (e < 0) {
        snprintf(why, sizeof why, "%s: missing", path);
        return 0;
    }
    *out = *(Unknown *)wys_dict_val((Dict *)obj.word, e);
    if (out->tag != want) {
        snprintf(why, sizeof why, "%s: expected %s, found %s", path, kind_name(want), kind_name(out->tag));
        return 0;
    }
    return 1;
}

LIT(k_a, "a");
LIT(k_b, "b");
LIT(k_name, "name");
LIT(k_user, "user");
LIT(k_age, "age");
LIT(k_title, "title");
LIT(k_steps, "steps");

// ---- ツール（本言語の関数の相当物）とトランポリン
static int add_runs = 0, greet_runs = 0, forbidden_runs = 0, py_runs = 0;

// @tool def add(a: int, b: int) -> int  （uses pure）
static Obj *tramp_add(const Obj *args) {
    Unknown v, a, b;
    wys_json_parse(args, &v);
    if (wys_json_failed() || !field(v, S(k_a), "a", U_INT, &a) || !field(v, S(k_b), "b", U_INT, &b)) {
        release_unknown(v);
        Obj *m = wys_new_str(why);
        wys_tool_fail(m);
        rc_release(m);
        return NULL;
    }
    add_runs++;
    char buf[64];
    snprintf(buf, sizeof buf, "%lld", (long long)(a.word + b.word));
    release_unknown(v);
    return wys_new_str(buf);
}

// @tool def greet(name: str, user: {age: int}) -> str  （uses io）
static Obj *tramp_greet(const Obj *args) {
    Unknown v, name, user, age;
    wys_json_parse(args, &v);
    if (wys_json_failed() || !field(v, S(k_name), "name", U_STR, &name) ||
        !field(v, S(k_user), "user", U_DICT, &user) || !field(user, S(k_age), "user.age", U_INT, &age)) {
        release_unknown(v);
        Obj *m = wys_new_str(why);
        wys_tool_fail(m);
        rc_release(m);
        return NULL;
    }
    greet_runs++;
    release_unknown(v);
    return wys_new_str("\"hello\"");
}

// 一覧にないツール（実行されてはならない）
static Obj *tramp_forbidden(const Obj *args) {
    (void)args;
    forbidden_runs++;
    return wys_new_str("null");
}

// @tool def run_py() uses python（agent は python を付与していない）
static Obj *tramp_py(const Obj *args) {
    (void)args;
    py_runs++;
    return wys_new_str("null");
}

static const ToolDesc TOOLS[] = {
    {"add", "add two integers", "{\"type\":\"object\",\"properties\":{\"a\":{\"type\":\"integer\"},\"b\":{\"type\":\"integer\"}},\"required\":[\"a\",\"b\"],\"additionalProperties\":false}", 0, 0, tramp_add},
    {"greet", "greet a user", "{\"type\":\"object\",\"properties\":{\"name\":{\"type\":\"string\"},\"user\":{\"type\":\"object\",\"properties\":{\"age\":{\"type\":\"integer\"}},\"required\":[\"age\"]}},\"required\":[\"name\",\"user\"]}", CAP_IO, 0, tramp_greet},
    {"forbidden", "not listed", "{\"type\":\"object\"}", 0, 0, tramp_forbidden},
    {"run_py", "needs python", "{\"type\":\"object\"}", CAP_PYTHON, 0, tramp_py},
};

LIT(prompt, "plan the work");
LIT(empty, "");
LIT(t_add, "add");
LIT(t_greet, "greet");
LIT(t_forbidden, "forbidden");
LIT(t_py, "run_py");
LIT(a_add, "{\"a\": 2, \"b\": 3}");
LIT(a_greet_bad, "{\"name\": \"x\", \"user\": {\"age\": \"old\"}}");
LIT(a_empty, "{}");
LIT(out_ok, "{\"title\": \"plan\", \"steps\": 3}");
LIT(out_bad, "{\"title\": 1, \"steps\": 3}");
LIT(colon, ": ");

static void show_failure(void) {
    Obj *k = wys_llm_err_kind(), *m = wys_llm_err_message();
    Obj *line = wys_str_concat(k, S(colon));
    Obj *line2 = wys_str_concat(line, m);
    wys_print_str(line2);
    rc_release(k);
    rc_release(m);
    rc_release(line);
    rc_release(line2);
}

// 出力の検証の glue（`llm_generate[Plan]` の `Plan = {title: str, steps: int}`）
static const char *PLAN_SCHEMA = "{\"type\":\"object\",\"properties\":{\"title\":{\"type\":\"string\"},\"steps\":{\"type\":\"integer\"}},\"required\":[\"title\",\"steps\"],\"additionalProperties\":false}";
static void generate_plan(void) {
    Obj *r = wys_llm_generate(S(prompt), PLAN_SCHEMA);
    if (!r) {
        show_failure();
        return;
    }
    Unknown v, title, steps;
    wys_json_parse(r, &v);
    rc_release(r);
    if (wys_json_failed() || !field(v, S(k_title), "title", U_STR, &title) ||
        !field(v, S(k_steps), "steps", U_INT, &steps)) {
        Obj *m = wys_new_str(why);
        printf("schema: ");
        fflush(stdout);
        wys_print_str(m);
        rc_release(m);
    } else {
        wys_print_str((Obj *)title.word);
        wys_print_int(steps.word);
    }
    release_unknown(v);
}

static const int64_t AGENT_TOOLS[] = {0, 1, 3};

int main(int argc, char **argv) {
    wys_runtime_init(argc, argv);
    wys_tools_register(TOOLS, 4);
    AgentCfg cfg = {"planner", AGENT_TOOLS, 3, CAP_IO | CAP_LLM, 0, 2, 0, 0};
    wys_agent_enter(&cfg);
    // 1. 一覧にあるツールを呼び、型の合う出力を受け取る
    wys_llm_mock_push(S(empty), S(t_add), S(a_add), 10, 5, 0);
    wys_llm_mock_push(S(out_ok), NULL, NULL, 10, 5, 0);
    generate_plan();
    // 2. 一覧にないツール → 実行の前に拒否
    wys_llm_mock_push(S(empty), S(t_forbidden), S(a_empty), 1, 1, 0);
    generate_plan();
    // 3. Capability の足りないツール（python）→ 拒否
    wys_llm_mock_push(S(empty), S(t_py), S(a_empty), 1, 1, 0);
    generate_plan();
    // 4. 引数が型に合わない → トランポリンが経路付きで拒否
    wys_llm_mock_push(S(empty), S(t_greet), S(a_greet_bad), 1, 1, 0);
    generate_plan();
    // 5. max_tool_calls = 2 を使い切った後のツールの要求 → 予算
    wys_llm_mock_push(S(empty), S(t_add), S(a_add), 1, 1, 0);
    generate_plan();
    // 6. 出力がスキーマに合わない → 経路付き
    wys_llm_mock_push(S(out_bad), NULL, NULL, 1, 1, 0);
    generate_plan();
    wys_agent_exit();
    // 7. タイムアウト（モックの遅延で進める）と、トークンの上限
    AgentCfg slow = {"slow", AGENT_TOOLS, 3, CAP_IO | CAP_LLM, 0, 0, 0, 100};
    wys_agent_enter(&slow);
    // 1 回目の応答（ツールの要求）の遅延で期限を過ぎるので、2 回目の呼び出しの前に止まる（ツールは期限の前に動く）
    wys_llm_mock_push(S(empty), S(t_add), S(a_add), 1, 1, 150);
    generate_plan();
    wys_agent_exit();
    AgentCfg tight = {"tight", AGENT_TOOLS, 3, CAP_IO | CAP_LLM, 0, 0, 20, 0};
    wys_agent_enter(&tight);
    wys_llm_mock_push(S(out_ok), NULL, NULL, 15, 10, 0);
    generate_plan();
    wys_agent_exit();
    printf("runs: add=%d greet=%d forbidden=%d run_py=%d\n", add_runs, greet_runs, forbidden_runs, py_runs);
    fflush(stdout);
    for (int64_t i = 0; i < wys_audit_count(); i++) {
        Obj *e = wys_audit_entry(i);
        wys_print_str(e);
        rc_release(e);
    }
    wys_ai_finish();
    wys_runtime_finish();
    return 0;
}

// 本言語の文字列（参照カウント 1）。生成コードは文字列定数か `wys_str_concat` で作るので、ここでも連結で作る
Obj *wys_new_str(const char *s) {
    struct { int64_t rc; int64_t len; char b[256]; } tmp = {-1, (int64_t)strlen(s), {0}};
    memcpy(tmp.b, s, strlen(s));
    return wys_str_concat((const Obj *)&tmp, S(empty));
}
