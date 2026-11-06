# AI SDK 設計（M5、要件定義書 13 の成果物 4）

- **状態:** 確定（ADR-0022 / ADR-0023 は 2026-11-06 に Accepted）
- **範囲:** AI-001〜AI-006、AC-006〜AC-008、NFR-005。トレース（AI-007）・記録済み応答の再生と評価ケース（AI-008）・費用の上限は M6。
- **関連:** ADR-0011、ADR-0014（論点 11）、ADR-0018（論点 12）、ADR-0020（論点 6・15・16）、ADR-0021、ADR-0022、ADR-0023、`docs/plans/m5.md`、`docs/poc/poc4-results.md`

## 1. 方針

- **専用構文にしない。** AI の機能は組み込みの型・関数と、`@tool` / `@agent` の 2 つのデコレータで提供する（要件定義書 4.5 の実装方針）。M5 のデコレータは**コンパイル時の属性**で、関数値が入ったら同じ構文のままライブラリのデコレータに置き換える（ADR-0022）。
- **保証はランタイムが担う。** LLM がツールを要求するたびに、実行の**前に**「一覧 → Capability → 予算 → 承認」の順で検査し、拒否は監査イベントに残す（AC-007 / AC-008）。LLM の出力はスキーマ検証を通ったものだけが型付きの値になり（AC-006）、ツールの引数も戻り値も JSON を境界にして検証する。プロンプトの指示に権限を依存させない（セキュリティ原則、ADR-0023）。
- **静的な検査は早期発見のため。** コンパイラはデコレータの引数、スキーマ型、ツールの `uses` と agent の `capabilities` を検査する（E14xx / E1501）が、これは補助で、保証ではない。
- **CI はネットワークに出ない。** 既定のプロバイダはランタイム内のモックで、Conformance はモックで決定的に検査する。実プロバイダは別プロセス `wys llm-proxy` に閉じ込め、生成した実行ファイルの依存を変えない（AC-004）。

## 2. 全体像

```text
利用者のコード                    コンパイラ（wys_typeck / wys_lower / wys_backend）
  @tool / @agent / 型 T   --->    属性の検査（E14xx / E1501）
                                  型 T ごとの glue: @wys_validate_t{k} / @wys_to_json_t{k} / @wys_schema_t{k}（JSON Schema の文字列定数）
                                  ツール表 @wys_tools（名前・説明・引数の JSON Schema・Effect・承認の要否・トランポリン）
                                  トランポリン @wys_tool_entry_f{}（JSON → 検証 → 呼び出し → JSON）
                                  @agent の本体を wys_agent_enter(cfg) / wys_agent_exit() で囲む

ランタイム（wys_runtime::ai）                               プロバイダ
  agent コンテキストのスタック（ツール一覧、Capability、      モック（既定、ランタイム内）
    承認の方針、予算の残り）                                  wys llm-proxy（別プロセス、Anthropic Messages API）
  llm_generate のループ（要求 → 応答 → ツールの検査と実行）   cmd:<path>（利用者のコマンド、同じプロトコル）
  監査イベント（audit_events() / WYS_AUDIT）                   ← stdin / stdout の JSON 行
```

## 3. 組み込みの型と関数（AI-001）

すべて標準添付で、import なしに使える（ADR-0022 論点 1）。レコードは構造的で、同じフィールドを持つ利用者のレコードと同じ型になる（`PyError` と同じ）。

