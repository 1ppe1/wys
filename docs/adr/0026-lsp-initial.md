# ADR-0026: LSP の初期版

- **状態:** Proposed
- **日付:** 2026-11-07
- **関連:** DX-003、DX-001、DX-004、AC-004、ADR-0017（`wys explain`）、ADR-0024（推論した Effect）、`docs/plans/m6.md` T9〜T11、`docs/poc/poc6-results.md`、`docs/design/lsp.md`

## 背景

要件定義書の DX-003 は「Language Server Protocol 対応、補完、定義ジャンプ、型ホバー」を求める（Should）。M6 の計画（`docs/plans/m6.md`）は、M6b で LSP の初期版を作り、範囲を診断・型のホバー・定義ジャンプ・quickfix・整形にし、補完・リネーム・参照検索は公開準備に送ると決めた。

PoC6（T9、`docs/poc/poc6-results.md`）で、次のことが分かった。

- `lsp-server` 0.7.9 + `lsp-types` 0.97.0 は、ツールチェーン（Rust 1.99.1、edition 2024、`unsafe_code = deny`）と問題なく合う。
- 変更のたびにフロントエンドを全部回しても、いまの大きさのファイルなら 1 ms に届かない（最大の例で中央値 0.14 ms）。
- 開いているバッファを `frontend_with` の読み込みの閉包に重ねるだけで、import 先の未保存の変更も反映できる。
- 位置は UTF-8 を交渉でき、受け付けないクライアントには UTF-16 に変換できる。
- レビューで、サーバーの方針として次の 4 つが要ると分かった: 壊れた通知や `file://` でない URI で止まらない、`main` を求めない、開いているすべての文書を解析し直して URI ごとに和集合を配る、フロントエンドのパニックとスタックの溢れを隔離する。

いまのフロントエンドは、型検査の結果（`Frontend.typeck`）をエラーがないときだけ返す。ホバーと定義ジャンプは、ほかの場所にエラーがあるファイル（編集の途中のほとんど）でも動かなければならない。

## 選択肢と決定

