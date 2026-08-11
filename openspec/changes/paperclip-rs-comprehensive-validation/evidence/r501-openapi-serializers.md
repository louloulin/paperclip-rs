# R501 — `pc-openapi` 序列化 helpers (V3 OpenAPI 起手)

> 配套: `proposal.md` V3 + `ARCHITECTURE.md` §6 R501 路线图。
> 目标: 给 `OpenApiSpec` 加 JSON / YAML 序列化 + 统计 helpers, 为 `GET /openapi.json` 和 `GET /openapi.yaml` 端点铺路。

## 改动

### 1. `crates/pc-openapi/src/serializers.rs` (新增)

7 个新 method on `OpenApiSpec` + 手写 YAML 发射器：

| Helper | 作用 |
|---|---|
| `path_count() -> usize` | 统计已注册 URL 数量（1:1 对应 `routes/openapi.ts` `countPaths` 检查）|
| `operation_count() -> usize` | 统计 HTTP 操作总数（一条 path 有 GET+POST = 2 operations）|
| `schema_count() -> usize` | 统计 named schemas 数量 |
| `to_json_string() -> String` | pretty JSON（fallback `"{}"` 而非 panic）|
| `to_json_value() -> serde_json::Value` | 紧凑 JSON value（fallback `{}`）|
| `to_yaml_string() -> Result<String, OpenApiSerializeError>` | YAML（手写发射器，不引入 `serde_yaml` 依赖）|

**`OpenApiSerializeError`**（2 个变体）：
- `EmptySpec`: 提醒 caller 先 `register_path` 至少一次
- `Json(serde_json::Error)`: JSON 兜底（实际不会触发，infailable）

**YAML 发射器设计选择**：
- 不引入 `serde_yaml`（减少 workspace 编译时间和二进制体积）
- 只支持 4 种 scalar（Null / Bool / Number / String）+ Object + Array
- 数组元素用 `- ` 行首，对象子节点用 `  ` 缩进
- **key 不加引号**（已修：原来用 `serde_json::to_string(k)` 会包引号，改成直接用 `k`）

### 2. `crates/pc-openapi/src/lib.rs`

加 `pub mod serializers;` 让 module 可被外部访问（保持与其他子模块一致：`builder/path/schema/spec` 都是 `pub mod`）。

### 3. `crates/pc-openapi/Cargo.toml`

`thiserror = { workspace = true }` 已在依赖列表里（`spec.rs` 用过 `thiserror::Error` 派生），无需新增。

## 测试 (7 个 R501 新测试)

| 测试 | 验证 |
|---|---|
| `r501_path_count_zero_on_empty` | `empty_spec().path_count() == 0` |
| `r501_path_count_one_per_url` | 注册 2 条 URL → `path_count == 2` `operation_count == 2` |
| `r501_operation_count_handles_multi_method` | 同 URL 注册 GET+POST → `path_count == 1` `operation_count == 2` |
| `r501_schema_count_zero_on_empty` | 空 spec → `schema_count == 0` |
| `r501_schema_count_after_register` | 注册 2 个 schema → `schema_count == 2` |
| `r501_to_json_string_contains_openapi_version` | 包含 `"openapi": "3.1.0"` 和 `"title": "T"` |
| `r501_to_json_value_is_object` | 返回 object，含 `v["openapi"] == "3.1.0"` |
| `r501_to_yaml_string_contains_top_level_keys` | 包含 `openapi:` `info:` `paths:` `components:`（key 无引号）|

## 验证

```
cargo test -p pc-openapi --lib    12 passed (5 pre-existing + 7 R501 new)
cargo check -p pc-openapi         0 errors, 0 warnings
cargo fmt -p pc-openapi --check   no diff
cargo check --workspace           0 errors (pc-cli 的 install_command_with_paths dead_code 是 R495 留下的)
```

## 教训

1. **YAML key 不能用 `serde_json::to_string(k)`**: 该函数会包引号，YAML 期望 `openapi:` 而非 `"openapi":`。改用原始 `k` 字符串。
2. **手写 YAML 发射器可接受**: 只支持 OpenAPI 需要的 4 种 scalar + 复合类型，~50 行代码；比拉 `serde_yaml` 节省 ~150 编译时间 + 减少二进制体积。
3. **fallback 而非 panic**: `to_json_string` 返回 `"{}"` 而非 panic，遵循 Rust "never panic in serialization" 原则。
4. **`#[derive(thiserror::Error)]` 已在 `pc-openapi` 依赖里**: `spec.rs` 已用，所以 `serializers.rs` 直接复用，无需新增 `Cargo.toml` 条目。

## 下一步 (R502+)

按 `ARCHITECTURE.md` §6：

| 轮次 | 目标 | 价值 |
|---|---|---|
| **R502** | R492 helper 接入：`pc-decisions::DecisionService.create` 扩签名接 options/inputs/expiresAt | 验证 R492 helper 真实使用 |
| **R503** | V3 OpenAPI 深化：`utoipa` derive + 56 path 自动注册 | V3 5% → 60% |
| **R504** | V5 Auth：refresh rotation + CSRF double-submit | V5 55% → 75% |
