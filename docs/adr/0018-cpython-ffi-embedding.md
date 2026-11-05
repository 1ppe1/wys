# ADR-0018: CPython FFI（埋め込み、Python 3.14、例外は値）

- **状態:** Accepted
- **日付:** 2026-11-05
- **関連:** 未決定事項 8、INTEROP-001 / 002 / 003、AC-004、AC-010、AI-004 / 005、ADR-0008、ADR-0011、ADR-0012、ADR-0013、ADR-0014、`docs/plans/m4.md`（T2・T5〜T8）、`docs/poc/poc3-results.md`、issue #48

## 背景

要件定義書は、CPython との FFI / 埋め込みで既存の Python ライブラリを使えること（INTEROP-001）と、境界で型・例外・所有権・スレッド制約を変換すること（INTEROP-002）を求めている。Python 境界の例外や型の不一致は、本言語側の定義済みの失敗として伝搬しなければならない（AC-010）。未決定事項 8 は「Python 連携方式と対応バージョン、GIL / スレッド境界」。

M4 の計画では、ゼロコピー（INTEROP-003）を最初から範囲外にし、Python のバージョンを1つに固定した。PoC3（T1、`docs/poc/poc3-results.md`）では、次のことを macOS arm64 と Linux x86_64 の CI で確かめた。

- 本言語の実行ファイルから CPython 3.14 を初期化して、import・属性参照・呼び出し・`int` / `str` の往復ができる。
- 例外6種類を「型名: メッセージ」の値として受け取れ、プロセスは落ちない。
- Python のオブジェクトを本言語のヒープオブジェクトで包むと、生存数の検査（終了コード 102）と AddressSanitizer がそのまま効く。
- Python を使わない実行ファイルの依存関係は変わらない（AC-004）。

レビューで、例外の整形そのものの失敗（`__str__` が例外を出す）と、NUL を含む名前で、プロセスが止まる不具合が見つかった（PR #45 で修正）。

いまの言語には、関数値（E0213）も `float` もない（`float` は M4 T12 で入れる）。Effect は `io` と `exit` だけ（ADR-0014）。

この ADR では、M4 の FFI の形を決める。async との関係は ADR-0019 で決める。

## 選択肢と決定

