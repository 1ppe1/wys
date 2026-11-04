# ADR-0006: Union / None / Generics / エラー処理の構文

- **状態:** Proposed
- **日付:** 2026-11-04
- **関連:** 未決定事項 3、LANG-003, LANG-004, LANG-006, LANG-008, AC-002, AC-003

## 背景

LANG-003/004 は Null 安全性、Union 型、Generics、パターンマッチと網羅性検査を求めている。
構文は Python 利用者になじみがあり、AI が生成しやすく、型検査で検証しやすいものにしたい。
要件定義書 12 章の構文イメージは非規範的なので、ここで初めて構文を決める。

## PoC2 の結果（`crates/wys_typeck/src/experimental/narrowing.rs`）

- `if x is None: return` の後で `x` を `T` に絞り込む流れ解析は、PoC1 の確定代入解析と同じ「環境を流す」構造で実装できた。
- 分岐の合流で型を和集合に戻すこと、再代入で絞り込みを解除することも確認した。
- Union に対する `match` の網羅性検査（足りない構成要素の列挙）と、到達しない分岐の検出を実装できた。

## 選択肢と推奨

| 項目 | 推奨 | 主な代替案 | 推奨の理由 |
|---|---|---|---|
| Union / Optional | `int \| str`、`str \| None` | `Optional[str]`, `str?` | Python 3.10 以降の型ヒントと同じ。`?` 記法は Python 利用者にとって新しい |
| None の検査 | `if x is None:` / `if x is not None:` による絞り込み | 専用演算子（`?.`, `!!`） | Python と同じ書き方で、PoC2 で流れ解析が可能と確認した。`?.` などは後から糖衣として追加できる |
| パターンマッチ | `match x:` / `case int():` / `case None:` / `case _:` | 独自構文 | Python 3.10 の `match` に合わせる。網羅性検査はコンパイル時にエラーにする |
| Generics | `def first[T](xs: list[T]) -> T:`、`type Pair[A, B] = ...` | `TypeVar` 方式 | Python 3.12 の PEP 695 構文。宣言が局所的で、AI が生成しやすい |
| Generics の実装 | 単相化（monomorphization） | 型消去 | ネイティブ性能（COMP-002）を優先する |
| エラー処理 | `Result[T, E]` 型と、伝搬用の後置演算子 `?` | 例外（`try` / `except`） | 失敗を型で表す（LANG-008、NFR-007）。例外は FFI 境界で `Result` に変換する（AC-010） |
| `Unknown` | `Unknown` 型。`match` または検証関数でのみ具体型に絞り込める | `Any` | `Any` は検査を無効にする印象が強い。LANG-006 の「明示的な検証」を名前で示す |

## 決定

（ユーザーレビュー待ち）上表の「推奨」の列を採用する。

## 影響

- 構文はなるべく Python の型ヒントと同じにする。違う点（`?` 演算子、`Unknown`、網羅性エラー）は言語仕様で明示する。
- `T | None` の値表現は `memory-model.md` 2.5 に従う。
- `case` のパターンで区別できるのは名目的な型だけとする（ADR-0007）。
- 見直し条件: M2 の実装で、`?` 演算子と Python の `match` 構文が衝突する箇所が見つかった場合。
