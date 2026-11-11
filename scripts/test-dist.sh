#!/usr/bin/env bash
# 配布物の検査（公開準備 T7、ADR-0030）。scripts/ci.sh から呼ぶ。
# release.sh で（ビルド済みの debug の成果物から）アーカイブを作り、install.sh --from-archive で一時 prefix に入れ、
# ランタイムの環境変数なしで hello をビルドして実行する。SHA-256 が合わないアーカイブは入れないことも確かめる。
set -euo pipefail
cd "$(dirname "$0")/.."

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

cargo build -q -p wys_cli -p wys_runtime -p wys_runtime_python
scripts/release.sh --skip-build --profile-dir target/debug --out "$tmp/dist" >/dev/null
archive=$(ls "$tmp"/dist/*.tar.gz)

scripts/install.sh --from-archive "$archive" --prefix "$tmp/prefix" >/dev/null
out=$(cd "$tmp" && env -u WYS_RUNTIME_LIB -u WYS_RUNTIME_PYTHON_LIB \
  "$tmp/prefix/bin/wys" run "$OLDPWD/examples/hello.wys")
expected=$(WYS_RUNTIME_LIB="$PWD/target/debug/libwys_runtime.a" target/debug/wys run examples/hello.wys)
if [ -z "$out" ] || [ "$out" != "$expected" ]; then
  echo "test-dist: hello の出力が違う: '$out'（期待 '$expected'）" >&2
  exit 1
fi

# 改ざんしたアーカイブ（SHA256SUMS と合わない）は入れない
cp "$archive" "$tmp/bad.tar.gz"
printf 'x' >> "$tmp/bad.tar.gz"
sed "s# $(basename "$archive")\$# bad.tar.gz#" "$tmp/dist/SHA256SUMS" > "$tmp/SHA256SUMS.bad"
if scripts/install.sh --from-archive "$tmp/bad.tar.gz" --sums "$tmp/SHA256SUMS.bad" --prefix "$tmp/prefix2" 2>/dev/null; then
  echo "test-dist: 改ざんしたアーカイブを入れてしまった" >&2
  exit 1
fi
[ ! -e "$tmp/prefix2/bin/wys" ] || { echo "test-dist: 改ざんしたアーカイブの中身が残った" >&2; exit 1; }
echo "test-dist: ok ($(basename "$archive"))"
