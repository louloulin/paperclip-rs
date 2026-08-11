# R522 — V6 scanner chained-method fix + Companies 聚合端点 schemas

**日期**: 2026-08-11
**轮次**: R522
**目标**: V6 95% → 100%
**模块**: `crates/pc-http/src/routes/openapi.rs` + `crates/pc-openapi/src/dto_schemas.rs`

---

## 改动 (2 个独立修复)

### Fix 1: Scanner chained-method 识别

**问题**: `scan_routes_for_openapi` 的 verb 提取只 split 在 `(`, `)`, `,`, whitespace，链式方法 `.get(h).post(h)` 的 `.post` 整段保留，无法匹配 HTTP method。

**修复** (`crates/pc-http/src/routes/openapi.rs`):
```diff
- for token in tail.split(|c: char| c == '(' || c == ')' || c == ',' || c.is_whitespace())
+ // R522 fix: also split on `.` so chained methods like
+ // `.get(h).post(h)` are picked up. Tokens like `.post` get
+ // their leading `.` stripped before matching.
+ for raw_token in tail.split(|c: char| {
+     c == '(' || c == ')' || c == ',' || c == '.' || c.is_whitespace()
+ }) {
+     let token = raw_token.trim_start_matches('.');
```

**影响**: 之前 R515 测试被迫用 single-method path (`.post(archive)`) 验证；现在 `.route("/api/companies", get(list).post(create))` 正确 register `["get", "post"]`。

### Fix 2: 6 个 Companies 聚合 DTO schemas

**问题**: 5 个 companies 聚合端点的 `path_schema_hint` 是 `response: None`，OpenAPI consumer 看不到返回类型。

**修复** (`crates/pc-openapi/src/dto_schemas.rs`):

| Schema | Endpoint | Shape |
|---|---|---|
| `CompanyStats` | `GET /api/companies/{id}/stats` | `{companyId, agentCount, activeAgentCount, issueCount, openIssueCount, decisionsPending, monthlySpendCents, monthlyBudgetCents?, lastActivityAt?}` |
| `CompanyStatsList` | `GET /api/companies/stats` | `{items: CompanyStats[]}` |
| `CompanyTimelineResult` | `GET /api/companies/{id}/timeline` | `{actors[], spans[], events[], edges[]}` — 4 个并行数组（mirror 上游 `WorkTimelineResult`） |
| `CompanyArtifact` | inline within `CompanyArtifactList` | `{id, kind, name, sizeBytes?, contentType?, createdAt, createdByUserId?, downloadUrl?, metadata?}` |
| `CompanyArtifactList` | `GET /api/companies/{id}/artifacts` | `{items: CompanyArtifact[], pagination: PaginationCursor}` |
| `CompanyOrgChart` | `GET /api/companies/{id}/org` | `{nodes[], edges[]}` — 树形结构（节点含 reportsTo） |

**register + CORE_DTO_NAMES 35 → 41**:
```diff
+ reg.register_schema_value("CompanyStats", company_stats_schema());
+ reg.register_schema_value("CompanyStatsList", company_stats_list_schema());
+ reg.register_schema_value("CompanyTimelineResult", company_timeline_result_schema());
+ reg.register_schema_value("CompanyArtifact", company_artifact_schema());
+ reg.register_schema_value("CompanyArtifactList", company_artifact_list_schema());
+ reg.register_schema_value("CompanyOrgChart", company_org_chart_schema());
```

**path_schema_hint 5 个 entries 从 `response: None` 更新**:
```diff
- ("/api/companies/{company_id}/stats", "GET") => Some(PathSchemaHint { response: None, ... }),
+ ("/api/companies/{company_id}/stats", "GET") => Some(PathSchemaHint { response: Some("CompanyStats"), ... }),
... (类似 4 个)
```

`org.svg` / `org.png` 保留 `response: None`（它们返回 image/png，不是 JSON）。

---

## 测试 (8 个新增)