| 論点 | 決定（推奨） | 主な代替案 | 理由 |
|---|---|---|---|
| 1. 範囲 | 診断（`publishDiagnostics`）、型のホバー（`hover`）、定義ジャンプ（`definition`）、quickfix（`codeAction`）、整形（`formatting`）。補完・リネーム・参照検索・シグネチャのヘルプ・セマンティックトークン・増分の解析・エディタの拡張は公開準備 | 補完（予約語と組み込みだけ）も入れる | 計画どおり。補完は名前解決とスコープの API が要り、範囲が倍になる。DX-003 の「補完」は公開準備で判定する |
| 2. クレートと transport | 別クレート `wys_lsp` と副コマンド `wys lsp`。stdio、同期、単一スレッド（解析だけ大きなスタックの別スレッド）。`lsp-server` + `lsp-types` | (a) `tower-lsp`（tokio）。(b) JSON-RPC を手書き | PoC6 で動いた。(a) は非同期の実行時と依存が重い。(b) は枠の読み書きと型の定義を自分で持つことになる。代償として、壊れた枠（`Content-Length` の中身が JSON でない）で読み取りが止まる（仕様の `-32700` で続けられない）のを受け入れる |
| 3. 依存 | `lsp-server` / `lsp-types` は `wys_lsp` と `wys_cli` だけに入る。ランタイムと生成した実行ファイルは変わらない（AC-004） | — | `cargo tree -p wys_runtime` で確かめる（PoC6 の基準 4） |
| 4. 解析の単位と時期 | `didOpen` / `didChange` / `didClose` のたびに、開いているすべての文書をエントリとして解析し直す。同期は全文（`TextDocumentSyncKind::FULL`）。増分の解析はしない | (a) 変わった文書だけ。(b) 依存グラフを持って影響のある文書だけ | (a) は import 先を変えても import する文書が古くなる（PoC6 のレビュー）。(b) は正しいが、いまの速さでは要らない。見直し条件に入れる |
| 5. 重ね合わせ | 開いている文書の全文を、`frontend_with` の読み込みでディスクより優先する。パスは T11 で正規化（`std::fs::canonicalize` が通れば）してから突き合わせる | 正規化しない | シンボリックリンクや macOS の `/tmp` と `/private/tmp` で重ね合わせが外れる（PoC6 の制限）。正規化できないパス（未保存の新しいファイル）はそのまま |
| 6. `main` | `main` を求めない入口（`wys test` と同じ `frontend_tests_with`）で解析する。`main` があれば検査する | `wys check` と同じく求める | エディタで開くファイルの多くは `main` のないモジュール |
| 7. 診断の配り方 | 解析の結果を URI ごとに和集合にし（`(range, code, message)` で重複を除く）、前に配った診断と変わった URI だけ配り、もう診断のない URI には空を配る。開いている文書は受け取った URI の文字列のまま、開いていない import 先はパスから作った `file://` の URI で配る。`code` はエラーコード、`severity` はエラー / 警告、`source` は `wys`、`relatedInformation` は診断の `related`（位置のあるものだけ） | エントリごとに帳簿を持つ | エントリごとだと、同じモジュールを import する文書を閉じたときに消し合う（PoC6 のレビュー） |
| 8. 位置の符号化 | `initialize` でクライアントが `utf-8` を申し出れば UTF-8、なければ UTF-16（LSP 3.17 の `positionEncoding`）。内部はバイト位置（`Span`）のまま、出入りで変換する | 常に UTF-16 | PoC6 で両方を確かめた |
| 9. 型検査の結果をエラーがあっても使う | フロントエンドに、エラーがあっても型検査の結果（`TypeckResult`）を返す入口を足す（`Frontend.typeck` はいまどおりエラーがないときだけ。LSP 用の別の欄 `analysis` に、構文エラーがなければ型検査まで進めた結果を入れる）。ホバーと定義ジャンプはその結果を使い、型が `Error` の式には型を出さない | エラーのない最後の結果を使い回す | 編集の途中のファイルでもホバーが動く。最後の結果を使い回すと、位置がずれて別の式の型を出す |
| 10. ホバー | 位置にある最も内側の式の型（`expr_types`。絞り込み後の型）、ローカル変数の宣言の型と、その位置での絞り込み（`FuncInfo.narrowings`）、関数の署名と Effect（宣言、または推論した集合。ADR-0024）と `@tool` / `@agent` の属性、レコードのフィールドの型。型の表記は `wys explain` と同じ（ADR-0017）。Markdown の 1 つのコードブロックと、短い説明の行 | 型だけ | AI と人の両方が、`wys explain` と同じ言葉で読める |
| 11. 定義ジャンプ | 関数の呼び出し → その関数の名前（`FuncSig.span`。モジュールの関数は import 先のファイル）、ローカル変数 → 宣言（`FuncInfo.local_spans`）、型の名前 → `type` の定義、`import m` → そのモジュールのファイルの先頭。組み込みの関数と型には定義がない（空の応答） | 参照検索も一緒に | 参照検索は公開準備（論点 1） |
| 12. quickfix | 診断の `hints` のうち `replacement` のあるものを、その診断に結び付いた `quickfix` の code action にする（1 つの診断に 1 つ。E1102 の `uses` の修正、W0201 / W0203 など）。`replacement` のないヒントは出さない | 修正案を LSP 側で別に作る | 診断 JSON の修正案（DX-002）をそのまま使えば、CLI と LSP で修正が食い違わない |
| 13. 整形 | `textDocument/formatting` で `wys_fmt::format_source` を全文に適用し、1 つの編集（全文の置き換え）で返す。字句エラーがあれば編集なし。範囲の整形はしない | 差分の編集を返す | 実装が小さい。エディタは全文の置き換えでもカーソルを保つ |
| 14. 失敗の扱い | 壊れた通知・要求、`file://` でない URI（`untitled:`、`git:` など）、知らない要求はサーバーを止めない（通知は無視するか `window/logMessage`、要求は `InvalidParams` / `MethodNotFound`）。解析は 256 MiB のスタックの別スレッドで回し、パニックやスタックの溢れは「その文書の診断を更新しない」＋ `window/logMessage`。`shutdown` なしの `exit` は終了コード 1 | — | PoC6 のレビュー。エディタは入力途中の全状態を送るので、コンパイラの弱点が桁違いに露出する |
| 15. 検査 | 3 層: (a) 純粋な関数の単体テスト（`hover_at` / `definition_at` / 位置の変換）。(b) Conformance の `tests/conformance/lsp/` に新しいキー `# expect-hover: 行:列 <文字列>` と `# expect-definition: 行:列 -> ファイル:行:列`（harness が `wys_lsp` の純粋な関数を呼ぶ）。(c) `serve(&Connection)` を `Connection::memory()` で動かす単体テストと、`wys lsp` を子プロセスにして JSON-RPC を話す e2e。CI はエディタを使わない | e2e だけ | 位置の期待値をケースのファイルに書けば、Conformance と同じく AI が読み書きしやすい |
| 16. エディタ | エディタの拡張は作らない。汎用の LSP クライアントの設定例（Neovim の組み込みクライアント、VS Code の汎用クライアントの拡張）を `docs/design/lsp.md` に書く。ユーザーの手元のエディタでの確認は、PoC6 の基準 2 と T11 の終わり | VS Code の拡張を作る | 公開の前に作る（拡張の配布と名前は公開準備で決める） |
| 17. 見直し条件 | 補完を入れるとき、ファイルが大きくなって全体の再解析が遅くなったとき（変更から診断まで 50 ms を超える）、マルチルートのワークスペース、Windows を対象にするとき、`lsp-server` の代替（`-32700` の扱い）が要るとき | — | — |

## 影響

- 実装は T11（`docs/plans/m6.md`）: フロントエンドの `analysis`（論点 9）、ホバー・定義・quickfix・整形、パスの正規化（論点 5）、`relatedInformation`、harness の新しいキーと `tests/conformance/lsp/`、e2e の全手順、`docs/design/lsp.md` の更新。
- 新しいエラーコードはない。`wys explain` の出力は変えない。
- 更新する文書: `docs/design/lsp.md`（新規）、用語集（quickfix、位置の符号化）、`docs/test-plan.md`（DX-003 の行）、`docs/README.md`（`lsp.md`）、CLAUDE.md（`wys lsp`）、ADR 一覧。
- 制限: 補完などは公開準備。Windows は CI の対象外。壊れた枠でサーバーが止まる（論点 2）。パーサに入れ子の深さの上限がない（`wys check` も深い入れ子で落ちる。LSP は論点 14 で隔離するが、パーサの上限は別の作業）。
