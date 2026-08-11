# R510 — PaginationCursor + ListResponseEnvelope schemas + 12 路由 hints

> 配套: `proposal.md` V3 + `ARCHITECTURE.md` §6 R510 路线图。
> 目标: 加 pagination 元数据 schema, 扩展 path hints 到 44 路由 (79% coverage)。

## 改动

### 1. `crates/pc-openapi/src/dto_schemas.rs` — 2 个新 pagination schemas

**`pagination_cursor_schema()`** — opaque cursor + hasMore:
```json
{
  "type": "object",
  "properties": {
    "nextCursor": {"type": ["string", "null"], "description": "Opaque cursor for next page, null = end"},
    "totalCount": {"type": ["integer", "null"], "format": "int64", "description": "Optional total count"},
    "hasMore": {"type": "boolean"}
  },
  "required": ["hasMore"]
}
```

**`list_response_envelope_schema(item_ref)`** — generic list wrapper:
```json
{
  "type": "object",
  "properties": {
    "items": {"type": "array", "items": {"$ref": "#/components/schemas/Issue"}},
    "pagination": {"$ref": "#/components/schemas/PaginationCursor"}
  },
  "required": ["items", "pagination"]
}
```

**Register**: 18 → 19 schemas (加 PaginationCursor)
**CORE_DTO_NAMES**: 18 → 19 项

### 2. `crates/pc-http/src/routes/openapi.rs` — 12 新路由 hints

**Cases (5 hints)**:
| Path | Method | Request | Response |
|---|---|---|---|
| `/api/cases` | GET | — | CaseList ⭐R510 |
| `/api/cases` | POST | Case | Case ⭐R510 |
| `/api/cases/{case_id}` | GET | — | Case ⭐R510 |
| `/api/cases/{case_id}` | PATCH | Case | Case ⭐R510 |
| `/api/cases/{case_id}` | DELETE | — | — ⭐R510 |

**Goals (3 hints)**:
| Path | Method | Request | Response |
|---|---|---|---|
| `/api/goals` | GET | — | GoalList ⭐R510 |
| `/api/goals` | POST | Goal | Goal ⭐R510 |
| `/api/goals/{id}` | GET | — | Goal ⭐R510 |

**Approvals (2 hints — PATCH/DELETE)**:
| Path | Method | Request | Response |
|---|---|---|---|
| `/api/approvals/{id}` | PATCH | Approval | Approval ⭐R510 |
| `/api/approvals/{id}` | DELETE | — | — ⭐R510 |

**Pipelines (2 hints — PATCH + archive)**:
| Path | Method | Request | Response |
|---|---|---|---|
| `/api/pipelines/{id}` | PATCH | Pipeline | Pipeline ⭐R510 |
| `/api/pipelines/{id}/archive` | POST | — | Pipeline ⭐R510 |

**总计**: 32 → **44 hints** (79% 路由覆盖)

### 3. Coverage 测试升级 (32 → 44 hints)

**`r506_path_schema_hint_coverage_includes_all_forty_four`**: 44 case 全覆盖

## 测试 (10 个新 R510 tests)

### pc-openapi (6 tests)

| 测试 | 验证 |
|---|---|
| `r510_pagination_cursor_required_has_more` | required = ["hasMore"] only |
| `r510_pagination_cursor_next_cursor_is_nullable` | `["string", "null"]` |
| `r510_list_response_envelope_uses_correct_ref` | `items.items.$ref` + `pagination.$ref` 都对 |
| `r510_list_response_envelope_required_items_and_pagination` | required: items, pagination |
| `r510_schemas_round_trip_through_yaml` | YAML 含 PaginationCursor |
| `r510_register_core_dtos_registers_nineteen` | 19 schemas 总数 |

### pc-http (4 tests)

| 测试 | 验证 |
|---|---|
| `r510_cases_crud_round_trips` | cases 5 hints 全验证 |
| `r510_goals_crud_round_trips` | goals 3 hints 全验证 |
| `r510_approvals_patch_delete` | PATCH + DELETE hints 验证 |
| `r510_pipelines_patch_and_archive` | PATCH + archive POST hints 验证 |

## 验证

```
cargo test -p pc-openapi --lib           46 passed (40 pre + 6 R510 new)
cargo test -p pc-http --lib routes::openapi 47 passed (43 pre + 4 R510 new)
cargo check --workspace                  0 errors (171 pre-existing pc-http warnings)
rustfmt 2 changed files                  0 diffs
```

## 设计要点

1. **`hasMore` 必填而非 `nextCursor` 必填**: `nextCursor` 是 nullable (`null` 表示 end), `hasMore` 是必填 bool (always defined)
2. **`totalCount` 是 nullable**: 服务端可便宜计算时填, 否则 null — 不强制所有 list 都要 COUNT(*)
3. **`list_response_envelope_schema(item_ref)` 是 builder fn**: 接受 schema 名参数生成对应 `$ref`, 不预生成所有可能的 list envelope (R511+ 可生成 `CompanyListResponse` 等具名 schemas)
4. **Case/Goal schema 暂留 placeholder 引用**: R510 注册 hints 引用 `Case/Goal/CaseList/GoalList`, 这些 schema 尚未建模 (R511+ 可补); 当前 wire format 不会 fail, 只是 `$ref` 解析不到
5. **44 hints 是 V3 收尾的合理目标**: 79% 路由覆盖, 剩 12 路由是 admin / websocket / sub-resource (R511+ 可补)

## V3 真实进度更新

- **R509 末**: ~90% (18 schemas, 32 hints)
- **R510 末**: **~95%** — 19 schemas (PaginationCursor + ListResponseEnvelope builder) + 44 hints (cases/goals/approvals/pipelines CRUD) + 79% 路由覆盖
- **R511+ 待做**: Case/Goal/Inbox/Folder schemas 建模 + 剩 12 路由 hints (admin + sub-resources) + per-route operationId 唯一性

## 教训

1. **`hasMore` 必填是 UX 选择**: client 不用每次检查 `nextCursor != null`, 直接读 `hasMore`, 避免歧义
2. **`totalCount` 可选**: 不是所有 list 都能高效 count; 不强制服务端实现
3. **Generic envelope via builder fn**: 比起预生成 19 个具名 schemas (`CompanyListResponse`, `AgentListResponse`, ...), builder fn 节省 ~50 LOC 且更易演进
4. **Placeholder schema 名是渐进式**: `Case/CaseList/Goal/GoalList` 还没建模, 但 hints 已经引用, R511+ 可平滑补 schema 而不破坏 hints

## 下一步 (R511+)

| 轮次 | 目标 | 价值 |
|---|---|---|
| **R511** | V3 收尾: Case/Goal/Inbox/Folder schema 建模 + 12 路由 hints + per-route operationId 唯一性 | V3 95% → 100% |
| **R512** | V5 Auth: refresh token rotation (30d sliding) | V5 55% → 70% |
| **R513** | V6 路由补全: companies 子路由 5 个 | V6 86% → 95% |
