#!/usr/bin/env python3
"""依存クレートのライセンスの一覧 docs/third-party-licenses.md を作る（ADR-0028 論点 10）。

  python3 scripts/third-party-licenses.py          # 書き出す
  python3 scripts/third-party-licenses.py --check  # 最新か、許可したライセンスだけかを確かめる（scripts/ci.sh）

`Cargo.lock` から `cargo metadata --locked` で全クレートを読む。ランタイム（利用者のプログラムに静的リンクする
`wys_runtime` / `wys_runtime_python`）の通常の依存には印を付ける。
"""
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "docs" / "third-party-licenses.md"
# 寛容なライセンスだけを許す。式の OR の選択肢のどれかが、これだけでできていればよい
ALLOWED = {
    "MIT", "MIT-0", "Apache-2.0", "Apache-2.0 WITH LLVM-exception", "BSD-2-Clause", "BSD-3-Clause",
    "ISC", "Zlib", "0BSD", "Unlicense", "Unicode-3.0", "CDLA-Permissive-2.0",
}
RUNTIME = ["wys_runtime", "wys_runtime_python"]


def allowed(expr: str) -> bool:
    """SPDX の式を、括弧と優先順位（`AND` は `OR` より強い）を保って評価する。`AND` は全部、`OR` はどれか 1 つが
    許可されていればよい。古い書き方の `/` は `OR` として読む。読めない式は許可しない。"""
    tokens = re.findall(r"\(|\)|[^\s()]+", expr.replace("/", " OR "))
    # `WITH <例外>` は直前の識別子と 1 つにまとめる
    merged: list[str] = []
    i = 0
    while i < len(tokens):
        if tokens[i] == "WITH" and merged and i + 1 < len(tokens):
            merged[-1] = f"{merged[-1]} WITH {tokens[i + 1]}"
            i += 2
            continue
        merged.append(tokens[i])
        i += 1
    pos = 0

    def peek() -> str | None:
        return merged[pos] if pos < len(merged) else None

    def take() -> str:
        nonlocal pos
        if pos >= len(merged):
            raise ValueError("unexpected end")
        pos += 1
        return merged[pos - 1]

    def or_expr() -> bool:
        ok = and_expr()
        while peek() == "OR":
            take()
            ok = and_expr() or ok
        return ok

    def and_expr() -> bool:
        ok = atom()
        while peek() == "AND":
            take()
            ok = atom() and ok
        return ok

    def atom() -> bool:
        t = take()
        if t == "(":
            ok = or_expr()
            if take() != ")":
                raise ValueError("missing )")
            return ok
        if t in ("AND", "OR", ")"):
            raise ValueError(f"unexpected {t}")
        return t in ALLOWED

    try:
        ok = or_expr()
        return ok and pos == len(merged)
    except ValueError:
        return False


def self_test() -> None:
    cases = {
        "MIT OR Apache-2.0": True,
        "MIT/Apache-2.0": True,
        "(MIT OR Apache-2.0) AND Unicode-3.0": True,
        "Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT": True,
        "MIT OR Apache-2.0 OR LGPL-2.1-or-later": True,
        "(MIT OR Apache-2.0) AND GPL-3.0-only": False,
        "GPL-3.0-only AND (MIT OR Apache-2.0)": False,
        "MIT AND GPL-3.0-only OR Apache-2.0": True,
        "MIT AND (GPL-3.0-only OR LGPL-2.1-or-later)": False,
        "GPL-3.0-only": False,
        "（記載なし）": False,
        "(MIT OR Apache-2.0": False,
        "MIT OR": False,
    }
    for expr, want in cases.items():
        got = allowed(expr)
        assert got == want, f"allowed({expr!r}) = {got}, want {want}"


def runtime_deps(meta: dict) -> set[str]:
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    pkgs = {p["id"]: p for p in meta["packages"]}
    todo = [i for i, p in pkgs.items() if p["name"] in RUNTIME]
    seen: set[str] = set()
    while todo:
        i = todo.pop()
        # 手続き的マクロ（serde_derive など）はコンパイル時にだけ動き、リンクされない
        if i in seen or any("proc-macro" in t["kind"] for t in pkgs[i]["targets"]):
            continue
        seen.add(i)
        for d in nodes[i]["deps"]:
            if any(k["kind"] is None for k in d["dep_kinds"]):
                todo.append(d["pkg"])
    return seen


def render() -> tuple[str, list[str]]:
    meta = json.loads(subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked"],
        cwd=ROOT, check=True, capture_output=True, text=True).stdout)
    ws = set(meta["workspace_members"])
    rt = runtime_deps(meta)
    rows, bad = [], []
    for p in sorted(meta["packages"], key=lambda p: (p["name"], p["version"])):
        if p["id"] in ws:
            continue
        lic = p.get("license") or "（記載なし）"
        if not allowed(lic):
            bad.append(f"{p['name']} {p['version']}: {lic}")
        mark = "○" if p["id"] in rt else ""
        repo = p.get("repository") or ""
        rows.append(f"| `{p['name']}` | {p['version']} | {lic} | {mark} | {repo} |")
    text = "\n".join([
        "# 依存クレートのライセンス",
        "",
        "`wys` が使う Rust のクレートと、そのライセンスの一覧（ADR-0028）。`scripts/third-party-licenses.py` が",
        "`Cargo.lock` から作る（手で編集しない。`scripts/ci.sh` が最新かを確かめる）。開発とビルドにだけ使うものも含む。",
        "「ランタイム」に ○ があるものは、利用者のプログラムの実行ファイルに静的リンクされる。各ライセンスの全文は",
        "それぞれのクレートの配布物にある。",
        "",
        f"クレートの数: {len(rows)}",
        "",
        "| クレート | 版 | ライセンス | ランタイム | リポジトリ |",
        "|---|---|---|:-:|---|",
        *rows,
        "",
    ])
    return text, bad


def main() -> int:
    self_test()
    text, bad = render()
    if bad:
        print("third-party-licenses: 許可していないライセンス:", *bad, sep="\n  ", file=sys.stderr)
        return 1
    if "--check" in sys.argv[1:]:
        if not OUT.exists() or OUT.read_text() != text:
            print("third-party-licenses: docs/third-party-licenses.md が古い。"
                  "python3 scripts/third-party-licenses.py で作り直す", file=sys.stderr)
            return 1
        return 0
    OUT.write_text(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
