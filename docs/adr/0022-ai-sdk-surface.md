# ADR-0022: AI SDK の形と表面の API（標準添付、スキーマ型、`llm_generate[T]`、属性としての `@tool` / `@agent`）

- **状態:** Accepted
- **日付:** 2026-11-06
- **関連:** 未決定事項 9、AI-001〜AI-003、AC-006、LANG-006、ADR-0007、ADR-0011、ADR-0013、ADR-0014（論点 11・12）、ADR-0018（論点 9・12）、ADR-0019、ADR-0020（論点 6）、ADR-0021、ADR-0023、`docs/design/ai-sdk.md`、`docs/plans/m5.md`（T3・T6〜T8）、`docs/poc/poc4-results.md`、issue #62、#63、#64

## 背景

要件定義書 4.5 は「`agent` / `tool` / `llm` は専用構文に固定せず、まず型付き標準 SDK・デコレータ / アノテーションとして実装する」と定める。未決定事項 9 は「AI SDK を標準添付するか別パッケージとするか」。いまの言語の前提は次のとおり。

- 関数値がない（E0213）ので、Python の「関数を受け取って関数を返す」デコレータは書けない。
- Effect は `io` / `exit` / `python` の注釈で、Runtime の表現を持たない（ADR-0014 論点 11）。`llm` は M5 で足す予定（論点 12）。
- JSON は `Unknown` の木で受け、呼び出しに型引数を書ける（ADR-0021）。スキーマ型・`validate[T]`・`to_json`・JSON Schema の生成は本 ADR に委ねられた。
- パッケージ管理（INTEROP-004）はない。
- PoC4 は成功した: ツール表とトランポリン、agent のコンテキスト、JSON → 型付きの引数の変換、Anthropic Messages API との往復が成り立つ。

この ADR は SDK の配布の形、組み込みの名前と型、スキーマ型と JSON Schema、`validate` / `to_json` / `llm_generate` の意味、デコレータの構文と「M5 ではコンパイル時の属性」であること、`@tool` / `@agent` の引数と静的な検査、ツールへの露出、関数値が入ったときの移行を決める。Runtime 側（プロバイダ、Capability、承認、予算、監査）は ADR-0023。

## 選択肢と決定