| 型 | フィールド | 用途 |
|---|---|---|
| `Message` | `{role: str, content: str}` | 会話の 1 発話。`role` は `user` / `assistant` |
| `LlmRequest` | `{model: str, system: str, messages: list[Message], max_tokens: int}` | `llm_generate` の要求。`model=""` は既定（`@agent(model=)` → `WYS_LLM_MODEL` → proxy の既定）。`max_tokens=0` は proxy の既定（**1 応答の出力の上限**。`@agent` の予算の `max_tokens`（合計）とは別） |
| `LlmError` | `{kind: str, message: str}` | `kind` は `provider` / `schema` / `budget` / `timeout` / `denied` / `approval` / `mock_exhausted`（5.3） |
| `SchemaError` | `{path: str, expected: str, found: str}` | `validate` の失敗。`path` は `steps[2].name`（根は `""`） |
| `JsonError` | `{message: str, offset: int}` | `parse_json` の失敗（ADR-0021） |
| `AuditEvent` | `{seq: int, kind: str, agent: str, tool: str \| None, detail: str}` | 監査イベント（9 章） |
| `MockResponse` | `{text: str, tool_calls: list[MockToolCall], input_tokens: int, output_tokens: int, latency_ms: int}` | モックの応答 1 つ |
| `MockToolCall` | `{name: str, args_json: str}` | モックが要求するツールの呼び出し |

| 関数 | 型 | Effect | 内容 |
|---|---|---|---|
| `llm_generate[T](req: LlmRequest)` | `async`、`Result[T, LlmError]` | `llm` | LLM を呼び、ツールの要求を検査しながら実行し、最終の本文を `T` として返す（5 章）。`T` は `str` かスキーマ型。型引数は必須（省略は E0403） |
| `validate[T](u: Unknown)` | `Result[T, SchemaError]` | なし | `Unknown` の木を型 `T` として検証し、型付きの値にする（4.3） |
| `to_json(v: T)` | `str` | なし | スキーマ型の値を JSON の文字列にする（4.4） |
| `parse_json(s: str)` | `Result[Unknown, JsonError]` | なし | ADR-0021 |
| `llm_mock_push(r: MockResponse)` | `None` | なし | モックの応答を末尾に積む。実プロバイダが設定されていれば実行時エラー（8.1） |
| `audit_events()` | `list[AuditEvent]` | なし | これまでの監査イベント（順序付き、`seq` 昇順） |

## 4. スキーマ型と JSON（AI-002）

### 4.1 スキーマ型

`validate[T]` / `to_json` / `llm_generate[T]` の `T`、`@tool` の引数と戻り値に使える型。

- `int` / `float` / `bool` / `str` / `None`、それらの Union（`int | None`、`int | str`）
- `list[T]`、`dict[str, T]`（`T` はスキーマ型）
- フィールドがすべてスキーマ型のレコード（Generic なレコードは具体化した型引数がスキーマ型であること）
- `Unknown`（「任意の JSON」。再帰的なスキーマはここで逃がす。ADR-0021 論点 10）

`Result` / `Task` / `PyObject` / `PyError` / 型パラメータを含む型 / キーが `str` でない `dict` / 一番外側の `None` は E1501。例外として `@tool` の戻り値の**最上位**だけ `Result[T, E]`（`T`・`E` はスキーマ型）を認める（6.2）。

### 4.2 JSON Schema の生成

コンパイラが型ごとに JSON Schema（draft 2020-12 の部分集合）の文字列定数 `@wys_schema_t{k}` を生成し、`llm_generate[T]` の出力スキーマと `@tool` の引数の定義に使う。

| 型 | JSON Schema |
|---|---|
| `int` / `float` / `bool` / `str` / `None` | `{"type":"integer"}` / `{"type":"number"}` / `{"type":"boolean"}` / `{"type":"string"}` / `{"type":"null"}` |
| Union | `{"anyOf":[...]}`（構成要素の順。`T \| None` は `null` を最後に） |
| `list[T]` | `{"type":"array","items":S(T)}` |
| `dict[str, T]` | `{"type":"object","additionalProperties":S(T)}` |
| レコード | `{"type":"object","properties":{...名前の順},"required":[全フィールド],"additionalProperties":false}`（フィールドの順は名前の順。ADR-0022 の訂正）。入れ子は展開する（`$ref` は使わない。再帰は E0504 で存在しない） |
| `Unknown` | `{}` |

