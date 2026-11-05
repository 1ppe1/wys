# ADR-0021: JSON と `Unknown` のコンテナ、呼び出しの型引数の明示

- **状態:** Accepted
- **日付:** 2026-11-05
- **関連:** AI-002、AC-006、LANG-006、AC-003、MEM-005、ADR-0011（見直し条件）、ADR-0012（§5 生存数）、ADR-0015、ADR-0016（論点 23）、ADR-0018、ADR-0019、`docs/design/memory-model.md`（2.6）、`docs/plans/m4.md`（見送り）、`docs/plans/m5.md`（T2・T4〜T6）、issue #63

## 背景

M5 の AI SDK は、LLM の出力（JSON）とツールの引数（JSON）を宣言した型で検証する（AI-002、AC-006）。いまの言語には JSON を受ける型がない。

- `Unknown`（ADR-0011）はスカラーだけ（`int` / `bool` / `str` / `float` / `None`）。lowering は `Unknown` を、この 5 つの Union に置き換えている（`wys_lower` の `unknown_union`）。ADR-0011 の見直し条件は「`list` / `dict` と JSON の入力を入れるとき、`Unknown` の構成要素を増やし、検証 API をスキーマ検証（AI-002）と共通化する」。
- 利用者定義の再帰レコード（`list` を経由するものを含む）は E0504 で拒否している。M4 の計画で「`weak` と一緒に、M5 の JSON とスキーマ検証のときに 1 つの ADR で決める」と見送った（ADR-0016 論点 23、memory-model 2.6）。
- Generic 関数の型引数は引数から推論するだけで、呼び出しに型引数を書けない。`validate[Plan](u)` のように引数から決まらない型引数は E0403 になる。
- ARC の安全網は、IR の検証器・終了時の生存数（102）・AddressSanitizer の 3 つ。循環参照は回収しない（memory-model 2.6）。

この ADR は、JSON の受け皿、`Unknown` の拡張の表現と規則、JSON の入出力の組み込み、呼び出しの型引数の明示、再帰レコードと `weak` の扱い、終了コード 102 のメッセージを決める。スキーマ型の規則・`validate[T]`・`to_json`・JSON Schema の生成（T6）の詳細と、LLM の API は ADR-0022 で決める。この ADR では、それらが乗る土台（`Unknown` と型引数）だけを決める。

## 選択肢と決定

