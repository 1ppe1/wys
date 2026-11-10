#!/usr/bin/env python3
"""`wys check` の時間を、1 つの関数の文の数を変えて計測する（#127）。

文の数に対して時間が線形に伸びるかを見る。生成するプログラムは 2 種類:

- assign: `x0 = 1` ... と代入だけを並べる
- branch: レコードを作り、フィールドを `if ... is not None:` で絞り込む（合流が文の数だけある）

方針（要件定義書 5.1 / P-07）: 絶対値と計測環境だけを記録する。倍率は書かない。

使い方: python3 benches/check_scaling.py [--cli target/release/wys] [--sizes 1000,2000,4000]
結果は benches/results/check-YYYYMMDD.tsv に追記する。
"""

import argparse
import os
import platform
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path


def assign(n: int) -> str:
    lines = ["def main() -> None:"]
    lines += [f"    x{i} = 1" for i in range(n)]
    return "\n".join(lines) + "\n"


def branch(n: int) -> str:
    lines = ["type P = {a: int | None}", "", "def main() -> None uses io:"]
    for i in range(n):
        lines += [
            f"    p{i} = P(a=1)",
            f"    if p{i}.a is not None:",
            f"        print(p{i}.a + 1)",
        ]
    return "\n".join(lines) + "\n"


SHAPES = {"assign": assign, "branch": branch}


def machine() -> str:
    try:
        return subprocess.run(
            ["sysctl", "-n", "machdep.cpu.brand_string"],
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return platform.machine()


def main() -> int:
    root = Path(__file__).resolve().parent.parent
    ap = argparse.ArgumentParser()
    ap.add_argument("--cli", default=str(root / "target/release/wys"))
    ap.add_argument("--sizes", default="1000,2000,4000,8000")
    ap.add_argument("--shapes", default="assign,branch")
    args = ap.parse_args()
    if not os.path.exists(args.cli):
        print(f"{args.cli} がない（cargo build --release を先に）", file=sys.stderr)
        return 2

    commit = subprocess.run(
        ["git", "rev-parse", "--short", "HEAD"], capture_output=True, text=True, cwd=root
    ).stdout.strip()
    dirty = subprocess.run(
        ["git", "status", "--porcelain"], capture_output=True, text=True, cwd=root
    ).stdout.strip()
    if dirty:
        commit += "+dirty"
    env_cols = [
        datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        commit,
        machine(),
        f"{platform.system()} {platform.release()}",
    ]

    out = root / "benches/results" / f"check-{datetime.now():%Y%m%d}.tsv"
    out.parent.mkdir(parents=True, exist_ok=True)
    if not out.exists():
        out.write_text("timestamp\tcommit\tmachine\tos\tshape\tstatements\tcheck_ms\n")

    rows = []
    with tempfile.TemporaryDirectory() as tmp:
        for shape in args.shapes.split(","):
            for n in (int(s) for s in args.sizes.split(",")):
                src = Path(tmp) / f"{shape}{n}.wys"
                src.write_text(SHAPES[shape](n))
                # 1 回空打ちしてから 3 回の最小値
                times = []
                for _ in range(4):
                    t0 = time.perf_counter()
                    r = subprocess.run([args.cli, "check", str(src)], capture_output=True)
                    times.append((time.perf_counter() - t0) * 1000)
                    if r.returncode != 0:
                        print(r.stderr.decode(), file=sys.stderr)
                        return 1
                ms = min(times[1:])
                rows.append(env_cols + [shape, str(n), f"{ms:.1f}"])
                print(f"{shape}\t{n}\t{ms:.1f} ms")
    with out.open("a") as f:
        for r in rows:
            f.write("\t".join(r) + "\n")
    print(f"→ {out.relative_to(root)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
