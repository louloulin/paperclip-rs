# R508 — Pipeline + Routine schemas + 9 PATCH/DELETE/Routines hints

> 配套: `proposal.md` V3 + `ARCHITECTURE.md` §6 R508 路线图。
> 目标: R507 暂缓的 Pipeline 单 schema 落地, 加 Routine 单 schema + list, 扩展 path hints 覆盖 PATCH/DELETE verbs + routines collection, 提升 path coverage 27% → 43%。

## 改动

### 1. `crates/pc-openapi/src/dto_schemas.rs` — 3 个新 schemas

**`pipeline_schema()`** (Pipeline 单 schema, 12 字段):
- id, companyId, projectId, key, name, description, enforceTransitions
- createdByUserId, createdByAgentId, archivedAt, createdAt, updatedAt
- `enforceTransitions` boolean (Pipeline-specific 概念)

**`routine_schema()`** (Routine 单 schema, 30 字段 — 域最复杂):
- 核心: id, companyId, title, priority enum [low/normal/high/urgent], status enum [active/paused/archived]
- 调度: concurrencyPolicy enum [skip/queue/parallel], catchUpPolicy enum [none/latest/all], activityGatePolicy, activityGateScope
- 元数据: originKind, originId, variables (object), env (object)
- 关联: latestRevisionId, latestRevisionNumber
- 操作: createdBy*, updatedBy*, responsibleUserId
- 时间: lastTriggeredAt, lastEnqueuedAt, createdAt, updatedAt

**`routine_list_schema()`** (array of Routine, R507 同 pattern)

**`register_core_dtos` 扩展**: 12 → 15 schemas
**`CORE_DTO_NAMES` 扩展**: 12 → 15 项

### 2. `crates/pc-http/src/routes/openapi.rs` — 9 新路由 hints

**4 个 PATCH hints** (round-trip X → X):
| Path | Method | Request | Response |
|---|---|---|---|
| `/api/companies/{id}` | PATCH | Company | Company |
| `/api/agents/{id}` | PATCH | Agent | Agent |
| `/api/issues/{id}` | PATCH | Issue | Issue |
| `/api/decisions/{id}` | PATCH | Decision | Decision |

**4 个 DELETE hints** (no body):
| Path | Method | Request | Response |
|---|---|---|---|
| `/api/companies/{id}` | DELETE | None | None |
| `/api/agents/{id}` | DELETE | None | None |
| `/api/issues/{id}` | DELETE | None | None |
| `/api/decisions/{id}` | DELETE | None | None |

DELETE 返回 `None` 让 `build_responses_block` 不插入 content, 仍返回 200 (R509+ 可改成 204)。

**1 个 Routines collection hint**:
| Path | Method | Request | Response |
|---|---|---|---|
| `/api/routines` | GET | None | RoutineList |

**总计**: 15 → **24 hints**

### 3. Coverage 测试升级 (15 → 24 hints)

**`r506_path_schema_hint_coverage_includes_all_twenty_four`**:
- 类型签名改为 `(&str, &str, Option<&str>, Option<&str>)` (response 也是 Option, 支持 DELETE)
- 24 case 全覆盖 (10 + 5 + 9 = 24)

## 测试 (12 个新 R508 tests)

### pc-openapi (7 tests)

| 测试 | 验证 |
|---|---|
| `r508_pipeline_schema_has_required_fields` | Pipeline.required: id, companyId, key, name, enforceTransitions, createdAt, updatedAt |
| `r508_pipeline_schema_nullable_fields_use_array_null_pattern` | Option<T> → `["T", "null"]` (projectId, archivedAt) |
| `r508_routine_schema_status_enum_has_three_values` | Routine.status enum: active/paused/archived |
| `r508_routine_schema_concurrency_policy_enum` | Routine.concurrencyPolicy enum: skip/queue/parallel |
| `r508_routine_list_schema_uses_ref` | RoutineList = array of $ref:Routine |
| `r508_schemas_round_trip_through_yaml` | YAML 含 Pipeline / Routine / RoutineList 节点 |
| `r508_register_core_dtos_registers_fifteen` | 15 schemas 总数 |

### pc-http (5 tests)

| 测试 | 验证 |
|---|---|
| `r508_companies_patch_returns_company` | PATCH round-trip |
| `r508_agents_delete_has_no_body` | DELETE: request=None, response=None |
| `r508_issues_patch_round_trips` | PATCH round-trip |
| `r508_decisions_delete_has_no_body` | DELETE: no body |
| `r508_routines_get_returns_list` | Routines GET → RoutineList |

## 验证

```
cargo test -p pc-openapi --lib           34 passed (27 pre + 7 R508 new)
cargo test -p pc-http --lib routes::openapi 34 passed (29 pre + 5 R508 new)
cargo check --workspace                  0 errors (171 pre-existing pc-http warnings)
rustfmt 2 changed files                  0 diffs
```

## 设计要点

1. **DELETE 返回 None 表达 "no body"**: 不强行返回 "204 No Content" 占位; 让 OpenAPI consumer 看 `response: None` 推断
2. **Routine 是最复杂的 schema**: 30 字段, 4 个 enum, 5 个时间戳, 6 个 FK — R508 一次性建模避免 R509+ 还要补
3. **PATCH round-trip (X → X)**: 与 POST 一致, REST 语义清晰
4. **Coverage 测试类型签名升级**: 从 `(str, str, str, Option<str>)` 到 `(str, str, Option<str>, Option<str>)`, 反映 DELETE 真实语义
5. **`>= 5` 模式扩展**: R504 `== 5` → `>= 5`, R507 `== 9` → `== 12` → R508 `== 15`, 每次精确数字校验 schema count 防漏注册

## V3 真实进度更新

- **R507 末**: ~78% (12 schemas, 15 hints)
- **R508 末**: **~85%** — 15 schemas (Pipeline + Routine + RoutineList) + 24 hints (4 PATCH + 4 DELETE + Routines GET + 既有 15)
- **R509+ 待做**: per-path 错误响应 schema (400/422/500) + 56 主路由剩余 32 路由 hints + server-side pagination query params

## 教训

1. **DELETE 语义的双重表达**: 用 `Option<None>` 而非 "no body" 字符串, 让类型系统帮我们追踪 "此操作是否返回内容"
2. **Routine 是 V3 最大的 schema**: 30 字段, 提前一次性建模避免后续 patch, 但也是单测最多的地方 (7 个 R508 测试覆盖各种 enum + nullable pattern)
3. **enum 值必须列举全**: Routine.concurrencyPolicy 的 `["skip", "queue", "parallel"]` 是基于上游 paperclip `concurrencyPolicy` 表; 漏一个就破坏 wire compatibility

## 下一步 (R509+)

| 轮次 | 目标 | 价值 |
|---|---|---|
| **R509** | V3 继续: 错误响应 schema (ValidationError 400/422/500) | V3 85% → 90% |
| **R510** | V5 Auth: refresh token rotation | V5 55% → 70% |
| **R511** | V6 路由补全: companies 子路由 5 个 | V6 86% → 95% |
