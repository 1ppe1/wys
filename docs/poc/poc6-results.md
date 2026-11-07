# PoC6: LSP のスパイク（M6 T9）

- **状態:** 成功基準 1・3・4・5 を満たした。**基準 2（ユーザーの手元のエディタでの確認）はユーザーの確認待ち**（下の「基準 2 の手順」）。転換基準には当たらない
- **期限:** 3 営業日（`docs/plans/m6.md`「PoC6 の判定」、2026-11-12 まで）。着手 2026-11-07
- **コード:** `crates/wys_lsp`（新しいクレート。`lsp-server` 0.7.9 + `lsp-types` 0.97.0）、`wys lsp`（`crates/wys_cli/src/main.rs`）、`wys_span::SourceMap::file_at`。T10 / T11 で本実装に育てる（この PR は捨てずにマージする）

## 構成

- `wys lsp` は stdio で JSON-RPC を話す（同期・単一スレッド）。`initialize` で位置の符号化を交渉し、クライアントが `utf-8` を受け付ければ UTF-8、そうでなければ UTF-16 にする（LSP 3.17 の `positionEncoding`）。同期は全文（`TextDocumentSyncKind::FULL`）。
- `didOpen` / `didChange` / `didClose` のたびに、**開いているすべての文書**をエントリとして解析し直す（開いているバッファを読み込みに重ね、import 先もディスクより優先）。診断は URI ごとに和集合にして（同じモジュールを import する文書があっても消し合わない）、変わった URI だけ `textDocument/publishDiagnostics` で配り、もう診断のない URI には空を配る。開いている文書は、クライアントから受け取った URI の文字列のまま配る。
- エディタで開くファイルの多くは `main` のないモジュールなので、`main` を求めない入口（`wys test` と同じ `frontend_tests_with`）で解析する。`main` があれば検査する。
- 解析は 256 MiB のスタックの別スレッドで回す。フロントエンドがパニックしてもサーバーは落ちず、`window/logMessage` で知らせる。スタックの溢れは隔離できない（Rust はプロセス全体を止める）。大きなスタックで溢れる深さを引き上げただけで（500 段の入れ子の括弧は通る）、それより深い入力ではサーバーが止まる。パーサに入れ子の深さの上限を入れる（T11、ADR-0026 論点 14）。
- 壊れた通知と `file://` でない URI（`untitled:`、`git:` など）はサーバーを止めず、無視するか `window/logMessage` で知らせる。`shutdown` なしの `exit` は終了コード 1（LSP の仕様）。
- 診断の `code` はエラーコード（`E0201` など）、`severity` はエラー / 警告、`source` は `wys`。ほかの要求（ホバー・定義など）は PoC6 では `MethodNotFound` で答える（T11）。
- サーバーは `serve(&Connection)` で transport から切り離してあり（`wys lsp` の `run_stdio` はその薄い皮）、`Connection::memory()` の単体テストで、同じモジュールを import する 2 つの文書と `didClose`、`file://` でない URI と壊れた通知、`shutdown` なしの `exit` を確かめる。ほかに、位置の変換、`main` のないモジュール、深い入れ子、URI とパスの変換の単体テスト、JSON-RPC の全体は e2e の `lsp_publishes_diagnostics_over_json_rpc`（子プロセス、ネットワークなし）。

## 成功基準の判定

| 基準 | 結果 |
|---|---|
| 1. stdio で `initialize` / `initialized` / `didOpen` / `didChange` / `didClose` / `shutdown` / `exit` を扱い、E0201 が `code` / `range` / `message` 付きで出る。import 先の診断はそのファイルの URI に配られる | 成功（e2e と単体テスト） |
| 2. ユーザーの手元のエディタ 1 つで 1 が見える | **ユーザーの確認待ち** |
| 3. JSON-RPC を話す e2e が CI で通る | 成功（`./scripts/ci.sh`、Linux の Docker でも） |
| 4. `cargo tree -p wys_runtime` が変わらない | 成功（`main` と比べてパス以外の違いなし。依存は `wys_lsp` / `wys_cli` だけ） |
| 5. 計測と記録 | 下の表 |

