#!/bin/sh
# 第一次调用：返回 unknown session 错误，exit 1
# 第二次调用：返回成功（fresh session）
# 通过环境变量 PAPERCLIP_RETRY_COUNTER 接收计数器文件路径
counter_file="${PAPERCLIP_RETRY_COUNTER}"
count=$(cat "$counter_file" 2>/dev/null || echo 0)
count=$((count+1))
echo "$count" > "$counter_file"
if [ "$count" -eq 1 ]; then
  printf '%s\n' '{"type":"result","is_error":true,"errors":[{"message":"No conversation found with session ID: abc"}],"session_id":"abc"}'
  exit 1
else
  printf '%s\n' '{"type":"thread.started","thread_id":"fresh_sess"}' '{"type":"result","is_error":false,"result":"fresh done","session_id":"fresh_sess","model":"claude-opus-4-7"}'
fi
