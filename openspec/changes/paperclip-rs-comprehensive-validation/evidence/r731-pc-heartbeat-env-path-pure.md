# R731 — pc-heartbeat/env_path_pure.rs

## 目标

补足 Node paperclip/server/src/services/heartbeat.ts 中
sameResolvedPath / deriveRepoNameFromRepoUrl / isFalsyRuntimeEnvValue /
truncateRunEventString / appendExcerpt 五个零依赖 pure helper。

## 新增 helpers (5 个)

| Node 函数 | Rust 函数 |
|---|---|
| sameResolvedPath(left, right) | same_resolved_path(left, right) |
| deriveRepoNameFromRepoUrl(url) | derive_repo_name_from_repo_url(repo_url) |
| isFalsyRuntimeEnvValue(value) | is_falsy_runtime_env_value(value) |
| truncateRunEventString(value) | truncate_run_event_string(value) |
| appendExcerpt(prev, chunk) | append_excerpt(prev, chunk) |

## 常量

- MAX_EXCERPT_BYTES = 4096
- MAX_RUN_EVENT_PAYLOAD_STRING_CHARS = 8192
- RUNTIME_ENV_FALSY = ["", "false", "0", "off", "no"]

## 测试结果

cargo test -p pc-heartbeat --lib env_path_pure
test result: ok. 16 passed; 0 failed

## 关键设计

- same_resolved_path：用 std::path::absolute 替代 Node path.resolve；空路径直接 None 返回 false
- derive_repo_name_from_repo_url：极简 URL 解析（http/https/git@ 前缀检查），fallback 到 split('/') 取末段
- is_falsy_runtime_env_value：trim + lowercase 后命中 RUNTIME_ENV_FALSY 白名单
- truncate_run_event_string：按 codepoint 切（unicode 安全）
- append_excerpt：按 byte cap 截断尾部（Node MAX_EXCERPT_BYTES）

## 文件

- 新增：crates/pc-heartbeat/src/env_path_pure.rs (7060 bytes)
- 修改：crates/pc-heartbeat/src/lib.rs (+1 行 pub mod env_path_pure;)
