#!/usr/bin/env python3
"""benches/report.py の単体テスト（scripts/ci.sh から実行する）。"""

import os
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import report  # noqa: E402

BENCH = (
    "timestamp\tcommit\tmachine\tos\tprogram\tcompile_ms\tbinary_bytes\trun_ms\tmax_rss_kb\tretains\treleases\n"
    "t1\tabc\tM\tDarwin\tbenches/programs/fib.wys\t100\t1000\t5.0\t10\t1\t2\n"
    "t2\tabc\tM\tDarwin\tbenches/programs/fib.wys\t90\t1000\t4.5\t10\t1\t2\n"
    "t3\tabc\tM\tDarwin\texamples/hello.wys\t80\t900\t1.0\t5\t0\t0\n"
)
CHECK = (
    "timestamp\tcommit\tmachine\tos\tshape\tstatements\tcheck_ms\n"
    "t1\tabc\tM\tDarwin\tbranch\t8000\t47.6\n"
    "t2\tabc\tM\tDarwin\tassign\t100000\t163.1\n"
    "t3\tabc\tM\tDarwin\tbranch\t2000\t14.1\n"
)


def write(text):
    f = tempfile.NamedTemporaryFile("w", suffix=".tsv", delete=False)
    f.write(text)
    f.close()
    return f.name


def main():
    out = report.bench(write(BENCH))
    assert "計測環境: M / Darwin / コミット abc" in out, out
    assert "| `fib.wys` | 90 | 1,000 | 4.5 | 10 | 1 | 2 |" in out, out  # 最後の行を使う
    assert out.count("`fib.wys`") == 1, out
    assert "| `hello.wys` | 80 | 900 | 1.0 | 5 | 0 | 0 |" in out, out
    out = report.check(write(CHECK))
    lines = [l for l in out.splitlines() if l.startswith("| assign") or l.startswith("| branch")]
    assert lines == [
        "| assign | 100,000 | 163.1 |",
        "| branch | 2,000 | 14.1 |",
        "| branch | 8,000 | 47.6 |",
    ], lines
    for word in ("倍", "faster", "times"):
        assert word not in out, word
    print("test_report: ok")


if __name__ == "__main__":
    main()
