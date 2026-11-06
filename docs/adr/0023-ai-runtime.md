# ADR-0023: AI Runtime（プロバイダ抽象 `wys llm-proxy`、Capability・承認・予算の強制、監査とプライバシー）

- **状態:** Accepted
- **日付:** 2026-11-06
- **関連:** 未決定事項 10、AI-004〜AI-007、AC-007〜AC-009、NFR-005、NFR-007、AC-004、ADR-0008、ADR-0014（論点 11）、ADR-0018（論点 2・12）、ADR-0020（論点 10・15・16）、ADR-0021、ADR-0022、`docs/design/ai-sdk.md`、`docs/plans/m5.md`（T9〜T11）、`docs/poc/poc4-results.md`、issue #62、#65

## 背景

未決定事項 10 は「モデルプロバイダ抽象化、費用上限の算定方式、秘密情報保管、トレースのプライバシー方針」。要件定義書のセキュリティ原則は「LLM 出力およびツール戻り値は信頼しない。プロンプト内の指示だけで権限を制御しない。外部リソースへの権限を保証するには Runtime 側の強制が必要」、NFR-005 は「最小権限、秘密情報のログ抑制、外部データの非信頼扱い、監査可能な実行記録」。ロードマップのリスク表は「Capability の付与がプロンプト依存になる → Runtime 強制だけで AC-007 を満たす」。

前提: 生成した実行ファイルの依存は OS 標準のみ（AC-004、ADR-0008）。Effect は Runtime の表現を持たない（ADR-0014 論点 11）。`python` はサンドボックスではない（ADR-0018 論点 12）。タスクは単一スレッドの協調的なコルーチンで、キャンセルは予約のまま（ADR-0020 論点 15）、Python の呼び出し中はブロックする（論点 16）。

PoC4（2026-11-06 成功）で分かったこと: 検査の順は「一覧 → Capability → 予算 → 承認」でよい。期限は呼び出しの前だけでなく応答を受け取った直後にも見る必要がある。入れ子の agent の予算はスタック上のすべての agent で検査し、すべてに数える。ランタイムはツールを呼ぶ前に内部の借りを返す必要がある（ツールの中から `llm_generate` を呼べる）。前の応答は編集せずそのまま戻す（thinking のブロック）。`tool_choice: any` は Claude Opus 5.5 で使えず `auto` で足りる。`ureq` + `rustls` は `wys_cli` だけの依存にできた。

## 選択肢と決定

