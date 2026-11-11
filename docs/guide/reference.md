# リファレンス（v0.1）

本言語の利用者向けの要約。正確な規則は、文法 `docs/spec/grammar.ebnf`、各 ADR（`docs/adr/`）、AI SDK の設計 `docs/design/ai-sdk.md` にある。0.y の間、構文と組み込みは変わり得る（ADR-0029）。

## 1. コマンド

| コマンド | 内容 |
|---|---|
| `wys check [--format text\|json] <file>` | 型を検査して診断を出す。`json` は機械向け（`docs/spec/diagnostics-schema.json`） |
| `wys build <file> [-o <out>] [--release]` | Native の実行ファイルを作る（`--release` で -O2） |
| `wys run <file> [-- <args>...]` | ビルドして実行する |
| `wys test <file>` | `test_` で始まる関数を 1 つずつ別のプロセスで実行する |
| `wys fmt [--check] <file>` | 整形する |
| `wys explain <file \| CODE>` | 型・絞り込み・参照カウントの操作の理由（JSON）か、エラーコードの説明 |
| `wys eval [--record] [--format text\|json] <dir>...` | AI の評価ケースを記録の再生で実行し、成功率を出す |
| `wys lsp` | Language Server（診断・ホバー・定義ジャンプ・quickfix・整形。`docs/design/lsp.md`） |
| `wys llm-proxy` | 実プロバイダ（Anthropic）への proxy（ランタイムが起動する） |
| `wys dump-ir` / `wys dump-llvm <file>` | 中間表現と LLVM IR を出す |
| `wys --version`、`wys version [--format json]` | 版・コミット・target・rustc |

## 2. 型

| 型 | 例 | メモ |
|---|---|---|
| `int` | `42` | 64 ビット。溢れは実行時エラー |
| `float` | `3.5` | IEEE 754 の倍精度 |
| `bool` | `True`、`False` | |
| `str` | `"text"` | Unicode |
| `None` | `None` | |
| `T \| U`、`T \| None` | `int \| None` | Union。`is None` / `match` で絞り込む |
| `list[T]` | `[1, 2]` | 可変。添字は負なら末尾から |
| `dict[K, V]` | `{"a": 1}` | 反復は挿入の順 |
| レコード | `type Point = {x: int, y: int}` | 構造的な型（ADR-0007）。`Point(x=1, y=2)` で作る |
| `Result[T, E]` | `Ok(1)`、`Err("e")` | `?` で伝搬、`match` で取り出す |
| `Unknown` | `parse_json(s)` の結果 | 検証（`match`、`validate[T]`）するまで使えない |
| `Task[T]` | `spawn(f())` | `await` で結果を待つ |
| `PyObject` | `py_import("math")` | Python のオブジェクト |
| Generic | `def first[T](xs: list[T]) -> T` | 単相化する |

## 3. 主な組み込み

| 関数 | 型 | Effect |
|---|---|---|
| `print(x)` | 値を 1 行に出す | `io` |
| `len(x)` | `str` / `list` / `dict` の長さ | なし |
| `range(n)`、`range(a, b)` | `for` で使う | なし |
| `assert(cond)` | 偽なら実行時エラー。後ろの文では条件を真として絞り込む | なし |
| `exit(code)`、`panic(msg)` | 終了する | `exit` / なし |
| `arg_count()`、`arg(i)` | コマンドライン引数 | なし |
| `to_json(x)` | 値を JSON の文字列にする（`int` などを文字列にするのにも使う） | なし |
| `parse_json(s)` | `Result[Unknown, JsonError]` | なし |
| `validate[T](u)` | `Result[T, SchemaError]` | なし |
| `sleep_ms(ms)`、`spawn(task)` | 非同期（`await` する） | なし |
| `llm_generate[T](req)` | `Result[T, LlmError]`（`await` する） | `llm` |
| `llm_usage()`、`audit_events()` | 使用量、監査イベント | なし |
| `llm_mock_push(resp)` | モックの応答を積む（テスト用） | なし |
| `py_import(name)`、`py_value(x)` | Python との連携 | `python` |

AI SDK の型（`LlmRequest`、`Message`、`LlmError`、`SchemaError`、`AuditEvent`、`MockResponse` など）は `docs/design/ai-sdk.md` 2 章。

## 4. 環境変数

| 変数 | 内容 |
|---|---|
| `WYS_LLM_PROVIDER` | 未設定 = モック / `anthropic` / `cmd:<path>` / `replay:<path>` |
| `WYS_LLM_MODEL`、`WYS_LLM_PROXY` | 既定のモデル、proxy（`wys` の実行ファイル）の場所 |
| `WYS_CAPABILITIES` | 実行時の Capability を縮める（例 `io,llm`） |
| `WYS_APPROVAL` | `deny`（既定）/ `allow` / `prompt` |
| `WYS_AUDIT`、`WYS_TRACE`、`WYS_TRACE_CONTENT` | 監査とトレースの出力先（`<path>` か `stderr`）、トレースに本文を含める |
| `WYS_LLM_RECORD` | 要求と応答の記録先（本文を含む。0600） |
| `WYS_VIRTUAL_TIME` | `1` で `sleep_ms` を仮想の時計で進める（テスト用） |
| `WYS_PYTHON` | Python を使うプログラムのビルドに使う Python（既定 `python3.14`） |
| `WYS_RUNTIME_LIB`、`WYS_RUNTIME_PYTHON_LIB` | ランタイムのライブラリの場所（既定は実行ファイルの隣か `../lib`） |
| `WYS_SANITIZE=address`、`WYS_RC_STATS=1`、`WYS_ARC_OPT=0` | 開発用: AddressSanitizer、参照カウントの操作の回数、Move の最適化を止める |
| `ANTHROPIC_API_KEY` | proxy だけが読む |

0.y の間、変数の名前は変わり得る（ADR-0027 の改名で接頭辞が変わる）。

## 5. 診断とエラーコード

- エラー（`E0201` など）と警告（`W0201` など）の一覧と説明: `docs/spec/error-codes.md`、`wys explain <CODE>`。
- コードの意味は変えず、再利用しない（ADR-0029）。
- `wys check --format json` の診断には、位置（`span`）、原因（`cause`）、修正案（`hints`。置き換えのあるものは LSP の quickfix になる）が入る。
