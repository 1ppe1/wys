#!/usr/bin/env bash
# AI 修正ループ用（DX-005）: .wys ファイルを検査し、
#   - 診断があれば診断 JSON を標準出力に出して終了コード 1
#   - なければビルドして実行し、プログラムの出力と終了コードを返す
# 使い方: scripts/ai-loop.sh <file.wys>
set -euo pipefail
cd "$(dirname "$0")/.."

if [ $# -ne 1 ]; then
  echo "usage: scripts/ai-loop.sh <file.wys>" >&2
  exit 2
fi

cargo build -q -p wys_cli -p wys_runtime
cli=target/debug/wys_cli

if ! json=$("$cli" check --format json "$1"); then
  echo "$json"
  exit 1
fi
exec "$cli" run "$1"
