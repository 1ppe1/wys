# テスト計画（v0.2: M2 完了時点）

要件定義書「13. 次の設計成果物」の 5（MVP テスト計画）の初版。受け入れ基準（AC）と要件を、どのテストで確かめているかを対応づける。

## 1. テストの種類

| 種類 | 置き場所 | 実行 | 内容 |
|---|---|---|---|
| 単体テスト | 各クレートの `#[cfg(test)]` | `cargo test` | 字句・構文・型検査・IR 検証器・ランタイム関数 |
| PoC5 のケース検証 | `scripts/poc5/cases/` | `scripts/poc5/verify.sh`（CI） | 壊れた版は失敗し、正解の修正例は合格する |
| ARC の検査（M3 T3） | `scripts/ci.sh` | Conformance を Debug（`-O0`）、Release（`WYS_OPT=2`）、AddressSanitizer（`WYS_SANITIZE=address`）の3つのモードで実行 | 終了時の生存オブジェクト数（102）と ASan。検査自体が働くことは e2e の `arc_checks_detect_injected_errors` |
| Conformance テスト | `tests/conformance/{lang,syntax,types,diagnostics}/*.wys`、`tests/conformance/modules/*/main.wys` | `cargo test -p wys_testharness` | 言語仕様の適合性。期待値は `.wys` の先頭コメントで宣言する |
| 診断スナップショット | `crates/wys_testharness/tests/snapshots/` | 同上（insta） | 診断 JSON の形を固定する |
| e2e テスト | `crates/wys_cli/tests/e2e.rs` | `cargo test -p wys_cli` | CLI の挙動（終了コード、出力形式、ビルドした実行ファイル） |
| explain のスナップショットとスキーマ検証（M3 T8、ADR-0017） | `tests/conformance/explain/*.wys`、`crates/wys_testharness/tests/snapshots/conformance__explain_*.snap` | `cargo test -p wys_testharness`（`explain_json_snapshots`、`all_explain_reports_match_schema`） | `explain/` のケースは通常の実行ケースとしても動き、`wys explain` の JSON を insta で固定する。エラーのない全ケースの JSON を `docs/spec/explain-schema.json` で検証する |
| ベンチ計測（M3 T7） | `benches/measure.sh`、`benches/programs/*.wys` | 手動（結果は `benches/results/bench-YYYYMMDD.tsv`、git 管理外）。CI はベンチ用プログラムの `fmt --check` と `check` だけ | コンパイル時間、バイナリサイズ、実行時間、最大常駐メモリ、retain / release の回数（`WYS_RC_STATS=1` でビルドした計測用の実行ファイル）。絶対値と計測環境だけを記録し、倍率は書かない。計数の仕組みは e2e の `rc_stats_count_reference_count_operations` |

CI（`.github/workflows/ci.yml`、macOS arm64 と Linux x86_64 の matrix）は `scripts/ci.sh`（fmt / clippy / test、`examples/` の `wys fmt --check`、PoC5 のケース検証）を実行する。

## 2. M1 の受け入れ基準と確認方法

| 基準 | 内容 | 確認しているテスト | 状態 |
|---|---|---|---|
| **AC-001** | 型の合わない値を関数に渡すと、コンパイル時にソース位置付きエラーになる | `tests/conformance/types/E0201_argument_mismatch.wys`、`wys_typeck` の `ac001_argument_type_mismatch`、e2e の `ac001_ac005_type_error_is_reported_as_json` | 合格 |
| **AC-004** | Hello World / CLI を Native ビルドし、MVP 対象環境（macOS arm64、ADR-0003）で外部インタプリタなしに起動できる | e2e の `ac004_hello_world_builds_a_standalone_executable`（起動と stdout に加え、`otool -L` で動的依存が OS 標準ライブラリだけであることを確認）、`tests/conformance/lang/hello.wys` | 合格 |
| **AC-005** | 型エラーの診断を JSON で取得し、エラーID・範囲・原因・修正ヒントを機械的に取り出せる | conformance の `all_diagnostics_match_schema`（全ケースの JSON をスキーマで検証）、`diagnostics_json_snapshots`、`wys_diagnostics` の `json_output_matches_schema` | 合格 |
| LANG-001 | Python 風のインデント構文、関数、変数 | `tests/conformance/lang/*`、`syntax/E0003`・`E0004`、`wys_lexer` / `wys_parser` の単体テスト | 合格（モジュールは M2） |
| LANG-002 | ローカル型推論と、関数シグネチャの型宣言 | `syntax/E0103`・`E0104`、`types/E0201_reassign_other_type.wys` | 合格 |
| LANG-007 | 型の不一致の静的検出、暗黙の型変換の禁止 | `types/E0201_*`、`types/E0207_*`、`wys_typeck` の `no_implicit_conversions` | 合格 |
| COMP-001 | 字句・構文・型検査・IR・コード生成の分離 | クレートの構成（`wys_lexer` → `wys_parser` → `wys_typeck` → `wys_lower` / `wys_ir` → `wys_backend`）、IR 検証器の単体テスト | 合格 |
| COMP-002 | CPU 向け Native 実行ファイルの生成 | AC-004 と同じ | 合格 |
| COMP-003 | 最適化できない箇所で意味を変えない | 整数オーバーフロー・0除算の実行時検査（`lang/runtime_*`）、短絡評価（`lang/short_circuit.wys`） | 合格（最適化は M3 で再確認） |
| DX-001 | `build` / `run` / `check` を提供する（`test` / `fmt` は M2） | e2e テスト全体 | `build` / `run` / `check` は合格 |
| DX-002 | 機械可読な診断 | AC-005 と同じ | 合格 |

