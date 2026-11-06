// PoC4（M5 T1）: 別プロセスのプロバイダ（`WYS_LLM_PROVIDER=cmd:<path>`、JSON 行）との往復。
// LLM がツール `add` を要求し、ランタイムが検査して実行し、結果を返した後の最終の JSON を受け取る。
// 本物の Anthropic API には `wys llm-proxy` を指す（`scripts/llm-smoke.sh`）。テストでは固定の応答を返すスクリプト
#include <stdint.h>
#include <stdio.h>
#include <string.h>

typedef struct Obj Obj;
#define LIT(name, text) \
    static struct { int64_t rc; int64_t len; char b[sizeof(text)]; } name = {-1, sizeof(text) - 1, text}
#define S(x) ((const Obj *)&(x))

void wys_runtime_init(int argc, char **argv);
void wys_runtime_finish(void);
void wys_print_str(const Obj *s);
Obj *wys_str_concat(const Obj *a, const Obj *b);
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
void wys_tools_register(const ToolDesc *t, int64_t n);
void wys_agent_enter(const AgentCfg *c);
void wys_agent_exit(void);
Obj *wys_llm_generate(const Obj *prompt, const char *schema);
Obj *wys_llm_err_message(void);
void wys_ai_finish(void);

LIT(empty, "");
LIT(five, "5");
LIT(prompt, "Use the add tool to add 2 and 3, then answer with a plan whose steps is the sum.");

static void rc_release(Obj *p) {
    int64_t *rc = (int64_t *)p;
    if (*rc >= 0 && --*rc == 0) wys_free_object(p);
}

// 本物の glue は引数を検証する。ここでは結果を固定して、往復のプロトコルだけを見る
static Obj *tramp_add(const Obj *args) {
    (void)args;
    return wys_str_concat(S(five), S(empty));
}

static const ToolDesc TOOLS[] = {
    {"add", "Add two integers and return the sum.",
     "{\"type\":\"object\",\"properties\":{\"a\":{\"type\":\"integer\"},\"b\":{\"type\":\"integer\"}},\"required\":[\"a\",\"b\"],\"additionalProperties\":false}",
     0, 0, tramp_add},
};
static const int64_t AGENT_TOOLS[] = {0};

int main(int argc, char **argv) {
    wys_runtime_init(argc, argv);
    wys_tools_register(TOOLS, 1);
    AgentCfg cfg = {"planner", AGENT_TOOLS, 1, 8, 4, 2, 0, 0};
    wys_agent_enter(&cfg);
    Obj *r = wys_llm_generate(S(prompt),
        "{\"type\":\"object\",\"properties\":{\"title\":{\"type\":\"string\"},\"steps\":{\"type\":\"integer\"}},\"required\":[\"title\",\"steps\"],\"additionalProperties\":false}");
    if (r) {
        wys_print_str(r);
        rc_release(r);
    } else {
        Obj *m = wys_llm_err_message();
        wys_print_str(m);
        rc_release(m);
    }
    wys_agent_exit();
    wys_ai_finish();
    wys_runtime_finish();
    return 0;
}
