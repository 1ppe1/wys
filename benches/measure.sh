#!/usr/bin/env bash
# ベンチ計測（M3 T7）。benches/programs/ と examples/hello.wys について、次の値を計測し、
# benches/results/bench-YYYYMMDD.tsv に TSV で追記する。
#
# - compile_ms:   `wys build --release` の時間
# - binary_bytes: 生成した実行ファイルの大きさ
# - run_ms:       実行時間（1回空打ちしてから、RUNS 回の最小値）。hello.wys の値は起動時間にあたる
# - max_rss_kb:   最大常駐メモリ（KiB。OS の単位の違いはここで揃える）
# - retains / releases: 参照カウントの操作の回数（`WYS_RC_STATS=1` で別にビルドして1回実行する）
#
# 方針（要件定義書 5.1 / P-07）:
# - 絶対値と計測環境（機種・OS・コミット）だけを記録する。「Python 比 ○ 倍」などの倍率は書かない。
# - 数値の比較は同じ機種・同じコミット範囲で行う。
#
# 使い方: benches/measure.sh [プログラム...]   （省略時はすべて。RUNS=5 で回数を変えられる）
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build -q --release -p wys_cli -p wys_runtime
cli=target/release/wys
export WYS_RUNTIME_LIB="$PWD/target/release/libwys_runtime.a"
runs=${RUNS:-10}

if [ $# -gt 0 ]; then
  programs=("$@")
else
  programs=(examples/hello.wys benches/programs/*.wys)
fi

out_dir=benches/results
mkdir -p "$out_dir"
out="$out_dir/bench-$(date +%Y%m%d).tsv"
[ -f "$out" ] || printf 'timestamp\tcommit\tmachine\tos\tprogram\tcompile_ms\tbinary_bytes\trun_ms\tmax_rss_kb\tretains\treleases\n' > "$out"

commit=$(git rev-parse --short HEAD)
if [ -n "$(git status --porcelain)" ]; then
  commit="$commit+dirty"
fi
machine=$(sysctl -n machdep.cpu.brand_string 2>/dev/null || uname -m)
os="$(uname -s) $(uname -r)"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# 時間とメモリは1つの Python プロセス内で計測する（シェルから時刻を取るたびにプロセスを起動すると、その分が誤差になる）。
# 最大常駐メモリは os.wait4 で子プロセスごとに取る（macOS はバイト、Linux は KiB で返る）。
measure() {
  python3 - "$@" <<'PY'
import os, re, subprocess, sys, time
cli, src, exe, stats_exe, runs = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4], int(sys.argv[5])
env = dict(os.environ)
env.pop("WYS_RC_STATS", None)
t0 = time.perf_counter()
subprocess.run([cli, "build", "--release", src, "-o", exe], check=True, env=env)
compile_ms = (time.perf_counter() - t0) * 1000

def run_once(path, extra_env=None):
    e = dict(env)
    if extra_env:
        e.update(extra_env)
    t = time.perf_counter()
    p = subprocess.Popen([path], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, env=e)
    _, status, usage = os.wait4(p.pid, 0)
    elapsed = (time.perf_counter() - t) * 1000
    err = p.stderr.read().decode()
    p.stderr.close()
    code = os.waitstatus_to_exitcode(status)
    if code != 0:
        sys.exit(f"{path} exited with {code}: {err}")
    rss = usage.ru_maxrss // 1024 if sys.platform == "darwin" else usage.ru_maxrss
    return elapsed, rss, err

run_once(exe)
times, rss = [], 0
for _ in range(runs):
    ms, r, _ = run_once(exe)
    times.append(ms)
    rss = max(rss, r)

subprocess.run([cli, "build", "--release", src, "-o", stats_exe], check=True, env={**env, "WYS_RC_STATS": "1"})
_, _, err = run_once(stats_exe, {"WYS_RC_STATS": "1"})
m = re.search(r"rc stats: retain (\d+), release (\d+)", err)
retains, releases = (m.group(1), m.group(2)) if m else ("", "")
print(f"{compile_ms:.0f}\t{min(times):.1f}\t{rss}\t{retains}\t{releases}")
PY
}

for src in "${programs[@]}"; do
  name=$(basename "$src" .wys)
  exe="$tmp/$name"
  read -r compile_ms run_ms rss retains releases < <(measure "$cli" "$src" "$exe" "$tmp/$name.stats" "$runs")
  size=$(stat -f %z "$exe" 2>/dev/null || stat -c %s "$exe")
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$(date -u +%FT%TZ)" "$commit" "$machine" "$os" "$src" \
    "$compile_ms" "$size" "$run_ms" "$rss" "$retains" "$releases" | tee -a "$out"
done
