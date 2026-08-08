#!/bin/sh
# 验证：如果 args 中包含 --resume，立即退出（说明 session_id 未通过校验就被透传了）
if echo "$@" | grep -q -- '--resume'; then
  echo 'unexpected --resume' >&2
  exit 2
fi
printf '%s\n' '{"type":"result","is_error":false,"result":"ok","session_id":"sess1"}'