記録（絶対値。Apple M5 Pro、macOS 26.4.1、`wys` の Release ビルド。`RUNS=50 benches/lsp_latency.py target/release/wys <file>`: 型エラーのある関数を足す・外す `didChange` を交互に 50 回送り、その文書の `publishDiagnostics` が届くまでの時間。解析を別スレッドで回す分を含む）:

| ファイル | 大きさ | 最速 | 中央値 | p95 |
|---|---|---|---|---|
| `examples/agent_demo.wys`（例の中で最大） | 2,500 バイト | 0.10 ms | 0.14 ms | 0.27 ms |
| `tests/conformance/modules/E1404_tool_base_name_twice/main.wys`（`modules/` で最大のケース。import あり） | 651 バイト | 0.19 ms | 0.21 ms | 0.28 ms |

- 位置の符号化: e2e のクライアントは `utf-8` を申し出て、サーバーは `utf-8` を選んだ。申し出がなければ UTF-16 に変換する（単体テストで、「あい」が UTF-8 で 6 バイト、UTF-16 で 2 単位になることを確かめた）。
- クレート: `lsp-server` 0.7.9、`lsp-types` 0.97.0。ツールチェーン（Rust 1.99.1、edition 2024、`unsafe_code = deny`）と問題なく合った（転換基準 (a) には当たらない）。

## 分かったこと（T10 の ADR-0026 と `docs/design/lsp.md` に反映する）

- 変更のたびに全体を回しても、いまのファイルの大きさでは 1 ms に届かない。増分の解析は公開準備まで要らない。
- `frontend_with` の読み込みの閉包に開いているバッファを重ねるだけで、import 先の未保存の変更も診断に反映できる（単体テスト `open_buffers_override_imported_files`）。
- 診断の位置は `SourceMap` の通しのバイト位置から、ファイルと行の先頭を引いて LSP の位置にできる（`SourceMap::file_at` を足した）。

## レビューで直したこと（Fable、2026-11-07）

- 壊れた通知や `file://` でない URI で、サーバーが終了していた → 無視して続ける。
- `main` のないモジュールを開くと、いつも E0210 が出ていた → `main` を求めない入口にした。
- 同じモジュールを import する 2 つの文書の片方を閉じると、もう片方が import している先の診断まで消えていた。モジュールの側を変えても、それを import する文書が解析し直されなかった → 開いているすべての文書を解析し直し、URI ごとの和集合にした。
- フロントエンドのパニックとスタックの溢れ（500 段の入れ子の括弧）でサーバーが落ちていた → パニックは別スレッドで隔離し、スタックは 256 MiB にして 500 段を通るようにした。スタックの溢れそのものは隔離できない（Codex のレビュー）ので、パーサに入れ子の深さの上限を入れる（T11。`wys check` も同じ入力で落ちる）。
- `file://localhost/`、クエリとフラグメント、Windows のドライブ文字の URI を受け付けるようにした。

## 制限（T10 の ADR-0026 で扱う）

- 同じファイルを別の綴り（シンボリックリンク、macOS の `/tmp` と `/private/tmp`）で開くと、重ね合わせが外れてディスクの内容を読む。パスの正規化は T11。
- Windows は CI の対象外（URI の変換は入れたが、確かめていない）。
- `lsp-server` は、壊れた枠（`Content-Length` の中身が JSON でない）で読み取りのスレッドが止まり、サーバーが終わる（仕様の `-32700` で続ける口がない）。`lsp-server` を選ぶ代償として受け入れる。

## 基準 2 の手順（ユーザー）

`cargo build --release` の後、エディタの汎用 LSP クライアントで `target/release/wys lsp` を `.wys` のファイルに結び付ける。例（Neovim 0.11 の組み込みクライアント。`~/.config/nvim/init.lua`）:

```lua
vim.filetype.add({ extension = { wys = "wys" } })
vim.lsp.config("wys", { cmd = { "/path/to/new_py/target/release/wys", "lsp" }, filetypes = { "wys" }, root_markers = { ".git" } })
vim.lsp.enable("wys")
```

`examples/type_error.wys` を開き、E0201 の診断（下線と `E0201` のメッセージ）が見えれば成功。VS Code では、汎用クライアントの拡張（任意の言語サーバーを設定で起動できるもの）に同じコマンドを設定する。
