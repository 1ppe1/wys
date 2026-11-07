# 用語集

文書間で用語の意味と表記を揃えるための基準。新しい用語を使い始めたらここに追加する。

| 用語 | 意味 | 関連 |
|---|---|---|
| 本言語 | このプロジェクトで設計する言語。正式名称が決まるまでの呼称 | 未決定事項 1 |
| wys | 内部コードネーム（Winny と Singularity に由来）。Rust クレート名の接頭辞 `wys_` に使う。**正式な言語名ではない**。CLI の実行ファイル名も `wys`（ユーザーが決定） | 未決定事項 1 |
| Must / Should / Could | 要件の優先度（MoSCoW）。Must=MVPに必須、Should=強く望ましい、Could=余力があれば | 要件定義書 4章 |
| MVP | 各フェーズの完了条件を満たす最小構成 | 要件定義書 7章 |
| Unknown | 未検証の外部値を表す概念上の型。明示的な絞り込み・検証を経て具体型になる。名称は仮 | LANG-006, AC-003 |
| Null安全性 | `None` の可能性がある値を未検証で使うことを型チェックで禁止する性質 | LANG-003, AC-002 |
| 構造的型付け | 型の名前ではなく形（持っているフィールド・メソッド）で互換性を判定する方式。対義語は名目的型付け | LANG-005, 未決定事項 2 |
| 網羅性検査 | パターンマッチで全ケースを扱っているかをコンパイル時に確認すること | LANG-004 |
| 所有権（Ownership） | 値を誰が管理・解放するかを決めるコンパイラ内部のモデル | MEM-002 |
| Borrow / Move | 所有権を移さずに参照する操作 / 所有権を移す操作。高性能処理向けに明示的に使う | MEM-004 |
| ARC / ORC | 参照カウントによる自動メモリ管理。ORCは循環参照の回収を加えた方式（Nimの用語） | MEM-003, 未決定事項 4 |
| 説明レポート（explain） | `wys explain <file>` が出す JSON。関数ごとのシグネチャ、ローカル変数の型、絞り込み、ARC 操作とその理由。静的な事実だけを含む（DX-004） | ADR-0017, docs/spec/explain-schema.json |
| ARC 操作の理由 | ARC パスが `retain` / `release` を挿入した理由の識別子（`borrow_from_mutable_place` など7種類）。IR の命令が持ち、`dump-ir` と `wys explain` に出す | ADR-0017, docs/design/ir.md |
| Effect | 関数が持つ副作用（ネットワーク、ファイル操作など）の種類。静的診断とRuntime強制の両方に使う。Phase 1 は `io`（`print`、`arg`）と `exit`（`exit`）の2つで、注釈だけを検査する（パニックは Effect にしない）。M4 で `python`（Python のコードの実行、`io` を含意しない。ADR-0018）、M5 で `llm` が加わった。M6 から、注釈のない関数の Effect は推論する（ADR-0024） | AI-009, 未決定事項 5, ADR-0014 |
| 経路（絞り込み） | ローカル変数か引数を根とし、レコードのフィールド参照だけを連ねた式（`p.x`、`a.b.c`）。変数と同じく `is None` や `match` で絞り込め、根に代入すると絞り込みは消える | ADR-0016 |
| `uses` 節 | 関数の戻り値の型の後ろに書く Effect の宣言（`def f() -> None uses io:`）。Effect がなければ `uses pure`。書いた関数は宣言を契約として検査する。書かない関数の Effect は推論する（M6、ADR-0024） | ADR-0014、ADR-0024 |
| 推論した Effect | `uses` 節のない関数の Effect。本体の呼び出しの Effect の和集合を、再帰も含めて不動点まで求める。注釈のある呼び出し元はこの集合で E1102 を検査する。宣言した Effect のうち要求されないものは W0203。`dump-ir` では `uses(inferred) io`、`wys explain` では `inferred` | ADR-0024 |
| Capability | エージェントやコードが使ってよい能力（ファイル、ネットワーク、外部サービスなど）を表す権限。Runtimeが強制する | AI-004, P-05 |
| 承認フロー | 高リスク操作の実行前に人間などの承認を求め、Runtimeが強制する仕組み | AI-005 |
| 予算（Budget） | タイムアウト、実行回数、トークン数、費用の上限 | AI-006 |
| トレース | モデル呼び出し、ツール実行、使用量、失敗の記録。M6 では `WYS_TRACE=<path>\|stderr` にスパンと監査イベントを JSON 行で出す（監査とは別の流れ。本文は `WYS_TRACE_CONTENT=1` のときだけ）。プログラムからは `llm_usage()` で集計を読む | AI-007, NFR-003, ADR-0025 |
| スパン | トレースの 1 区間（`agent` / `llm_call` / `tool_call`）。終わったときに 1 行出し、`parent` で入れ子をたどる | ADR-0025 論点 2 |
| モック / 再生 | モデル応答を差し替える / 記録済み応答を再利用して、外部呼び出しなしでテストすること。M6 では `WYS_LLM_RECORD=<path>` で記録し、`WYS_LLM_PROVIDER=replay:<path>` で再生する | AI-008, AC-009, ADR-0025 |
| 記録 | 要求と応答の対を JSON 行で並べたファイル（先頭にヘッダ）。どのプロバイダでも作れ、本文を含む（API キーは含まない） | ADR-0025 論点 6 |
| 指紋（要求の） | 再生で記録と突き合わせる、要求から `v` と `timeout_ms` を除いたもの。n 回目の呼び出しは n 回目の記録と、指紋が一致しなければ `Err(kind="replay")` | ADR-0025 論点 7・8 |
| 価格表 | proxy（`wys llm-proxy`）が持つ、モデルごとの 100 万トークンあたりの USD（入力・出力・キャッシュの書き込みと読み込み）。応答に `cost_usd` を添え、ランタイムが `@agent(max_cost_usd=)` と比べる。知らないモデルには付けない（上限のある agent では費用が分からないとして止める） | AI-006, ADR-0025 論点 12・13 |
| 監査イベント | 権限拒否や承認などの、後から追跡できる実行記録。M5 ではランタイムが順序付きで持ち（`audit_events()`、`WYS_AUDIT=<path>\|stderr` で JSON 行）、種類は `llm_called` / `tool_called` / `tool_denied` / `capability_denied` / `approval_requested` / `approval_granted` / `approval_denied` / `budget_exceeded` / `schema_rejected`。プロンプトと応答の本文は含めない | AC-007, NFR-005, ADR-0023 論点 15 |
| 構造化診断 | エラーコード・位置・原因・修正ヒントなどを機械可読形式（JSONなど）で出す診断 | DX-002, AC-005 |
| 共通IR | フロントエンドと各バックエンドの間にある中間表現 | COMP-001 |
| Nativeバックエンド | CPU向けの実行ファイルを生成するバックエンド。LLVMが第一候補 | COMP-002, COMP-004 |
| LLVM / MLIR | コンパイラ基盤。LLVMはNativeコード生成、MLIRはGPU/SIMDなど多層の最適化に使う | COMP-004, COMP-007 |
| 実行境界 | 動的値・Native・アクセラレータ・FFIの間でデータや制御を受け渡す地点。検証と変換を行う | COMP-008, P-02 |
| FFI | 他言語の関数を呼び出すための仕組み。本言語では主にCPython連携 | INTEROP-001 |
| GIL | CPythonのグローバルインタプリタロック。Python連携時のスレッド制約になる。本言語では起動時に取得し、手放さない | 未決定事項 8、ADR-0018 |
| Conformance test | 言語仕様どおりに動くかを確かめる適合性テスト | 13章 成果物 5 |
| Conformance ランナー | `tests/conformance/` の `.wys` ケースを実行し、コメントで宣言した期待値（診断・出力・終了コード）と突き合わせる仕組み（`crates/wys_testharness`） | 13章 成果物 5 |
| スナップショットテスト | 出力（診断 JSON など）をファイルに保存し、変化を差分としてレビューするテスト。insta を使う | DX-002 |
| 閉ループ（AI 修正ループ） | 診断 JSON → 修正 → 型検査・テスト、を自動で繰り返す開発の進め方。`scripts/ai-loop.sh` | DX-005, PoC 5 |
| Backend トレイト | IR からオブジェクトファイルを作る処理の共通インターフェース。実装を差し替えられる（`crates/wys_backend`） | ADR-0004, P-03 |
| 絞り込み（narrowing） | `if x is None:` などの検査の後や、Union 型の変数への代入の後で、変数の型を狭く扱うこと（代入による絞り込みは `None` だけの型にはしない） | ADR-0006, AC-002 |
| 束縛パターン | `case Ok(x):` / `case Err(e):` のように、構築子に一致した値の中身を新しい変数に束縛する `match` のパターン。束縛変数は、その `case` の本体の中だけで使える | ADR-0013, LANG-008 |
| `Result[T, E]` | 失敗し得る処理の結果を表す組み込みの型。成功は `Ok(v)`、失敗は `Err(e)` で作り、`match` で取り出す。後置の `?` は、`Err` なら関数からそのまま返し、`Ok` なら中身を取り出す | ADR-0006, ADR-0013, LANG-008 |
| パニック | 回復を想定しない実行時エラー（整数オーバーフロー、境界外アクセス、`assert` の失敗、`panic(msg)`）。stderr に `runtime error: …` を出して終了コード 101 で終わる。想定内の失敗は `Result` で表す | ADR-0006, ADR-0013, NFR-007 |
| 辞書（`dict[K, V]`） | キーから値を引く可変なコレクション。キーは `int` / `bool` / `str`。反復は挿入順。キーがないかもしれないときは `d.get(k)`（`V \| None`）か `k in d` を使う | ADR-0015 |
| 単相化 | Generics を、使われる型ごとに具体化したコードとして生成する実装方式 | ADR-0006 |
| 幅部分型 | 構造的なレコード型で、必要なフィールドをすべて持っていれば互換とみなす規則 | ADR-0007 |
| 平坦化（Union） | `(A \| B) \| B` を `A \| B` にする正規化。単相化で型パラメータを置き換えた後にもやり直す | ADR-0006 |
| 開いた型 | 取りうる値の種類が将来増える型。`Unknown` は開いた型なので、`match` には `case _:` が必須。M4 で `float` が、M5 で `list` / `dict`（JSON）が加わった（ADR-0019、ADR-0021） | ADR-0011 |
| 内部名 | モジュールの定義に付ける、プログラム全体で一意な名前（`util/text.wys` の `f` は `util.text.f`）。エントリファイルの定義は元の名前のまま | ADR-0010 |
| ソースマップ | 複数のソースファイルを1本の通し番号の位置で扱う仕組み（`wys_span::SourceMap`）。診断の `span.file` はここから引く | ADR-0010 |
| 警告 | ビルドを止めない診断（`W` で始まるコード）。`check` は警告だけなら終了コード 0 | `docs/spec/error-codes.md` |
| 埋め込み（Python） | 本言語の実行ファイルが CPython を初期化して使う連携の方式。Python を使うプログラムだけが `libpython` をリンクする | ADR-0018 |
| `PyObject` | Python のオブジェクトを包んだハンドルの型（名目的）。本言語の参照カウントで管理し、最後の release で Python の参照を手放す。表示・比較・`dict` のキーには使えない（E1202） | ADR-0018 |
| 属性（デコレータ） | `def` の前に書く `@tool(...)` / `@agent(...)`。M5 ではコンパイラが解釈するコンパイル時の属性で、関数値が入ったら同じ構文のままライブラリのデコレータに置き換える | ADR-0022 |
| スキーマ型 | `validate[T]` / `to_json` / `llm_generate[T]` と `@tool` の引数・戻り値に使える型。JSON への写像が一意に決まる型（スカラー、それらの Union、`list`、`dict[str, ..]`、それらだけを持つレコード、`Unknown`） | ADR-0022 |
| `JsonError` | `parse_json` の失敗を表す組み込みのレコード `{message, offset}`。`offset` は UTF-8 のバイト位置 | ADR-0021 |
| 根のコンテキスト | `@agent` の外の、agent コンテキストのスタックの底。ツールなし、予算なし、Capability はすべて（`WYS_CAPABILITIES` で縮小される）。`llm_generate` を agent の外から呼ぶとここで数える | ADR-0023 論点 9 |
| ツール表 | 生成コードが `wys_tools_register` でランタイムに渡す `@tool` の表（名前・説明・引数の JSON Schema・Effect のビット・承認の要否・トランポリン）。agent の設定はこの表の添字でツールを指す | ADR-0022 論点 15 |
| proxy（`wys llm-proxy`） | Anthropic Messages API の adapter として働く `wys` の副コマンド。`WYS_LLM_PROVIDER=anthropic` のときにランタイムが子プロセスとして起動する。HTTPS と API キーはこのプロセスだけが持つ | ADR-0023 論点 1・5・6 |
| JSON 行のプロトコル | ランタイムと外部のプロバイダ（proxy か `cmd:<path>`）が stdin / stdout で 1 行に 1 つの JSON をやり取りする約束（要求は `v` / `model` / `system` / `messages` / `tools` / `schema` / `max_tokens` / `timeout_ms`、応答は `text` / `tool_calls` / `usage` / `stop` / `assistant` か `error`） | ADR-0023 論点 2、`docs/design/ai-sdk.md` 8.2 |
| トランポリン（ツールの） | lowering が `@tool` ごとに合成する IR の関数 `__wys_tool_entry_<名前>(args: str) -> str`。引数の JSON を検証して型付きの値にし、ツールを呼び、戻り値を JSON にする。ランタイムは検査を通ったツールだけをこれで呼ぶ | ADR-0022 論点 15、`docs/design/ai-sdk.md` 15 章 |
| agent コンテキストのスタック（タスクごと） | 今どの agent の中にいるかを表すスタック。タスクごとに持ち、`spawn` した子は親のスタックを引き継ぐ（予算は共有して数える） | ADR-0023 の訂正（2026-11-06） |
| モックのプロバイダ | `WYS_LLM_PROVIDER` を設定しないときの既定のプロバイダ。`llm_mock_push` で積んだ応答を順に返すので、Conformance と CI はネットワークに出ずに決定的に動く | ADR-0022 論点 10、ADR-0023 論点 1 |
| 承認の方針 | `approval="required"` のツールを実行してよいかを決める配備時の設定 `WYS_APPROVAL`（`deny` が既定 / `allow` / `prompt`: stderr に問い、stdin から 1 行読む）。拒否はツールの結果として LLM に返して続ける（`prompt` で答えがない EOF と、未知の値は `Err(kind="approval")` で中止） | AI-005、ADR-0023 論点 12 |
| `PyError` | Python の例外を表す組み込みのレコード `{kind, message, traceback}`。Python の操作は `Result[.., PyError]` を返す。構造的なので、同じフィールドを持つレコードと同じ型になる | ADR-0018 |
| ADR | Architecture Decision Record。設計判断とその理由を記録する文書 | docs/adr/ |
| タスク | `spawn` が作る、並行に進む処理の単位（`Task[T]`）。単一スレッドの上で協調的に切り替わる | ADR-0020 |
| スタックフル・コルーチン | 自分のスタックを持ち、どの深さからでも中断・再開できるコルーチン。本言語のタスクの実装方式 | ADR-0020 |
| 中断点 | タスクが止まり、ほかのタスクへ切り替わり得る地点。`await` と `sleep_ms` だけ | ADR-0020 |
| 協調的スケジューラ | 実行中のタスクが中断点に来たときだけ切り替えるスケジューラ（割り込まない）。実行キューは FIFO | ADR-0020 |
| 仮想時計 | `sleep_ms` の期限を決めるランタイムの時計。実行キューが空のときだけ進むので、タスクの順序は実時間に左右されない。`WYS_VIRTUAL_TIME=1` なら実時間を待たない | ADR-0020 |
| サービス呼び出し（メインスタック） | タスクの中からの Python の呼び出しを、メインスレッドのスタックの上で実行すること。CPython のスタックの保護はメインスタックでしか働かないため | ADR-0020 論点 16 |
| PoC | 技術的な実現性を確かめる小さな試作 | 要件定義書 11章 |
