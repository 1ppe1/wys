# 共通IR 設計メモ（M2 完了時点）

実装: `crates/wys_ir`。AST からの変換は `crates/wys_lower`、LLVM IR への変換は `crates/wys_backend`。

## 構造

- `Program` = 文字列定数の表 + 関数の列 + エントリ関数。
- `Function` = 引数を含むローカル変数の型、一時値の型、基本ブロックの列。
- `Block` = 命令列 + 終端命令（`return` / `jump` / `branch` / `unreachable`）。

## 値のルール

| 種類 | 性質 |
|---|---|
| ローカル（`l0`, `l1`...） | 可変のスロット。`load` / `store` で読み書きする。ブロックをまたいで値を渡すときはローカルを使う |
| 一時値（`t0`, `t1`...） | 一度だけ定義する。**定義したブロック内でのみ使える** |

ブロックをまたぐ一時値を禁止しているので、PoC1 では φ 関数が要らない。LLVM 側では `alloca` + `load` / `store` になる（最適化時に mem2reg で SSA 化される）。

## 型

`int`（i64）、`bool`、`str`（ヒープオブジェクトへのポインタ1語、M3 T2a、ADR-0012）、Union（`u0`, `u1`...）、レコード（`r0`, `r1`...）。`None` 型の式は一時値を作らない。

## Union（M2 T1b、ADR-0009）

- `Program.unions` に構成要素を登録する（例: `union u0 = int | str | None`）。構成要素の番号がタグ。`None` の構成要素は中身を持たない。
- 命令: `make_union uN.k(t)`（構成要素 k の値を作る）、`union_is t, k`（タグが k か）、`union_payload t, k`（中身を取り出す。タグが k の値にだけ使う）。
- 型が変わる地点（代入、引数、戻り値、絞り込まれた変数の読み出し）で、lowering が変換を挿入する。基本型 → Union は `make_union`、Union → 基本型は `union_payload`、Union → Union はタグで分岐して詰め直す。
- LLVM では `%union.N = type { i8, [W x i64] }`（W は最大の中身の語数。`str` は2語）。命令が使う作業領域は関数の入口で `alloca` する。
- `Result[T, E]`（M3 T5a、ADR-0013）は、構成要素が2つの Union として表す（構成要素 0 が `Ok`、1 が `Err`。`T` と `E` が同じ型でも別の構成要素）。`Result` は Union の構成要素になれるので、Union の構成要素に別の Union が入ることがある。検証器は、構成要素の Union が自分より前に登録されていること（循環しないこと）を確かめる。
- `dict[K, V]`（M3 T6a、ADR-0015）は `Program.dicts` に登録し、ポインタ1語で表す。命令は `dict_new` / `dict_len` / `dict_contains` / `dict_get`（キーがなければパニック）/ `dict_set` / `dict_pop`（T6b。値を +1 所有で返す）/ `dict_key_at` / `dict_iter_next`。削除したエントリは墓標にし、表の伸長で詰め直す。ハッシュ表（挿入順のエントリ配列と開番地法の索引表）はランタイムが持ち、キーと値の参照カウント操作は、バックエンドが型ごとに生成する `@wys_dict_set{i}` / `@wys_dict_get{i}` / `@wys_release_dict{i}` が行う。`dict_get` と `dict_key_at` の結果は、`list_get` と同じく ARC パスがすぐ retain する（借用の安全条件）。
- ランタイム（C ABI）には Union を渡さない。`print` は Union のタグで分岐して、構成要素ごとの組み込み関数を呼ぶ（構造を持つ値は、生成した表示関数。下の「構造を持つ値の表示」）。

## 意味論

- `add` / `sub` / `mul` はオーバーフローを検査する。`floordiv` / `mod` は Python と同じ丸めで、0除算を検査する。
- `and` / `or` は、隠しローカルを使った分岐に変換する（短絡評価）。

## 検証器

`wys_ir::verify` は、一時値の多重定義・ブロック外での使用・型の不一致・存在しないブロックや関数の参照を検出する。検証の失敗はコンパイラのバグとして扱う（内部エラーで停止する）。