| 論点 | 決定（推奨） | 主な代替案 | 理由 |
|---|---|---|---|
| 1. 配布の形 | **標準添付。** 組み込みの型・関数はコンパイラ（`wys_typeck` の組み込みの表）とランタイム（`wys_runtime::ai`）が持ち、import なしに使える。実プロバイダの adapter は `wys_cli` の副コマンド `wys llm-proxy`（ADR-0023） | (a) 別パッケージ。(b) 標準ライブラリのモジュール（`import ai`） | パッケージ管理（INTEROP-004）がないので (a) は不可能。(b) はモジュールの仕組みで組み込みの型を定義できない（コンパイラが glue を生成する `validate[T]` は言語の一部）。INTEROP-004 のときに見直す |
| 2. 組み込みの名前 | `llm_generate`、`llm_mock_push`、`validate`、`to_json`、`parse_json`（ADR-0021）、`audit_events`。接頭辞は `llm_` だけで、名前空間は作らない。再定義は E0209 | (a) `llm.generate` のような修飾名。(b) `ai_` 接頭辞で統一 | 組み込みは `print` / `spawn` と同じ平らな名前（ADR-0020 論点 4）。要件定義書 12 の `llm.generate` は非規範的で、修飾名には名前空間の仕組みが要る |
| 3. 組み込みのレコード | `Message` / `LlmRequest` / `LlmError` / `SchemaError` / `JsonError` / `AuditEvent` / `MockResponse` / `MockToolCall`（フィールドは `ai-sdk.md` 3 章）。**構造的**で、同じフィールドを持つ利用者のレコードと同じ型（`PyError` と同じ。ADR-0018 論点 9）。名前の再定義は `PyError` と同じ規則。`AuditEvent.tool` は `str \| None` | (a) 名目的な型にする。(b) `LlmError` を Union（`Provider \| Schema \| ...`）にする | 構造的なら利用者がテストで同じ形のレコードを作れ、`Result[T, LlmError]` を自分のエラー型と合成しやすい。(b) は 1 つの Union に 2 つのレコードを置けない（E0508）ので、`kind: str` で分ける（`PyError.kind` と同じ） |
| 4. スキーマ型 | `int` / `float` / `bool` / `str` / `None`、それらの Union、`list[T]`、`dict[str, T]`、フィールドがすべてスキーマ型のレコード（Generic は具体化後に判定）、`Unknown`。`Result` / `Task` / `PyObject` / `PyError` / 型パラメータ / キーが `str` でない `dict` は **E1501**。例外: `@tool` の戻り値の最上位だけ `Result[T, E]`（`T`・`E` がスキーマ型）を認め、`Err` は `is_error` の `tool_result` として LLM に返す | (a) `Result` を一般に認める。(b) `@tool` の戻り値も `Result` を認めない | JSON に写像が一意に決まる型だけにする。(a) は `Ok` / `Err` の JSON の形を決める必要があり、LLM の出力に `Result` を要求する場面もない。(b) はツールの失敗を `panic` か文字列で返すことになり、「失敗の明示的な扱い」（要件定義書 12 の契約）に反する |
| 5. JSON Schema の生成 | 型ごとに draft 2020-12 の部分集合の文字列定数 `@wys_schema_t{k}` を生成する（写像は `ai-sdk.md` 4.2）。レコードは `properties` + 全フィールドの `required` + `additionalProperties: false`、`T \| None` は `anyOf` に `null`、`Unknown` は `{}`、`dict[str, T]` は `additionalProperties: S(T)`。数値・文字列・配列の制約、`enum`、`$ref`、`title` / `description` は生成しない。**Anthropic の構造化出力の部分集合**（再帰的スキーマ、`minimum` / `maximum` / `multipleOf`、`minLength` / `maxLength`、`false` 以外の `additionalProperties` が非対応。すべての object に `additionalProperties: false` が必須）に対し、レコード・スカラー・Union・`list` は乗り、`dict[str, T]` と `Unknown` は乗らない。これは型検査で止めず、proxy が `Err(kind="provider")` を返す | (a) `dict[str, T]` / `Unknown` を `llm_generate` と `@tool` では E1501 にする。(b) `dict` を `{"type":"object"}` に緩めて送る | 制約を生成しないのは本言語の型に情報がないからで、プロバイダの都合と一致する。(a) はプロバイダ固有の制限をコンパイラに持ち込み、モックや `cmd:` では不要な制限になる。(b) も部分集合に乗らない。再帰は E0504 で存在しないので `$ref` は要らない |
| 6. `validate[T]` の意味 | `validate[T](u: Unknown) -> Result[T, SchemaError]`、純粋。規則は `ai-sdk.md` 4.3: 整数は `float` にも受け付ける、`float` は `int` に受け付けない、Union は構成要素の順に試す、レコードの**宣言にないキーは無視する**（幅部分型）、`Unknown` は木を共有する。失敗は最初の不一致 1 つ（`path` / `expected` / `found`）。途中で失敗したら作りかけの値をすべて release する。型 `T` ごとに復号の glue `@wys_validate_t{k}` を生成する | (a) 宣言にないキーを拒否する。(b) すべての不一致を列挙する。(c) 整数を `float` に受け付けない | `validate` は LLM の出力だけでなく任意の JSON（外部 API の応答）に使うので、構造的型付けの幅部分型と同じく余分を無視する（スキーマの `additionalProperties: false` は LLM に余分を書かせないためで、矛盾しない）。(b) は glue が大きくなり、AC-006 には最初の 1 つで足りる。(c) は LLM が `3.0` を `3` と書くので実用にならない |
| 7. `to_json` | `to_json(v: T) -> str`、純粋、`T` はスキーマ型（E1501）。空白なし、レコードは定義の順、`dict` は挿入順、`float` は ADR-0019 の表示、NaN / 無限大は実行時エラー（101）。型ごとに `@wys_to_json_t{k}` | `print` を JSON の表示にする | ADR-0021 論点 6（未検証の値を出力に流さない）。JSON の文字列化は明示の関数にする |
| 8. `llm_generate[T]` | **`async` の組み込み** `llm_generate[T](req: LlmRequest) -> Result[T, LlmError]`、Effect `llm`。型引数は必須（E0403 のヒントで示す）。`T = str` なら出力スキーマを付けず本文をそのまま返し、それ以外は `@wys_schema_t{k}` を要求に付け、本文を `parse_json` → `validate[T]`（失敗は `kind="schema"`、`message` に経路）。ツールの往復はこの 1 回の呼び出しの中でランタイムが行う（`ai-sdk.md` 5.1）。**agent の外からも呼べる**（根のコンテキスト。ADR-0023 論点 9）。M5 の実装はブロックする（ADR-0023 論点 18） | (a) 同期の組み込みにする。(b) `generate_text` と `generate[T]` を分ける。(c) agent の外では `Err(kind="denied")`（PoC4 の試作） | (a) は M6 で非ブロッキングにしたとき利用者のコードが変わる。(b) は `T = str` で表せる。(c) はツールを使わない一発の呼び出し（AC-006 の検査）にまで `@agent` を強いる。根のコンテキストでも Capability と `WYS_CAPABILITIES` は効く |
| 9. `LlmRequest` の規則 | `model=""` は既定（`@agent(model=)` → `WYS_LLM_MODEL` → proxy の既定）、`max_tokens=0` は proxy の既定（1 応答の出力の上限）。`messages` が空、`role` が `user` / `assistant` 以外は `Err(kind="provider", message="invalid request: ...")`。`system` は空でよい。`assistant` の `Message` は文字列として送る（thinking のブロックは呼び出しをまたいで引き継がない） | (a) 不正な要求を型で防ぐ（`role` を Union のリテラル型に）。(b) `kind="request"` を足す | リテラル型はない。`kind` は `docs/plans/m5.md` T8 で決めた 7 つのままにし、要求の不正は「プロバイダに送れない」として `provider` に含める |
| 10. モック | `llm_mock_push(r: MockResponse) -> None`（純粋）。`WYS_LLM_PROVIDER` 未設定のとき `llm_generate` が先頭から消費し、空なら `Err(kind="mock_exhausted")`。`tool_calls` が複数なら順に 1 つずつ検査して実行する。実プロバイダが設定されているときに呼ぶと実行時エラー（101） | (a) モックを `cmd:` のスクリプトで書く。(b) プロバイダが設定されていればモックを黙って無視する | ランタイム内のモックなら Conformance が子プロセスなしで決定的に動く。(b) はテストの設定のまま実プロバイダに課金する事故を黙って通す |
| 11. デコレータの構文 | `decorator = "@" IDENT [ "(" [ kw_args ] ")" ] NEWLINE` を `[ "pub" ] [ "async" ] "def"` の直前に 0 個以上。引数はキーワード引数だけで、値はリテラル（`int` / `str` / `bool`）か、文字列と関数名（`a`、`m.f`）の `list`。字句 `@`、AST `FuncDef.decorators`、`fmt`、`grammar.ebnf`。`type` の前や位置引数は E0101 | (a) `@tool` を関数の属性の構文（`def f() -> T uses io tool(...):`）にする。(b) 文字列の注釈 | 一般的な形を保てば、関数値が入ったときにライブラリのデコレータへそのまま移行できる（論点 17）。(a) は本言語だけの構文で、移行先がない |
| 12. M5 では「コンパイル時の属性」 | コンパイラが解釈する名前は `tool` と `agent` だけ（ほかは **E1401**。ヒントで一覧）。引数の誤り（未知のキーワード、位置引数、リテラルでも名前の `list` でもない値、型の不一致、重複、`approval` が `"none"` / `"required"` 以外、負の予算）は **E1402**。`capabilities` の未知の名前は E1101（Effect の名前と同じ集合）。属性は呼び出しの意味を変えない（ツールは普通の関数としても呼べる） | 関数値と利用者定義のデコレータを先に入れる | 関数値（E0213）は M5 の範囲外（`docs/plans/m5.md` 範囲外）。属性なら字句・文法・AST・検査だけで済む |
| 13. `@tool` の引数と関数の条件 | `description: str = ""`、`approval: "none" \| "required" = "none"`。関数は `uses` を書く（`uses pure` でよい。**E1403**）、引数と戻り値はスキーマ型（E1501。論点 4 の `Result` の例外）、`main` / `test_*` には付けない（E1403）、`async def` でもよい（トランポリンがその場で `await` する）。ツールの名前は関数の名前（修飾なし。Anthropic の名前の制約 `[a-zA-Z0-9_-]`）。引数を 1 つのレコードとみなして JSON Schema と検証の glue を作る | (a) `description` を必須にする。(b) `name=` で名前を変えられる。(c) `effects=[...]` を属性に書く（要件定義書 12） | `uses` が Effect の唯一の宣言（ADR-0014）で、(c) は二重の宣言になる。(a) は説明なしのツールを書けないようにするが、試作では摩擦が大きい（推奨にとどめる）。(b) は名前の衝突の規則が増える |
| 14. `@agent` の引数と関数の条件 | `tools: list = []`、`capabilities: list[str] = ["io", "llm"]`、`model: str = ""`、`max_llm_calls` / `max_tool_calls` / `max_tokens: int = 0`（上限なし）、`timeout_ms: int = 0`（期限なし）。関数は `uses` を書く（E1403）、`main` / `test_*` には付けない、`@tool` と同時には付けない（E1403）。`tools` の要素は `@tool` の関数で、同じ名前が 2 つないこと（**E1404**）。ツールの `uses` ⊆ `capabilities`（**E1405**。足りない名前を列挙し、`capabilities` に足す修正とツールの定義を `related` に）。`capabilities` ⊆ 自身の `uses`（**E1406**。`uses` に足すか `capabilities` を狭める修正を示す）。`async def` は要求しない。lowering が本体を `wys_agent_enter(cfg)` / `wys_agent_exit()` で囲む | (a) `capabilities` の既定を自身の `uses` にする。(b) E1406 を入れない。(c) `@agent` を `async def` に限る。(d) `output=` / `max_cost_usd=`（要件定義書 12） | 既定の `io, llm` は `docs/plans/m5.md` の承認どおり（`python` / `exit` は明示）。(b) だと `uses llm` の agent がランタイム経由で `io` / `python` のツールを動かせ、署名が Effect を隠す（`wys explain` の `required` も誤る）。(c) は不要な規則（`llm_generate` を呼ばない agent は無意味だが有害ではない）。(d) は `output` が `llm_generate[T]` の `T` で表せ、費用は M6 |
| 15. コンパイラの生成物 | ツール表 `@wys_tools`（名前・説明・引数の JSON Schema・`uses` の Effect のビット・承認の要否・トランポリン）、トランポリン `@wys_tool_entry_f{}`（`args_json` → `parse_json` → `validate[引数のレコード]` → 呼び出し → `to_json`。`@wys_task_entry_f{}` と同じ型）、`@agent` ごとの `AgentCfg` の定数（名前・ツール表の番号・Capability のビット・予算）。IR: `Function.tool` / `Function.agent`。検証器: `tools` の要素が `tool` の関数であること | ランタイムが型の情報を実行時に持つ | ADR-0012 / ADR-0020 と同じく、型ごとの glue を生成し、ランタイムは型を知らない。PoC4 で 5 フィールドのレコードの glue が C で 30 行ほどと確かめた |
| 16. `dump-ir` / `wys explain` | `dump-ir` は `uses` の後に `@tool(approval=required)` / `@agent(tools=[add], capabilities=[io, llm], max_tool_calls=2)`。`wys explain` の `function` に `tool`（`{description, approval}` か `null`）と `agent`（`{tools, capabilities, model, max_llm_calls, max_tool_calls, max_tokens, timeout_ms}` か `null`）を足す（項目の追加なので版は上げない。ADR-0017 論点 1）。JSON Schema・ツール表・glue は出さない | スキーマも `explain` に出す | 静的な事実（型と絞り込みの理由）だけを出す方針。スキーマは型から機械的に決まり、見たければ別の副コマンドを M6 以降で検討する |
| 17. 関数値が入ったときの移行 | 構文は変えない。`@tool(...)` / `@agent(...)` は「関数を受け取って関数を返す」ライブラリのデコレータになり、`tools=[add]` は関数値の `list` になる。E1401 / E1402 / E1404 は一般の型検査（デコレータの型）に吸収され、E1403 / E1405 / E1406 は残る（属性の意味の検査）。IR の `Function.tool` / `Function.agent` とツール表はそのまま。利用者のコードは変えない | M5 で属性専用の構文にする | 要件定義書 4.5「利用実績に応じて言語機能への昇格を検討する」の逆方向も開けておく |
| 18. 範囲外 | 利用者定義のデコレータ、`@agent(output=, max_cost_usd=, system=)`、ツールの `name=`、`enum` / 範囲などのスキーマの制約、ストリーミング、`Message` の多モーダルな内容、`LlmRequest` の省略（補助関数） | — | `docs/plans/m5.md` 範囲外。補助関数はレコードの構築で十分書ける |
| 19. 見直し条件 | 関数値を入れるとき（論点 17）。INTEROP-004（パッケージ管理）のとき（論点 1）。`dict` / `Unknown` を LLM の出力に使う需要が出たとき（論点 5 の (a)）。再帰的なスキーマの需要（ADR-0021 論点 12）。M6 のトレースで `explain` に動的な情報を足すとき（論点 16） | — | — |

