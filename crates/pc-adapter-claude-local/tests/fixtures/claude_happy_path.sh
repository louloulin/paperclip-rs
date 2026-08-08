#!/bin/sh
printf '%s\n' '{"type":"thread.started","thread_id":"v2_sess"}' '{"type":"item.completed","item":{"type":"agent_message","text":"hi"}}' '{"type":"result","is_error":false,"result":"v2 done","session_id":"v2_sess","model":"claude-opus-4-7","stop_reason":"end_turn","usage":{"input_tokens":7,"output_tokens":3,"cache_read_input_tokens":2}}'
