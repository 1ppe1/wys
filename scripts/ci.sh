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
echo "==> wys fmt --check examples"
for f in examples/*.wys; do
  ./target/debug/wys fmt --check "$f"
done
echo "==> OK"
