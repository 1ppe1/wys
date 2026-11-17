# Getting started

> This is a translation of [docs/guide/getting-started.md](../guide/getting-started.md) as of commit `cdfcc83`. The Japanese document is the source of truth (ADR-0031).

First steps with `wys`. You write code that reads much like Python, the compiler checks the types, and then it builds a native executable. A test (`guide_code_blocks_run`) runs every `wys` example in this document and checks that the output matches what is written here.

## 1. Install

You need `clang`, which is used for linking. Python 3.14 and its development headers are needed only for programs that call Python.

Install a release. The script verifies the SHA-256 of the archive before unpacking it (ADR-0030):

```sh
curl -fsSL https://raw.githubusercontent.com/1ppe1/wys/main/scripts/install.sh | sh    # the latest release, into ~/.local
scripts/install.sh --prefix "$HOME/.local"          # the same, from a checkout
scripts/install.sh --from-archive wys-0.1.0-aarch64-apple-darwin.tar.gz   # from a local archive
```

Or build from source (you need the Rust toolchain):

```sh
cargo build --release
target/release/wys --version
```

Supported platforms are macOS arm64 and Linux x86_64 (ADR-0029). Windows is not supported.

## 2. Your first program

`hello.wys`:

```wys
# expect-stdout: Hello, world!
def main() -> None:
    print("Hello, world!")
```

```sh
wys run hello.wys        # build and run
wys build hello.wys      # build the executable `hello` (--release for -O2)
wys check hello.wys      # type-check only (--format json for machine-readable diagnostics)
```

Lines such as `# expect-stdout:` are plain comments. The test for this document reads them as the expected output or diagnostics.

## 3. Types and functions

Function parameters and return values have type annotations. The type of a local variable is inferred from its first assignment. There are no implicit conversions.

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

Type errors are found at compile time:

```wys
# expect-error: E0207 3:11
def main() -> None:
    print(1 + "a")
```

```text
error[E0207]: operator `+` cannot be applied to `int` and `str`
```

`wys explain E0207` prints the description of an error code. The full list is in `docs/spec/error-codes.md` (Japanese).

## 4. `None` and narrowing

A value that may be `None` has the type `T | None`. Check it with `is None` or `is not None` before using it. Using it without a check is a compile error (E0301).

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

## 5. Records, lists, and dicts

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

Records are structural types: two records with the same fields are the same type (ADR-0007).

## 6. `Result`, `?`, and `match`

An operation that can fail returns `Result[T, E]`. The `?` operator returns an `Err` to the caller as it is, and unwraps the value of an `Ok`.

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

A `match` that does not cover every case is a compile error (E0303).

## 7. Values from outside: `Unknown` and `validate[T]`

A value that comes from outside, such as JSON, has the type `Unknown` and cannot be used until it is validated. `validate[T]` checks that it fits a type.

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

## 8. Tests and formatting

Functions whose names start with `test_` run as tests. No `main` is needed:

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
wys fmt clamp_test.wys          # format (--check only verifies)
```

## 9. Async

Write `async def` and `await`, and create tasks with `spawn`. Tasks are switched cooperatively (ADR-0020).

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

## 10. What to read next

- [AI SDK](ai-sdk.md): typed LLM calls, tools and agents, capabilities, approvals, and budgets
- [Stability and compatibility](stability.md) and [Security](security.md)
- [Python interop](../guide/python.md) (Japanese)
- [Reference](../guide/reference.md) (Japanese): commands, builtins, environment variables, and error codes
