# エラーコード一覧

コンパイラが出す診断のコード。`check --format json` の `code` フィールドに入る（[diagnostics-schema.json](diagnostics-schema.json)）。

- 一度割り当てたコードの意味は変えない。使わなくなったら「廃止」と書いて残す（要件IDと同じ運用）。
- 範囲: `E00xx` 字句解析、`E01xx` 構文解析、`E02xx` 型検査、`E03xx` 絞り込み・`match`、`E04xx` Generics、`E05xx` レコード、`E06xx` `Unknown`、`E07xx` モジュール（ADR-0010）、`E09xx` コレクション（`list` / `for` / 添字）、`E10xx` `Result` と `?`（ADR-0013）、`E11xx` Effect（ADR-0014）、`E08xx` テスト（`wys test`）、`W02xx` 未使用の警告（ADR-0016）、`W03xx` 警告（M2 計画、`docs/plans/m2.md`）。
- メッセージは英語。コードと範囲で機械的に判別できるようにする。
- `wys explain E0201` で、下の表の行（段階と意味）を JSON で引ける（ADR-0017）。表の形式（`| コード | 段階 | 意味 |`）はツールの入力でもあるので、列を変えるときは `crates/wys_explain` も直す。

| コード | 段階 | 意味 |
|---|---|---|
| E0001 | 字句 | 想定外の文字 |
| E0002 | 字句 | 文字列リテラルが閉じていない |
| E0003 | 字句 | インデントを戻した位置が、外側のどのレベルとも一致しない |
| E0004 | 字句 | インデントにタブが使われている |
| E0005 | 字句 | 整数リテラルが 64 ビットに収まらない |
| E0006 | 字句 | 未知のエスケープシーケンス |
| E0008 | 字句 | ファイル末尾で `(` が閉じていない |
| E0009 | 字句 | 不正な浮動小数点リテラル（丸めると無限大になる `1e400`。ADR-0019） |
| E0101 | 構文 | 想定外のトークン（期待したものを示す） |
| E0102 | 構文 | 比較演算の連鎖（`a < b < c`）は未対応 |
| E0103 | 構文 | 引数に型注釈がない |
| E0104 | 構文 | 関数に戻り値の型注釈がない |
| E0105 | 構文 | 入れ子の関数定義は未対応 |
| E0106 | 構文 | トップレベルに文がある（関数の中に書く） |
| E0201 | 型 | 型が一致しない（代入、引数、戻り値、条件式など）。`print` が受け付けるのは `int` / `bool` / `str`、レコード、`list`、`dict`、`Result`、それらの Union で、型パラメータを含む値は表示できない（ADR-0016） |
| E0202 | 型 | 未定義の変数 |
| E0203 | 型 | 未知の型名 |
| E0204 | 型 | 代入されていない可能性がある変数を使っている |
| E0205 | 型 | 引数の個数が一致しない |
| E0206 | 型 | 値を返す関数が、`return` せずに終わる経路がある |
| E0207 | 型 | 演算子をその型に使えない（暗黙の型変換はしない）。`int` と `float` を混ぜた演算（`float(n)` / `int(x)` で変換する）、`int / int`（`//` か `float(a) / float(b)`）、`float` の `//` と `%` も含む（ADR-0019）。`is` を `None` 以外と比べた場合も含む。`==` / `!=` は、同じ型どうし（レコード・`list`・`dict`・`Result` は構成要素も比べられること）か、片方の構成要素がもう片方に含まれる Union どうし（`o == 1`）だけ。定義の違うレコード、型パラメータ、`None` との比較（`x == None`。`is None` を使う）は E0207（ADR-0016） |
| E0208 | 型 | 未定義の関数、または関数でないものの呼び出し |
| E0209 | 型 | 関数の重複定義、または組み込み関数の再定義 |
| E0210 | 型 | `main` がない、またはシグネチャが `def main() -> None` でない |
| E0211 | 型 | `return` の値の有無が戻り値の型と合わない |
| E0213 | 型 | 関数を値として使っている（未対応） |
| E0214 | 型 | 引数名の重複 |
| E0215 | 型 | 型が `None` だけの変数・引数・レコードのフィールド（`x: int \| None = None` のように Optional 型で注釈すれば可）。Generic な型に `None` を与えて、フィールドや `dict` の値が `None` だけになる場合（`Pair[bool, None]`、フィールドの値から型引数を `None` と推論した場合）も同じ |
| E0216 | 型 | 関数呼び出しにキーワード引数を使っている（キーワード引数はレコードの作成専用） |
| E0301 | 絞り込み | `None` の可能性がある値を、`None` を許さない場所で使っている（AC-002）。`if x is not None:` などで先に検査する。`print` の引数も、トップレベルの `T \| None` はこのエラーになる（構造の中の `None` は `None` と表示する）。レコードのフィールドの経路（`p.x`、`a.b.c`）も同じく絞り込める。添字の値（`xs[i]`、`d[k]`）は絞り込まないので、ローカル変数に取り出してから検査する（ADR-0016） |
| E0302 | 絞り込み | `None` になり得ない値を `is None` / `is not None` で検査している（結果が常に同じ） |
| E0303 | match | `match` が網羅的でない。足りない構成要素の `case` を修正ヒントに列挙する |
| E0304 | match | 構造的レコードを `case` のパターンに使っている（区別できない、ADR-0007） |
| E0305 | match | パターンの型が被検査値の型に含まれず、決して一致しない |
| E0401 | Generics | 型パラメータを含む値に、型パターン（`case int():` など）を使っている。Generic なコードはすべての型引数で動く必要がある |
| E0402 | Generics | 型引数の個数が合わない（`Pair[int]`、`int[str]`、型引数なしの `Pair`） |
| E0403 | Generics | 型引数を推論できない（引数から型パラメータが決まらない。`None` だけでは決まらない） |
| E0404 | Generics | 多相再帰: Generic 関数を呼ぶたびに型引数が大きくなり、単相化が終わらない（型の入れ子が32段を超えた） |
| E0501 | レコード | 存在しないフィールド（作成時のキーワード引数、または `p.x` の参照） |
| E0502 | レコード | レコードの作成でフィールドが足りない |
| E0503 | レコード | フィールドの重複（定義、または作成時の指定） |
| E0504 | レコード | 自分自身を参照する型（再帰的なレコードは、`list` を経由するものも含めてまだ未対応。M5 の JSON とスキーマ検証のときに `weak` と一緒に決める。ADR-0009、ADR-0016 論点 23、`docs/plans/m4.md`） |
| E0505 | レコード | レコードを位置引数で作成している（キーワード引数で書く） |
| E0508 | レコード | 1つの Union に2つ以上のレコード型がある（実行時に区別できない、ADR-0007） |
| E0601 | Unknown | 検証していない `Unknown` の値を、具体的な型が必要な場所（演算、`print`、引数、変数、フィールド参照、`is None`）で使っている（AC-003）。`match` で確かめる。`list[Unknown]` のように構造の中にある場合も、`==` と `print` では同じ |
| E0602 | Unknown | `Unknown` の `match` に `case _:` がない（開いた型なので、すべての種類を書いても必要、ADR-0011） |
| E0603 | Unknown | `Unknown` を Union に入れている（`Unknown \| None` など）。`Unknown` はすべての値を含み、`match` でだけ絞り込む（ADR-0011） |
| E0701 | モジュール | import したモジュールのファイルが見つからない（エントリファイルのディレクトリからの相対パス、ADR-0010） |
| E0702 | モジュール | `pub` が付いていない定義を、ほかのモジュールから使っている（修正ヒント: 定義に `pub` を付ける） |
| E0703 | モジュール | 循環 import（経路を示す） |
| E0704 | モジュール | import した名前（`from ... import` の名前、`as` の別名）が、モジュール内の定義か、別のモジュール・定義を指す別の import と衝突している（修正ヒント: `as` で別名を付ける。ADR-0016）。組み込み関数の名前（`len`、`Ok` など）は別名にできない。（M3 では、修飾名で呼ぶモジュールと同じ名前のローカル変数がある場合もこのエラーにしていた。M4 T4 で AST にメソッド呼び出しの印を付けたので廃止した。ローカル変数は、束縛した位置より後で同じ名前のモジュールを隠す） |
| E0705 | モジュール | import したモジュールに、その名前の定義がない |
| E0706 | モジュール | このモジュールで import していないモジュールを、修飾名（`lib.f`）で参照している。ほかのモジュールが読み込んでいても使えない。`import lib as l` の後の `lib.f` と、`import a.b` の後の `a.f`（`a` はモジュールの経路の途中）も同じ（修正ヒント: `import lib` を足す、別名で書く、または `a.b.f` と書く）。自分自身のモジュール名での修飾（`lib.wys` の中の `lib.g`）と、ローカル変数のメソッド呼び出し（`xs.append`）は対象外 |
| E0801 | テスト | `wys test` のテスト関数（`test_` で始まる関数）が、引数なし・戻り値 `None` になっていない |
| E0901 | コレクション | 空のリスト `[]` の要素の型が決まらない（`xs: list[int] = []` のように注釈する） |
| E0902 | コレクション | `range` を `for` の外で使っている |
| E0903 | コレクション | `for` で反復できない値（`list[T]` か `range(...)` だけ） |
| E0904 | コレクション | `list` 以外に添字を付けている |
| E0905 | コレクション | その型にないメソッド（`list` の `append`、`dict` の `get` と `pop`、`PyObject` の `attr` / `call` / `to_int` / `to_bool` / `to_str` / `is_none`）。`list` / `dict` のメソッドを、変数でない受け手（`f().append(1)`）に呼んだ場合も含む |
| E0906 | コレクション | 空の `{}` のキーと値の型が決まらない（`d: dict[str, int] = {}` のように注釈する） |
| E0907 | コレクション | `dict` のキーに使えない型（キーは `int` / `bool` / `str`。ADR-0015） |
| E0908 | コレクション | 値の型が `None` を含む `dict` に `.get` を使っている（「キーがない」と区別できない。`in` と `d[k]` を使う） |
| E0909 | コレクション | `in` の右辺が `dict` ではない |
| E1001 | Result | `Ok` / `Err` の、もう一方の型が期待型から決まらない（`r: Result[int, str] = Ok(1)` のように注釈する。ADR-0013） |
| E1002 | Result | `Result` の型引数の制約違反（エラー型が `None` や `Unknown`、1つの Union に `Result` が2つ） |
| E1003 | Result | `case Ok(x):` / `case Err(e):` を、型がちょうど `Result` ではない被検査値に使っている（先に `case Result():` や `is None` で絞り込む） |
| E1004 | Result | 束縛の個数や対象が正しくない（`Ok()`、`int(n)`。束縛を書けるのは `Ok` / `Err` だけ） |
| E1005 | Result | 束縛変数の名前が、既存の変数や引数と同じ |
| E1010 | Result | `?` を `Result` 以外に使っている（`T \| None` には使えない。ADR-0006） |
| E1011 | Result | `Result` を返さない関数（`main` を含む）で `?` を使っている |
| E1012 | Result | `?` の被演算子のエラー型が、関数が宣言したエラー型に収まらない（収まらない構成要素を列挙する） |
| E1101 | Effect | 未知の Effect 名（`io` / `exit` / `python` / `pure`。ADR-0014、`python` は ADR-0018） |
| E1102 | Effect | 呼び出し先の Effect を、呼び出し元の `uses` が宣言していない（修正案で `uses` 節を書き換える） |
| E1103 | Effect | `uses` を書いた関数から、`uses` のない関数を呼んでいる |
| E1104 | Effect | `uses` の並びが正しくない（同じ名前の重複、`pure` とほかの名前の併記） |
| E1201 | Python | `py_value` に渡せない型（レコード、`Result`、`Unknown`、型パラメータを含む値など。渡せるのは `int` / `bool` / `str` / `float` / `None` / `PyObject` と、それらを要素に持つ `list` / `dict`、構成要素がすべて渡せる Union（M4 T7）。ADR-0018） |
| E1202 | Python | `PyObject` か `PyError`（同じフィールドの構造的なレコードを含む）を、具体的な値が要る場所（`print`、`==` / `!=`、演算、`dict` のキー）で使っている。`PyError` はフィールド（`e.kind`、`e.message`、`e.traceback`）を読む。Python のコード（`__str__`、`__eq__`）を動かすことになるので、`to_int()` などで変換してから使う（ADR-0018） |
| E1301 | async | `await` か `spawn` を `async def` の外で使っている（`main` なら `async def main` にするよう促す。`related` に関数の定義。ADR-0020） |
| E1302 | async | `await` の被演算子が `Task` でも `async def` の関数の呼び出しでもない（同期の関数の呼び出しなら「`await` を外す」） |
| E1303 | async | `async def` の関数（`sleep_ms` を含む）の呼び出しを、`await` の直下か `spawn` の引数以外（式文、代入、引数、演算）に書いている（ヒント: `await f(...)` か `spawn(f(...))`） |
| E1304 | async | `spawn` の引数が `async def` の関数の呼び出し 1 つではない（`spawn(t)`、同期の関数の呼び出し、`spawn(await f())`、`spawn(sleep_ms(n))`（組み込みの中断点はタスクにできない）、引数が 1 つでない） |
| W0201 | 未使用（警告） | 代入されたが一度も読まれないローカル変数（`for` の変数、`case Ok(x)` の束縛を含む。ADR-0016）。`_` で始まる名前と引数は対象外。読まれたかは関数の中の名前で判定する。エラーのある関数（署名のエラーを含む）では出さない。同じ関数のほかの警告とは位置の順に並ぶ |
| W0202 | 未使用（警告） | 使われない import（`import m`、`from m import f`、別名を含む。ADR-0016）。名前解決のエラーがあるモジュールでは出さない（型検査のエラーでは止めない）。型パラメータと同じ名前の import は、型パラメータに隠されるので使われない扱い |
| W0301 | match（警告） | それより前の `case` ですでに網羅されているため、到達しない `case`。警告だけならビルドできる |

