# AI SDK

`wys` の AI の機能は、専用の構文ではなく標準の関数とデコレータ（`@tool` / `@agent`）で書く（ADR-0022）。LLM の出力とツールの戻り値は信頼せず、権限・承認・予算は**ランタイムが強制する**（プロンプトでの指示に頼らない）。設計の詳細は `docs/design/ai-sdk.md`、保証範囲は [セキュリティ](../security.md)。

## 1. 型付きの出力・ツール・agent

`llm_generate[T](req)` は、モデルの出力を型 `T` として検証して返す（合わなければ `Err`）。`@tool` の関数はモデルが呼べるツールになり、引数は型どおりに検証されてから本体に渡る。`@agent` は、使えるツール・Capability・予算を宣言する。

既定のプロバイダはランタイムの中のモックで、`llm_mock_push` で積んだ応答を順に返す（ネットワークに出ない。テストに使う）。

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

この例では:

- `char_count` は `uses pure`（副作用なし）のツールで、そのまま実行される。
- `save_note` は `approval="required"` なので、承認の方針 `WYS_APPROVAL` に従う。既定は `deny`（拒否）で、拒否はモデルに伝えられ、モデルは続きを生成する。`WYS_APPROVAL=allow` なら実行し、`prompt` なら端末で尋ねる。
- `audit_events()` で監査イベントを読める。`WYS_AUDIT=stderr` で JSON 行として出せる。

## 2. 権限と予算

| 仕組み | 書き方 | 強制 |
|---|---|---|
| Effect | 関数の `uses io, llm`（ツールは必須） | コンパイル時（E1102 など）。注釈のない関数は推論する（ADR-0024） |
| Capability | `@agent(capabilities=["io", "llm"])`、配備時に `WYS_CAPABILITIES` で縮める | 実行時（`llm` / `python` / `exit`。`io` は静的な検査だけ） |
| 承認 | `@tool(approval="required")` と `WYS_APPROVAL` | 実行時。既定は拒否 |
| 予算 | `@agent(max_llm_calls=, max_tool_calls=, max_tokens=, timeout_ms=, max_cost_usd=)` | 実行時。超えたら `Err(kind="budget")` / `Err(kind="timeout")` |

## 3. 実際のモデルにつなぐ

```sh
export ANTHROPIC_API_KEY=...            # proxy（wys llm-proxy）だけが読む
WYS_LLM_PROVIDER=anthropic WYS_LLM_MODEL=<モデル名> wys run program.wys
```

`llm_mock_push` を使うプログラムに実プロバイダを設定すると実行時エラーになる（テストと本番を混ぜない）。費用がかかる。

## 4. トレース・記録と再生・評価

- `WYS_TRACE=stderr`: agent・LLM の呼び出し・ツールの呼び出しのスパン（本文は既定で含めない。`WYS_TRACE_CONTENT=1` で含める）。`llm_usage()` で使用量を読める。
- `WYS_LLM_RECORD=r.jsonl` で要求と応答を記録し、`WYS_LLM_PROVIDER=replay:r.jsonl` でネットワークなしに再生する（記録には本文が入る）。
- `wys eval <dir>...`: 評価ケース（`<dir>/case.wys` と記録）を再生で実行し、成功率を出す。
