#!/usr/bin/env bash
# PoC5（DX-005）: AI 修正ループの成功率を計測する。**モデルを呼び出すので費用がかかる。ユーザーが実行する。**
#
# 各ケースについて、作業用のコピーで次を最大 K 回繰り返す:
#   1. `wys test main_test.wys` を実行する（コンパイルエラーなら診断 JSON、テストの失敗なら失敗の出力）
#   2. 合格していなければ、その出力を添えて `claude -p` に lib.wys の修正を依頼する（Read と Edit だけを許可）
# main_test.wys が書き換えられたら、そのケースは失敗として扱う。
#
# 使い方: scripts/poc5/run.sh [K]          （既定 K=3）
# 環境変数: WYS_POC5_MODEL（claude --model に渡す。未指定なら claude の既定モデル）、WYS_POC5_CASES（ケース名の部分一致で絞る）
set -euo pipefail
cd "$(dirname "$0")/../.."
K="${1:-3}"
command -v claude >/dev/null || { echo "claude CLI が見つかりません" >&2; exit 2; }
cargo build -q -p wys_cli -p wys_runtime
wys="$PWD/target/debug/wys"
export WYS_RUNTIME_LIB="$PWD/target/debug/libwys_runtime.a"
model_args=()
[ -n "${WYS_POC5_MODEL:-}" ] && model_args=(--model "$WYS_POC5_MODEL")

out_dir=scripts/poc5/results
mkdir -p "$out_dir"
stamp=$(date +%Y%m%d-%H%M%S)
out="$out_dir/$stamp.tsv"
printf 'case\tsolved\tattempts\ttest_file_changed\n' > "$out"
{
  echo "# date: $(date -u +%FT%TZ)"
  echo "# commit: $(git rev-parse --short HEAD)"
  echo "# machine: $(sysctl -n machdep.cpu.brand_string 2>/dev/null || uname -m)"
  echo "# claude: $(claude --version 2>/dev/null | head -1)"
  echo "# model: ${WYS_POC5_MODEL:-(claude default)}"
  echo "# K: $K"
} > "$out_dir/$stamp.meta"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
for case in scripts/poc5/cases/*/; do
  name=$(basename "$case")
  [ -n "${WYS_POC5_CASES:-}" ] && [[ "$name" != *"$WYS_POC5_CASES"* ]] && continue
  dir="$work/$name"
  mkdir -p "$dir"
  cp "$case/lib.wys" "$case/main_test.wys" "$dir/"
  before=$(shasum "$dir/main_test.wys" | cut -d' ' -f1)
  solved=0
  attempts=0
  changed=0
  for ((i = 1; i <= K; i++)); do
    if output=$(cd "$dir" && "$wys" test --format json main_test.wys 2>&1); then
      solved=1
      break
    fi
    attempts=$i
    prompt="You are fixing a program written in wys, a new Python-like language.
Edit ONLY lib.wys in the current directory so that \`wys test main_test.wys\` passes.
Do not edit main_test.wys. Keep the public function names and signatures used by main_test.wys.
wys basics: types are int, bool, str, None, unions (A | B), records ({x: int}); there are no implicit conversions;
values that may be None must be checked (if x is not None:); Unknown values must be checked with match and need case _:;
only definitions marked pub can be imported from other files.
Here is the current output of \`wys test --format json main_test.wys\` (compile diagnostics as JSON, or test results):

$output"
    (cd "$dir" && claude -p "$prompt" "${model_args[@]}" --permission-mode acceptEdits \
      --allowedTools "Read" "Edit" --output-format text >/dev/null 2>&1) || true
    if [ "$(shasum "$dir/main_test.wys" | cut -d' ' -f1)" != "$before" ]; then
      changed=1
      solved=0
      break
    fi
  done
  # 最後の修正の後にもう一度確かめる
  if [ "$solved" -eq 0 ] && [ "$changed" -eq 0 ] && (cd "$dir" && "$wys" test main_test.wys >/dev/null 2>&1); then
    solved=1
  fi
  printf '%s\t%s\t%s\t%s\n' "$name" "$solved" "$attempts" "$changed" | tee -a "$out"
done

total=$(($(wc -l < "$out") - 1))
ok=$(awk -F'\t' 'NR > 1 && $2 == 1' "$out" | wc -l | tr -d ' ')
echo
echo "solved $ok / $total within K=$K attempts  (details: $out, $out_dir/$stamp.meta)"
