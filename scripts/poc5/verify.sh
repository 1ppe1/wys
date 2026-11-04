#!/usr/bin/env bash
# PoC5 のケースが正しいことを確かめる: 壊れた版（lib.wys）はテストに失敗し、正解の修正例（fixed.wys）は合格する。
# 言語の変更でケースが壊れていないかを CI で確かめるために使う（モデルは呼ばない）。
set -euo pipefail
cd "$(dirname "$0")/../.."
wys="$PWD/target/debug/wys"
[ -x "$wys" ] || cargo build -q -p wys_cli -p wys_runtime
export WYS_RUNTIME_LIB="${WYS_RUNTIME_LIB:-$PWD/target/debug/libwys_runtime.a}"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
bad=0
for case in scripts/poc5/cases/*/; do
  name=$(basename "$case")
  rm -rf "${tmp:?}"/*
  cp "$case/main_test.wys" "$tmp/"
  cp "$case/lib.wys" "$tmp/lib.wys"
  if "$wys" test "$tmp/main_test.wys" >/dev/null 2>&1; then
    echo "FAIL $name: the broken version already passes"; bad=1
  fi
  cp "$case/fixed.wys" "$tmp/lib.wys"
  if ! "$wys" test "$tmp/main_test.wys" >/dev/null 2>&1; then
    echo "FAIL $name: the fixed version does not pass"; bad=1
  fi
done
[ "$bad" -eq 0 ] && echo "poc5 cases ok ($(ls -d scripts/poc5/cases/*/ | wc -l | tr -d ' ') cases)"
exit "$bad"