## 3. まだ確かめていないこと（後続のマイルストーン）

| 内容 | 予定 |
|---|---|
| AC-002（`None` の可能性がある値の未検証利用を拒否） | **合格（M2 T1a / T1b）**。拒否: `types/E0301_*`。検査後の値が正しく動くこと: `lang/optional_*`、`lang/union_*` |
| LANG-004（`match` と網羅性検査、Generics） | **合格（M2 T2 / T4）**。`lang/match_*`、`lang/generic_*`、`types/E030x_*`・`W0301_*`・`E040x_*`、e2e の `warnings_are_reported_but_do_not_fail` |
| LANG-003 / LANG-005（レコード型、構造的な幅部分型） | **合格（M2 T3）**。`lang/record_*`、`lang/empty_record`（フィールドのないレコードを `list` / `dict` / Union に入れる。AddressSanitizer モードを含む）、`types/E05xx_*`・`E0304_*`・`E0215_generic_*`（Generic な型に `None` を与える） |
| DX-005（AI 修正ループ、PoC5） | **準備済み・未計測（M2 T9）**。ケース20件と `scripts/poc5/verify.sh`（CI）。計測は `scripts/poc5/run.sh`（ユーザー実行）。結果は `docs/poc/poc5-results.md` |
| DX-001（`fmt`） | **合格（M2 T8）**。`wys_fmt` の `formatting_preserves_tokens_and_is_idempotent`（全 Conformance ケースと例で、トークン列が変わらないことと冪等性）、e2e の `wys_fmt_rewrites_and_checks`、CI で `examples/` の `fmt --check` |
| DX-001（`test`） | **合格（M2 T7）**。e2e の `wys_test_*`、`types/E0801_*`（`# expect-mode: test`） |
| MEM-005 / NFR-001（use-after-free・二重解放・リークの検出）、COMP-005（Debug / Release） | **合格（M3）**。M3 T3 で検査の仕組みを導入（3つのモードの Conformance、注入した誤りの検出）。判定は `docs/plans/m3.md` の出口判定 |
| `list[T]` と `for`（M3 T4a） | `lang/list_*`・`for_range`・`runtime_index_out_of_range`、`types/E090x_*`。要素が `str` のリストも含め、3つのモードで生存オブジェクト 0 |
| 参照型の要素・入れ子のリストと借用の安全条件（M3 T4b） | `lang/list_borrow_safety.wys`（`xs[i]` の値を、`xs` を変更する関数の呼び出しと同じ式で使う。ASan で use-after-free を検出したため、`xs[i]` の結果を retain するよう修正）、`lang/list_nested_and_records.wys`。代入による絞り込み: `lang/assign_narrowing.wys` |
| `Result[T, E]` と束縛パターン（M3 T5a、ADR-0013） | `lang/result_*`・`generic_result_match`、`types/E100x_*`・`E0303_result_missing_err`、e2e の `result_values_run_and_missing_types_are_hinted`。参照型の中身を持つ `Result` も3つのモードで生存オブジェクト 0 |
| `?` による伝搬（M3 T5b、ADR-0013） | `lang/try_*`（式の途中（`try_mid_expression`）・ループの中・Generics・エラー型の Union への詰め直し）、`types/E1010`〜`E1012`、`syntax/E0101_try_then_field`、e2e の `try_propagates_and_reports_uncovered_errors`。途中で返す経路も3つのモードで生存オブジェクト 0 |
| パニックの仕様と `panic(msg)`（M3 T5c、NFR-007、ADR-0013） | `lang/panic_builtin.wys`（stdout を書き出してから stderr に `runtime error: … at 場所`、終了コード 101）、`lang/panic_noreturn.wys`（戻らない文）、e2e の `panic_flushes_stdout_and_exits_with_101`。ASan モードでは LeakSanitizer を無効にする（リークの判定は生存オブジェクト数 102） |
| `dict[K, V]`（M3 T6a、ADR-0015） | `lang/dict_*`（リテラル、`in`、`.get`、キーの種類、1000 個の伸長と挿入順、参照型の値、借用の安全条件）、`for_dict_update_value`、`runtime_key_not_found`・`runtime_dict_changed_during_iteration`、`types/E0906`〜`E0909`・`E0201_dict_key_mismatch`、ランタイムの単体テスト `dicts_keep_insertion_order_and_grow`、e2e の `dict_counts_words_in_insertion_order`。3つのモードで生存オブジェクト 0 |
| `d.pop(k)`（M3 T6b、ADR-0015） | `lang/dict_pop`（削除後の再挿入は末尾）・`dict_pop_churn`（2000 回の追加と削除、伸長での詰め直し、`str` のキーと値）・`runtime_pop_missing_key`・`runtime_pop_during_iteration`、ランタイムの単体テスト。3つのモードで生存オブジェクト 0 |
| DX-004 の初期版 `wys explain`（M3 T8、ADR-0017） | `wys_ir` の ARC パスの単体テスト（理由）、`wys_backend` の `arc_reasons_and_effects_do_not_change_the_generated_code`、`wys_explain` の単体テスト（絞り込み、ローカル変数の名前、Generic のインスタンス、エラーコードの表）、`explain/` のスナップショット、全ケースのスキーマ検証、e2e の `explain_*`。Effect（T9b）は `explain/effects.wys` と `wys_explain` の `reports_declared_and_required_effects`。初版の制限: `main` のないテスト用のファイルは説明できない（E0210 の診断を返す） |
| Effect の注釈と検査（M3 T9a、AI-009 の静的な部分、ADR-0014） | `lang/effects_annotated_program.wys`（注釈は実行結果を変えない。自己再帰、Generic、注釈のない関数からの呼び出し）、`types/E1101`〜`E1104`・`E0203_uses_as_return_type`、`syntax/E0101_uses_without_effect`、`modules/E1102_across_modules`、`wys_typeck` の単体テスト（組み込み関数の表、呼び出しの記録）、e2e の `effects_are_checked_and_do_not_change_the_program`（T9b: `dump-ir` と `wys explain` にも出る）。推論は M6、Runtime 強制は M5 |
| 不変レコードのフィールドの絞り込み（M3 T10a、AC-002、ADR-0016） | `lang/record_field_narrowing.wys`（`is not None`、入れ子の経路、`assert`、`match`、`and`）、`lang/generic_unknown_optional.wys`（Generic の `T \| None` に `Unknown`）、`types/E0301_field_narrowing_reset_on_reassign`・`E0301_field_narrowing_reset_on_bind`（`case Ok(p):` の束縛）・`E0301_field_narrowing_reset_on_for`（ループ変数）・`E0301_for_variable_reset_after_loop`・`E0301_field_narrowing_in_loop`・`E0301_index_not_narrowed`・`E0302_field_never_none`、`explain/field_narrowing.wys` |
| 構造を持つ値の `==` / `!=`（M3 T10b、ADR-0016） | `lang/structural_equality.wys`（レコード、入れ子、`list`、順序の違う `dict`、`pop` 後の `dict`、`Result`、Union と単体の値を左右どちらに置いても）、`types/E0207_record_width_equality`・`E0207_eq_none_literal`・`E0601_unknown_in_equality`・`E0601_unknown_nested_in_equality`、e2e の `structural_values_compare_by_content`。3つのモードで生存オブジェクト 0 |
| `float`（M4 T12、ADR-0019） | `lang/float_basic.wys`（リテラル、四則、比較、変換、Python の `repr` と同じ表示、構造の中、Optional）・`float_nan`・`float_division_by_zero`・`float_to_int_out_of_range`・`float_from_unknown`（`parse_scalar` と `case float():`）、`syntax/E0009_float_literal_too_large`、`types/E0207_int_plus_float`・`E0207_float_floor_division`・`E0201_float_int_argument`・`E0907_float_dict_key`、`wys_runtime` の単体テスト（`float_repr` を Python の `repr` で確かめた値と比べる、`parse_scalar` の分類）、e2e の `floats_follow_ieee_754_and_print_like_python` |
| Python の言語側の表面（M4 T6、ADR-0018、AC-010 の一部） | `python/round_trip.wys`（`py_import`、`attr`、`call`、`to_int` / `to_str` / `to_bool`、`is_none`、`?`）・`python/exceptions.wys`（例外の種類、メッセージ、トレースバック、NUL を含む名前）。Python 3.14 がなければ飛ばし、CI では必須。`types/E1201_py_value_record`・`E1202_print_pyobject`・`E1202_compare_pyobject`・`E1102_python_effect`、e2e の `python_libraries_can_be_called_and_exceptions_are_values`（式の受け手 `(m.attr(..)?).call(..)`） |
| Python のランタイムとリンク（M4 T5、ADR-0018） | `wys_runtime_python/tests/embed.rs`（C ABI: 往復、`bool` / `None`、例外の種類ごとの型名とメッセージ、整形の失敗、NUL を含む名前、トレースバック、ASan、解放漏れの検出 102）、`wys_driver/tests/python_link.rs`（IR から Python を使う実行ファイルを作って動かす、ASan、Python を使わないプログラムは Python なしでリンクできる）。Python 3.14 がなければ飛ばし、CI（`WYS_REQUIRE_PYTHON=1`）では必須 |
| 未使用の警告（M3 T10e、ADR-0016） | `types/W0201_unused_local.wys`（代入・`for` の変数・`case Ok(x)`。`_` で始まる名前、メソッドの受け手、ループで更新する変数は対象外）、`modules/W0202_unused_import`（`import m`、別名、`from ... import` の名前）、`types/W0201_ordered_with_other_warnings`（W0301 と位置の順）・`E0203_no_unused_warning_with_signature_error`（署名のエラーでも抑止）、e2e の `unused_names_are_warned_but_do_not_fail`。エラーのある関数・モジュールでは出さないので、既存のエラーのケースは変えていない |
| 構造を持つ値の `print`（M3 T10c、ADR-0016） | `lang/structural_print.wys`（レコードのフィールド名の順、空の `list` / `dict`、挿入順の `dict`、構造の中の `str` のエスケープと `None`、`Result`、Generic のレコード、トップレベルの Union、幅部分型で変換した値の型名。フィールドの集合が同じ型は ID を共有するので、最初に定義した `type` の名前で表示する）、`types/E0301_print_optional_record`・`E0601_unknown_nested_in_print`・`E0201_print_type_param`、e2e の `structural_values_are_printed`。3つのモードで生存オブジェクト 0 |
| LANG-001（モジュール） | **合格（M2 T6）**。`tests/conformance/modules/<ケース名>/main.wys`（import するファイルを同じディレクトリに置く）。名前解決の単体テストは `wys_resolve`。`import as`（M3 T10d、ADR-0016）: `modules/import_as`（モジュールと名前の別名、別名を付けてローカルの定義と共存、ほかのモジュールのレコードの表示名）・`E0704_alias_conflicts_with_def`・`E0704_two_imports_same_name`・`E0706_module_name_hidden_by_alias`・`E0706_module_not_imported`・`E0704_alias_is_builtin`・`method_on_local_named_like_module`（モジュールと同じ名前のローカル変数のメソッド呼び出し、自分自身のモジュール名での修飾）・`local_variable_named_like_module`（M4 T4: 別名の呼び出しはモジュールを指し、ローカル変数は同じ名前のモジュールを隠す）・`module_call_before_local_of_same_name`（ローカル変数の判定は位置で行う）・`E0706_prefix_of_imported_module`、`types/E0205_method_keyword_argument`・`E0204_method_receiver_unassigned`、e2e の `modules_and_names_can_be_imported_under_aliases`。制限: `case` のパターンには修飾した型名（`case t.Tag():`）を書けない（M2 からの構文の制限。`from ... import` で取り込んだ名前なら書ける） |
| AC-003（`Unknown` の検証） | **合格（M2 T5）**。拒否: `types/E0601_*`・`E0602_*`。検証後の利用: `lang/unknown_from_args.wys`（コマンドライン引数を `# expect-args:` で渡す） |
| Linux x86_64 での CI | **M2 T0 で追加済み** |
| メモリ安全性の境界テスト（NFR-001）、AddressSanitizer での実行 | M3（ADR-0005） |
| コンパイル時間・バイナリサイズの継続計測 | M3 |
| AC-006〜AC-010（AI Runtime、Python 連携） | Phase 2 |

## 4. 運用ルール

- 言語の挙動を変えるときは、先に Conformance ケースを追加・変更する（CLAUDE.md の「開発の進め方」）。
- エラーコードを追加したら `docs/spec/error-codes.md` に書き、対応するケースを作る（`every_error_code_has_a_case` が検査する）。
- 診断 JSON の形を変えたら、スキーマ（`schema_version`）とスナップショットを同時に更新する。
