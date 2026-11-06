#!/usr/bin/env bash
# PoC4（M5 T1）: 本物の Anthropic API との往復を手元で 1 回試す（CI では動かさない。費用がかかる）。
# ANTHROPIC_API_KEY を自分の環境に設定してから実行する。キーは `wys llm-proxy` だけが読む。
# 使い方: scripts/llm-smoke.sh   （WYS_LLM_MODEL でモデルを変えられる。既定は claude-opus-5-5）
set -euo pipefail
cd "$(dirname "$0")/.."
: "${ANTHROPIC_API_KEY:?set ANTHROPIC_API_KEY in your environment first}"
cargo build -q -p wys_cli -p wys_runtime
out=$(mktemp -d)
clang crates/wys_runtime/tests/ai_proxy_poc.c target/debug/libwys_runtime.a -o "$out/ai-proxy" \
  $( [ "$(uname)" = Linux ] && echo -lpthread -ldl -lm )
start=$(python3 -c 'import time; print(time.time())')
WYS_LLM_PROVIDER=anthropic WYS_LLM_PROXY="$PWD/target/debug/wys" WYS_AUDIT=stderr "$out/ai-proxy"
end=$(python3 -c 'import time; print(time.time())')
python3 -c "print(f'round trip: {($end - $start) * 1000:.0f} ms')"
