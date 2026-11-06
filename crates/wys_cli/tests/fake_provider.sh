#!/bin/sh
# テスト用のプロバイダ（`WYS_LLM_PROVIDER=cmd:<path>`、ADR-0023 論点 1・2）: 受け取った要求を 1 行ずつ $FAKE_LOG に追記し、
# n 回目の要求には $FAKE_REPLIES の n 行目をそのまま返す。行が尽きたら何も返さずに終わる（子が死んだときの検査）
n=0
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$FAKE_LOG"
  n=$((n + 1))
  reply=$(sed -n "${n}p" "$FAKE_REPLIES")
  if [ -z "$reply" ]; then
    exit 0
  fi
  printf '%s\n' "$reply"
done
