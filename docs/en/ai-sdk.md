# AI SDK

> This is a translation of [docs/guide/ai-sdk.md](../guide/ai-sdk.md) as of commit `cdfcc83`. The Japanese document is the source of truth (ADR-0031).

The AI features of `wys` are written with standard functions and decorators (`@tool` / `@agent`), not with dedicated syntax (ADR-0022). LLM output and tool results are not trusted, and capabilities, approvals, and budgets are **enforced by the runtime**, not by instructions in a prompt. The design is described in `docs/design/ai-sdk.md` (Japanese), and the scope of the guarantees in [Security](security.md).

## 1. Typed output, tools, and agents

`llm_generate[T](req)` validates the model's output as the type `T` and returns it, or returns `Err` if it does not fit. A function marked `@tool` becomes a tool the model can call, and its arguments are validated against their types before the body runs. `@agent` declares the tools, capabilities, and budgets an agent may use.

The default provider is a mock inside the runtime. It returns the responses queued with `llm_mock_push`, in order. It does not use the network and is meant for tests.

```wys
# expect-stdout: notes: 25 chars, saved=false
# expect-stdout: 0 llm_called
# expect-stdout: 1 tool_called char_count
# expect-stdout: 2 llm_called
# expect-stdout: 3 approval_requested save_note
# expect-stdout: 4 approval_denied save_note
# expect-stdout: 5 llm_called
type Summary = {title: str, chars: int, saved: bool}

@tool(description="Count the characters in a text.")
def char_count(text: str) -> int uses pure:
    return len(text)

@tool(description="Save a note to disk.", approval="required")
def save_note(title: str) -> None uses io:
    print("saved " + title)

@agent(
    tools=[char_count, save_note],
    capabilities=["io", "llm"],
    max_llm_calls=4,
    max_tool_calls=4,
    timeout_ms=60000
)
async def summarizer(text: str) -> Result[Summary, LlmError] uses io, llm:
    req = LlmRequest(
        model="",
        system="Summarize the text. Use the tools.",
        messages=[Message(role="user", content=text)],
        max_tokens=1024
    )
    return await llm_generate[Summary](req)

async def main() -> None uses io, llm:
    llm_mock_push(
        MockResponse(
            text="",
            tool_calls=[
                MockToolCall(
                    name="char_count",
                    args_json="{\"text\": \"typed output from a model\"}"
                )
            ],
            input_tokens=20,
            output_tokens=5,
            latency_ms=0
        )
    )
    llm_mock_push(
        MockResponse(
            text="",
            tool_calls=[MockToolCall(name="save_note", args_json="{\"title\": \"notes\"}")],
            input_tokens=30,
            output_tokens=5,
            latency_ms=0
        )
    )
    llm_mock_push(
        MockResponse(
            text="{\"title\": \"notes\", \"chars\": 25, \"saved\": false}",
            tool_calls=[],
            input_tokens=40,
            output_tokens=10,
            latency_ms=0
        )
    )
    match await summarizer("typed output from a model"):
        case Ok(s):
            print(s.title + ": " + to_json(s.chars) + " chars, saved=" + to_json(s.saved))
        case Err(e):
            print("error " + e.kind + ": " + e.message)
    for ev in audit_events():
        match ev.tool:
            case str():
                print(to_json(ev.seq) + " " + ev.kind + " " + ev.tool)
            case None:
                print(to_json(ev.seq) + " " + ev.kind)
```

In this example:

- `char_count` is a tool with `uses pure` (no side effects) and simply runs.
- `save_note` has `approval="required"`, so it follows the approval policy `WYS_APPROVAL`. The default is `deny`. The denial is reported to the model, and the model continues generating. With `WYS_APPROVAL=allow` the tool runs, and with `prompt` the runtime asks on the terminal.
- `audit_events()` returns the audit events. `WYS_AUDIT=stderr` prints them as JSON lines.

## 2. Capabilities and budgets

| Mechanism | How to write it | Enforcement |
|---|---|---|
| Effect | `uses io, llm` on a function (required on tools) | Compile time (E1102 and others). Inferred for functions without an annotation (ADR-0024) |
| Capability | `@agent(capabilities=["io", "llm"])`, narrowed at deployment with `WYS_CAPABILITIES` | Run time (`llm` / `python` / `exit`. `io` is checked statically only) |
| Approval | `@tool(approval="required")` and `WYS_APPROVAL` | Run time. Denied by default |
| Budget | `@agent(max_llm_calls=, max_tool_calls=, max_tokens=, timeout_ms=, max_cost_usd=)` | Run time. Exceeding it gives `Err(kind="budget")` / `Err(kind="timeout")` |

## 3. Connecting to a real model

```sh
export ANTHROPIC_API_KEY=...            # read only by the proxy (wys llm-proxy)
WYS_LLM_PROVIDER=anthropic WYS_LLM_MODEL=<model name> wys run program.wys
```

Setting a real provider for a program that uses `llm_mock_push` is a runtime error, so that tests and production are not mixed. Real providers cost money.

## 4. Tracing, record and replay, and evaluation

- `WYS_TRACE=stderr`: spans for agents, LLM calls, and tool calls. Content is not included by default; `WYS_TRACE_CONTENT=1` includes it. `llm_usage()` returns the usage.
- `WYS_LLM_RECORD=r.jsonl` records requests and responses, and `WYS_LLM_PROVIDER=replay:r.jsonl` replays them without the network. The recording contains the content.
- `wys eval <dir>...`: runs evaluation cases (`<dir>/case.wys` and a recording) by replay and reports the success rate.
