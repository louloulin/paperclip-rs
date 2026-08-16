# R729-R730 — pc-tool/connection/pure.rs + pc-heartbeat/run_log_pure.rs

## 目标

补足 Node paperclip/server/src/services/{tool-access,heartbeat}.ts 中
零 DB pure helpers，重点是 connection 字段校验与 run log 压缩。

## R729 — pc-tool/connection/pure.rs

### 新增 helpers (6 个)

| Node 语义 | Rust 函数 |
|---|---|
| connection name 校验（非空 + 长度上限 128） | validate_connection_name(name) |
| connection status 校验（在白名单中） | validate_connection_status(status) |
| config 必须是 JSON object | validate_config_object(config) |
| credential refs 必须是 JSON array | validate_credential_refs(refs) |
| status 小写化 + trim | normalize_status(status) |
| connection name 等价比较（trim + case-insensitive） | connection_name_eq(a, b) |

### 常量

- TOOL_CONNECTION_NAME_MAX_LEN = 128
- ALLOWED_CONNECTION_STATUSES = [active, paused, error, reconnecting, disabled]

### 测试结果

cargo test -p pc-tool --lib connection::pure
test result: ok. 16 passed; 0 failed

## R730 — pc-heartbeat/run_log_pure.rs

### 新增 helpers (4 个)

| Node 函数 | Rust 函数 |
|---|---|
| compactRunLogChunk(chunk, maxChars) | compact_run_log_chunk(chunk, max, head_frac, tail_frac) |
| (default 参数封装) | compact_run_log_chunk_default(chunk) |
| redactInlineBase64ImageData(chunk) | redact_inline_base64_image_data(chunk) |
| (helper, 计算 head/tail/marker) | plan_compaction(text_len, max, head_frac, tail_frac) |

### 常量

- DEFAULT_MAX_PERSISTED_LOG_CHUNK_CHARS = 20000（对齐 Node MAX_PERSISTED_LOG_CHUNK_CHARS）
- DEFAULT_HEAD_FRACTION = 0.6（head 60%）
- DEFAULT_TAIL_FRACTION = 0.25（tail 25%）

### 测试结果

cargo test -p pc-heartbeat --lib run_log_pure
test result: ok. 7 passed; 0 failed

## 关键设计

- connection/pure.rs：
  - 校验函数返回 Result<(), String> 与 service 层 Validation 错误信息字面对齐
  - normalize_status 不做校验，只做大小写规范化
  - connection_name_eq 用 eq_ignore_ascii_case 避免 unicode 复杂性
- run_log_pure.rs：
  - INLINE_BASE64_IMAGE_DATA_RE Lazy<Regex> 编译一次
  - Pipeline: redact_base64 → redact_sensitive_text → truncate
  - head/tail 按 codepoint 切（用 chars() 而非 bytes）保证 unicode 安全
  - plan_compaction 抽出来便于单测 + 复用

## 文件

### R729
- 新增：crates/pc-tool/src/connection/pure.rs (4798 bytes)
- 修改：crates/pc-tool/src/connection/mod.rs (+1 行 mod pure;)

### R730
- 新增：crates/pc-heartbeat/src/run_log_pure.rs (5670 bytes)
- 修改：crates/pc-heartbeat/src/lib.rs (+1 行 pub mod run_log_pure;)
- 修改：crates/pc-heartbeat/Cargo.toml (+1 依赖 pc-secret-redaction)
