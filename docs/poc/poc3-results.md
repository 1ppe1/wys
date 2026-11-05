# PoC3: CPython の埋め込み（M4 T1）

- **状態:** **成功。** macOS arm64 と Linux x86_64 の両方で、成功基準 1〜4 を満たした（基準 5 の時間は macOS で計測）。転換基準には当たらない
- **期限:** 10 営業日（`docs/plans/m4.md`「PoC3 の判定」）。着手 2026-11-05
- **コード:** `crates/wys_runtime_python/`（試作の staticlib）、`crates/wys_runtime_python/tests/embed.rs` と `embed_poc.c`

## 構成

- `libwys_runtime_python.a` は `wys_runtime` を含む staticlib（`wys_runtime` の rlib に依存する）。Python を使うプログラムは、`libwys_runtime.a` の代わりにこれと `libpython3.14` をリンクする。Rust の staticlib を2つ同じ実行ファイルにリンクすると標準ライブラリの記号が重なるので、上位集合にした。`nm` で `wys_print_int` / `wys_runtime_finish` などが含まれることを確かめた。
- CPython の C API は、Stable ABI の関数だけを手書きの `extern "C"` で宣言した（19 個: `Py_InitializeEx`、`PyImport_ImportModule`、`PyObject_GetAttrString`、`PyObject_CallObject`、`PyErr_GetRaisedException`、`PyType_GetName` など）。マクロ（`Py_TYPE`、`PyLong_Check`）は使わず、型の判定は変換関数の失敗（`TypeError`）で行う。
- Python のオブジェクトは、本言語のヒープオブジェクト（ADR-0012 のヘッダ + `PyObject*` 1語）で包んだハンドルにした。`wys_py_release` が参照カウント 0 のときに `Py_DecRef` してから解放する。生存数の検査（終了コード 102）がハンドルにも効く。
- 失敗する操作は null を返し、例外を「型名: メッセージ」の文字列として保存する（`wys_py_take_error`）。プロセスは落ちない。
- テストの呼び出し側は、生成コードの代わりに C で書いた（`embed_poc.c`）。ABI は生成コードの LLVM IR と同じで、文字列は本言語のオブジェクト（不死、参照カウント −1）で渡す。

## 結果（macOS arm64）

機種: Apple M5 Pro、macOS 26.4.1、Apple clang 21.0.0、rustc 1.99.1、Python 3.14.6（Homebrew、GIL あり、`Py_GIL_DISABLED=0`）。

| 基準 | 結果 |
|---|---|
| 1. 初期化、`json` / `math` の import と呼び出し、`int` / `str` の往復 | 成功（`math.factorial(10)` → `3628800`、`json.dumps("hi")` → `"hi"`） |
| 2. 例外の捕捉（型名とメッセージ） | 成功。`ValueError`（2種類）、`ModuleNotFoundError`（`ImportError` の派生）、`AttributeError`、`ZeroDivisionError`、変換の型不一致の `TypeError`。終了時の生存数 0 |
| 3. Python を使わない実行ファイルの依存関係 | 変わらない（`libwys_runtime.a` はそのまま。e2e の `ac004_hello_world_builds_a_standalone_executable` が通る） |
| 4. AddressSanitizer | 通る（呼び出し側と `libwys_runtime_python.a` を `-fsanitize=address` でリンク。`libpython` は計装しない） |
| 5. 時間の記録 | 下の表 |
| 追加: ハンドルの解放漏れの検出 | 1つ解放しないと、終了コード 102（`1 object(s) were not freed`） |

時間（絶対値。倍率は書かない）:

| 項目 | 値 |
|---|---|
| `Py_InitializeEx` | 約 13〜15 ms（Release、3回）。ASan では約 19 ms |
| `math.gcd(12, 18)` の往復（呼び出し、`int` への変換、ハンドルの解放。20 万回の平均） | 約 61〜68 ns（Release の runtime と `-O2`）、約 235 ns（Debug の runtime）、約 425 ns（ASan） |

リンクのフラグ: `python3.14-config --embed --ldflags`（`-lpython3.14 -ldl -framework CoreFoundation`）。実行ファイルは `Python.framework` に動的に依存する。テストは `sysconfig` の `LIBDIR` を rpath に足す。

## Linux x86_64

CI の `ubuntu-24.04` ジョブ（`actions/setup-python` の CPython 3.14.10、`WYS_REQUIRE_PYTHON=1`）で、同じ3つのテスト（往復と例外、AddressSanitizer、解放漏れの検出）が通った（2026-11-05、Actions run 41963090554）。`macos-15` ジョブも同じ版で通った。setup-python の Linux 版は共有ライブラリの `libpython3.14.so` なので、rpath（`LIBDIR`）が要る。時間は CI の共有マシンでは安定しないので記録しない。

## 言語からの計測（M4 T8）

`benches/programs/py_roundtrip.wys`（`math.gcd` の呼び出しと `int` への変換を 200,000 回、`?` で失敗を確かめる）を `benches/measure.sh` で計測した（Apple M5 Pro、macOS 26.4.1、Python 3.14.6、`--release`、3回の最小値）。絶対値だけを記録する。

| 項目 | 値 |
|---|---|
| 実行時間（初期化を含む） | 43.7 ms |
| 最大常駐メモリ | 15,504 KiB |
| retain / release の回数 | 600,006 / 1,400,008（Python のハンドルの release を含む） |

## ADR-0018 への申し送り

- 例外の整形（`PyObject_Str`、`PyType_GetName`）そのものが失敗することがある（`__str__` が例外を出すなど）。null を C API に渡すと落ちる（PR #45 のレビューで判明し、修正した）。整形の結果は必ず確かめ、2つ目の例外は消して代わりの文字列にする。
- 名前に NUL が含まれる場合も、プロセスを止めずに失敗として返す（同上）。境界の関数は、引数の値が原因の失敗をすべて「失敗の値」で返す方針にする。

- 上位集合の staticlib 方式で、AC-004 を保ったまま Python を使うプログラムだけ `libpython` に依存させられる。
- `Py_FinalizeEx` は終了時に呼んで問題がなかった（`json` / `math` / `operator` だけ。C 拡張で落ちる例は未確認）。
- 例外のトレースバックは取っていない（`PyErr_GetRaisedException` の後、`traceback` モジュールで整形できる。ADR-0018 で決める）。
- 名前と ABI（`wys_py_*`）は試作。ハンドルの取り違えを型で防ぐのは T6（`PyObject` 型）で行う。
