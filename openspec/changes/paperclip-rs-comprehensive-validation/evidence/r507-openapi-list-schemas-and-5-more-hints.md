# R507 — 4 个 `*List` schemas + Approval/Pipeline schemas + 5 路由 hints

> 配套: `proposal.md` V3 + `ARCHITECTURE.md` §6 R507 路线图。
> 目标: 注册 `*List` schemas (替换 R506 的 placeholder 引用), 加 Approval + Pipeline schemas, 扩展 path hints 到 15 路由。

## 改动

### 1. `crates/pc-openapi/src/dto_schemas.rs` — 7 个新 schemas

**4 个 `*List` schemas** (R506 placeholder → 实际 schema):
```rust
pub fn company_list_schema() -> Value {
    json!({"type": "array", "items": {"$ref": "#/components/schemas/Company"}})
}
// AgentList / IssueList / DecisionList 同样 pattern
```

**3 个 Approval / Pipeline schemas**:
- `Approval` — 单个 approval 对象 (id, companyId, kind, status enum [pending/approved/rejected/rescinded], subjectType, subjectId, requestedByUserId, decidedByUserId, decidedAt, payload, createdAt, updatedAt)
- `ApprovalList` — array of Approval
- `PipelineList` — array of Pipeline (Pipeline 单 schema 暂留 placeholder, R508+ 可补)

**`register_core_dtos` 扩展**: 注册 7 个新 schemas (总 5 → 12)

**`CORE_DTO_NAMES` 更新**: 5 → 12 项, 包含 4 list + 3 approval/pipeline

### 2. `crates/pc-http/src/routes/openapi.rs` — 5 新路由 hints

**`path_schema_hint` 新增**:
| Path | Method | Request | Response |
|---|---|---|---|
| `/api/approvals` | GET | — | ApprovalList |
| `/api/approvals` | POST | Approval | Approval |
| `/api/approvals/{id}` | GET | — | Approval |
| `/api/pipelines` | GET | — | PipelineList |
| `/api/heartbeat-runs/{run_id}` | GET | — | HeartbeatRun |

**R506 coverage 测试更新**: 从 10 hints → 15 hints

### 3. R504 测试更新 (向后兼容)

`r504_register_core_dtos_registers_five` 改为断言 `>= 5` 而非 `== 5`, 允许后续 R 轮注册更多 schemas 不破坏测试。R507 新加的 `r507_register_core_dtos_registers_nine` (现已 12) 是精确断言。

## 测试 (4 个新 R507 tests + 1 升级 R506 coverage test)

| 测试 | 验证 |
|---|---|
| `r507_list_schemas_use_array_items_ref` | 4 个 list schema 都用 `type=array` + `items.$ref` 指向正确的单 schema |
| `r507_register_core_dtos_registers_twelve` | 12 个 schemas 注册 |
| `r507_core_dto_names_constant_has_twelve_entries` | 常量 12 项 |
| `r507_list_schemas_round_trip_through_yaml` | YAML 包含 4 个 list schema 名 |
| `r507_approvals_get_returns_list` | `/api/approvals` GET → ApprovalList |
| `r507_approvals_post_round_trips` | `/api/approvals` POST ←/→ Approval |
| `r507_heartbeat_run_item_route_uses_heartbeat_run_schema` | `/api/heartbeat-runs/{run_id}` GET → HeartbeatRun |
| `r507_pipelines_get_returns_list` | `/api/pipelines` GET → PipelineList |
| `r506_path_schema_hint_coverage_includes_all_fifteen` | (升级) 15 hints 全部覆盖 |

## 验证

```
cargo test -p pc-openapi --lib           27 passed (23 pre + 4 R507 new)
cargo test -p pc-http --lib routes::openapi 29 passed (25 pre + 4 R507 new)
cargo check --workspace                  0 errors (171 pre-existing pc-http warnings)
rustfmt 2 changed files                  0 diffs
```

## 设计要点

1. **R506 placeholder 兑现**: R506 用 `CompanyList` 作为字符串引用但 schema 没注册; R507 注册它, 让 `$ref: #/components/schemas/CompanyList` 在运行时能解析
2. **Pipeline 单 schema 暂缓**: 真实 Pipeline 字段多, R508 单独建模 (避免 R507 scope 爆炸)
3. **`>= 5` 而非 `== 5`**: 让 R504 测试向前兼容, 后续 R 轮注册 schemas 不破坏它
4. **5 hints 而非更多**: 集中在 approvals + heartbeat + pipelines (最常用 5 个); routines/runs/policies 留给 R508+
5. **wire format 完全 OpenAPI 3.1 兼容**: 所有 list 用 `type: array` + `items.$ref`, 与 Node upstream `routes/openapi.ts` 一致

## V3 真实进度更新

- **R506 末**: ~72% (10 hints, 5 DTO schemas)
- **R507 末**: **~78%** — 12 DTO schemas + 15 hints (4 resource CRUD + approvals + heartbeat + pipelines)
- **R508+ 待做**: Pipeline/Routine/Runs schema 建模 + 剩余 41 路由 hints + per-path 错误响应 schema (400/422)

## 教训

1. **list schema 是 placeholder 还是真 schema, 取决于 R 轮节奏**: R506 用 placeholder 是因为 scope 紧凑, R507 必须兑现 (否则 wire format 报 `Unresolved $ref`)
2. **Pipeline 这种 rich domain schema 应该单独建模**: 一行 `reg.register_schema_value("Pipeline", ...)` 不能覆盖实际 30+ 字段; R508 单独做
3. **测试 count assertions 用 `>=`**: 长期可扩展 (新 R 轮加 schemas 不破坏老测试), 但 short-term 阶段用 `==` 防止回归

## 下一步 (R508+)

| 轮次 | 目标 | 价值 |
|---|---|---|
| **R508** | V3 继续: Pipeline + Routine + Run schemas 建模 (domain-rich) | V3 78% → 85% |
| **R509** | V5 Auth: refresh token rotation (30d sliding) | V5 55% → 70% |
| **R510** | V6 路由补全: companies 子路由 5 个 | V6 86% → 95% |