生成しないもの: 数値の範囲（`minimum` など）、文字列の長さ・パターン、配列の要素数、`enum`、`title` / `description`。本言語の型にそれらの情報がなく、Anthropic の構造化出力もこれらを受け付けない。**Anthropic の部分集合に乗らないのは `dict[str, T]`（`additionalProperties` が `false` でない）と `Unknown`（`{}`）**で、これらを `llm_generate[T]` の `T` や `@tool` の引数に使うと、`anthropic` のプロバイダは `Err(kind="provider")` を返し得る（モックと `cmd:` は影響しない）。型検査では止めない（プロバイダに依存する制限はコンパイラに持ち込まない）。ADR-0022 論点 5。

### 4.3 `validate[T]` の規則

| JSON の値 | 受け付ける `T` |
|---|---|
| `null` | `None`（または `None` を含む Union） |
| `true` / `false` | `bool` |
| 整数（`parse_json` が `int` にしたもの） | `int`、**`float`**（`float(n)` に変換する。LLM は `3.0` を `3` と書く） |
| `float` | `float` だけ（`int` には受け付けない。`3.0` でも） |
| 文字列 | `str` |
| 配列 | `list[T]`（要素を順に検証） |
| オブジェクト | レコード（宣言したフィールドをすべて要求する。**宣言にないキーは無視する**（幅部分型、ADR-0007））、`dict[str, T]`（値を順に検証） |
| 何でも | `Unknown`（木をそのまま共有する。ADR-0021 論点 4） |

`validate` の引数は `Unknown` に置ける値（`Unknown`、`match` でスカラーに絞り込んだ値、スカラーとその Union、`None`）。`case list():` / `case dict():` で絞り込んだ値は `list[Unknown]` / `dict[str, Unknown]` 型で `Unknown` に戻せないので渡せない（E0201）。絞り込む前の変数を渡す（ADR-0021 論点 2。2026-11-06 にユーザーが現状のままと決めた。必要になったら ADR-0021 を見直す）。

Union は構成要素の順（型検査がそろえた順: `int`、`bool`、`str`、`float`、レコード、`list`、`dict`、`Unknown`、`None`）に試し、最初に合ったものにする（`int | float` に `3` は `int`）。構成要素の内側の失敗は Union の段の失敗として報告する。失敗は最初の不一致 1 つを `SchemaError` で返す: `path` は根からの経路（`""`、`title`、`steps[2].name`、`dict` は `.key`）、`expected` は本言語の型の表記（`str`、`int | None`、`list[int]`、レコードはその名前。Generic なレコードは型引数なし）、`found` は JSON の種類（`null` / `bool` / `int` / `float` / `str` / `list` / `dict`）か `missing`（フィールドがない）。途中で失敗したときは作りかけの値をすべて release する（3 つのモードで生存数 0 を検査する）。

### 4.4 `to_json`

スキーマ型の値を RFC 8259 の文字列にする。空白なし。レコードはフィールドの名前の順（ADR-0022 の訂正）、`dict` は挿入順、`float` は ADR-0019 の表示（JSON の数値の文法に収まる）、`None` は `null`、`Unknown` は木のまま。NaN / 無限大は実行時エラー（101、`cannot encode NaN or infinity as JSON`）。`validate` → `to_json` → `parse_json` → `validate` は同じ値に戻る（`lang/json_roundtrip.wys`）。

## 5. `llm_generate` の動作（AI-001 / AI-002、AC-006）

### 5.1 手順