| 論点 | 決定（推奨） | 主な代替案 | 理由 |
|---|---|---|---|
| 1. プロバイダ抽象 | **別プロセスとの JSON 行のプロトコル。** プロバイダは 3 種: (a) ランタイム内のモック（`WYS_LLM_PROVIDER` 未設定。既定）、(b) `anthropic` = `wys llm-proxy`（`wys_cli` の副コマンド）、(c) `cmd:<path>` = 利用者のコマンド（同じプロトコル）。(b) と (c) はランタイムが `std::process::Command` で起動し、stdin / stdout で 1 行ずつやり取りする。記録済み応答の再生は M6（AI-008 / AC-009） | (a) Python の埋め込み（`urllib`）で HTTP。(b) ランタイムに HTTP クライアント（`ureq` / `libcurl`）を入れる。(c) プロバイダの trait をランタイムに持ち、adapter を静的リンクする | (a) は LLM を使うすべてのプログラムが Python を要求し、呼び出し中はタスクが止まる。(b) は TLS のために外部クレートか `libcurl` が staticlib に入り、Linux で AC-004 が崩れる。(c) も同じ。別プロセスなら HTTPS と秘密情報を proxy に閉じ込められ、`cmd:` で利用者が任意のプロバイダ（ほかの API、ローカルのモデル、再生）を書ける。これがプロバイダ抽象の実体 |
| 2. プロトコル | 要求（1 行）: `{"v":1, "model", "system", "messages", "tools":[{name, description, input_schema}], "schema": object \| null, "max_tokens", "timeout_ms"}`。`messages` の要素は `{"role":"user","content":str}` / `{"role":"assistant","content": str \| 不透明な値}` / `{"role":"tool","id","content","is_error"}`。応答（1 行）: `{"text", "tool_calls":[{id, name, args}], "usage":{input_tokens, output_tokens}, "stop", "assistant": 不透明な値}` か `{"error": str}`。ランタイムは `assistant` を解釈せず、次の要求の `assistant` の `content` にそのまま戻す。`0` の `max_tokens` / `timeout_ms` は「指定なし」。`tool_calls` が複数なら順に 1 つずつ処理し、結果をまとめて次の要求に入れる | (a) Messages API の形をそのまま流す。(b) gRPC / HTTP のローカルサーバ。(c) `assistant` をテキストとツール呼び出しに正規化して戻す | (a) はプロバイダごとに違う形をランタイムが知ることになる。(b) は依存が増える。(c) は PoC4 で否定された（Claude Opus 5.5 の thinking のブロックは会話に結び付いていて、編集すると拒否され得る）。不透明に運べば adapter だけが形を知る。`v` は `cmd:` の作者がプロトコルの変更に備えるため |
| 3. 要求の組み立て | `model` は `LlmRequest.model` → `@agent(model=)` → `WYS_LLM_MODEL` → proxy の既定（`claude-opus-5-5`）の順。`tools` は現在の agent の一覧にあるツールの定義（根のコンテキストでは空）。`schema` は `T != str` のときだけ。`timeout_ms` はスタック上の agent の残り時間の最小（0 = なし）で、proxy はこれと自分のソケットのタイムアウトの小さいほうを使う | `timeout_ms` を送らない | 期限を proxy に伝えないと、HTTP の待ちが agent の期限を大きく超える（M5 はキャンセルがない。論点 14） |
| 4. `wys` の探索と子プロセス | `anthropic` のときの `wys` の探索順: `WYS_LLM_PROXY` → ビルド時に生成コードへ埋めたコンパイラの絶対パス（`current_exe`）→ `PATH` の `wys`。`cmd:<path>` は `<path>` を引数なしで起動する（シェルを通さない）。子プロセスは最初の `llm_generate` で起動してプロセスに 1 つ、stderr は継承、終了時（`wys_runtime_finish` の前）に stdin を閉じて `wait` する。起動できない・応答が壊れている・子が死んだときは `Err(kind="provider")` | (a) `PATH` だけ。(b) 呼び出しごとに起動する | 埋めたパスがあれば、生成した実行ファイルをビルドした場所から動かすときに設定が要らない。1 プロセスなら接続の再利用（`ureq` の `Agent`）が効く |
| 5. Anthropic の adapter | Messages API、`anthropic-version: 2026-10-21`。ツールは `tool_choice: {type:"auto", disable_parallel_tool_use:true}`（ランタイムが 1 回に 1 つ検査する。`any` は Claude Opus 5.5 で 400）。出力スキーマは `output_config.format = {type:"json_schema", schema}`。安全分類器の拒否は `fallbacks: "default"`（ヘッダ `server-side-fallback-2026-07-01`）で推奨のモデルに再実行し、それでも `refusal` なら `error` 行。`max_tokens` の既定 16000。ソケットは接続 10 秒、全体 600 秒（要求の `timeout_ms` があれば小さいほう）。`usage` は `input_tokens` / `output_tokens` を転記する（キャッシュの内訳は M6 の費用で扱う） | (a) `strict: true` のツール定義。(b) 再試行（429 / 5xx）を proxy で行う | (a) は `dict` / `Unknown` を含むスキーマで拒否されるので M5 では付けない（ADR-0022 論点 5。引数の検証はトランポリンが行う）。(b) は予算に見えない呼び出しが増えるので M5 では行わず、`error` 行を返す |
| 6. 秘密情報 | `ANTHROPIC_API_KEY` を読むのは proxy だけ。ランタイムと本言語のプログラムに環境変数を読む組み込みはなく、要求・応答にキーは入らない。proxy の `error` 行は `error.type: error.message` だけで、ヘッダや本文を含めない。proxy は stderr に要求・応答を書かない。キーをリポジトリに置かない（`scripts/llm-smoke.sh` は環境変数を要求する） | (a) キーをランタイムが読んで proxy に渡す。(b) 設定ファイル | 環境変数は子プロセスが継承するので、渡す経路が要らない。限界: `python` の Capability を持つツールは `os.environ` でキーを読める（論点 17） |
| 7. Capability の名前と既定 | **Capability の名前 = Effect の名前**（`io` / `exit` / `python` / `llm`）。`@agent` の `capabilities` の既定は `io, llm`。`python` / `exit` は明示しないと付与しない。M5 に `fs` / `net` の組み込みはないので入れない（足すときは Effect と一緒に） | (a) Effect と別の体系（`fs.read` など階層的）。(b) 既定をすべてにする。(c) 既定を `llm` だけにする | 1 つの名前の集合なら、ツールの `uses` と agent の `capabilities` を静的（E1405）にも実行時にも同じ規則で突き合わせられる（ADR-0014 論点 11 の想定）。(a) は ADR-0014 論点 12 で見送った階層的な名前が要る。(b) は最小権限に反し、`python` はサンドボックスではない。(c) は `print` するツールにまで明示を強いる |
| 8. 配備時の縮小 `WYS_CAPABILITIES` | 未設定 = 縮小なし。設定したら、根のコンテキストとすべての agent の Capability をその集合との積にする（**広げることはできない**）。空文字列 = すべて外す。未知の名前は無視する（縮小の方向に倒れる） | (a) 未知の名前で起動時エラー。(b) agent ごとに指定する | 失敗するなら閉じる方向に。agent ごとの指定は名前の規則が要り、M5 では 1 本で足りる |
| 9. agent コンテキストと入れ子 | ランタイムがスタックで持つ。底は**根のコンテキスト**（ツールなし、Capability は 4 つ ∩ `WYS_CAPABILITIES`、予算なし）。`wys_agent_enter(cfg)` で積み、`wys_agent_exit()` で下ろす（不整合はパニック `agent context mismatch (this is a compiler bug)`）。入れ子: Capability は `cfg.capabilities ∩ 外側 ∩ WYS_CAPABILITIES`、ツールの一覧は自分のもの、予算は自分の分を持ち、**検査はスタック上のすべての agent で、使った分はすべてに数える**（PoC4。外側の残りを超えない）。ツールの中から `@agent` を呼べば入れ子、`llm_generate` を直接呼べば現在のコンテキストで数える。ランタイムはツールを呼ぶ前に内部の状態の借りを返す | (a) 入れ子を禁止する。(b) 内側のツールの一覧も外側との積にする | (a) は AI-010（複数エージェント）の土台を失う。(b) はツールの一覧が「この LLM が要求できるもの」で、Effect の縮小で足りる |
| 10. 検査の順序と拒否 | ツールの要求ごとに、**実行の前に** (1) agent の一覧 → (2) ツールの `uses` ⊆ Capability → (3) 予算 → (4) 承認。(1)(2) の失敗は `Err(kind="denied")` で `llm_generate` を**中止**し、監査 `tool_denied`（モデルの逸脱。続けさせない）。(3) は `Err(kind="budget" \| "timeout")`、監査 `budget_exceeded`。(4) の拒否は `denied by approver` を `is_error` の `tool_result` として LLM に返して**続行**し、監査 `approval_denied`（正常な運用。LLM は別の方法を探せる）。引数の検証の失敗は `Err(kind="schema")`、監査 `schema_rejected`。ツールの `Err` は `is_error` で LLM に返す。すべて `Err(LlmError)` でパニックにしない | (a) 一覧にないツールも `is_error` で LLM に返して続ける。(b) 承認の拒否も中止する | (a) は「許可されていないものを要求する」モデルに会話を続けさせ、AC-007 の「拒否」が曖昧になる。(b) は承認が運用の一部（人が断ることがある）なのに、agent 全体を失敗にする |
| 11. 入口の再検査 | 防御を重ねるため、`wys_py_*`（`Err(PyError{kind:"CapabilityError"})`）、`wys_exit`（実行時エラー 101 `capability exit is not granted`）、`wys_llm_*`（`Err(kind="denied")`）の入口で現在のコンテキストの Capability を確かめ、監査 `capability_denied` を残す。`io`（`print` / `arg`）は入口で検査しない | (a) 入口の検査をしない（ツールの実行前の検査だけ）。(b) `print` も検査する | ツールの本体が、宣言した `uses` にない組み込みを呼ぶ経路（注釈のない関数を経由。ADR-0014 は書いた関数だけを検査する）を実行時に閉じる。`print` は害が小さく、検査のコストが見合わない |
| 12. 承認の方針 `WYS_APPROVAL` | `deny`（既定、未設定）: 常に拒否。`allow`: 常に許可。`prompt`: stderr に `approve tool <name> (agent <agent>) with args <JSON>? [y/N] ` を出し、stdin から 1 行読む。`y` / `yes` で許可、それ以外は拒否、EOF は `Err(kind="approval")`。未知の値も `Err(kind="approval")`。監査 `approval_requested` → `approval_granted` / `approval_denied` | (a) 既定を `prompt`。(b) 承認を関数（コールバック）で書く | 既定は閉じる方向（CI で stdin を待たない）。(b) は関数値がない。`prompt` は e2e で stdin をつないで検査する。承認待ちが無限に続くことは `max_llm_calls` が抑える |
| 13. 予算の種類と算定 | `max_llm_calls`（プロバイダを呼ぶたびに 1。モックも）、`max_tool_calls`（ツールを実行するたびに 1。拒否は数えない）、`max_tokens`（応答の `usage.input_tokens + output_tokens` の合計。モックは `MockResponse` の値）、`timeout_ms`（論点 14）。`0` は上限なし。検査は呼ぶ前（回数・トークン・時間）、応答の直後（トークン・時間）、ツールの前（回数・時間）。超えたら `Err(kind="budget")`、監査 `budget_exceeded`。費用（`max_cost_usd`）は **M6**（価格表は古くなるので proxy の adapter に置き、ランタイムは `usage` だけを数える） | (a) トークンを出力だけ数える。(b) 費用を M5 で概算する | 入力も課金されるので合計で数える。概算の価格表はすぐ古くなり、誤った安心を与える |
| 14. タイムアウトは呼び出しの境界で | 期限は agent に入ってからの経過時間。判定は LLM 呼び出しの前、**応答を受け取った直後**（期限を過ぎて届いた応答は受け取らず、要求されたツールも呼ばない。PoC4 で計画に加えた）、ツールの実行の前。超えたら `Err(kind="timeout")`。実行中のツール・HTTP は中断しない。時計は単調時計、`WYS_VIRTUAL_TIME=1` では実時間を数えず、モックの `latency_ms` はどちらのモードでも待たずに時計を進める。**タスクのキャンセル・`with_timeout`・タスクグループは M6**（ADR-0020 論点 15 の予約のまま） | (a) キャンセルを M5 で入れる。(b) 応答の後は見ない | (a) は lowering と ARC パスの変更で規模が XL（`docs/plans/m5.md` 分割の判断基準 3）。(b) は期限を過ぎた後にツールを動かしてしまう。境界の判定で AC-008 は満たせる |
| 15. 監査イベント | `AuditEvent = {seq: int, kind: str, agent: str, tool: str \| None, detail: str}`。`kind` は `llm_called` / `tool_called` / `tool_denied` / `capability_denied` / `approval_requested` / `approval_granted` / `approval_denied` / `budget_exceeded` / `schema_rejected`。`agent` は内部名（根は `""`）。時刻は含めない。`audit_events() -> list[AuditEvent]`（純粋）で読め、`WYS_AUDIT=<path>\|stderr` で同じ内容を JSON 行として追記する（`path` は追記、開けなければ無視） | (a) 時刻を含める。(b) 監査をファイルだけにする | 時刻を含めないので Conformance で `# expect-stdout` に書ける。プログラムから読めれば AC-007 を `.wys` のケースで検査できる |
| 16. プライバシー | **プロンプト・応答・ツールの引数と戻り値の本文は、監査にも既定のトレースにも含めない**（`detail` は長さ・名前・経路・方針だけ。`schema_rejected` の `found` は JSON の種類で値ではない）。M6 のトレース（AI-007）も本文は既定で含めず、明示の opt-in（環境変数）にする。承認の `prompt` が引数を見せるのは承認者への対話で、記録ではない。proxy はログを書かない | 本文を含めてデバッグしやすくする | NFR-005（秘密情報のログ抑制）。本文には利用者のデータや秘密が入り得る。デバッグは M6 の opt-in で |
| 17. `python` はサンドボックスではない | `python` を付与した agent のツールは Python の内側でファイル・ネットワーク・環境変数（API キーを含む）に何でもできる。ランタイムは Python の内側を強制できない（ADR-0018 論点 12）。M5 の扱い: `python` は既定で付与しない、`WYS_CAPABILITIES` で配備時に外せる、入口で再検査する。プロセスの隔離（サブプロセス、サンドボックス）は Phase 3 | M5 で Python をサブプロセスにする | 規模。`ai-sdk.md` と本 ADR に限界を明記し、既定で付与しないことで扱う |
| 18. `llm_generate` は M5 ではブロックする | proxy の応答を待つ間、ほかのタスクは進まない（Python と同じ。ADR-0020 論点 16）。API は `async` なので、M6 でスケジューラが proxy のパイプを待てるようにしても利用者のコードは変わらない。`async def` のツールはその場で `await` するので、ツールの中の `sleep_ms` / `await` では切り替わる | M5 で非ブロッキングにする | スケジューラに I/O の待ちを足す変更は M6 / Phase 3（`docs/plans/m5.md` 範囲外） |
| 19. モック | ランタイム内。`llm_mock_push` の順に消費、空なら `Err(kind="mock_exhausted")`。実プロバイダが設定されていれば `llm_mock_push` は実行時エラー（101） | — | ADR-0022 論点 10 |
| 20. 範囲外 | トレース（AI-007）、記録済み応答の再生と評価ケース（AI-008 / AC-009）、費用の上限と価格表、ストリーミング、ほかの adapter（`cmd:` で書ける）、再試行、`fs` / `net` の Capability、非ブロッキングの I/O、Python の隔離、複数エージェントの連携（AI-010） | — | `docs/plans/m5.md` 範囲外 |
| 21. 見直し条件 | M6 のトレース・再生を入れるとき（論点 2 のプロトコルに記録・再生の形を足す。論点 15 の形を広げる）。非ブロッキングの I/O を入れるとき（論点 18）。`fs` / `net` の組み込みを入れるとき（論点 7）。費用の上限（論点 13）。キャンセルを入れるとき（論点 14）。Python の隔離（論点 17）。Anthropic の API の版が変わり `fallbacks` / `tool_choice` / `output_config` の扱いが変わったとき（論点 5）。`cmd:` の作者からプロトコルの変更の要望が出たとき（論点 2 の `v`） | — | — |

