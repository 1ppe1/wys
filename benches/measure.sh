#!/usr/bin/env bash
# ベンチ計測の骨格（M3 で本格化する）。
# コンパイル時間・生成バイナリのサイズ・起動時間を計測し、benches/results/ に TSV で追記する。
#
# 方針（要件定義書 5.1 / P-07）:
# - 絶対値と計測環境（機種・OS・コミット）だけを記録する。「Python 比 ○ 倍」などの倍率は書かない。
# - 数値の比較は同じ機種・同じコミット範囲で行う。
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build -q --release -p wys_cli -p wys_runtime
cli=target/release/wys
export WYS_RUNTIME_LIB="$PWD/target/release/libwys_runtime.a"

out_dir=benches/results
mkdir -p "$out_dir"
out="$out_dir/$(date +%Y%m%d).tsv"
[ -f "$out" ] || printf 'timestamp\tcommit\tmachine\tos\tprogram\tcompile_ms\tbinary_bytes\tstartup_ms\n' > "$out"

commit=$(git rev-parse --short HEAD)
machine=$(sysctl -n machdep.cpu.brand_string 2>/dev/null || uname -m)
os="$(uname -s) $(uname -r)"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# 時間は1つの Python プロセス内で計測する（シェルから時刻を取るたびにプロセスを起動すると、その分が誤差になる）。
# 起動時間は、初回起動時の OS の検査を除くため、1回空打ちしてから10回の最小値を取る。
measure() {
  python3 - "$@" <<'PY'
import subprocess, sys, time
cli, src, exe = sys.argv[1:4]
t0 = time.perf_counter()
subprocess.run([cli, "build", src, "-o", exe], check=True)
compile_ms = (time.perf_counter() - t0) * 1000
subprocess.run([exe], check=True, stdout=subprocess.DEVNULL)
runs = []
for _ in range(10):
    t = time.perf_counter()
    subprocess.run([exe], check=True, stdout=subprocess.DEVNULL)
    runs.append((time.perf_counter() - t) * 1000)
print(f"{compile_ms:.0f}\t{min(runs):.1f}")
PY
}

for src in examples/hello.wys examples/fib.wys; do
  exe="$tmp/$(basename "$src" .wys)"
  read -r compile_ms startup_ms < <(measure "$cli" "$src" "$exe")
  size=$(stat -f %z "$exe" 2>/dev/null || stat -c %s "$exe")
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$(date -u +%FT%TZ)" "$commit" "$machine" "$os" "$src" \
    "$compile_ms" "$size" "$startup_ms" | tee -a "$out"
done