| 論点 | 決定（推奨） | 主な代替案 | 理由 |
|---|---|---|---|
| 1. JSON の受け皿 | **`Unknown` を拡張する。** 種類に `list[Unknown]` と `dict[str, Unknown]` を足す。JSON の値（null / bool / 数値 / 文字列 / 配列 / オブジェクト）は、そのまま `Unknown` の 7 つの種類（`None` / `bool` / `int` / `float` / `str` / `list[Unknown]` / `dict[str, Unknown]`）に対応する | (a) 名目的なハンドル型 `Json`（`PyObject` と同じ方式。`get(key)` / `index(i)` / `to_int()` などが `Result` を返す）。(b) 新しい型 `Json` を `Unknown` と別に作り、`match` で絞り込む | ADR-0011 の見直し条件どおり。外部の値を絞り込む方法が `match` 1 つのまま（ADR-0011「方法を 1 つにして、AI が生成するコードを単純にする」）。(a) はメソッドの数だけ API が増え、`match` と別の書き方になる。(b) は `Unknown` と役割が重なる。PoC4（T1）の転換基準 (c) で ARC の不変条件が崩れたときは (a) に移る |
| 2. 絞り込み | `match` に `case list():` と `case dict():` を足す。**被検査値が `Unknown`（または `Unknown` を含む Union）のときだけ**書ける。ほかの型（`list[int] \| None` など）では、いまと同じく E0203（型パターンに書ける型は `int` / `bool` / `str` / `float` と、`Unknown` に対する `list` / `dict`）。絞り込んだ後の型は `list[Unknown]` / `dict[str, Unknown]` で、要素はやはり `Unknown`（もう一度 `match` する）。`case list[int]():` のような要素の型を書くパターンは**書けない**（要素ごとの検証が要るので、それは `validate[T]` の仕事。E0101）。`case _:` 必須（E0602）と、決して一致しないパターン（E0305）の規則は変えない。`list` / `dict` の値は `Unknown` の場所に置けない（いまの規則のまま。ADR-0011「`Unknown` への代入」はスカラーだけ。`list[Unknown]` も置けない。E0201）。そのため、絞り込みの中で `v: Unknown = xs`（`xs` が `list[Unknown]` に絞られている）も E0201 で、スカラーに絞った場合と非対称になる（元の `Unknown` の変数を使えばよい）。コンテナの種類を持つ `Unknown` を作れるのは `parse_json` だけ | (a) `case list[int]():` を書けるようにする。(b) `list[Unknown]` / `dict[str, Unknown]` を `Unknown` に置けるようにする。(c) `list[int] \| None` などにも `case list():` を許す | (a) は要素の数だけ実行時に検査が要り、パターン 1 つで O(n) になる。検証の場所を `validate[T]` 1 つに集める。(b) は `xs.append(xs)` で循環を作れる（論点 4）。テストのために `Unknown` を組み立てたいときは `parse_json` に文字列を渡す。(c) は既存の `is None` と `case` の絞り込みで足り、規則が増えるだけ |
| 3. 実行時表現 | `Unknown` は IR の**独自の型** `Ty::Unknown` にする（いまの「5 つの Union に置き換える」をやめる）。値はタグ 1 バイト + 1 語（いまの Union と同じ配置）で、タグに `list` / `dict` の 2 つを足す。`list` / `dict` の種類の 1 語は、既存の `list[Unknown]` / `dict[str, Unknown]` のヒープオブジェクト（ADR-0012 のヘッダ付き）へのポインタ。retain / release はタグで分岐し、参照型の種類（`str` / `list` / `dict`）だけを数える。`list[Unknown]` の解放処理は要素の `Unknown` を release する（型ごとの解放処理を 1 つずつ生成する。再帰はこの 1 組の型の間に閉じる） | Union のまま種類を足す | Union の構成要素に `list[Unknown]` を入れると、`Unknown` の展開が自分自身を含んで終わらない（`unknown_union` が再帰型になる）。名前を持つ型にすれば、再帰は「`Unknown` → `list[Unknown]` → `Unknown`」という型の ID の参照で閉じ、型の表は有限のまま。配置を変えないので、既存の `Unknown` のスカラーのコードは同じ機械語になる |
| 4. 循環と生存数 | **不変条件: `Unknown` の値から到達できる `list` / `dict` のオブジェクトは、決して変更されない**（`Unknown` は変更のできない木）。これを型の規則で守る: **要素の型（`dict` は値の型）が `Unknown` を含む `list` / `dict`（型検査の `contains_unknown` が真。`list[Unknown]`、`dict[str, Unknown]`、`list[Unknown \| None]`、`list[list[Unknown]]` など）は、型として読み取り専用**にする。`append` / `pop` / 添字への代入 / `dict` の代入はすべて E1503。値に印を付けないので、代入・引数・`for`・レコードのフィールド・Generics（`Pair[list[Unknown]]`）・`Result` / `Task` の中・`d[k]` / `d.get(k)` の戻り値のどれを通っても同じ型で検査される。そうした `list` / `dict` を作れるのは、リテラル（`[u1, u2]`、`{"k": u}`）・`parse_json`・`validate[T]`（ADR-0022。木の `list` / `dict` を共有して返してよい）だけ。`Unknown` のコンテナは `parse_json` だけが作る（論点 2）ので、木の中に外側の値を入れる経路はない。読み取り専用がないと、`o = parse_json("[[]]")?` の内側 `ys` に絞り込んで `ys.append(o)` とすれば循環になる | (a) 書き換えを許し、循環はリークになり得ると書く。(b) 書き換えのたびに循環を検査する。(c) `case list():` と `validate` が浅いコピーを返す（型の規則は増えないが O(n)。`validate` のフィールドも毎回コピー） | (a) は生存数 0 の基準（ADR-0012 §5）を利用者のコードで崩せることになり、102 の検査の意味が「コンパイラの誤り」から外れる。(b) は O(n) の検査が要る。(c) はコピーを忘れた経路が 1 つでもあれば穴になる。型の規則なら検査の漏れがない。JSON は「読んで検証する」ためのもので、書き換えて組み立て直すのは具体的な型（`list[str]` など）に `validate` してからでよい |
| 5. `parse_json` | 組み込み `parse_json(s: str) -> Result[Unknown, JsonError]`（`JsonError = {message: str, offset: int}`。`offset` は UTF-8 のバイト位置）。規則: RFC 8259。数値は、小数点も指数もなく `i64` に収まるなら `int`（`-0` も `int` の 0）、それ以外で `f64` に表せるなら `float`（`1.0`・`1e2`・`i64` を超える整数）。`f64` で表せない値（`1e400`）は**エラー**。重複したキーは**エラー**（`duplicate key "k"`）。入れ子の深さの上限は 128（超えたら `nesting too deep`）。先頭の BOM は受け付けない。Effect はなし（純粋）。ランタイムの実装には **`serde_json`** を使う（`wys_runtime` の依存に足す）。ただし `serde_json::Value` は使わない（重複キーを後勝ちで黙って通し、`-0` を `float` にする）。`Deserializer` を独自の visitor で駆動して、`Unknown` の木を直接組み立てる。`serde_json` のエラーの行・列はバイト位置に換算し、深さの上限のエラー（`recursion limit exceeded`）は上の文言に写す | (a) 重複キーは後勝ち（JSON.parse / Python と同じ）。(b) 手書きの JSON パーサ | (a) は LLM の出力で同じキーを 2 回書かれたときに、検証を通った値がどちらの値か読む側で分からなくなる。失敗にするほうが AC-006 の「定義済みの失敗」に合う。深さの上限はスタック（タスクのスタックは 1 MiB、ADR-0020 論点 3。木の解放処理も同じ深さで再帰する）を守るため。(b) は検証済みのクレートより誤りやすい。ADR-0018 / 0020 の「外部クレートを使わない」は `unsafe` / FFI / アセンブリについての方針で、純 Rust のクレートは別（生成した実行ファイルの依存は変わらない。AC-004） |
| 6. `Unknown` の表示と比較 | 変えない。`print` / `==` / 演算 / `dict` のキーに `Unknown` は使えない（E0601）。`list[Unknown]` / `dict[str, Unknown]` も、要素が `Unknown` なので `print` / `==` は E0601。JSON の文字列に戻すのは `to_json`（ADR-0022） | `print` を JSON の表示にする | 未検証の値を出力に流さない（AC-003 の考え方）。表示は検証した後の型で行う |
| 7. `dict[str, Unknown]` の読み方 | `d.get(k)` は **E0908**（ADR-0015 論点 7 の「値が `None` を含む `dict` の `.get`」と同じ扱い。`Unknown` は JSON の `null` を持つので、「キーがない」と区別できない）。キーの有無は `k in d` で確かめ、`d[k]` で読む。`d[k]` はキーがなければ、いまと同じく実行時エラー（101）なので、未検証の入力を `match` で読むときは `in` を先に書く。外部の入力を広く読むなら `validate[T]` に任せる（論点 2 の方針） | `d.get(k)` を `Unknown \| None` で返す | `Unknown` が `None` を含むので、`Unknown \| None` は `Unknown` と同じ型に平坦化され、区別が消える |
| 8. `parse_scalar` / `arg` | 変えない。`parse_scalar` はスカラーだけを返す（`[1]` という文字列は `str`） | `parse_scalar` を `parse_json` にする | 既存のプログラムの意味を変えない |
| 9. 呼び出しの型引数の明示 | `f[T, U](x)` の形で、呼び出しに型引数を書ける。文法: `primary = IDENT [ type_args ] "(" [ args ] ")" \| ...`、`type_args = "[" type { "," type } "]"`。`IDENT "["` の後を型の並びとして読み、`"]"` の直後に `"("` が続かなければ巻き戻して添字（`xs[i]`）として読む。修飾名（`m.f[T](x)`）も同じ（`.` IDENT の後も同じ先読みで `a.b[i]` と区別する）。`type_args` の末尾のカンマは、型の注釈の型引数と同じ規則にする。関数値がない（E0213）ので、`a[b](c)` の形はいままで合法でなく、この先読みで一意に決まる。`xs[i](1)` と書いた場合の診断は「`xs` は Generic の関数ではない」（E1502 か、関数でなければ E0213）を優先し、「`i` は型ではない」にはしない。メソッドの呼び出し（`x.m[T](...)`）には書けない（E0101。組み込みのメソッドに Generic なものがない）。規則: 型引数を書いたら全部書く（個数が違えば E0402）。書いた型引数は推論より優先し、引数の型と合わなければ通常の型の不一致（E0201）。Generic でない関数に書いたら E1502。**レコードの構築子にも同じ文法で型引数を書けるようにする**（いまは書けない。`Pair[int](...)` は添字として読まれて E0101）。構築子でも、書いた型引数が推論より優先し、個数が違えば E0402 | (a) 期待型から推論する（`let p: Plan = validate(u)?`）。(b) 型を値として渡す（`validate(u, Plan)`） | (a) は `?` や `await` をまたぐと期待型が決まらず（`validate(u)?` の期待型は `Result[Plan, E]` ではなく `Plan`）、推論の規則が複雑になる。(b) は型を値にする仕組みが要る。要件定義書 12 の非規範的な例（`llm.generate[ResearchResult](...)`）とも合う。E0403 のヒントで `f[T](...)` を示せる |
| 10. 利用者定義の再帰レコードと `weak` | **M5 では入れない。** E0504 は残す。再帰的なスキーマ（木構造など）が要るときは、再帰する部分を `Unknown` のフィールド（「任意の JSON」）にして、使う側で `validate[T]` をもう一度呼ぶ | (a) `list` を経由する再帰レコードを入れ、循環参照のリークを許す（memory-model 2.6 の「MVP では回収しない」）。(b) (a) に加えて `weak` を入れる | 再帰レコードは循環参照を作れるようにする（可変な `list` のフィールドに自分を入れる）ので、生存数 0 の基準（ADR-0012 §5）を利用者のコードで崩せる。そうなると 102 が「コンパイラの誤り」か「利用者の循環」か区別できない。M5 で要る再帰は JSON だけで、それは組み込みの `Unknown` に閉じる（論点 3・4）。`weak` は循環を作れる型ができてから要る |
| 11. 終了コード 102 のメッセージ | 「`N object(s) were not freed (this is a compiler bug)`」を「`N object(s) were not freed (a reference cycle through a Task result, or a compiler bug)`」に変える。この ADR の後も、利用者のコードで作れる循環は `Task` の結果を経由するもの（memory-model 2.6。結果の `list` に自分の `Task` を入れる）だけで、それ以外で生存数が残るのはコンパイラかランタイムの誤り | (a) 変えない。(b) 「循環参照の可能性」を一般的に書く | (a) は `Task` の循環を書いた利用者に「コンパイラの誤り」と告げてしまう。(b) は論点 4・10 で一般の循環を閉じたので不正確。原因を 2 つに限って示す |
| 12. 見直し条件 | 再帰的なスキーマの需要が `Unknown` で逃がせないほど出たとき（論点 10 を (a) / (b) に変え、102 の文言を見直す）。Phase 3 の循環回収器の評価（memory-model 2.6）のとき。`Unknown` の拡張で ARC の不変条件が崩れたとき（PoC4 の転換基準 (c)、論点 1 の (a) へ） | — | — |

