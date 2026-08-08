#!/bin/sh
# 模拟 poisoned session error：返回带有非 msg_ 前缀 previous_message_id 的结果
# 通过环境变量 PAPERCLIP_RETRY_COUNTER 接收计数器文件路径
counter_file="${PAPERCLIP_RETRY_COUNTER}"
count=$(cat "$counter_file" 2>/dev/null || echo 0)
count=$((count+1))
echo "$count" > "$counter_file"
if [ "$count" -eq 1 ]; then
  # 第一次：poisoned 错误，exit 1（必须包含 "diagnostics.previous_message_id" 和 "starts with `msg_`"）
  printf '%s\n' '{"type":"result","is_error":true,"errors":[{"message":"diagnostics.previous_message_id `bad-id` starts with `msg_`"}],"session_id":"abc"}'
  exit 1
else
  # 第二次：fresh session 成功
  printf '%s\n' '{"type":"thread.started","thread_id":"fresh_poisoned"}' '{"type":"result","is_error":false,"result":"fresh done","session_id":"fresh_poisoned","model":"claude-opus-4-7"}'
fi
