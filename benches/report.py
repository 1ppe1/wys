#!/usr/bin/env python3
"""計測の TSV（benches/results/）から、docs/benchmarks.md に貼る Markdown の表を作る（公開準備 T10、NFR-002）。

絶対値と計測環境だけを出す。倍率・比較の文言は出さない（要件定義書 5.1）。

使い方:
  benches/report.py bench <bench-YYYYMMDD.tsv>     measure.sh の結果（各プログラムの最後の行）
  benches/report.py check <check-YYYYMMDD.tsv>     check_scaling.py の結果（形と文の数ごとの最後の行）
"""

import csv
import sys


def rows(path):
    with open(path, newline="") as f:
        return list(csv.DictReader(f, delimiter="\t"))


def last_by(rs, key):
    """キーごとに最後の行（同じ日に何度も測ったときは最新）"""
    out = {}
    for r in rs:
        out[key(r)] = r
    return out


def env_line(rs):
    commits = sorted({r["commit"] for r in rs})
    machines = sorted({r["machine"] for r in rs})
    oss = sorted({r["os"] for r in rs})
    return f"計測環境: {', '.join(machines)} / {', '.join(oss)} / コミット {', '.join(commits)}"


def fmt_int(v):
    if v in ("", "-"):
        return "—"
    return f"{int(float(v)):,}"


def bench(path):
    rs = rows(path)
    latest = last_by(rs, lambda r: r["program"])
    picked = list(latest.values())
    lines = [
        env_line(picked),
        "",
        "| プログラム | コンパイル（ms） | 実行ファイル（バイト） | 実行（ms） | 最大常駐メモリ（KiB） | retain | release |",
        "|---|---:|---:|---:|---:|---:|---:|",
    ]
    for r in picked:
        name = r["program"].rsplit("/", 1)[-1]
        lines.append(
            f"| `{name}` | {fmt_int(r['compile_ms'])} | {fmt_int(r['binary_bytes'])} | {r['run_ms']} "
            f"| {fmt_int(r['max_rss_kb'])} | {fmt_int(r['retains'])} | {fmt_int(r['releases'])} |"
        )
    return "\n".join(lines)


def check(path):
    rs = rows(path)
    latest = last_by(rs, lambda r: (r["shape"], int(r["statements"])))
    picked = [latest[k] for k in sorted(latest)]
    lines = [
        env_line(picked),
        "",
        "| 形 | 文の数（branch は if の数） | `wys check`（ms） |",
        "|---|---:|---:|",
    ]
    for r in picked:
        lines.append(f"| {r['shape']} | {int(r['statements']):,} | {r['check_ms']} |")
    return "\n".join(lines)


def main():
    if len(sys.argv) != 3 or sys.argv[1] not in ("bench", "check"):
        print(__doc__, file=sys.stderr)
        return 2
    print(bench(sys.argv[2]) if sys.argv[1] == "bench" else check(sys.argv[2]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