## テキスト形式

`wys dump-ir <file>` で出力する。例:

```text
entry f0

fn f0 main() {
  local l0: int
bb0:
  t0 = const 1
  t1 = const 2
  t2 = add t0, t1
  store l0, t2
  t3 = load l0
  call @print_int(t3)
  return
}
```

## レコード（M2 T3、ADR-0007 / ADR-0009）

- `Program.records` にフィールド（名前と型、名前順）を登録する。値型で、受け渡しはコピー。
- 命令: `make_record rN(t...)`（フィールドの順に値を並べて作る）、`field t, i`（i 番目のフィールド）。
- 幅部分型の変換は、変換先のフィールドを `field` で取り出して `make_record` で作り直す。
- LLVM では `%record.N = type { ... }`。Union の中身の大きさは、レコードのフィールドの語数の和で見積もる。

## `Unknown`（M2 T5、ADR-0011）

- IR には `Unknown` 型はない。lowering が `int | bool | str | float | None` の Union に置き換える（`float` は ADR-0019 で足した）。
- 外部入力は組み込み関数（`arg_count`、`arg_exists`、`arg_str`、`scalar_kind`、`scalar_int`、`scalar_bool`）を lowering が組み合わせて作る。ランタイムとの境界はスカラーとポインタだけ。

## `wys test`（M2 T7）

- テストのときは、`main` の代わりに `__wys_tests` をエントリにする。これは `test_index`（環境変数 `WYS_TEST_INDEX`）の番号のテスト関数を1つだけ呼ぶ。
- `assert` は分岐と `assert_fail`（場所の文字列を渡す、戻らない）になる。

## Generics（M2 T4）

- IR に型パラメータは現れない。lowering が、使われた型引数の組み合わせごとに関数を単相化する（`unwrap_or__1`、`unwrap_or__2` のように名前に番号を付ける）。
- Generic でない関数を先に登録し、Generic 関数のインスタンスは呼び出しを見つけたときに作業キューに積む。
- Union とレコードの IR の ID は、実際に使われた具体的な型にだけ、使われた順に割り当てる（型パラメータを含む型は IR に入らない）。

## 構造を持つ値の比較（M3 T10b、ADR-0016）

- `eq` / `ne` は、`int` / `bool` に加えて、すべての型の値どうしに使える（被演算子の型は同じ）。`str` は `@wys_str_eq`、`list` / `dict` / Union / レコードは、バックエンドが型ごとに生成する `@wys_eq_list{i}` / `@wys_eq_dict{i}` / `@wys_eq_u{i}` / `@wys_eq_r{i}` を呼ぶ。生成するのは、比較に使われる型とその構成要素の型だけ。
- 比較は構造的: `list` は長さと順序どおりの要素、`dict` は要素数と、各キーが相手にあって値が等しいこと（順序は問わない）、Union（`Result` を含む）はタグと payload、レコードはフィールドごと。
- 被演算子は借用で読み、参照カウントは変えない。型の違う値の比較（`o == 1`）は、lowering が広い方の Union に揃えてから比べる。

## float（M4 T12、ADR-0019）

- 型 `float` は IEEE 754 の倍精度の値型（LLVM の `double`）。定数は `f64` のビット列で持ち、`dump-ir` には 10 進で出す。
- `add` / `sub` / `mul` / `div` と比較を `float` どうしに使える。`div` は `float` だけで、`floordiv` / `mod` は `int` だけ。結果は IEEE 754 のとおり（あふれは無限大）。`div` はランタイムの `@wys_float_div` を呼び、0 での割り算を実行時エラーにする。比較は順序付き（`fcmp olt` など。NaN を含めば偽）で、`ne` だけは `une`（NaN を含めば真）。
- 単項の `-` は lowering が `-0.0 - x` にする（`0.0 - x` だと負のゼロの符号を失う）。
- 組み込み関数 `float_from_int`（`sitofp`）、`int_from_float`（`@wys_float_to_int`。NaN・無限大・範囲外は実行時エラー）、`print_float`、`scalar_float`（`parse_scalar`。`scalar_kind` の 4）。表示は Python の `repr` と同じ書式（ランタイムの `float_repr`）。

