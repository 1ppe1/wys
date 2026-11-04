# 共通IR 設計メモ（M2 T1b 時点）

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

`int`（i64）、`bool`、`str`（定数文字列へのポインタと長さ）、Union（`u0`, `u1`...）。`None` 型の式は一時値を作らない。

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

## 今後（M2 以降）

- Effect の置き場所（関数シグネチャの属性）を確保する。
- ARC 導入時（M3）に、retain/release 命令とヒープ型を追加する。
