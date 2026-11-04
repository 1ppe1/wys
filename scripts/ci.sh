#!/usr/bin/env bash
# CI と同じチェックを手元で実行する。CLAUDE.md の「開発コマンド」と同じ内容。
set -euo pipefail
cd "$(dirname "$0")/.."

echo "==> cargo fmt"
cargo fmt --all -- --check
echo "==> cargo clippy"
cargo clippy --workspace --all-targets -- -D warnings
echo "==> cargo test"
cargo test --workspace
# ARC の検査（ADR-0005 / ADR-0012）: Conformance を Release（-O2）と AddressSanitizer でも実行する
echo "==> conformance (release -O2)"
WYS_OPT=2 cargo test -q -p wys_testharness --test conformance conformance_suite
echo "==> conformance (AddressSanitizer)"
# リークの判定は終了時の生存オブジェクト数（102）で行う。パニックや exit() では解放せずに終わるので、
# LeakSanitizer（Linux の ASan では既定で有効）は無効にする（ADR-0013）
ASAN_OPTIONS=detect_leaks=0 WYS_SANITIZE=address cargo test -q -p wys_testharness --test conformance conformance_suite
echo "==> wys fmt --check examples"
for f in examples/*.wys; do
  ./target/debug/wys fmt --check "$f"
done
echo "==> poc5 cases"
./scripts/poc5/verify.sh
echo "==> OK"