## Python のハンドル（M4 T5、ADR-0018）

- 型 `py` は、Python のオブジェクトを包んだハンドル（ヒープオブジェクトへのポインタ1語）。retain は本言語の参照カウントだけを増やし、release は `@wys_py_release`（参照カウントが 0 になったら `Py_DecRef` してから解放する）。比較と表示はできない（型検査が E1202 で止める）。
- 組み込み関数 `py_import(str)`、`py_getattr(py, str)`、`py_call(py, list[py])`、`py_from_int` / `py_from_bool` / `py_from_str`、`py_none()` は、新しいハンドルを +1 所有で返す。`py_to_int` / `py_to_bool` / `py_to_str`（+1 所有の文字列）は値を読み、`py_is_none` は失敗しない。
- 失敗し得る操作の直後に `py_failed()` で確かめ、失敗なら `py_err_kind()` / `py_err_message()` / `py_err_traceback()`（+1 所有の文字列）で理由を読む。lowering がこれで `Result[.., PyError]` を作る（T6 / T7）。失敗したときの戻り値（null のハンドル、`0` など）は使わない。null のハンドルの retain / release は何もしない。
- Python を使うプログラム（`wys_ir::uses_python`: Python の組み込みを呼ぶか、`py` 型を使う）だけ、バックエンドが Python のランタイムの宣言と、`main` の終わりの `@wys_py_finish`（`Py_FinalizeEx`）を入れ、driver が `libwys_runtime_python.a` と `libpython3.14` をリンクする。それ以外のプログラムの生成コードとリンクは変わらない。

## 構造を持つ値の表示（M3 T10c、ADR-0016）

- 組み込み関数 `print_value` は、レコード・`list`・`dict`・Union（`Result` を含む）の値を1つ受け取り、表示して改行する。`int` / `bool` / `str` は従来どおり `print_int` / `print_bool` / `print_str`。トップレベルの Union は、lowering がタグで分岐して、構成要素ごとにどちらかを呼ぶ。
- IR の型は構造だけを持つので、表示に要る名前は `Program.display` に置く。`record_names` はレコード型の名前（`records` と同じ順。最初に定義した `type` の名前で、型引数は含まない）、`results` は `Result` を表す Union の一覧（構成要素 0 が `Ok`、1 が `Err`）。型の同一性や配置には関係しない。検証器は、名前の数がレコード型の数と同じこと、`results` が構成要素2つの Union を指すことを確かめる。`dump-ir` には出さない。
- バックエンドは、表示に使われる型とその構成要素の型ごとに `@wys_show_list{i}` / `@wys_show_dict{i}` / `@wys_show_u{i}` / `@wys_show_r{i}` を生成する。これらは改行せずに書き出し、構造の中の `str` は `@wys_write_str`（引用符とエスケープ）、区切りや名前は定数のバイト列を `@wys_write_bytes` で書く。文字列を組み立てないので確保がなく、値は借用で読む（参照カウントは変えない）。

## ARC（M3 T2a、ADR-0012）

