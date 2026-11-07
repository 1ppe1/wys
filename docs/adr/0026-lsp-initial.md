# ADR-0026: LSP の初期版

- **状態:** Accepted
- **日付:** 2026-11-07
- **関連:** DX-003、DX-001、DX-004、AC-004、ADR-0017（`wys explain`）、ADR-0024（推論した Effect）、`docs/plans/m6.md` T9〜T11、`docs/poc/poc6-results.md`、`docs/design/lsp.md`

## 背景

要件定義書の DX-003 は「Language Server Protocol 対応、補完、定義ジャンプ、型ホバー」を求める（Should）。M6 の計画（`docs/plans/m6.md`）は、M6b で LSP の初期版を作り、範囲を診断・型のホバー・定義ジャンプ・quickfix・整形にし、補完・リネーム・参照検索は公開準備に送ると決めた。

PoC6（T9、`docs/poc/poc6-results.md`）で、次のことが分かった。

- `lsp-server` 0.7.9 + `lsp-types` 0.97.0 は、ツールチェーン（Rust 1.99.1、edition 2024、`unsafe_code = deny`）と問題なく合う。
- 変更のたびにフロントエンドを全部回しても、いまの大きさのファイルなら 1 ms に届かない（最大の例で中央値 0.14 ms）。
- 開いているバッファを `frontend_with` の読み込みの閉包に重ねるだけで、import 先の未保存の変更も反映できる。
- 位置は UTF-8 を交渉でき、受け付けないクライアントには UTF-16 に変換できる。
- レビューで、サーバーの方針として次の 4 つが要ると分かった: 壊れた通知や `file://` でない URI で止まらない、`main` を求めない、開いているすべての文書を解析し直して URI ごとに和集合を配る、フロントエンドのパニックを隔離する（スタックの溢れは隔離できないので、パーサに深さの上限が要る）。

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
| 7. 診断の配り方 | 解析の結果を URI ごとに和集合にし（LSP の診断の全項目が等しいものを重複として除く）、前に配った診断と変わった URI だけ配り、もう診断のない URI には空を配る。開いている文書は受け取った URI の文字列のまま、開いていない import 先はパスから作った `file://` の URI で配る。`code` はエラーコード、`severity` はエラー / 警告、`source` は `wys`、`relatedInformation` は診断の `related`（位置のあるものだけ。開いていないファイルの URI はパスから作る） | エントリごとに帳簿を持つ | エントリごとだと、同じモジュールを import する文書を閉じたときに消し合う（PoC6 のレビュー） |
| 8. 位置の符号化 | `initialize` でクライアントが `utf-8` を申し出れば UTF-8、なければ UTF-16（LSP 3.17 の `positionEncoding`）。内部はバイト位置（`Span`）のまま、出入りで変換する。要求の位置（LSP → バイト）は、行末を超える `character` を行末に、文字の途中（UTF-16 の代理ペアの間、UTF-8 の多バイト文字の途中）を文字の先頭に丸める | 常に UTF-16 | PoC6 で両方を確かめた |
| 9. 型検査の結果をエラーがあっても使う | `Frontend` に LSP 用の欄 `analysis: Option<Analysis>`（`Analysis { module: wys_ast::Module, typeck: TypeckResult }`。位置から式を引くには AST が要る）を足す。エントリと import 先のすべてに構文エラーと名前解決のエラー（E07xx）がなければ、`check_program` を呼んだすべての経路（型のエラーあり・テスト関数なし・成功）で `Some` にする（`Frontend.typeck` はいまどおりエラーがないときだけ）。ホバーと定義ジャンプはこれを使い、型が `Error` の式には型を出さない。import 先の構文が壊れている間は、それを import する文書のホバー・定義は `null` になる（制限） | エラーのない最後の結果を使い回す | 編集の途中のファイルでもホバーが動く。最後の結果を使い回すと、位置がずれて別の式の型を出す |
| 10. ホバー | 位置 `p` について、`span.start <= p < span.end` を満たす最も小さい `Expr` を選ぶ（`Expr` でない名前、すなわち代入の左辺・`for` の変数・引数・`case` の束縛・関数の名前・`import` の名前は別に走査する）。式なら絞り込み後の型（`expr_types`）。変数の参照なら、加えて宣言の型（`narrowings` を位置で引く。絞り込まれていなければ同じ）。変数の宣言の位置なら宣言の型（`local_spans`）。呼び出しの `callee` の上なら署名: 利用者の関数は内部名（`wys explain` と同じ `a.b.f`）で `def a.b.f(x: int) -> str uses io`（推論した Effect は `uses(inferred) io`。ADR-0024）、`pub` / `async` / `@tool` / `@agent` を添える。組み込みの関数は呼び出しの型と Effect（`builtin_effects`）を「組み込み」と添えて、メソッド（`append`、`PyObject` のメソッドなど）は結果の型だけ。レコードのフィールドはフィールドの型。キーワード・空白・コメント・文字列の中・型注釈の上は `null`。型の表記は `wys explain` と同じ（ADR-0017）。Markdown の 1 つのコードブロックと、短い説明の行 | 型だけ | AI と人の両方が、`wys explain` と同じ言葉で読める |
| 11. 定義ジャンプ | 関数の呼び出し（`m.f` を含む）→ その関数の名前（`FuncSig.span`。モジュールの関数は import 先のファイル）、ローカル変数（`case` の束縛を含む）→ 宣言（`FuncInfo.local_spans`）、型の名前 → `type` の定義、`import m` / `import m as n` のモジュール名 → そのモジュールのファイルの先頭、`from m import f` / `f as g` の名前 → `f` の定義、`@agent(tools=[...])` の関数名 → その関数。組み込みの関数・組み込みのレコード（`LlmRequest` など）・型パラメータには定義がない（空の応答）。実装のため、名前解決が `import` を捨てないようにする（`resolve` が各モジュールの `imports` を残す）。`case` の束縛は `FuncInfo.locals` / `local_spans` に登録し、`wys explain` の `locals` の `kind` に `binding` を足す（項目の追加なので版は上げない。explain のスナップショットを更新する） | 参照検索も一緒に | 参照検索は公開準備（論点 1） |
| 12. quickfix | 診断の `hints` のうち `replacement` のあるものごとに、`quickfix` の code action を 1 つ作る（いまは E0103 / E0104 / E1102 / W0203）。診断を配るときに、修正を `Diagnostic.data` に JSON（`{fixes: [{title, uri, range, text}]}`）で入れ、`codeAction` は `context.diagnostics[].data` から `WorkspaceEdit` を作る（サーバー側に帳簿が要らず、重複や古さに強い）。編集先の URI は `replacement` の位置のファイル（診断のファイルと違い得る）。解析の結果ではなく診断から作るので、構文エラー（E0103 / E0104）の修正も出せる。`replacement` のないヒントは出さない | 修正案を LSP 側で別に作る | 診断 JSON の修正案（DX-002）をそのまま使えば、CLI と LSP で修正が食い違わない |
| 13. 整形 | `textDocument/formatting` で `wys_fmt::format_source` を開いている文書の全文に適用し、1 つの編集（全文の置き換え）で返す。構文エラーがあっても整形する（`wys fmt` と同じくトークン列で整形する）。字句エラーがあれば `null`、整形しても変わらなければ空の配列。範囲の整形はしない | 差分の編集を返す | 実装が小さい。エディタは全文の置き換えでもカーソルを保つ |
| 14. 失敗の扱い | 壊れた通知・要求、`file://` でない URI（`untitled:`、`git:` など）、知らない要求はサーバーを止めない（通知は無視するか `window/logMessage`、要求は `InvalidParams` / `MethodNotFound`）。解析は 256 MiB のスタックの別スレッドで回し、パニックは「その文書の診断を更新しない」＋ `window/logMessage`。**スタックの溢れは隔離できない**（Rust はスレッドに関係なくプロセス全体を止める。大きなスタックは溢れる深さを引き上げるだけ）ので、T11 でパーサに入れ子の深さの上限を入れ、深すぎる入力は新しいエラーコードの診断にする（`wys check` も同じ入力で落ちるのを直す。Codex のレビュー）。開いている文書ごとに、その文書をエントリにした最後の解析の結果（`Frontend`。`analysis` を含む。スレッドから返すので `Send`）を持ち、ホバー・定義はそれを使う。解析が落ちた文書は結果を捨てて `null` にする（古い結果は位置がずれる）。`shutdown` なしの `exit` は終了コード 1 | — | PoC6 のレビュー。エディタは入力途中の全状態を送るので、コンパイラの弱点が桁違いに露出する |
| 15. 検査 | 3 層: (a) 純粋な関数の単体テスト（`hover_at` / `definition_at` / 位置の変換）。(b) Conformance の `tests/conformance/lsp/<ケース>/main.wys`（`modules/` と同じく、ディレクトリの `main.wys` だけがケース。モジュール越しの定義のため）に新しいキー `# expect-hover: 行:列 <文字列>`（ホバーのコードブロックの 1 行目と完全一致）と `# expect-definition: 行:列 -> ファイル:行:列`（ファイルはケースのディレクトリからの相対。計画の `-> 行:列` を広げた）。行・列は 1 始まりで、列は Unicode スカラー値（`wys_span::Position` と同じ）。これらのキーのあるケースは実行せず、LSP と同じ入口で解析だけ行う。harness は `wys_lsp` の純粋な関数を呼ぶ（`wys_lsp` → `wys_driver` なので循環しない）。(c) `serve(&Connection)` を `Connection::memory()` で動かす単体テストと、`wys lsp` を子プロセスにして JSON-RPC を話す e2e。CI はエディタを使わない | e2e だけ | 位置の期待値をケースのファイルに書けば、Conformance と同じく AI が読み書きしやすい |
| 16. エディタ | エディタの拡張は作らない。汎用の LSP クライアントの設定例（Neovim の組み込みクライアント、VS Code の汎用クライアントの拡張）を `docs/design/lsp.md` に書く。ユーザーの手元のエディタでの確認は、PoC6 の基準 2 と T11 の終わり | VS Code の拡張を作る | 公開の前に作る（拡張の配布と名前は公開準備で決める） |
| 17. 見直し条件 | 補完を入れるとき、ファイルが大きくなって全体の再解析が遅くなったとき（変更から診断まで 50 ms を超える）、マルチルートのワークスペース、Windows を対象にするとき、`lsp-server` の代替（`-32700` の扱い）が要るとき | — | — |

