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
echo "==> wys fmt --check examples, benches"
for f in examples/*.wys benches/programs/*.wys; do
  ./target/debug/wys fmt --check "$f"
done
echo "==> bench programs (check)"
for f in benches/programs/*.wys; do
  ./target/debug/wys check "$f" >/dev/null
done
echo "==> evaluation cases (replay, ADR-0025)"
./target/debug/wys eval tests/evals/*/
for f in tests/evals/*/case.wys; do
  ./target/debug/wys fmt --check "$f"
done
echo "==> release archive and install (ADR-0030)"
./scripts/test-dist.sh
echo "==> benchmark report and wording (NFR-002, 5.1)"
python3 -B benches/test_report.py
# 性能は絶対値だけを書く: 倍率・比較の文言があれば失敗（要件定義書 5.1）
if grep -nE '[0-9.]+ ?倍|倍速|x faster|times faster|faster than|slower than|[0-9.]+x (faster|slower)' docs/benchmarks.md README.md; then
  echo "error: performance ratios must not be written (requirements 5.1)" >&2
  exit 1
fi
echo "==> license files and dependency licenses (ADR-0028)"
for f in LICENSE-MIT LICENSE-APACHE CONTRIBUTING.md CODE_OF_CONDUCT.md SECURITY.md CHANGELOG.md docs/third-party-licenses.md; do
  [ -f "$f" ] || { echo "error: $f is missing (ADR-0028)" >&2; exit 1; }
done
grep -qx 'license = "MIT OR Apache-2.0"' Cargo.toml || { echo "error: Cargo.toml license must be MIT OR Apache-2.0" >&2; exit 1; }
python3 -B scripts/third-party-licenses.py --check
echo "==> poc5 cases"
./scripts/poc5/verify.sh
echo "==> OK"