## 新しいエラーコード

| コード | 内容 |
|---|---|
| E1401 | 未知のデコレータ名（M5 で解釈するのは `tool` / `agent` だけ。ヒントで一覧） |
| E1402 | デコレータの引数の誤り（未知のキーワード、位置引数、リテラルでも関数名の `list` でもない値、型の不一致、重複、`approval` が `"none"` / `"required"` 以外、負の予算） |
| E1403 | `@tool` / `@agent` の関数に `uses` がない、`main` / `test_*` に付けている、または両方を同じ関数に付けている |
| E1404 | `@agent` の `tools` の要素が `@tool` の関数でない、または同じ名前のツールが 2 つある |
| E1405 | ツールの `uses` が `@agent` の `capabilities` に含まれない（足りない名前を列挙し、`capabilities` に足す修正とツールの定義を `related` に示す） |
| E1406 | `@agent` の `capabilities` が自身の `uses` に含まれない（`uses` に足すか `capabilities` を狭める修正を示す） |
| E1501 | スキーマ型でない型（`Result` / `Task` / `PyObject` / `PyError` / 型パラメータ / キーが `str` でない `dict`）を `validate` / `llm_generate` / `to_json` の型引数か `@tool` の引数・戻り値に使っている（`@tool` の戻り値の最上位の `Result[T, E]` は除く） |

