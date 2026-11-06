#!/usr/bin/env bash
# 本物の Anthropic API との往復を手元で 1 回試す（CI では動かさない。費用がかかる）。M5 T11、PoC4 の成功基準 4。
# 本言語のプログラム（ツール 1 つ・agent 1 つ・出力スキーマ付き）を WYS_LLM_PROVIDER=anthropic で実行し、
# ランタイムが起動した `wys llm-proxy` を通して tool_use → tool_result → 最終の JSON を往復する。
# 標準エラーに監査イベント（WYS_AUDIT=stderr）、標準出力にプログラムの出力、最後に往復の時間（コンパイルを除く）が出る。
#
#   export ANTHROPIC_API_KEY=...   # 自分の環境で。キーはリポジトリに置かない（ADR-0023 論点 6）
#   scripts/llm-smoke.sh           # モデルは WYS_LLM_MODEL で変えられる（既定は proxy の claude-opus-5-5）
set -euo pipefail

if [ -z "${ANTHROPIC_API_KEY:-}" ]; then
  echo "llm-smoke: set ANTHROPIC_API_KEY in your environment first" >&2
  exit 2
fi

cd "$(dirname "$0")/.."
cargo build -q
wys="$PWD/target/debug/wys"
dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT

cat > "$dir/smoke.wys" <<'EOF'
type Plan = {title: str, steps: int}

@tool(description="Adds two integers and returns the sum.")
def add(a: int, b: int) -> int uses io:
    print("tool add was called")
    return a + b

@agent(tools=[add], capabilities=["io", "llm"], max_llm_calls=4, max_tool_calls=2, timeout_ms=120000)
async def planner() -> Result[Plan, LlmError] uses io, llm:
    req = LlmRequest(model="", system="Use the add tool to compute 2 + 3, then make a plan whose number of steps is the sum.", messages=[Message(role="user", content="Make the plan.")], max_tokens=4000)
    return await llm_generate[Plan](req)

async def main() -> None uses io, llm:
    match await planner():
        case Ok(p):
            print(to_json(p))
        case Err(e):
            print("error " + e.kind + ": " + e.message)
EOF

"$wys" build "$dir/smoke.wys" -o "$dir/smoke"
now_ms() { perl -MTime::HiRes=time -e 'printf "%d\n", time * 1000'; }
start=$(now_ms)
WYS_LLM_PROVIDER=anthropic WYS_LLM_PROXY="$wys" WYS_AUDIT=stderr "$dir/smoke"
end=$(now_ms)
echo "llm-smoke: round trip $((end - start)) ms (LLM calls, the tool and the proxy start-up)" >&2
