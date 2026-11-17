# agent の例

実際のモデルにつないで動かせる例。どれも `main` を持つふつうのプログラムで、`llm_mock_push` を使わない。

| 例 | 見せること |
|---|---|
| [extract](extract/main.wys) | 型付きの抽出。モデルの出力を `Invoice` として検証する（`llm_generate[T]`） |
| [budget](budget/main.wys) | 予算。ツールを呼び続けるモデルを、回数の上限でランタイムが止める |

実際のモデルで動かす（費用がかかる。`ANTHROPIC_API_KEY` は `wys llm-proxy` だけが読む）:

```sh
export ANTHROPIC_API_KEY=...
WYS_LLM_PROVIDER=anthropic WYS_LLM_MODEL=<モデル名> wys run examples/agents/extract/main.wys
```

ネットワークなしで動かす（CI と同じ）:

```sh
WYS_LLM_PROVIDER=replay:examples/agents/extract/recording.jsonl wys run examples/agents/extract/main.wys
```

`recording.jsonl` は、テスト用のプロバイダ（`crates/wys_cli/tests/fake_provider.sh`）が返す決まった応答を記録したもので、**実際のモデルの出力ではない**。実際のモデルでは、出力も呼び出しの回数も変わり得る（たとえば budget の例で、モデルが早く答えれば予算に届かない）。プログラムを変えて記録と要求が合わなくなったら、`WYS_LLM_RECORD` を付けて実行し直す。

---

Examples that run against a real model. Each is an ordinary program with `main` and does not use `llm_mock_push`. Run them with `WYS_LLM_PROVIDER=anthropic WYS_LLM_MODEL=<model>` (this costs money), or offline with `WYS_LLM_PROVIDER=replay:<dir>/recording.jsonl`. The recordings are canned responses from a test provider, **not real model output**; a real model may answer differently.