## 新しいエラーコード

| コード | 内容 |
|---|---|
| E1502 | Generic でない関数・組み込みに型引数を書いている（`f[int](x)` で `f` が Generic でない）。ヒントで型引数を外す修正を示す |
| E1503 | 要素（`dict` は値）の型が `Unknown` を含む `list` / `dict` を書き換えている（`append` / `pop` / 添字への代入）。ヒント: `validate[T]` で具体的な型にしてから変更する（論点 4） |

E1501（スキーマ型でない型）は ADR-0022 で決める（同じ E15xx 帯）。`case list[int]():` は構文エラー（E0101）。型引数の個数は E0402、推論できないときは E0403（ヒントに `f[T](...)` を足す）。

## 決定

上表の「決定（推奨）」の列を採用する（ユーザー決定、2026-11-05）。

## 影響

- 実装は `docs/plans/m5.md` の T4（呼び出しの型引数）と T5（`Unknown` のコンテナと `parse_json`）。T0 で E0504 のメッセージをこの ADR を指すように直す。
- IR に `Ty::Unknown` を足す（Union のまま `list[Unknown]` を入れると、`ir_ty` → `ir_union` → `ir_list` → `ir_ty` が終わらず、検証器の「構成要素の Union は自分より前」の規則とも衝突する）。lowering の `unknown_union` と `norm`、`coerce` の `MakeUnion` / `UnionIs` / `UnionPayload` に `Unknown` の分岐を足し、検証器・ARC パス・バックエンド（タグで分岐する retain / release、`list[Unknown]` / `dict[str, Unknown]` の解放処理）・`wys explain` を更新する。`Unknown` の `needs_drop` は定数で真とする（型の登録時に再帰を辿らない）。配置（タグ 1 バイト + 1 語）はいまの Union と同じで、タグの番号もいまの順に揃えるが、`dump-ir` と `explain` のスナップショットは変わる。
- `wys_runtime` が `serde_json` に依存する（純 Rust。生成した実行ファイルの依存は変わらない）。
- 読み取り専用の規則（論点 4）は型に付ける（`contains_unknown` を使う）。値に印は持たせない。
- Python との境界（ADR-0018）は変えない。Python → 本言語の `list` / `dict` は、いまと同じく `to_list` / `to_dict` か、ハンドルのまま返す。`Unknown` のコンテナを Python から作る経路は足さない（JSON が要るなら Python 側で `json.dumps` して `parse_json` に渡す）。
- 更新する文書: `grammar.ebnf`（`type_args` の呼び出し、`case list():`）、`error-codes.md`（E1502 / E1503、E0504 の文言、E0403 のヒント）、`docs/design/ir.md`（`Ty::Unknown`）、`memory-model.md`（2.6 の再帰レコードと `weak`、3 の「メモリリーク」の行）、用語集（スキーマ型は ADR-0022）、ADR-0011 に「見直し済み（ADR-0021）」の注記を足す（Accepted の ADR の本文は書き換えない。状態の欄の後に追記するだけ）。

## 訂正（2026-11-05、ユーザー承認）

論点 11 の前提「利用者のコードで作れる循環は `Task` の結果を経由するもの」は誤りだった（memory-model 2.6 の M4 での記述に従っていた）。`Task` の結果から自分の `Task` に届くには `Task[list[Task[...]]]` のような再帰的な型が要り、再帰的な型は E0504 で拒否する。論点 4 で `Unknown` も変更のできない木になったので、利用者のコードで循環を作る経路はない。したがって 102 のメッセージは**変えない**（論点 11 の代替案 (a)）。生存数が残るのは、コンパイラかランタイムの誤りだけ。memory-model 2.6 の記述も直した。本文の表は Accepted のときのまま残す。