| 論点 | 決定（推奨） | 主な代替案 | 理由 |
|---|---|---|---|
| 1. 方向 | **埋め込み。** 本言語の実行ファイルが CPython を初期化して使う。Python から本言語を呼ぶ方向（拡張モジュール、コールバック）は扱わない | (a) 拡張モジュール（Python から本言語の関数を呼ぶ）。(b) 両方 | 本言語のプログラムから Python のライブラリを使う（INTEROP-001）のが目的。コールバックには関数値が要る。拡張モジュールの方向は、関数値とパッケージの配布（INTEROP-004）が入るときに見直す |
| 2. 対応する Python | **CPython 3.14 だけ。** GIL ありのビルド。free-threaded ビルド（`Py_GIL_DISABLED`）は対象外。C API は Stable ABI の関数だけを、手書きの `extern "C"` で宣言する（外部クレートに依存しない。ユーザー決定）。マクロ（`Py_TYPE` など）は使わない | (a) 3.12 以降の複数の版。(b) `pyo3-ffi`。(c) `pyo3` | ロードマップのリスク表のとおり版を1つに固定し、検査の組み合わせを増やさない。Stable ABI の関数だけなら、次の版に上げるときの変更が小さい。PoC3 では 19 個の関数で足りた |
| 3. GIL とスレッド | 最初に Python を使ったときに初期化し、初期化したスレッド（本言語の唯一のスレッド）が GIL を持ったまま手放さない。M4b の `async` も同じスレッドで動かす（ADR-0019） | 呼び出しのたびに GIL を取って放す | 本言語はシングルスレッドなので、取って放す理由がない。スレッドを入れるとき（Phase 3）に見直す |
| 4. リンク | Python のランタイムは、`wys_runtime` を含む上位集合の staticlib（`libwys_runtime_python.a`）にする。Python を使うプログラム（論点 9 の組み込みを1つでも呼ぶもの）だけ、`libwys_runtime.a` の代わりにこれと `libpython3.14` をリンクする。リンクのフラグは `python3.14-config --embed --ldflags`、共有ライブラリの場所は `sysconfig` の `LIBDIR` を rpath に入れる。Python は `WYS_PYTHON`（既定は `python3.14`）で探す | (a) すべての実行ファイルに `libpython` をリンクする。(b) 実行時に `dlopen` する | Rust の staticlib を2つリンクすると、標準ライブラリの記号が重なる（PoC3）。上位集合にすれば、Python を使わない実行ファイルは今までと同じになる（AC-004）。`dlopen` は、記号の解決をすべて自前で行うことになり、PoC で確かめていない |
| 5. Python がない環境 | `wys check` / `wys explain` は Python なしで動く。`wys build` / `run` / `test` は、Python を使うプログラムのときだけ Python を探し、見つからなければツールのエラー（`cannot find Python 3.14; set WYS_PYTHON`、終了コード 2）にする。診断 JSON には出さない | 型検査で Python の有無を診断にする | 型検査はソースだけで決まるべき。環境の問題は診断ではない |
| 6. Python のオブジェクトの表現 | 組み込みの名目的な型 `PyObject`。値は、本言語のヒープオブジェクト（ADR-0012 のヘッダ + `PyObject*` 1語）で包んだハンドル。retain は本言語の参照カウントだけを増やし、最後の release で `Py_DecRef` してから解放する（型ごとの解放処理、drop glue）。`PyObject` はレコードではなく、構造的な部分型の判定にも入らない。Union の構成要素にはなれる（`PyObject \| None`） | `PyObject*` を直接持ち、retain / release を `Py_IncRef` / `Py_DecRef` に写す | ARC パス・検証器・生存数の検査・`wys explain` を変えずに済む。Python の参照カウントとの二重管理を、解放処理の1か所に閉じ込められる（PoC3 で確認） |
| 7. 終了処理 | `main` の終わり（生存数の検査の前）に `Py_FinalizeEx` を呼ぶ。失敗しても警告を出すだけで、終了コードは変えない。`exit()` やパニックでの終了では呼ばない | 呼ばない | 呼べば、Python 側の後始末（ファイルのフラッシュなど）が行われる。PoC3 の範囲では問題がなかった。C 拡張で落ちる例が出たら見直す |
| 8. 例外 | Python を呼ぶ操作はすべて `Result[..., PyError]` を返し、例外はパニックにしない（AC-010）。`PyError` は組み込みのレコード `{kind: str, message: str, traceback: str}`（`kind` は例外の型名、`message` は `str(e)`、`traceback` は整形したトレースバック。なければ空）。整形そのものが失敗したら、代わりの文字列（`<str() of the exception failed>`）にして2つ目の例外は消す。`SystemExit` と `KeyboardInterrupt` も `Err` にする（Python のコードがプロセスを終わらせない）。パニックになるのは、Python の初期化の失敗とメモリ不足だけ | (a) 例外をパニックにする。(b) 例外の型ごとに本言語の型を作る | AC-010 の「定義済みの失敗」を `Result` と `?`（ADR-0013）で表せる。型ごとの区別は `kind` の文字列で `match` できれば足りる。PoC3 と PR #45 のレビューで、整形の失敗を値にする必要が分かった |
| 9. 組み込みと `PyObject` のメソッド | 構文は足さない。組み込み関数 `py_import(name: str) -> Result[PyObject, PyError]` と `py_value(x) -> PyObject`（論点 10 の型の値を Python のオブジェクトに写す）。`PyObject` のメソッド: `attr(name: str) -> Result[PyObject, PyError]`、`call(args: list[PyObject]) -> Result[PyObject, PyError]`、`call_kw(args: list[PyObject], kwargs: dict[str, PyObject]) -> Result[PyObject, PyError]`、`to_int()` / `to_bool()` / `to_str()` / `to_float()`（`float` が入ってから）`-> Result[T, PyError]`、`to_list() -> Result[list[PyObject], PyError]`、`to_dict() -> Result[dict[str, PyObject], PyError]`、`is_none() -> bool`。名前やメソッドの引数に NUL を含む文字列を渡したら `Err`（`kind` は `ValueError`） | (a) `import python.json` のような構文。(b) Python → 本言語の値を `Unknown` で返す | 構文にすると、import が失敗し得ることを型で表せない。`Result` を返す関数なら正直に書ける。`Unknown`（ADR-0011）は `int` / `bool` / `str` / `None` しか持てず、`float` を足すと ADR-0011 を見直すことになるので、型ごとの変換関数にした |
| 10. 本言語 → Python の変換 | `py_value` が受け付けるのは `int` / `bool` / `str` / `None` / `float`（入ってから）/ `PyObject`（そのまま）と、それらを要素に持つ `list` / `dict`（キーは `int` / `bool` / `str`）。新しい Python のオブジェクトにコピーする（`list` は `list`、`dict` は `dict`、挿入順を保つ）。レコード、`Result`、`Unknown`、型パラメータを含む値は E1201 | レコードを `dict` に写す | レコードの写し方（フィールド名の順、型名）は決める材料が少ない。必要になったら足せる（制限を緩める方向） |
| 11. Python → 本言語の変換 | 論点 9 の変換メソッドだけ。厳密に変換する: `to_int` は Python の `int`（`bool` を含む。Python の部分型なので）を受け、i64 に収まらなければ `Err`（`OverflowError`）。`to_bool` は `bool` だけ（真偽値への暗黙の変換をしない）。`to_str` は `str` だけ。`to_list` は `list` と `tuple`、`to_dict` は キーがすべて `str` の `dict`。それ以外は `Err`（`kind` は `TypeError`） | Python の暗黙の変換（`__index__`、真偽値）に合わせる | 暗黙の変換をしない本言語の方針（LANG-002）に合わせる。境界の型の不一致を `Err` にできる（AC-010） |
| 12. Effect | Effect `python` を足す（ADR-0014 論点 1 の予告どおり）。論点 9 の組み込みとメソッドはすべて `python` を持つ。`python` は「Python のコードを実行するので、任意の副作用を起こし得る」ことを表し、`io` を含意しない（`uses io` だけの関数から呼ぶと E1102）。M5 の Capability Runtime は Python の内側を強制できない（サンドボックスではない）。ADR-0014 論点 5 の「メソッドは Effect を持たない」は、`PyObject` のメソッドを除く | (a) `io` に含める。(b) `ffi` を足す | Python のライブラリはファイルもネットワークも使える。`io` と区別しておけば、AI が生成したコードが Python を使うことを注釈で読める。セキュリティ原則（Runtime での強制）に従い、`python` を使う処理の権限は M5 で、プロセスの境界（サブプロセスや承認）で扱う |
| 13. 型検査の規則 | `PyObject` と `PyError` は `print`・`==`・演算・`dict` のキーに使えない（E1202。ヒントで変換メソッドを示す）。`PyError` はレコードなので、フィールドは読める（`e.kind`）。`py_value` の引数の型が論点 10 の外なら E1201 | `print(PyObject)` を `str()` で表示する | 表示や比較は Python のコードを動かす（`__str__`、`__eq__`）ので、Effect と失敗を伴う。暗黙に行わない |
| 14. 範囲外 | ゼロコピー（INTEROP-003、Phase 3）。コールバックと拡張モジュールの方向（関数値と一緒に）。`asyncio` との統合（Phase 3）。サブインタプリタとスレッド。C の FFI（`ffi` Effect）。Python の型ヒントからの型の生成 | — | M4 の範囲を、埋め込みで既存のライブラリを呼べることに絞る |