警告は足さない。`capabilities` の未知の名前は E1101、`llm` は E1101 の一覧に加わるだけ。型引数の個数は E0402、`validate` / `llm_generate` の型引数の省略は E0403（ヒントで `validate[T](...)` を示す）。実行時エラー（101）: `cannot encode NaN or infinity as JSON`（`to_json`）、`llm_mock_push is not allowed when WYS_LLM_PROVIDER is set`。

## 決定

上表の「決定（推奨）」の列を採用する（ユーザー決定、2026-11-06）。

## 影響

- 実装は `docs/plans/m5.md` の T6（スキーマ型と glue）、T7（デコレータと静的検査）、T8（型付き LLM API とモック）。T7 の構文の部分は本 ADR の承認を待たずに始めてよい。
- `@` が字句に加わる（既存のケースに `@` はない）。`async` / `await` と違い、`tool` / `agent` は予約語に**しない**（デコレータの名前として読むだけ）。
- `wys_typeck` に組み込みの型の表（8 つのレコード）、組み込み関数（`llm_generate` / `llm_mock_push` / `validate` / `to_json` / `audit_events`）、Effect `llm`、スキーマ型の判定、E14xx / E1501 を足す。`wys_lower` / `wys_ir` に `Function.tool` / `Function.agent`、glue の生成の指示。`wys_backend` に `@wys_schema_t{k}` / `@wys_validate_t{k}` / `@wys_to_json_t{k}` / ツール表 / トランポリン / `wys_agent_enter` / `wys_agent_exit` の挿入。
- 更新する文書: `grammar.ebnf`（`decorator`）、`error-codes.md`（E1401〜E1406、E1501、実行時エラー、組み込み関数の表）、`explain-schema.json`（`tool` / `agent`）、`docs/design/ir.md`（`Function.tool` / `agent`、glue）、用語集（属性、スキーマ型、ツール表、トランポリン）、`docs/test-plan.md`、ADR 一覧と未決定事項 9 の欄、`docs/README.md`（`ai-sdk.md`）。
- 初版の制限: `dict[str, T]` / `Unknown` のスキーマは Anthropic に乗らない（論点 5）。`description` を省略できる（LLM の挙動は悪くなる）。`llm_generate` の呼び出しをまたいで thinking のブロックは引き継がれない（論点 9）。
- 見直し条件は論点 19。