1. **コンテキスト。** 現在の agent コンテキスト（なければ根のコンテキスト。7.1）の Capability に `llm` がなければ `Err(kind="denied")`、監査 `capability_denied`。
2. **要求の検査。** `messages` が空、`role` が `user` / `assistant` 以外なら `Err(kind="provider", message="invalid request: ...")`。
3. **予算（前）。** スタック上のすべての agent で `max_llm_calls` / `max_tokens` / `timeout_ms` を見る。超えていれば `Err(kind="budget" | "timeout")`、監査 `budget_exceeded`。
4. **プロバイダを呼ぶ。** `system` / `messages` / agent の一覧にあるツールの定義（名前・説明・引数の JSON Schema）/ `T != str` なら出力スキーマ / `max_tokens` / 残り時間（`timeout_ms`）を 1 行の JSON で送り、1 行の応答を待つ（**この間ほかのタスクは進まない**。ADR-0023 論点 18）。モックなら先頭の `MockResponse` を消費する（空なら `Err(kind="mock_exhausted")`）。
5. **使用量を数える。** `usage.input_tokens + output_tokens`（モックは `input_tokens + output_tokens`、`latency_ms` は時計を進める）をスタック上のすべての agent に加え、`llm_calls` を 1 増やす。監査 `llm_called`（`tokens=N`）。
6. **予算（後）。** `max_tokens` と `timeout_ms` をもう一度見る。期限を過ぎて届いた応答は受け取らず、要求されたツールも呼ばない（PoC4 で決めた。`Err(kind="timeout")`）。
7. **ツールの要求がなければ終わり。** `stop` が `max_tokens` なら `Err(kind="provider", message="response truncated: max_tokens")`。`T = str` なら `text` をそのまま `Ok`。それ以外は `parse_json(text)` → `validate[T]`。失敗は `Err(kind="schema", message="<path>: expected <expected>, found <found>")`、監査 `schema_rejected`。
8. **ツールの要求があれば、1 つずつ順に**（Anthropic は `disable_parallel_tool_use` で 1 つ。`cmd:` が複数返したら順に）: **実行の前に** (a) agent の一覧にあるか → なければ `Err(kind="denied")`、監査 `tool_denied`（**`llm_generate` を中止する**。モデルの逸脱）。(b) ツールの `uses` ⊆ コンテキストの Capability → 足りなければ同じく `Err(kind="denied")`。(c) 予算（`max_tool_calls` / `timeout_ms`、すべての agent）→ `Err(kind="budget" | "timeout")`。(d) 承認が要るなら方針（7.4）→ 拒否なら `tool_result` に `denied by approver`（`is_error`）を入れて **続行**（正常な運用）。
9. **実行。** `tool_calls` を 1 増やし（すべての agent）、監査 `tool_called`（`args_len=N`）、トランポリンに `args` の JSON を渡す。引数の検証に失敗したら `Err(kind="schema", message="tool `name`: <path>: ...")`、監査 `schema_rejected`。戻り値の JSON（`Result` の `Err` なら `is_error`）を `tool_result` として、プロバイダが返した `assistant` の値を**そのまま**添えて次の要求に戻し、**3 へ**（プロバイダを呼ぶたびに予算（前）を確かめる。`max_llm_calls` はツールの往復の 2 回目以降の呼び出しも数える）。

### 5.2 `T = str`

出力スキーマを付けず、本文を検証せずに返す。ツールは使える。

### 5.3 `LlmError.kind`

| `kind` | 原因 |
|---|---|
| `provider` | proxy を起動できない、応答が壊れている、`error` 行（HTTP の失敗、`refusal`）、要求が不正、`max_tokens` で切れた |
| `schema` | 出力かツールの引数が型に合わない（`message` に経路） |
| `budget` | `max_llm_calls` / `max_tool_calls` / `max_tokens` を超えた |
| `timeout` | `timeout_ms` を超えた（呼び出しの境界で判定） |
| `denied` | 一覧にないツール、Capability の不足、`llm` のないコンテキストからの呼び出し |
| `approval` | `WYS_APPROVAL=prompt` で stdin から回答を読めない、`WYS_APPROVAL` が未知の値 |
| `mock_exhausted` | モックの応答が尽きた |