## 新しいエラーコード（E12xx 帯）

| コード | 内容 |
|---|---|
| E1201 | `py_value` に渡せない型（レコード、`Result`、`Unknown`、型パラメータを含む値）。ヒントで変換できる型を示す |
| E1202 | `PyObject` / `PyError` を、具体的な値が必要な場所（`print`、`==`、演算、`dict` のキー）で使っている。ヒントで `to_int()` などの変換メソッドを示す |

警告は追加しない。実行時: Python の初期化の失敗は `runtime error: cannot initialize Python`（終了コード 101）。

## 影響

- 実装は計画のとおり分ける。
  - **T5:** Python ランタイムの本実装（PoC3 の試作から）と、driver の条件付きリンク、CI の Python 3.14
  - **T6:** `PyObject` / `PyError`、組み込みとメソッドの型検査と lowering、Effect `python`、E1201 / E1202
  - **T7:** 論点 10・11 の変換と、論点 8 の例外の写像（トレースバックを含む）
  - **T8:** `tests/conformance/python/`、3つのモード、ベンチ
- 既存のプログラムは変わらない（Python を使わなければ、リンクも生成コードも同じ）。
- 更新する文書: `error-codes.md`（E12xx）、`docs/design/ir.md`（ハンドルの解放処理）、`memory-model.md`（ハンドルと Python の参照）、ADR-0014 の Effect の一覧（`docs/spec` の記述）、用語集（埋め込み、ハンドル、`PyObject`、`PyError`）、`docs/test-plan.md`、ADR 一覧と未決定事項 8 の欄。
- 初版の制限:
  - Python は 3.14 の GIL ありのビルドだけ。
  - Python から本言語を呼べない。
  - 値はすべてコピーで渡す（ゼロコピーなし）。
  - `float` の変換は T12 の後。
- 見直し条件:
  - Python 3.15 が出たとき（Stable ABI の関数だけなので、版を足す変更は小さい見込み）
  - 関数値を入れるとき（コールバック、拡張モジュール）
  - スレッドや Phase 3 の並列化を入れるとき（GIL、free-threaded ビルド）
  - M5 の Capability Runtime（`python` を使う処理の権限）
  - C 拡張で `Py_FinalizeEx` が落ちる例が出たとき
