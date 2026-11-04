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
- ランタイム（C ABI）には Union を渡さない。`print` は Union のタグで分岐して、構成要素ごとの組み込み関数を呼ぶ。

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

- IR には `Unknown` 型はない。lowering が `int | bool | str | None` の Union に置き換える。
- 外部入力は組み込み関数（`arg_count`、`arg_exists`、`arg_str`、`scalar_kind`、`scalar_int`、`scalar_bool`）を lowering が組み合わせて作る。ランタイムとの境界はスカラーとポインタだけ。

## `wys test`（M2 T7）

- テストのときは、`main` の代わりに `__wys_tests` をエントリにする。これは `test_index`（環境変数 `WYS_TEST_INDEX`）の番号のテスト関数を1つだけ呼ぶ。
- `assert` は分岐と `assert_fail`（場所の文字列を渡す、戻らない）になる。

## Generics（M2 T4）

- IR に型パラメータは現れない。lowering が、使われた型引数の組み合わせごとに関数を単相化する（`unwrap_or__1`、`unwrap_or__2` のように名前に番号を付ける）。
- Generic でない関数を先に登録し、Generic 関数のインスタンスは呼び出しを見つけたときに作業キューに積む。
- Union とレコードの IR の ID は、実際に使われた具体的な型にだけ、使われた順に割り当てる（型パラメータを含む型は IR に入らない）。

## ARC（M3 T2a、ADR-0012）

- `str` はヒープオブジェクト `{ 参照カウント, 長さ, バイト列 }` へのポインタ。文字列リテラルは参照カウント −1 の不死の定数、コマンドライン引数も起動時に不死オブジェクトにする。
- 命令 `retain t` / `release t` は、lowering の後に `wys_ir::arc::insert` が挿入する。規則はモジュールの先頭コメント（memory-model 2.2 のとおり）。
- `wys_ir::arc::verify` が、ブロックごとに参照カウントの釣り合いを検査する（driver が毎回実行する）。
- 解放が必要かどうか（`DropInfo`）は Union / レコードごとにメモ化して計算する。バックエンドは、解放が必要な Union とレコードについて型ごとの `@wys_retain_*` / `@wys_release_*`（drop glue）を生成する。
- 解放が必要なローカルは入口でゼロ（null）に初期化する。生成した `main` は最後に `wys_runtime_finish` を呼び、解放されていないオブジェクトが残っていれば終了コード 102 で知らせる（コンパイラのバグ）。

## 今後（M2 以降）

- Effect の置き場所（関数シグネチャの属性）を確保する。
- ARC 導入時（M3）に、retain/release 命令とヒープ型を追加する。