## 新しいエラーコード

コンパイル時の診断は足さない（E14xx / E1501 は ADR-0022）。実行時の失敗はすべて `Err(LlmError)`（`kind`: `provider` / `schema` / `budget` / `timeout` / `denied` / `approval` / `mock_exhausted`。`ai-sdk.md` 5.3）で、パニックにしない。

実行時エラー（101）: `agent context mismatch (this is a compiler bug)`、`capability exit is not granted`（入口の再検査）、`llm_mock_push is not allowed when WYS_LLM_PROVIDER is set`。

環境変数の既定: `WYS_LLM_PROVIDER` 未設定 = モック、`WYS_APPROVAL` 未設定 = `deny`、`WYS_CAPABILITIES` 未設定 = 縮小なし、`WYS_AUDIT` 未設定 = 出力しない。

## 決定

上表の「決定（推奨）」の列を採用する（ユーザー決定、2026-11-06）。

## 影響

- 実装は `docs/plans/m5.md` の T9（ツール表と agent コンテキスト）、T10（承認・予算・タイムアウト）、T11（`wys llm-proxy`）。PoC4 の `crates/wys_runtime/src/ai.rs` と `crates/wys_cli/src/llm_proxy.rs` は本 ADR の ABI とプロトコル（`assistant.content`、`v`、`system`、`timeout_ms`、`AuditEvent` の形、`capability_denied`）に合わせて書き直す。
- `wys_cli` が `ureq` 2 + `rustls` に依存する（`wys_runtime` と生成した実行ファイルは依存しない。AC-004 の e2e で確かめる）。
- 生成コードに、コンパイラの絶対パスの文字列定数を埋める（論点 4）。
- Conformance の harness に `# expect-env:` を足す（`WYS_CAPABILITIES` / `WYS_APPROVAL` の検査）。`tests/conformance/ai/` を 3 つのモードで実行する。e2e は `cmd:` に固定の応答を返すスクリプトで決定的に検査し、Anthropic との往復は `scripts/llm-smoke.sh` で手元だけ。
- 更新する文書: `error-codes.md`（実行時エラー）、用語集（agent コンテキスト、根のコンテキスト、proxy、JSON 行のプロトコル、配備時の縮小、監査イベントの更新）、`docs/test-plan.md`（AC-007 / AC-008、AI-004〜006）、CLAUDE.md（環境変数）、ADR 一覧と未決定事項 10 の欄。
- 初版の制限: `llm_generate` はブロックする。実行中のツール・HTTP は期限で中断しない。費用は数えない。`python` のツールは秘密情報に触れ得る。再試行をしないので一時的な失敗は `Err(kind="provider")` になる。
- 見直し条件は論点 21。
