# Python との連携

CPython 3.14 を埋め込んで、Python のライブラリを呼べる（ADR-0018）。Python との完全な互換ではなく、明示的な呼び出しの境界を通す。Python の呼び出しは `python` の Effect を要求し、失敗は `Result[..., PyError]` になる（例外はプログラムを止めない）。

```wys python
# expect-stdout: 3628800
# expect-stdout: "hi"
def factorial(n: int) -> Result[int, PyError] uses python:
    math = py_import("math")?
    f = math.attr("factorial")?
    r = f.call([py_value(n)])?
    return r.to_int()

def dumps(s: str) -> Result[str, PyError] uses python:
    json = py_import("json")?
    r = (json.attr("dumps")?).call([py_value(s)])?
    return r.to_str()

def main() -> None uses io, python:
    match factorial(10):
        case Ok(n):
            print(n)
        case Err(e):
            print(e.kind)
    match dumps("hi"):
        case Ok(s):
            print(s)
        case Err(e):
            print(e.message)
```

- `py_import` / `attr` / `call` で Python のオブジェクトを扱い、`to_int` / `to_str` / `to_bool` / `to_float` / `to_list` / `to_dict` で `wys` の値に戻す。`py_value` は `wys` の値を Python に渡す。
- ビルドには Python 3.14 と `python3.14-config` が要る（`WYS_PYTHON` で Python を指定できる）。
- **Python のコードはサンドボックスではない**: `python` の Capability を与えた agent は、Python を通して何でもできる（[セキュリティ](../security.md)）。