承認の拒否は `kind` にならない（LLM に返して続ける）。パニックにするのは agent コンテキストの不整合（コンパイラの誤り）だけ。

## 6. `@tool` / `@agent`（AI-003）

### 6.1 構文

```text
decorator  = "@" IDENT [ "(" [ kw_args ] ")" ] NEWLINE ;
kw_args    = IDENT "=" attr_value { "," IDENT "=" attr_value } [ "," ] ;
attr_value = INT | STRING | "True" | "False" | "[" [ attr_elem { "," attr_elem } [ "," ] ] "]" ;
attr_elem  = STRING | IDENT { "." IDENT } ;
```

`{ decorator }` は `[ "pub" ] [ "async" ] "def"` の直前にだけ書ける（`type` の前は E0101）。M5 で解釈する名前は `tool` と `agent` だけ（ほかは E1401）。`fmt` はデコレータを `def` の直前の行に 1 つずつ置き、キーワード引数をレコードの構築と同じ形に整える。

### 6.2 `@tool`

| 引数 | 型 | 既定 | 意味 |
|---|---|---|---|
| `description` | `str` | `""` | LLM に見せる説明（書くことを強く推奨する） |
| `approval` | `"none"` / `"required"` | `"none"` | 実行の前に承認が要るか（7.4） |

関数の条件: `uses` を書く（`uses pure` でよい。E1403）、引数と戻り値はスキーマ型（E1501。戻り値の最上位だけ `Result[T, E]` を認め、`Err(e)` は `to_json(e)` を `is_error` の `tool_result` として LLM に返す）、`main` / `test_*` ではない（E1403）、`async def` でもよい（トランポリンがその場で `await` する）。ツールの名前は関数の名前（モジュールの修飾は付けない）。ツールは普通の関数としても呼べる（属性は呼び出しの意味を変えない）。

コンパイラは、引数を 1 つのレコードとみなして JSON Schema と検証の glue を作り、ツール表の 1 行（名前・説明・引数の JSON Schema・`uses` の Effect・承認の要否・トランポリン）を生成する。トランポリンは `args_json` → `parse_json` → `validate[引数のレコード]` → 呼び出し → `to_json(戻り値)`。

### 6.3 `@agent`

| 引数 | 型 | 既定 | 意味 |
|---|---|---|---|
| `tools` | `@tool` の関数名の `list` | `[]` | LLM が要求できるツール（E1404: `@tool` でない、同じ名前が 2 つ） |
| `capabilities` | Effect 名の `str` の `list` | `["io", "llm"]` | 付与する Capability（名前は E1101 の集合。`python` / `exit` は明示） |
| `model` | `str` | `""` | 既定のモデル（`LlmRequest.model` が空のとき） |
| `max_llm_calls` / `max_tool_calls` / `max_tokens` | `int` | `0`（上限なし） | 予算（7.5）。`max_tokens` は合計で、`LlmRequest.max_tokens`（1 応答）とは別。負の値は E1402 |
| `timeout_ms` | `int` | `0`（期限なし） | agent に入ってからの期限（7.6） |

関数の条件: `uses` を書く（E1403）、`main` / `test_*` ではない、`@tool` と同時に付けない（E1403）、ツールの `uses` ⊆ `capabilities`（E1405）、`capabilities` ⊆ 自身の `uses`（E1406。agent がツールを通して起こし得る Effect を署名が隠さない）。`async def` でなくてもよいが、`llm_generate` を呼ぶには `async def` が要る（ADR-0020 論点 6）。lowering は本体を `wys_agent_enter(cfg)` / `wys_agent_exit()` で囲む（`return` / `?` / パニック以外のすべての出口）。agent の名前は関数の内部名（監査の `agent`）。

### 6.4 `dump-ir` / `wys explain`