欠番（未割り当て）: E0212、E0506、E0507。廃止: E0405（`list` はまだ使えない。M3 T4a で `list` に対応したため廃止）、E0007（浮動小数点リテラル（未対応）。M4 T12 で `float` に対応したため廃止、ADR-0019）。一度割り当てたコードは再利用しない。

## 実行時エラー（パニック、ADR-0013）

実行時エラー（パニック）は、stdout を書き出してから stderr に `runtime error: <内容>` を出し、終了コード **101** で終わる。機械的にはこの接頭辞で判別する。場所が分かるものは ` at ファイル:行:列` を付ける。スタックトレースは出さない（Phase 1）。パニックと `exit()` では、終了時の生存オブジェクト数の検査（102）は行わない（正常終了したときだけ）。

| 内容 | 発生条件 |
|---|---|
| integer overflow | `+` `-` `*` 単項 `-` `//` の結果が 64 ビットに収まらない |
| integer division by zero | `//` の除数が 0 |
| integer modulo by zero | `%` の除数が 0 |
| float division by zero | `float` の `/` の除数が 0.0（`-0.0` を含む。ADR-0019） |
| cannot convert <x> to int | `int(x)` の `x` が NaN・無限大・`int` の範囲外（ADR-0019） |
| assertion failed at ファイル:行:列 | `assert(cond)` の条件が偽 |
| <msg> at ファイル:行:列 | `panic(msg)` を呼んだ |
| index out of range: index i, length n | リストの添字が範囲外（負の添字は末尾から数える） |
| key not found: キー | `d[k]` / `d.pop(k)` のキーがない（`str` のキーは引用符付きで表示する） |
| dict changed size during iteration | `for k in d:` の途中で `d` のキーの数が変わった |

