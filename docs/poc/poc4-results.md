# PoC4: AI 安全性のランタイム（M5 T1）

- **状態:** **成功。** 成功基準 1〜5 をすべて満たした（基準 4 は 2026-11-06 にユーザーの API キーで手元で実行）。転換基準には当たらない
- **期限:** 10 営業日（`docs/plans/m5.md`「PoC4 の判定」、2026-11-19 まで）。着手 2026-11-06
- **コード（試作。PoC3 と同じく結果と一緒にマージし、T8〜T11 で本実装に置き換える。コンパイラは変えていない）:** `crates/wys_runtime/src/ai.rs`（ランタイムの試作）、`crates/wys_runtime/tests/ai_poc.c` / `ai_proxy_poc.c`（生成コードの相当物）と `ai_poc.rs`、`crates/wys_runtime/tests/fake_provider.sh`、`crates/wys_cli/src/llm_proxy.rs`（`wys llm-proxy`）、`scripts/llm-smoke.sh`

## 構成

- **ツール表とトランポリン:** 生成コードが `ToolDesc`（名前・説明・引数の JSON Schema・宣言した Effect のビット・承認の要否・トランポリン）の表を `wys_tools_register` で登録する。トランポリンは引数の JSON（`str`）を `wys_json_parse`（T5）で `Unknown` の木にし、型ごとの検証の glue（`validate[T]` が生成する予定のもの）で型付きの値にしてから関数を呼び、戻り値を JSON の `str` にして返す。検証に失敗したら null を返し、経路付きの理由を `wys_tool_fail` で残す。
- **agent のコンテキスト:** `@agent` の本体を `wys_agent_enter(cfg)` / `wys_agent_exit()` で囲む（lowering がすべての出口に入れる予定）。ランタイムがスタックで持ち、入れ子の agent は Capability を外側との積に縮める。Capability は Effect と同じ名前（`io` / `exit` / `python` / `llm`）のビットで、静的な付与と `WYS_CAPABILITIES`（配備時の縮小。広げられない）の積。
- **`wys_llm_generate(prompt, schema)`:** LLM を呼び、ツールの要求のたびに**実行の前に**、(1) agent の一覧にあるか、(2) ツールの Effect が Capability に含まれるか、(3) 予算（`max_llm_calls` / `max_tool_calls` / `max_tokens` / `timeout_ms`）を検査する。拒否したら監査イベント（`tool_denied` / `budget_exceeded` など。本文は含めない）を残して `Err` を返す。承認が要るツールは `WYS_APPROVAL=allow` でなければ「denied by approver」を LLM に返して続ける。保証はプロンプトではなく、この検査が担う（ロードマップのリスク表「AI SDK の強制方式」）。
- **プロバイダ:** 既定はランタイム内のモック（`wys_llm_mock_push` で応答・ツールの要求・使用量・遅延を積む）。`WYS_LLM_PROVIDER=cmd:<path>` は利用者のコマンド、`anthropic` は `wys llm-proxy`（`WYS_LLM_PROXY` か `PATH` の `wys`）を子プロセスとして起動し、stdin / stdout の JSON 行でやり取りする。前の応答（thinking のブロックを含む）は、手を加えずに次の要求に戻す（Claude Opus 5.5 の thinking のブロックは会話に結び付いているので、編集すると拒否され得る）。
- **`wys llm-proxy`:** `wys_cli` の副コマンド。HTTPS（`ureq` 2 + `rustls`）と API キー（`ANTHROPIC_API_KEY`）はこのプロセスだけが持ち、生成した実行ファイルとランタイムは秘密情報に触れず、依存も変わらない（AC-004）。要求は Messages API（`anthropic-version: 2026-10-21`、既定のモデル `claude-opus-5-5`）。ツールは `tool_choice: auto` と `disable_parallel_tool_use`（ランタイムが 1 回に 1 つずつ検査する）、出力のスキーマは `output_config.format`（`json_schema`）、安全分類器による拒否は `fallbacks: "default"`（ヘッダ `server-side-fallback-2026-07-01`）で推奨のモデルに再実行させ、それでも `refusal` なら `Err`。

## 結果（macOS arm64）

機種: Apple M5 Pro、macOS 26.4.1、Apple clang 21.0.0、rustc 1.99.1。

