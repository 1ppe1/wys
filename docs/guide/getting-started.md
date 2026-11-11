# 入門

`wys` の最初の一歩。Python に近い書き方で、型を検査してから Native の実行ファイルを作る。この文書の `wys` のコード例は、テスト（`guide_code_blocks_run`）がすべて実行して、書いた出力と一致することを確かめている。

## 1. インストール

前提: `clang`（リンクに使う）。Python と連携するプログラムを作るときだけ Python 3.14 と開発用のヘッダ。

配布物から入れる（ADR-0030。SHA-256 を確かめてから展開する）:

```sh
scripts/install.sh --prefix "$HOME/.local"          # GitHub Releases の最新
scripts/install.sh --from-archive wys-0.1.0-aarch64-apple-darwin.tar.gz   # 手元のアーカイブ
```

ソースから作る（Rust のツールチェーンが要る）:

```sh
cargo build --release
target/release/wys --version
```

対象環境は macOS arm64 と Linux x86_64（ADR-0029）。Windows は対象外。

## 2. 最初のプログラム

`hello.wys`:

```wys
# expect-stdout: Hello, world!
def main() -> None:
    print("Hello, world!")
```

```sh
wys run hello.wys        # ビルドして実行
wys build hello.wys      # 実行ファイル hello を作る（--release で -O2）
wys check hello.wys      # 型を検査するだけ（--format json で機械向けの診断）
```

`# expect-stdout:` などの行はただのコメント（この文書のテストが、出力や診断の期待として読む）。

## 3. 型と関数

関数の引数と戻り値には型を書く。ローカル変数は最初の代入から推論する。暗黙の型変換はない。

```wys
# expect-stdout: 55
# expect-stdout: True
def fib(n: int) -> int:
    if n < 2:
        return n
    return fib(n - 1) + fib(n - 2)

def main() -> None:
    print(fib(10))
    done = fib(10) == 55 and not False
    print(done)
```

型の誤りはコンパイル時に見つかる:

```wys
# expect-error: E0207 3:11
def main() -> None:
    print(1 + "a")
```

```text
error[E0207]: operator `+` cannot be applied to `int` and `str`
```

`wys explain E0207` でエラーコードの説明を読める。一覧は `docs/spec/error-codes.md`。

## 4. `None` と絞り込み

`None` になり得る値は `T | None` と書き、`is None` / `is not None` で調べてから使う。調べずに使うとコンパイルエラー（E0301）。

```wys
# expect-stdout: nothing
# expect-stdout: positive even
# expect-stdout: other
def describe(n: int | None) -> str:
    if n is None:
        return "nothing"
    elif n > 0 and n % 2 == 0:
        return "positive even"
    return "other"

def main() -> None:
    print(describe(None))
    print(describe(4))
    print(describe(3))
```

## 5. レコード・リスト・辞書

```wys
# expect-stdout: 4
# expect-stdout: 60
# expect-stdout: 2
# expect-stdout: True
type Point = {x: int, y: int}

def main() -> None:
    p = Point(x=1, y=3)
    print(p.x + p.y)
    xs = [10, 20]
    xs.append(30)
    total = 0
    for x in xs:
        total = total + x
    print(total)
    d = {"a": 1, "b": 2}
    print(d["b"])
    print("a" in d)
```

レコードは構造的な型（同じフィールドを持てば同じ型として扱える。ADR-0007）。

## 6. `Result` と `?`、`match`

失敗し得る処理は `Result[T, E]` を返す。`?` は `Err` をそのまま呼び出し元に返し、`Ok` なら中身を取り出す。

```wys
# expect-stdout: 3
# expect-stdout: not a digit: x
def parse_digit(s: str) -> Result[int, str]:
    if s == "1":
        return Ok(1)
    if s == "2":
        return Ok(2)
    return Err("not a digit: " + s)

def sum2(a: str, b: str) -> Result[int, str]:
    x = parse_digit(a)?
    y = parse_digit(b)?
    return Ok(x + y)

def show(r: Result[int, str]) -> None:
    match r:
        case Ok(v):
            print(v)
        case Err(e):
            print(e)

def main() -> None:
    show(sum2("1", "2"))
    show(sum2("1", "x"))
```

`match` はすべての場合を書かないとコンパイルエラー（E0303）。

## 7. 外から来る値: `Unknown` と `validate[T]`

JSON など外から来る値は `Unknown` 型で、検証するまで使えない。`validate[T]` で型に合うかを確かめる。

```wys
# expect-stdout: plan 2
type Plan = {title: str, steps: int}

def main() -> None:
    match parse_json("{\"title\": \"plan\", \"steps\": 2}"):
        case Ok(v):
            match validate[Plan](v):
                case Ok(p):
                    print(p.title + " " + to_json(p.steps))
                case Err(e):
                    print("not a plan: " + e.path + " is " + e.found)
        case Err(e):
            print(e.message)
```

## 8. テストと整形

`test_` で始まる関数をテストとして実行する（`main` は要らない）:

```wys
# expect-mode: test
def clamp(n: int, low: int, high: int) -> int:
    if n < low:
        return low
    elif n > high:
        return high
    return n

def test_clamp() -> None:
    assert(clamp(5, 0, 10) == 5)
    assert(clamp(42, 0, 10) == 10)
```

```sh
wys test clamp_test.wys
wys fmt clamp_test.wys          # 整形（--check で確かめるだけ）
```

## 9. 非同期

`async def` と `await`、`spawn` でタスクを作る。タスクは協調的に切り替わる（ADR-0020）。

```wys
# expect-stdout: waiting
# expect-stdout: hello, sooner
# expect-stdout: hello, later
# expect-stdout: sooner then later
async def greet(name: str, ms: int) -> str:
    await sleep_ms(ms)
    print("hello, " + name)
    return name

async def main() -> None:
    late = spawn(greet("later", 20))
    soon = spawn(greet("sooner", 10))
    print("waiting")
    first = await soon
    second = await late
    print(first + " then " + second)
```

## 10. 次に読むもの

- [AI SDK](ai-sdk.md): 型付きの LLM 呼び出し、ツールと agent、権限・承認・予算
- [Python との連携](python.md)
- [リファレンス](reference.md): コマンド、組み込み、環境変数、エラーコード
- [安定性と互換性](../stability.md)、[セキュリティ](../security.md)
