# ADR-0017: `wys explain` の報告と、ARC 操作の理由

- **状態:** Accepted
- **日付:** 2026-11-04
- **関連:** DX-002、DX-004、NFR-008、ADR-0005、ADR-0012、`docs/plans/m3.md` T8

## 背景

DX-004 は、「`explain` 相当の機能で、型制約、Effect、最適化できない理由を説明する」ことを求めている。ADR-0012 では、retain / release のコストが問題になったら、Move 最適化と `explain` の計測で判断すると書いた。

いまのコンパイラは、必要な情報をすでに持っている。

- 型検査の結果（関数のシグネチャ、ローカル変数の宣言型、式ごとの絞り込み後の型）
- ARC パスが挿入した retain / release

ただし、型検査の結果は driver が lowering の後に捨てている。また、retain / release が「なぜ」そこにあるかは、IR に残っていない。

T7 では、実行時の回数を `WYS_RC_STATS` で数えられるようにした。T8 では、静的な事実（どこに、なぜ）を AI が機械的に読める形で出す。

## 選択肢と決定

| 論点 | 決定（推奨） | 主な代替案 | 理由 |
|---|---|---|---|
| 1. 出力の契約 | 専用のスキーマ `docs/spec/explain-schema.json` を作り、`schema_version`（初版は `"0"`）を付ける。運用は診断 JSON と同じにする。v0 の間は項目の追加だけを許し、削除や意味の変更をしたら版を上げる | 診断 JSON に `note` として混ぜる | 診断（エラーの報告）と explain（プログラムの説明）は別の文書。AI ツールが依存する契約なので、版を付けて管理する（NFR-008） |
| 2. 何を説明するか | 静的な事実だけ。関数のシグネチャ、ローカル変数の型、絞り込み、ARC 操作の挿入位置と理由 | 実行時の回数や時間も含める | 計測は `WYS_RC_STATS` と `benches/measure.sh`（T7）の役割。両者を混ぜない。倍率は書かない |
| 3. 理由の持ち方 | IR の `Retain` / `Release` 命令に理由（`RcReason`）を持たせる。バックエンドは理由を読まないので、生成コードは変わらない。`dump-ir` では `retain t3 ; borrow_from_mutable_place` のように末尾に出す | 関数ごとの側表（ブロックと添字から理由を引く） | 後続のパス（M3b の Move 最適化）が命令を動かしたり消したりしても、理由が命令についていく |
| 4. 理由の種類 | 7種類。retain は `param_slot_entry`、`consume_borrowed`、`consume_shared_owned`、`borrow_from_mutable_place`。release は `store_overwrites_slot`、`unconsumed_owned_at_block_end`、`slot_at_function_exit`。識別子は安定した英語の文字列にし、説明文（`message`）は診断と同じく英語にする | 自由記述の文字列 | ARC パスの挿入地点と1対1に対応する。識別子なら機械的に集計できる |
| 5. 位置情報 | 関数、ローカル変数、絞り込みにはソースの範囲を付ける。ARC 操作には IR の位置（関数インスタンス名、ブロック、命令の添字）を付け、`dump-ir` と突き合わせられるようにする | IR の命令にソースの範囲を持たせる | IR に範囲を持たせるのは別の作業になる。初版の制限として明記する |
| 6. Effect | 初版には項目を置かない。ADR-0014（T9）で形が決まってから、v0 に項目を追加する | 空の `effects: []` を先に置く | 意味の決まっていない項目を契約に入れない |
| 7. CLI | `wys explain <file>` で報告を出す。`wys explain E0201` のように、エラーコードを渡すと `error-codes.md` の行を返す。既定の形式は `json`（`check` の既定は `text` なので一致しないが、explain は機械向けの報告なので json を既定にする）。初版では `--format text` を使い方のエラーにする | `check` と同じく `text` を既定にする | 主な利用者は AI。text 版は需要が出てから作る |
| 8. エラーがあるとき | ソースに型エラーがあれば、`check --format json` と同じ診断 JSON を出して終了コード 1 にする。存在しないエラーコードは使い方のエラー（終了コード 2）にし、新しい診断コードは作らない | 新しい診断コードを作る | 診断コードは「ソースに対する」もの。使い方の誤りとは分ける |
| 9. 見送るもの | 位置を指定した型の問い合わせ（`--at line:col`）は、M6 の LSP（DX-003）で扱う | 初版で入れる | ファイル単位の報告が全関数・全変数の型を返すので、AI の用途ではまだ要らない |

## 影響

- 実装を2つに分ける。
  - **T8a:** この ADR、IR の理由、ARC パスで理由を設定、`dump-ir` の表示、`docs/design/ir.md`
  - **T8b:** driver で型検査の結果を保持、新しいクレート `wys_explain`、`wys explain`（ファイルとエラーコード）、スキーマと検証テスト、スナップショット、e2e
- ARC パスは、挿入する位置も数も変えない。理由を付けるだけにする（3つのモードと生存数の検査で確かめる）。
- 更新する文書:
  - `docs/spec/explain-schema.json`（新規）
  - `docs/design/ir.md`（ARC 操作の理由）
  - `docs/spec/error-codes.md`（`wys explain CODE` で引けること）
  - `docs/test-plan.md`、`docs/roadmap.md`、用語集、`CLAUDE.md` の開発コマンド
  - ADR 一覧
- 初版の制限:
  - ARC 操作にはソースの範囲がない。
  - Generic 関数のインスタンスは、IR 名（`name__N`）でしか区別できない。
  - Effect は未対応。
  - text 形式の出力はない。
- 見直し条件:
  - IR の命令にソースの範囲を持たせたとき（ARC 操作に範囲を付ける）。
  - Move 最適化を入れたとき（消した組を報告に含めるか決める）。
  - ADR-0014 で Effect の形が決まったとき。
