# LSP の設計（M6、ADR-0026）

`wys lsp` は、エディタに診断・型のホバー・定義ジャンプ・quickfix・整形を提供する Language Server Protocol のサーバー（DX-003 の初期版）。決定の理由は ADR-0026、試作の結果は `docs/poc/poc6-results.md`。

## 1. 構成

```text
エディタ（汎用の LSP クライアント）
   │ stdio（JSON-RPC、Content-Length の枠）
   ▼
wys lsp（wys_cli の副コマンド）── wys_lsp::run_stdio ── serve(&Connection)
                                                         │ 開いている文書: URI → (パス, 全文)
                                                         ▼
                              wys_driver（frontend_tests_with、読み込みに文書を重ねる）
```

- クレート `wys_lsp`（`lsp-server` + `lsp-types`）。ランタイムと生成した実行ファイルは依存しない（AC-004）。
- 同期・単一スレッド。解析だけ、256 MiB のスタックの別スレッドで回す（パニックを隔離する。スタックの溢れは隔離できず、プロセスが止まるので、パーサの入れ子の深さの上限で防ぐ。T11）。

## 2. 対応する要求と通知

| メソッド | 状態 | 内容 |
|---|---|---|
| `initialize` / `initialized` | M6 T9 | `positionEncoding`（UTF-8 か UTF-16）、`textDocumentSync: Full` を返す。T11 で `hoverProvider` / `definitionProvider` / `codeActionProvider`（`quickfix`）/ `documentFormattingProvider` を足す |
| `textDocument/didOpen` / `didChange` / `didClose` | M6 T9 | 開いているすべての文書を解析し直し、診断を配る（3 章） |
| `textDocument/publishDiagnostics` | M6 T9 | サーバー → クライアント |
| `textDocument/hover` | M6 T11 | 4 章 |
| `textDocument/definition` | M6 T11 | 5 章 |
| `textDocument/codeAction` | M6 T11 | 診断の `replacement` を quickfix に（6 章） |
| `textDocument/formatting` | M6 T11 | `wys fmt` と同じ整形（全文の置き換え） |
| `shutdown` / `exit` | M6 T9 | `shutdown` なしの `exit` は終了コード 1 |
| それ以外 | — | 要求は `MethodNotFound`、壊れた通知は無視して `window/logMessage` |

## 3. 診断

- 開いている文書をそれぞれエントリとして、`main` を求めない入口で解析する（`main` があれば検査する）。読み込みは開いている文書の全文をディスクより優先する。
- 結果を URI ごとに和集合にして（重複を除く）、変わった URI だけ配り、もう診断のない URI には空を配る。開いている文書は受け取った URI の文字列のまま、import 先はパスから作った `file://` の URI。
- 1 つの診断: `range`（交渉した符号化）、`severity`（エラー 1 / 警告 2）、`code`（`E0201` など）、`source: "wys"`、`message`、`relatedInformation`（T11。診断 JSON の `related` のうち位置のあるもの）。
- 壊れた通知と `file://` でない URI は無視する（`window/logMessage`）。解析が落ちたら、その文書の診断を更新せずに `window/logMessage`。

## 4. ホバー（T11）

位置 `p` を含む最も小さい式（`span.start <= p < span.end`）について、Markdown の 1 つのコードブロックと説明の行を返す（式でない名前の規則は ADR-0026 論点 10）。型の表記は `wys explain` と同じ。キーワード・空白・コメント・文字列の中・型注釈の上は `null`。

| 位置 | 表示 |
|---|---|
| 式 | 絞り込み後の型（例 `int`、`Plan`、`list[str]`） |
| ローカル変数 | `name: 宣言の型`。絞り込まれていれば `（ここでは T）` |
| 関数の名前・呼び出し | 署名（内部名で `def a.b.f(x: int) -> str uses io`。推論した Effect は `uses(inferred) io`）、`pub` / `async` / `@tool` / `@agent`。組み込みの関数は呼び出しの型と Effect、メソッドは結果の型だけ |
| レコードのフィールド | フィールドの型 |

型が `Error` の式には出さない。エラーのあるファイルでも、構文エラーと名前解決のエラーがなければ型検査まで進めた結果（`Frontend.analysis`）を使う（ADR-0026 論点 9）。import 先の構文が壊れている間は `null`。

## 5. 定義ジャンプ（T11）

関数（`m.f` を含む）→ 名前の位置（モジュールの関数は import 先のファイル）、ローカル変数（`case` の束縛を含む）→ 宣言、型 → `type` の定義、`import m` / `import m as n` → モジュールのファイルの先頭、`from m import f` / `f as g` → `f` の定義、`@agent(tools=[...])` の関数名 → その関数。組み込みの関数・組み込みのレコード・型パラメータには定義がない（空の応答）。

## 6. quickfix（T11）

診断の `hints` のうち `replacement` のあるものごとに `quickfix` を作る（いまは E0103 / E0104 / E1102 / W0203）。修正は配る診断の `data` に入れ（`{fixes: [{title, uri, range, text}]}`）、`codeAction` はクライアントが送り返す `context.diagnostics[].data` から `WorkspaceEdit` を作る。編集先の URI は `replacement` の位置のファイル。CLI の修正案（`check --format json` の `hints[].replacement`）と同じ。

## 7. 検査

| 層 | 内容 |
|---|---|
| 単体テスト | 位置の変換、`diagnostics`、URI とパス、`serve` を `Connection::memory()` で（同じモジュールを import する文書、`didClose`、壊れた通知、`exit`） |
| Conformance（T11） | `tests/conformance/lsp/<ケース>/main.wys`。`# expect-hover: 行:列 <コードブロックの 1 行目>`、`# expect-definition: 行:列 -> ファイル:行:列`（ファイルはケースのディレクトリからの相対。行・列は 1 始まり、列は Unicode スカラー値）。実行せず、LSP と同じ入口で解析だけ行う |
| e2e | `wys lsp` を子プロセスにして JSON-RPC の全手順（`lsp_publishes_diagnostics_over_json_rpc`） |

遅延の計測は `benches/lsp_latency.py`（絶対値だけ）。

## 8. クライアントの設定例

`cargo build --release` の後:

Neovim 0.11（組み込みのクライアント。`~/.config/nvim/init.lua`）:

```lua
vim.filetype.add({ extension = { wys = "wys" } })
vim.lsp.config("wys", { cmd = { "/path/to/new_py/target/release/wys", "lsp" }, filetypes = { "wys" }, root_markers = { ".git" } })
vim.lsp.enable("wys")
```

VS Code: 任意の言語サーバーを設定で起動できる汎用の拡張に、コマンド `/path/to/new_py/target/release/wys lsp` と、言語 `wys`（拡張子 `.wys`）を設定する。

## 9. 制限と今後

補完・リネーム・参照検索・シグネチャのヘルプ・セマンティックトークン・増分の解析・エディタの拡張は公開準備。Windows は CI の対象外。壊れた枠（`Content-Length` の中身が JSON でない）でサーバーが止まる（`lsp-server` の制限）。同じファイルを別のパスで開いたときの重ね合わせは T11 でパスを正規化して扱う。
