# R504 — `pc-openapi` 加 5 个核心 DTO schema (Decision/Company/Issue/Agent/HeartbeatRun)

> 配套: `proposal.md` V3 + `ARCHITECTURE.md` §6 R504 路线图。
> 目标: 给 OpenAPI 3.1 spec 注入 5 个最-consumed DTO schema, 取代当前路由扫描器只生成空 `responses: {200: {description: OK}}` 的占位。

## 改动

### 1. `crates/pc-openapi/src/dto_schemas.rs` (新增, 396 LOC)

**5 个 hand-written OpenAPI 3.1 component schemas** (mirror `pc-repos` row types):

| Schema | Fields | Mirrors |
|---|---|---|
| `Decision` | 22 字段 (id, companyId, bundleId, options[], status enum, expiresAt, signedSpec, targetSnapshots, continuationPolicy, metadata, ...) | `pc_repos::decision::DecisionRow` |
| `Company` | 19 字段 (id, name, status enum [active/paused/archived], budgetMonthlyCents, issuePrefix, ...) | `pc_repos::company::CompanyRow` |
| `Issue` | 17 字段 (id, companyId, parentId, status, workMode, priority enum, assigneeAgentId, ...) | `pc_repos::issue::IssueRow` |
| `Agent` | 21 字段 (id, name, role, adapterType, adapterConfig object, permissions object, budgetMonthlyCents, ...) | `pc_repos::agent::AgentRow` |
| `HeartbeatRun` | 7 字段 (id, agentId, status enum [running/succeeded/failed/cancelled/timed_out], startedAt, finishedAt, error, prompt) | `pc_repos::heartbeat::HeartbeatRunSummaryRow` |

**2 个 companion schemas**:
- `DecisionOption` (id, label, effects[], targetIds[])
- `DecisionEffect` (type enum [comment_on_issue / update_issue_status / cancel_issue_tree / resolve_blocker / update_issue_assignee / add_issue_label / remove_issue_label], targetIssueId, staleness, body, status)

**注册 façade**:
- `register_core_dtos(&mut OpenApiRegistry)` — 幂等注册 5 个 schema
- `into_schema_ref(&Value) -> SchemaRef` — 把 JSON schema 包装成 SchemaRef
- `CORE_DTO_NAMES: &[&str]` — 常量, 给 UI type sync (V4) 用

**设计选择**:
- **手写而非 derive**: 当前 Rust row types 用 `serde_json::Value` 表示 `options / permissions / execution_state` 等动态字段, 不能直接 `#[derive(ToSchema)]`. 等 R505+ 用 `utoipa` 时, 这个文件就是 reference for what each schema should look like.
- **不依赖 `pc-repos`**: pc-openapi 保持低耦合, 只用 `serde_json::Value` 定义 schemas
- **3.1 nullable pattern**: `Option<T>` → `{"type": ["T", "null"]}` (符合 OpenAPI 3.1)
- **enum 枚举明示**: `status: { enum: ["open", "decided", "cancelled", "expired", "dismissed"] }` (UI type 生成友好)

### 2. `crates/pc-openapi/src/lib.rs`

- 加 `pub mod dto_schemas;`
- 加 `pub use dto_schemas::{register_core_dtos, CORE_DTO_NAMES};`

## 测试 (11 个 R504 新测试)

| 测试 | 验证 |
|---|---|
| `r504_register_core_dtos_registers_five` | 注册 5 个, `schema_count() == 5` |
| `r504_register_core_dtos_is_idempotent` | 同一 registry 注册两次不会 double count (覆盖, 不同 registry 各注册两次同样) |
| `r504_core_dto_names_constant_matches_registry` | `CORE_DTO_NAMES` 5 项在 `components.schemas` 里都能找到 |
| `r504_decision_schema_has_required_fields` | Decision required: id, companyId, title, body, options, status |
| `r504_company_schema_has_status_enum` | Company.status enum: active / paused / archived |
| `r504_issue_schema_uses_nullable_pattern` | Option<Uuid> → `["string", "null"]` |
| `r504_agent_schema_marks_known_status_values` | Agent.status enum: active / paused / error |
| `r504_heartbeat_run_schema_required_started_at` | HeartbeatRun required: startedAt |
| `r504_schemas_serialize_to_openapi_3_1` | 序列化后包含 `openapi: "3.1.0"` + 5 个 schema 名 |
| `r504_schemas_round_trip_through_yaml` | YAML 输出包含 5 个 schema 名 |
| `r504_into_schema_ref_does_not_panic_on_empty_properties` | 防御: 无 properties 也不 panic |

## 验证

```
cargo test -p pc-openapi --lib    23 passed (12 pre + 11 R504 new)
cargo check -p pc-openapi         0 errors
rustfmt -p pc-openapi --check     0 diffs (新文件 dto_schemas.rs)
cargo check --workspace           0 errors
```

## 设计要点 (高内聚低耦合)

1. **零依赖 pc-repos**: pc-openapi 不 import `DecisionRow` 等具体类型, 只用 `serde_json::Value` 定义 schema. 这意味着 schemas 可以独立演进, 而 Rust row types 可以演进而 schema 不动 (反之亦然)
2. **SchemaRef 复用**: 用现有 `SchemaRef::object_with(properties, required)` 包装 JSON, 不引入新 schema 抽象
3. **幂等 register**: 同一 registry 多次 `register_core_dtos` 不会 double count (R504 测试覆盖)
4. **常量与代码同步**: `CORE_DTO_NAMES` 常量与 `register_core_dtos` 函数一一对应, 测试保证不漂移

## 后续 (R505+)

| 轮次 | 目标 | 价值 |
|---|---|---|
| **R505** | 把 `register_core_dtos` 接入 `pc-http::routes::openapi`, 让 `/openapi.json` / `/openapi.yaml` 真实包含 5 个 schemas | V3 50% → 60% |
| **R506** | 加 path-level `requestBody` + `responses` schemas (覆盖 56 主路由中最常用的 10 个) | V3 60% → 70% |
| **R507** | V5 Auth: refresh token rotation | V5 55% → 70% |