`dump-ir` は `uses` の後に `@tool(approval=required)` / `@agent(tools=[add], capabilities=[io, llm], max_tool_calls=2)` を出す。`wys explain` の `function` に `tool`（`{description, approval}` か `null`）と `agent`（`{tools, capabilities, model, max_llm_calls, max_tool_calls, max_tokens, timeout_ms}` か `null`）を足す（項目の追加なので版は上げない）。JSON Schema の文字列、ツール表、glue は静的な事実ではないので出さない。

## 7. ランタイムの強制（AI-004〜AI-006、AC-007 / AC-008）

### 7.1 agent コンテキスト

ランタイムはコンテキストをスタックで持つ。底は**根のコンテキスト**（ツールなし、Capability は 4 つすべて ∩ `WYS_CAPABILITIES`、予算なし）で、`@agent` の本体に入るたびに 1 つ積む。入れ子の agent は `capabilities` を外側との積に縮め、予算は自分の分を持つが、検査はスタック上のすべての agent に対して行い、使った分もすべてに数える（外側の残りを超えない。PoC4）。ツールの中から `@agent` の関数を呼べば入れ子になり、`llm_generate` を直接呼べば現在のコンテキストで数える。

### 7.2 Capability

名前は Effect と同じ `io` / `exit` / `python` / `llm`（M5 に `fs` / `net` の組み込みはない）。agent の既定は `io, llm`。`WYS_CAPABILITIES=io,llm` は配備時の**縮小**で、広げることはできない（未設定 = 縮小なし、空 = すべて外す、未知の名前は無視 = 縮小の方向）。静的な `uses` ⊆ `capabilities`（E1405）は早期発見で、保証は 5.1 の (b) の検査が担う。防御を重ねるため、`wys_py_*`（`Err(PyError kind="CapabilityError")`）、`wys_exit`（実行時エラー 101）、`wys_llm_*`（`Err(kind="denied")`）の入口でも現在のコンテキストの Capability を確かめ、監査 `capability_denied` を残す。`io`（`print` / `arg`）は入口で検査しない。

### 7.3 拒否の扱い

| 事象 | 扱い |
|---|---|
| 一覧にないツール、Capability の不足 | ツールの本体を動かさず `Err(kind="denied")` で `llm_generate` を中止、監査 `tool_denied` |
| 承認の拒否 | ツールを動かさず `denied by approver` を `is_error` の `tool_result` で LLM に返して続行、監査 `approval_denied` |
| 引数の検証の失敗 | `Err(kind="schema")`、監査 `schema_rejected` |

### 7.4 承認（AI-005）

`approval="required"` のツールは、実行の前に `WYS_APPROVAL` の方針に従う。`deny`（既定、未設定）: 常に拒否。`allow`: 常に許可。`prompt`: stderr に `approve tool <name> (agent <agent>) with args <JSON>? [y/N] ` を出し、stdin から 1 行読む。`y` / `yes` なら許可、それ以外は拒否、EOF なら `Err(kind="approval")`。監査 `approval_requested` → `approval_granted` / `approval_denied`。

### 7.5 予算（AI-006）

| 種類 | 数え方 | 判定 |
|---|---|---|
| `max_llm_calls` | プロバイダ（モックを含む）を呼ぶたびに 1 | 呼ぶ前 |
| `max_tool_calls` | ツールを実行するたびに 1（拒否・承認の拒否は数えない） | 実行の前 |
| `max_tokens` | 応答の `usage.input_tokens + output_tokens` の合計 | 呼ぶ前と、応答の直後 |
| `timeout_ms` | 7.6 | 呼ぶ前、応答の直後、ツールの前 |

費用（`max_cost_usd`）は価格表が要るので M6（proxy の adapter に置く）。

### 7.6 タイムアウト