プログラムの終了時に解放されていないオブジェクトが残っていた場合は、`runtime error: N object(s) were not freed (this is a compiler bug)` を出して終了コード 102 で終わる（ARC の誤りを検出するための検査、ADR-0012）。

## 組み込み関数（外部入力は ADR-0011）

| 関数 | 型 | 内容 |
|---|---|---|
| `arg_count()` | `int` | コマンドライン引数の数（プログラム名を除く） |
| `arg(i)` | `str \| None` | `i` 番目（0始まり）の引数。範囲外なら `None` |
| `panic(msg)` | `None` | `msg`（`str`）を出して実行時エラーで終了する。戻らない文として扱う（値を返す関数の最後に書いてよい）。`unwrap` の代わりに `case Err(e): panic(e)` と書く（ADR-0013） |
| `assert(cond)` | `None` | `cond` が偽なら実行時エラーで終了する。文として書くと、以降は `cond` が真の場合として絞り込む（`assert(x is not None)`） |
| `len(x)` | `int` | 文字列の長さ（文字数 = Unicode のコードポイント数。Python の `len` と同じ）、リストの要素数、または辞書のキーの数 |
| `range(n)` / `range(a, b)` | — | `for` の中だけで使える整数の範囲 |
| `parse_scalar(s)` | `Unknown` | 10進の整数 → `int`、JSON の数値の文法で小数点か指数を含むもの（`1.5`、`-0.5`、`1e3`。無限大になるものは除く）→ `float`、`true` / `false` → `bool`、`null` → `None`、それ以外 → `str`（ADR-0019） |
| `py_import(name)` | `Result[PyObject, PyError]` | Python のモジュールを import する。Effect `python`（ADR-0018） |
| `py_value(x)` | `PyObject` | `int` / `bool` / `str` / `float` / `None`、それらの `list` / `dict`（挿入順）と Union を、新しい Python のオブジェクトにコピーする（`PyObject` はそのまま）。Effect `python` |
| `h.attr(name)` / `h.call(args)` / `h.call_kw(args, kwargs)` | `Result[PyObject, PyError]` | 属性の参照と呼び出し（`args` は `list[PyObject]`、`kwargs` は `dict[str, PyObject]`）。Effect `python` |
| `h.to_int()` / `h.to_bool()` / `h.to_str()` / `h.to_float()` | `Result[int / bool / str / float, PyError]` | Python の値を本言語の値にする。暗黙の変換はしない（`to_bool` は `bool` だけ、`to_int` は `int` と `bool` だけ、`to_float` は `float` だけ）。Effect `python` |
| `h.to_list()` / `h.to_dict()` | `Result[list[PyObject] / dict[str, PyObject], PyError]` | Python の `list` / `tuple`、キーがすべて `str` の `dict` を、要素のハンドルの `list` / `dict` にする（要素はコピーしない）。Effect `python` |
| `h.is_none()` | `bool` | Python の `None` か。Effect `python` |
| `float(n)` | `float` | `int` を `float` にする（2^53 を超える値は最も近い値に丸める。ADR-0019） |
| `int(x)` | `int` | `float` を 0 の方向に切り捨てて `int` にする。NaN・無限大・範囲外は実行時エラー（ADR-0019） |
| `spawn(f(args))` | `Task[T]` | `async def f` の呼び出しを新しいタスクとして実行キューの末尾に入れる（`T` は `f` の戻り値の型）。`async def` の中だけで使える（ADR-0020） |
| `sleep_ms(n)` | `None` | 仮想時計で `n` ミリ秒の間、現在のタスクを止める（`async` の関数。`await sleep_ms(n)` と書く。ADR-0020） |