- `str` はヒープオブジェクト `{ 参照カウント, 長さ, バイト列 }` へのポインタ。文字列リテラルは参照カウント −1 の不死の定数、コマンドライン引数も起動時に不死オブジェクトにする。
- 命令 `retain t` / `release t` は、lowering の後に `wys_ir::arc::insert` が挿入する。規則はモジュールの先頭コメント（memory-model 2.2 のとおり）。
- **Move 最適化（M4 T3、memory-model 2.3）:** `insert` の後に `wys_ir::arc::optimize` が冗長な組を取り除き、その後に検証器（`arc::verify`）が釣り合いを確かめる。`WYS_ARC_OPT=0` で止められる。取り除いた操作は `Function::elided_rc` に記録し、`wys explain` の `elided` に出す（項目の追加なので版は 0 のまま）。取り除いた理由（`elision`）:

  | id | 内容 |
  |---|---|
  | `param_never_reassigned` | 関数の中で代入しない引数のスロットは、呼び出し側の参照を借りる。入口の `param_slot_entry` と出口の `slot_at_function_exit` の組を取り除く |
  | `moved_from_slot` | ローカル変数の最後の使用（その後どの経路でもスロットの今の値を読まない。ブロックごとの後ろ向きの生存解析で、ループの戻り経路を含む）で、借用を retain して1回だけ消費するなら、`load` を `take`（値を +1 所有で取り出し、スロットを空にする）にして retain を取り除く。スロットの後の release（上書きと関数の出口）は空の値に対するもので、何もしない。代入しない引数（呼び出し側の参照を借りているスロット）は対象外 |
  | `borrow_guard_unneeded` | 可変な場所から読んだ値（`list_get` / `dict_get` / `dict_key_at`）を、最後の使用までに場所を変え得る命令（関数の呼び出し、`py_*`、要素の書き換え、release）がないなら、`borrow_from_mutable_place` の retain とブロックの終わりの release の組を取り除く。最後の使用が関数の呼び出しの引数なら残す。値から取り出したもの（`Field` / `UnionPayload`。`xs[0].name`）の使用も、その値の使用として数え、終端命令で使う場合も残す |
- `wys_ir::arc::verify` が、ブロックごとに参照カウントの釣り合いを検査する（driver が毎回実行する）。
- 各 `retain` / `release` は、挿入した理由（`RcReason`、ADR-0017）を持つ。バックエンドは理由を読まないので、生成コードは変わらない。IR テキストでは `retain t3 ; borrow_from_mutable_place` のように末尾に出す（消費の理由は `consume_borrowed:store` のように消費する地点を付ける）。命令を動かしたり消したりする後続のパス（M3b の Move 最適化）は、理由を保つ。

  | 命令 | 理由 | 挿入する地点 |
  |---|---|---|
  | retain | `param_slot_entry` | 関数の入口で、引数をスロットに所有させる |
  | retain | `consume_borrowed:<地点>` | 借用した値（ローカルからの読み出し、リテラル、取り出した値、不死の文字列）を +1 で消費する。地点は `store` / `return` / `make_union` / `make_record` / `list_new` / `list_set` / `list_push` / `dict_new` / `dict_set` |
  | retain | `consume_shared_owned:<地点>` | すでに消費した +1 所有の値を、もう一度消費する |
  | retain | `borrow_from_mutable_place` | `list_get` / `dict_get` / `dict_key_at` の直後（借用の安全条件） |
  | release | `store_overwrites_slot` | `store` で、スロットの古い値を手放す |
  | release | `unconsumed_owned_at_block_end` | ブロックの終わりまで消費されなかった +1 所有の値 |
  | release | `slot_at_function_exit` | 関数を抜けるときに、スロットが持つ値を手放す |
- 解放が必要かどうか（`DropInfo`）は Union / レコードごとにメモ化して計算する。バックエンドは、解放が必要な Union とレコードについて型ごとの `@wys_retain_*` / `@wys_release_*`（drop glue）を生成する。
- 解放が必要なローカルは入口でゼロ（null）に初期化する。生成した `main` は最後に `wys_runtime_finish` を呼び、解放されていないオブジェクトが残っていれば終了コード 102 で知らせる（コンパイラのバグ）。

## Effect（M3 T9b、ADR-0014）

- 関数は `uses` 節で宣言した Effect を属性 `Function.effects` に持つ。注釈がなければ `None`、`uses pure` なら空。Generic 関数のインスタンスは、宣言した Effect を引き継ぐ。
- IR テキストでは `fn f0 log(l0: str) uses io {` のように、戻り値の型の後に出す（`uses pure` も同じ）。
- バックエンドは読まないので、生成コードは変わらない。M5 の Capability Runtime や M6 の推論が、この属性を使う。