## 影響

- 実装は T11（`docs/plans/m6.md`）: フロントエンドの `analysis`（論点 9）、`resolve` の `imports` の保持と `case` の束縛の登録（論点 11）、ホバー・定義・quickfix（`Diagnostic.data`）・整形、位置の逆変換（論点 8）、パスの正規化（論点 5）、`relatedInformation`、文書ごとの解析の結果の保持（論点 14）、harness の新しいキーと `tests/conformance/lsp/`、e2e の全手順、`docs/design/lsp.md` の更新。いずれも型検査の内部の作り直しは要らないので、計画の分割の判断基準 3（XL 化）には当たらない。
- 新しいエラーコードはない。`wys explain` は `locals` の `kind` に `binding` が増える（論点 11）。
- 更新する文書: `docs/design/lsp.md`（新規）、用語集（`LSP` の行の書き換え、ホバー、定義ジャンプ、quickfix、位置の符号化）、`explain-schema.json`（`kind` の `binding`）、`docs/test-plan.md`（DX-003 の行）、`docs/README.md`（`lsp.md`）、CLAUDE.md（`wys lsp`）、ADR 一覧。
- 制限: 補完などは公開準備。Windows は CI の対象外。壊れた枠でサーバーが止まる（論点 2）。パーサに入れ子の深さの上限がない（`wys check` も深い入れ子で落ちる。LSP は論点 14 で隔離するが、パーサの上限は別の作業）。