期限は agent に入ってからの経過時間で、呼び出しの境界（LLM 呼び出しの前と応答の直後、ツールの実行の前）で判定する。超えていれば `Err(kind="timeout")`。実行中のツールや HTTP の待ちは中断しない（タスクのキャンセルは M6。ADR-0020 論点 15）。HTTP の待ち時間は proxy のソケットのタイムアウトと、要求に添える残り時間で切る。時計は単調時計で、`WYS_VIRTUAL_TIME=1` では実時間を数えない。モックの `latency_ms` はどちらのモードでも待たずに時計を進める（Conformance を決定的にする）。

## 8. プロバイダ（ADR-0023）

### 8.1 選択

| `WYS_LLM_PROVIDER` | プロバイダ |
|---|---|
| 未設定 | ランタイム内のモック。`llm_mock_push` の順に消費する |
| `anthropic` | `wys llm-proxy` を子プロセスとして起動する。`wys` の探索順: `WYS_LLM_PROXY` → ビルド時に埋めたコンパイラの絶対パス → `PATH` |
| `cmd:<path>` | `<path>` を引数なしで起動する（シェルを通さない）。同じプロトコル |
| それ以外 | `Err(kind="provider")` |

実プロバイダが設定されているときに `llm_mock_push` を呼ぶと実行時エラー（101。テストの設定が本番で実呼び出しに化けないため）。子プロセスは最初の `llm_generate` で起動し、プロセスに 1 つ、終了時に stdin を閉じて待つ。子が死んだら `Err(kind="provider")`。

### 8.2 JSON 行のプロトコル

要求（ランタイム → プロバイダ、1 行）:

```json
{"v": 1, "model": "", "system": "...", "messages": [...], "tools": [{"name": "add", "description": "...", "input_schema": {...}}],
 "schema": {...}, "max_tokens": 0, "timeout_ms": 0}
```

`schema` は出力スキーマがなければ `null`。`messages` の要素: `{"role":"user","content":str}`、`{"role":"assistant","content":str | <不透明な値>}`（利用者の履歴なら文字列。前の応答の `assistant` ならその値をそのまま）、`{"role":"tool","id":str,"content":str,"is_error":bool}`。`max_tokens` / `timeout_ms` の `0` は「指定なし」。

応答（1 行）:

```json
{"text": "...", "tool_calls": [{"id": "t1", "name": "add", "args": {...}}], "usage": {"input_tokens": 10, "output_tokens": 5}, "stop": "end_turn", "assistant": "<不透明な値>"}
```

失敗なら `{"error": "..."}`。ランタイムは `text` / `tool_calls` / `usage` / `stop` だけを読み、`assistant` は解釈せず次の要求に戻す（Claude Opus 5.5 の thinking のブロックは会話に結び付いていて、編集すると拒否され得る。PoC4）。

### 8.3 `wys llm-proxy`（Anthropic Messages API の adapter）

`ANTHROPIC_API_KEY` を proxy だけが読む。`anthropic-version: 2026-10-21`、既定のモデル `claude-opus-5-5`（`WYS_LLM_MODEL` で変更）、ツールは `tool_choice: {type: "auto", disable_parallel_tool_use: true}`、出力スキーマは `output_config.format`（`json_schema`）、安全分類器の拒否は `fallbacks: "default"`（ヘッダ `server-side-fallback-2026-07-01`）で再実行し、それでも `refusal` なら `error` 行。`max_tokens` の既定は 16000。ソケットのタイムアウトは接続 10 秒、全体 600 秒と要求の `timeout_ms` の小さいほう。HTTP の失敗は `error.type: error.message` だけを `error` 行にし、ヘッダやキーは含めない。proxy は stderr に要求や応答を書かない。

## 9. 監査とプライバシー（NFR-005、AC-007）

| `kind` | `tool` | `detail` |
|---|---|---|
| `llm_called` | `None` | `tokens=N` |
| `tool_called` | 名前 | `args_len=N` |
| `tool_denied` | 名前 | `not in the agent's tools` / `missing capability python` |
| `capability_denied` | 入口の名前（`py_import` など） | `capability python is not granted` |
| `approval_requested` / `approval_granted` / `approval_denied` | 名前 | 方針（`WYS_APPROVAL=deny` など） |
| `budget_exceeded` | 名前か `None` | `max_tool_calls` / `timeout_ms` など |
| `schema_rejected` | 名前か `None` | `<path>: expected <t>, found <k>` |