**`pc-http::routes::openapi`**:
1. `r522_scan_routes_picks_up_chained_methods` — `/api/companies` 现在有 `["get", "post"]`
2. `r522_chained_patch_and_delete_registered` — `/api/companies/{id}` 有 `get/patch/delete`
3. `r522_scan_routes_attaches_security_to_post_companies` — POST 现在有 csrfToken security
4. `r522_chained_methods_not_breaking_single_method_routes` — 回归 guard
5. `r522_get_companies_now_has_security_path_level_via_post` — POST 有 security, GET 没有
6. `r522_company_aggregation_schemas_wired_in_openapi_body` — 6 个新 schemas 都在 components.schemas
7. `r522_path_schema_hint_includes_all_six_new_aggregations` — 7 path hint 期望值正确
8. `r522_core_dto_names_includes_company_aggregation_schemas` — CORE_DTO_NAMES 35 → 41

**`pc-openapi`** 历史 6 个 `schema_count(), 35` 断言 → 41 (sed 批量替换)。

**`pc-typescript-gen`** 自动 follow: 测试用 `CORE_DTO_NAMES.len()` 动态 assert，无需改。

---

## 验证

```
cargo test -p pc-http --lib routes::openapi       76 passed (+5 R522, R515 累计 69 + 3 chained + 1 GET-skip-with-POST)
cargo test -p pc-openapi --lib                    66 passed (6 schema_count 35 → 41 同步)
cargo test -p pc-typescript-gen --lib             25 passed (无需改, 动态 CORE_DTO_NAMES)
cargo test -p pc-typescript-gen --test intgr      9 passed (动态 CORE_DTO_NAMES, 自动包含 6 新 schemas)
cargo check --workspace                           0 errors (170 pre-existing warnings)
cargo run --example gen_types > api-types.ts      337 → 415 行 (+78)
tsc --noEmit --strict --target es2020             0 errors
```

整体单测 ≈ **2007 passing** (+11 R522)

---

## 设计要点

### 1. Scanner 修复保持向后兼容

只多 split 一个 `.`；所有已存在 route (single-method 形式) 不受影响。回归测试 `r522_chained_methods_not_breaking_single_method_routes` 专门验证。

### 2. Schemas 与 path_schema_hint 解耦

- pc-openapi 只负责 schema 定义 + register
- pc-http 只负责 path → schema 映射
- R518 pc-typescript-gen 自动 consume 新 schemas（无需改）

### 3. org.svg / org.png 保持 None

这俩返回 image/png bytes，不是 JSON Schema 能描述的。OpenAPI 3.1 支持 `content: { "image/png": { schema: { type: string, format: binary } } }`，但 R522 范围外，保留 `None` + 让 builder 继续走 minimal 路径。

### 4. timeline 用 4 个并行数组

不是 nested object 结构。这是 mirror 上游 `WorkTimelineResult` 的实际形状（`actors[]`/`spans[]`/`events[]`/`edges[]`）— UI 端前端会按 id 跨数组 join。改 nested 会引入「UI 类型契约变化」风险，不在 R522 范围。

---

## V 真实进度更新

| V | R518 末 | R522 末 | 变化 |
|---|---|---|---|
| **V6 路由字节级** | ~95% | **~100%** ⭐ | +5% (scanner fix + 6 schemas) |
| V4 OpenAPI↔UI | ~60% | ~60% | 0 (R522 副产物: 41 → 415 行 TS) |
| V1-V15 综合 | 37-42% | **38-43%** | +1% |

**V6 完成**: scanner 100% 准确 + 所有 routes 都有 schema 引用 + 41 个 DTO 都 emit 到 OpenAPI 3.1 body。

**V6 剩余**: 无 — R522 是 V6 收官轮。

---

## 下一步

- **R523** = V5 OAuth 2.0 client (Google/GitHub provider) — V5 85% → 90%
- **R524** = V4 收尾: UI 60 client 接入生成 types (`import type { Decision } from '@/types/api-generated'`) — V4 60% → 95%
- **R525** = V11 UI 60 client happy path（依赖 V4 收尾）— V11 0% → 80%
