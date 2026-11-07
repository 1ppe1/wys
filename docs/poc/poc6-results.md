# PoC6: LSP のスパイク（M6 T9）

- **状態:** 成功基準 1・3・4・5 を満たした。**基準 2（ユーザーの手元のエディタでの確認）はユーザーの確認待ち**（下の「基準 2 の手順」）。転換基準には当たらない
- **期限:** 3 営業日（`docs/plans/m6.md`「PoC6 の判定」、2026-11-12 まで）。着手 2026-11-07
- **コード:** `crates/wys_lsp`（新しいクレート。`lsp-server` 0.7.9 + `lsp-types` 0.97.0）、`wys lsp`（`crates/wys_cli/src/main.rs`）、`wys_span::SourceMap::file_at`。T10 / T11 で本実装に育てる（この PR は捨てずにマージする）

## 構成

- `wys lsp` は stdio で JSON-RPC を話す（同期・単一スレッド）。`initialize` で位置の符号化を交渉し、クライアントが `utf-8` を受け付ければ UTF-8、そうでなければ UTF-16 にする（LSP 3.17 の `positionEncoding`）。同期は全文（`TextDocumentSyncKind::FULL`）。
- `didOpen` / `didChange` のたびに、開いているバッファを `wys_driver::frontend_with` の読み込みに重ねて（import 先もディスクより優先）フロントエンドを全部回し、診断をファイルごとに `textDocument/publishDiagnostics` で配る（import 先の診断はそのファイルの URI に）。前に配った URI で診断が消えたら空で配り直す。`didClose` で閉じたファイルの診断を消す。
- 診断の `code` はエラーコード（`E0201` など）、`severity` はエラー / 警告、`source` は `wys`。ほかの要求（ホバー・定義など）は PoC6 では `MethodNotFound` で答える（T11）。
- 純粋な部分（`wys_lsp::diagnostics`、`lsp_position`、URI とパスの変換）は単体テスト、JSON-RPC の全体は e2e の `lsp_publishes_diagnostics_over_json_rpc`（子プロセス、ネットワークなし。initialize → didOpen → 2 つの URI への publishDiagnostics → didChange で空の診断 → shutdown → exit）。

## 成功基準の判定

| 基準 | 結果 |
|---|---|
| 1. stdio で `initialize` / `initialized` / `didOpen` / `didChange` / `didClose` / `shutdown` / `exit` を扱い、E0201 が `code` / `range` / `message` 付きで出る。import 先の診断はそのファイルの URI に配られる | 成功（e2e と単体テスト） |
| 2. ユーザーの手元のエディタ 1 つで 1 が見える | **ユーザーの確認待ち** |
| 3. JSON-RPC を話す e2e が CI で通る | 成功（`./scripts/ci.sh`、Linux の Docker でも） |
| 4. `cargo tree -p wys_runtime` が変わらない | 成功（`main` と比べてパス以外の違いなし。依存は `wys_lsp` / `wys_cli` だけ） |
| 5. 計測と記録 | 下の表 |

記録（絶対値。Apple M5 Pro、macOS 25.4、`wys` の Release ビルド。同じ内容の `didChange` を 20 回送り、`publishDiagnostics` が届くまでの最速値）:

| ファイル | 大きさ | `didChange` → `publishDiagnostics` |
|---|---|---|
| `examples/agent_demo.wys`（例の中で最大） | 2,320 バイト | 0.20 ms |
| `tests/conformance/modules/E1404_tool_base_name_twice/main.wys`（`modules/` で最大のケース。import あり） | 565 バイト | 0.08 ms |

- 位置の符号化: e2e のクライアントは `utf-8` を申し出て、サーバーは `utf-8` を選んだ。申し出がなければ UTF-16 に変換する（単体テストで、「あい」が UTF-8 で 6 バイト、UTF-16 で 2 単位になることを確かめた）。
- クレート: `lsp-server` 0.7.9、`lsp-types` 0.97.0。ツールチェーン（Rust 1.99.1、edition 2024、`unsafe_code = deny`）と問題なく合った（転換基準 (a) には当たらない）。

## 分かったこと（T10 の ADR-0026 と `docs/design/lsp.md` に反映する）

- 変更のたびに全体を回しても、いまのファイルの大きさでは 1 ms に届かない。増分の解析は公開準備まで要らない。
- `frontend_with` の読み込みの閉包に開いているバッファを重ねるだけで、import 先の未保存の変更も診断に反映できる（単体テスト `open_buffers_override_imported_files`）。
- 診断の位置は `SourceMap` の通しのバイト位置から、ファイルと行の先頭を引いて LSP の位置にできる（`SourceMap::file_at` を足した）。

## 基準 2 の手順（ユーザー）

`cargo build --release` の後、エディタの汎用 LSP クライアントで `target/release/wys lsp` を `.wys` のファイルに結び付ける。例（Neovim 0.11 の組み込みクライアント。`~/.config/nvim/init.lua`）:

```lua
vim.filetype.add({ extension = { wys = "wys" } })
vim.lsp.config("wys", { cmd = { "/path/to/new_py/target/release/wys", "lsp" }, filetypes = { "wys" }, root_markers = { ".git" } })
vim.lsp.enable("wys")
```

`examples/type_error.wys` を開き、E0201 の診断（下線と `E0201` のメッセージ）が見えれば成功。VS Code では、汎用クライアントの拡張（任意の言語サーバーを設定で起動できるもの）に同じコマンドを設定する。