`agent` は現在の agent の内部名（根なら `""`）。時刻は含めない（決定性）。`WYS_AUDIT=<path>|stderr` で同じ内容を JSON 行として追記する。**プロンプト・応答・ツールの引数と戻り値の本文は監査に含めない**（長さと名前だけ）。M6 のトレースも本文は既定で含めず、明示の opt-in にする。承認の `prompt` は承認者に引数を見せるが、これは対話であって記録ではない。

## 10. 例

```python
type Plan = {title: str, steps: int}

@tool(description="Add two integers.")
def add(a: int, b: int) -> int uses pure:
    return a + b

@agent(tools=[add], capabilities=["llm"], max_tool_calls=2, timeout_ms=30000)
async def plan(topic: str) -> Result[Plan, LlmError] uses llm:
    req = LlmRequest(model="", system="Reply with a plan.", messages=[Message(role="user", content=topic)], max_tokens=0)
    return await llm_generate[Plan](req)

async def main() -> None uses io, llm:
    llm_mock_push(MockResponse(text="", tool_calls=[MockToolCall(name="add", args_json="{\"a\":2,\"b\":3}")], input_tokens=10, output_tokens=5, latency_ms=0))
    llm_mock_push(MockResponse(text="{\"title\":\"plan\",\"steps\":5}", tool_calls=[], input_tokens=20, output_tokens=8, latency_ms=0))
    match await plan("add 2 and 3"):
        case Ok(p):
            print(p.steps)
        case Err(e):
            print(e.kind)
```

（例は `examples/agent_demo.wys` で確定する。）

## 11. 環境変数

| 変数 | 読む側 | 意味 |
|---|---|---|
| `WYS_LLM_PROVIDER` | ランタイム | 未設定 = モック / `anthropic` / `cmd:<path>` |
| `WYS_LLM_PROXY` | ランタイム | `wys` の実行ファイルの場所（任意） |
| `WYS_LLM_MODEL` | ランタイム・proxy | 既定のモデル |
| `WYS_CAPABILITIES` | ランタイム | 配備時の Capability の縮小 |
| `WYS_APPROVAL` | ランタイム | `deny`（既定）/ `allow` / `prompt` |
| `WYS_AUDIT` | ランタイム | 監査イベントの出力先 |
| `WYS_VIRTUAL_TIME` | ランタイム | 実時間を数えない（ADR-0020） |
| `ANTHROPIC_API_KEY` | **proxy だけ** | API キー |

## 12. 初版の制限と M6 以降

- `llm_generate` はブロックする（proxy を待つ間ほかのタスクは進まない）。API は `async` なので M6 で非ブロッキングにしても利用者のコードは変わらない。
- 実行中のツール・HTTP はタイムアウトで中断しない（キャンセルは M6）。
- `llm_generate` の呼び出しをまたぐと thinking のブロックは引き継がれない（利用者の `Message` は文字列だけ）。
- `dict[str, T]` と `Unknown` のスキーマは Anthropic の構造化出力に乗らない（4.2）。
- `python` を付与したツールは Python の内側で何でもできる（サンドボックスではない。ADR-0018 論点 12、ADR-0023 論点 17）。
- 費用の上限、トレース、記録済み応答の再生、ストリーミング、ほかの adapter、`python` の隔離は M6 以降（`docs/plans/m5.md` 範囲外）。

## 13. 用語（用語集に追加する）

属性（コンパイル時のデコレータ）、スキーマ型、ツール表、トランポリン、agent コンテキスト、根のコンテキスト、proxy（`wys llm-proxy`）、JSON 行のプロトコル、配備時の縮小。
