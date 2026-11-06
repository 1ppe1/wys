#!/bin/sh
# PoC4 のテスト用のプロバイダ: 1 回目はツール add の要求、2 回目は最終の JSON を返す。受け取った要求を $FAKE_LOG に書く
n=0
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$FAKE_LOG"
  n=$((n + 1))
  if [ "$n" = 1 ]; then
    printf '%s\n' '{"text":"","tool_calls":[{"id":"t1","name":"add","args":{"a":2,"b":3}}],"usage":{"input_tokens":10,"output_tokens":5},"stop":"tool_use","assistant":[{"type":"thinking","thinking":"","signature":"sig"},{"type":"tool_use","id":"t1","name":"add","input":{"a":2,"b":3}}]}'
  else
    printf '%s\n' '{"text":"{\"title\":\"plan\",\"steps\":5}","tool_calls":[],"usage":{"input_tokens":20,"output_tokens":8},"stop":"end_turn","assistant":[{"type":"text","text":"{\"title\":\"plan\",\"steps\":5}"}]}'
  fi
done