| 基準 | 結果 |
|---|---|
| 1. ツール 2 つと agent 1 つの相当物がモックに対して動く。一覧にないツールは実行の前に拒否して監査イベントが残る。`max_tool_calls` の後の要求は `Err(budget)` | 成功（`ai_poc.c`）。`add`（`int` 2 つ）を呼んで結果を返す。一覧にない `forbidden` と、`python` を要する `run_py` は本体が一度も動かず（`runs: ... forbidden=0 run_py=0`）、`tool_denied` が残る。`max_tool_calls=2` の後は `budget: max_tool_calls exceeded` と `budget_exceeded`。ほかに `timeout_ms`（モックの遅延で進める仮想の時間。期限を過ぎて届いた応答は受け取らず、要求されたツールも呼ばない）、`max_tokens`、入れ子の agent（外側の残りの予算を超えて使えない。使った分は外側にも数える） |
| 2. レコードに合わない JSON は経路付きの `Err(schema)`、合えば型付きの値を `print` できる。失敗しても生存数 0、3 つのモード | 成功。ツールの引数の不一致は `schema: tool \`greet\`: user.age: expected int, found str`（入れ子のレコード）、出力の不一致は `schema: title: expected str, found int`、合う出力は `plan` / `3` を表示。Debug・`-O2`・AddressSanitizer で、終了時の生存数 0（終了コード 0） |
| 3. `Unknown` に `list` / `dict` を足し、深さ 3 の JSON を `match` で読み、3 つのモードで生存数 0 | 成功（T5、PR #102 でマージ済み）。`lang/unknown_json_tree`（深さ 3）・`unknown_json_drop`（関数の戻り値・レコード・`Task` を経由）を Debug・Release・ASan で、macOS arm64 と Linux x86_64 の両方 |
| 4. `wys llm-proxy` が Anthropic Messages API とスキーマ付きの要求・ツール 1 つで `tool_use` → `tool_result` → 最終の JSON を 1 往復する | 成功（`scripts/llm-smoke.sh`、`claude-opus-5-5`）。監査イベントは `llm_called`（696 トークン）→ `tool_called: add` → `llm_called`（742 トークン）の順で、最終の出力はスキーマどおりの `{"steps":5,"title":"Five-step plan: the add tool returned 5 for 2 + 3"}`（ツールの結果 5 を使っている）。プロトコルは、固定の応答を返すプロバイダ（`fake_provider.sh`）で通常のビルドと ASan の両方で確かめた: ツールの一覧とスキーマが要求に入る、ツールの呼び出しの ID が結果に付く、前の応答（thinking のブロックを含む）がそのまま次の要求に戻る。要求と応答の変換は `llm_proxy.rs` の単体テスト |
| 5. 記録 | 下の表 |

記録（絶対値。倍率は書かない）:

| 項目 | 値 |
|---|---|
| 5 フィールドのレコード 1 つの検証の glue の行数 | C で 30 行ほど（共通の補助関数 `field` が 20 行、フィールドごとに 2 行。`ai_poc.c`）。生成する LLVM IR では型ごとの関数になるので、行数はこれより多い |
| 1 MB の JSON の `parse_json`（12,316 要素の配列、各要素が 5 フィールドの object） | 13.6 ms（`libwys_runtime.a` の Release と `-O2`、5 回の最速値）。`validate` の時間は T6 で glue ができてから測る |
| proxy の往復の時間 | 6,328 ms（LLM の呼び出し 2 回とツール 1 回、proxy の起動を含む。1 回だけの計測で、ネットワークとモデルの応答時間がほとんどを占める） |
| ASan モードで子プロセス（プロバイダ）を起動したときの挙動 | 問題なし（`ai_proxy_poc.c` を `-fsanitize=address` でリンクして、`fake_provider.sh` を起動。子プロセスは計装しない） |

## 分かったこと（ADR-0022 / 0023 と `docs/design/ai-sdk.md` に反映する）

- 検査の順は「一覧 → Capability → 予算 → 承認」でよい。一覧にないツールと Capability の不足は `llm_generate` を中止し（モデルの逸脱）、承認の拒否は LLM に返して続ける（正常な運用）。
- タイムアウトは呼び出しの境界で判定すれば足りる。境界は呼び出しの前と、応答を受け取った直後の両方（期限を過ぎて届いた応答は受け取らず、ツールを呼ばない。Codex のレビューで直した）。モックの遅延で仮想の時間を進めると、Conformance で決定的に検査できる。
- 入れ子の agent の予算は、検査をスタックのすべての agent に対して行い、使った分をすべての agent に数えれば、外側の残りを超えない（Codex のレビューで直した）。
- ツールの中から `llm_generate` を呼べるように、ランタイムはツールを呼ぶ前に内部の状態の借りを返す必要がある（入れ子の agent）。
- 実プロバイダの要求は、前の応答を**そのまま**次の要求に戻す形にする（Claude Opus 5.5 は thinking のブロックが会話に結び付いている）。プロトコルの `assistant` は不透明な値として運ぶ。
- 強制のツール呼び出し（`tool_choice: any`）は Claude Opus 5.5 で使えないので、`auto` とランタイムの検査で十分。
- `ureq` 2 と `rustls` は `wys_cli` だけの依存にできた。生成した実行ファイルとランタイムの staticlib は TLS に依存しない。

## 基準 4 の手順（ユーザー）

```sh
export ANTHROPIC_API_KEY=...   # 自分の環境で。キーはリポジトリに置かない
scripts/llm-smoke.sh           # 1 往復（費用がかかる。モデルは WYS_LLM_MODEL で変えられる。既定は claude-opus-5-5）
```

標準エラーに監査イベント（`WYS_AUDIT=stderr`）、標準出力に最終の JSON、最後に往復の時間が出る。

（追記 2026-11-06）試作の C のプログラムとそのテスト（`ai_poc.c` / `ai_proxy_poc.c` / `ai_poc.rs`）は、M5 T8 で ABI を ADR-0023 の形に書き直したときに削除し、検査は `tests/conformance/ai/` に移した。

（追記 2026-11-06）M5 T11 で、実プロバイダをランタイムから起動する形（`WYS_LLM_PROVIDER=anthropic`）につないだ。`scripts/llm-smoke.sh` は本言語のプログラムを使う形に書き直した（手順は上と同じ）。新しい形での実 API との往復の結果は、ユーザーが手元で実行した後に追記する。
