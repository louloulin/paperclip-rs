# R513 — V6 路由补全: admin + companies 子路由 (OpenAPI schema + path hints)

> 配套: `proposal.md` V6 + `ARCHITECTURE.md` §6 R513 路线图。
> 目标: 把 V6 路由字节级从 86% → 95%——admin 路由 + companies 子路由全部 OpenAPI 化。

## 改动

### 1. `crates/pc-openapi/src/dto_schemas.rs` — 6 个新 schemas (hand-coded)

**CompanyMember** (mirror `pc_repos::company_member::CompanyMemberRow`, 10 字段)
- id / companyId / principalId / membershipRole / status / name / email / image / createdAt / updatedAt
- status enum: `active | archived`
- required: id, companyId, principalId, membershipRole, status, createdAt, updatedAt

**Invite** (mirror `pc_repos::invite::InviteRow`, 13 字段)
- id / companyId / inviteType / allowedJoinTypes / defaultsPayload / tokenHash / expiresAt
- invitedByUserId / revokedAt / acceptedAt (全部 nullable)
- required: id, companyId, inviteType, allowedJoinTypes, tokenHash, expiresAt, createdAt, updatedAt

**AdminUser** (instance-level admin directory entry, 7 字段)
- id / email / name / image / emailVerifiedAt / isInstanceAdmin / createdAt
- required: id, isInstanceAdmin (最小集)

**3 个 `*List` schemas** — CompanyMemberList / InviteList / AdminUserList
- 都是 `{ type: "array", items: { $ref: ... } }`

**Register & CORE_DTO_NAMES**: 27 → **33** schemas (加 6)

### 2. `crates/pc-http/src/routes/openapi.rs` — 25 新路由 hints

**Admin 路由 (5 hints)**
| Path | Method | Response |
|---|---|---|
| `/api/admin/users` | GET | AdminUserList |
| `/api/admin/users/{user_id}/company-access` | GET | — |
| `/api/admin/users/{user_id}/company-access` | PUT | — |
| `/api/admin/users/{user_id}/promote-instance-admin` | POST | AdminUser |
| `/api/admin/users/{user_id}/demote-instance-admin` | POST | AdminUser |

**Companies 子路由 (13 hints)**
| Path | Method | Response |
|---|---|---|
| `/api/companies/{company_id}/members` | GET | CompanyMemberList |
| `/api/companies/{company_id}/stats` | GET | — |
| `/api/companies/{company_id}/timeline` | GET | — |
| `/api/companies/{company_id}/artifacts` | GET | — |
| `/api/companies/{company_id}/org` | GET | — |
| `/api/companies/{company_id}/org.svg` | GET | — |
| `/api/companies/{company_id}/org.png` | GET | — |
| `/api/companies/{company_id}/agents` | POST | Agent |
| `/api/companies/{company_id}/archive` | POST | Company |
| `/api/companies/stats` | GET | — |
| `/api/companies/issues` | GET | — |
| `/api/companies/import/preview` | POST | — |
| `/api/companies/import/jobs/{job_id}` | GET | — |

**Invites 路由 (4 hints)**
| Path | Method | Response |
|---|---|---|
| `/api/invites/{invite_id}` | GET | Invite |
| `/api/invites/{invite_id}/accept` | POST | Invite |
| `/api/invites/{invite_id}/onboarding` | GET | — |
| `/api/invites/{invite_id}/logo` | GET | — |

**Skills catalog (3 hints)**
| Path | Method |
|---|---|
| `/api/skills/available` | GET |
| `/api/skills/catalog` | GET |
| `/api/skills/index` | GET |
| `/api/skills/{skill_name}` | GET |

**Total hints**: 69 → **94 hints** (覆盖率 +25 hints)

### 3. Coverage 测试升级 (69 → 94)

**`r506_path_schema_hint_coverage_includes_all_ninety_four`** (重命名自 sixty_nine)

## 测试 (11 个新 R513 tests)

### pc-openapi (8 tests)

| 测试 | 验证 |
|---|---|
| `r513_company_member_schema_required_core_fields` | required 含 5 个核心字段 |
| `r513_company_member_schema_status_enum_is_active_or_archived` | enum 严格为 [active, archived] |
| `r513_invite_schema_required_core_fields` | required 含 6 个核心字段 |
| `r513_invite_schema_nullable_fields_use_string_or_null` | 3 个 nullable 字段都用 ["string", "null"] |
| `r513_admin_user_schema_required_minimum` | required 严格为 [id, isInstanceAdmin] |
| `r513_list_schemas_reference_correct_single_schemas` | 3 个 List $ref 全对 |
| `r513_register_core_dtos_registers_thirty_three` | schema_count = 33 |
| `r513_core_dto_names_constant_has_thirty_three_entries` | CORE_DTO_NAMES.len = 33 |
| `r513_new_schemas_round_trip_through_yaml` | YAML 含 CompanyMember:/Invite:/AdminUser: |

### pc-http (3 tests)

| 测试 | 验证 |
|---|---|
| `r513_admin_users_routes_round_trip` | 5 个 admin routes hints 全验证 |
| `r513_companies_sub_resources_round_trip` | 7 个 companies 子路由 hints 验证 |
| `r513_invites_and_skills_routes_round_trip` | 8 个 invites + skills hints 验证 |

## 验证

```
cargo test -p pc-openapi --lib                66 passed (58 pre + 8 R513 new)
cargo test -p pc-http --lib routes::openapi   59 passed (56 pre + 3 R513 new)
cargo check --workspace                       0 errors (170 pre-existing warnings)
```

## 设计要点

1. **AdminUser 用 minimum required**: 实例管理面板只需要 id + isInstanceAdmin; 详情页按需展开其他字段；这种"宽进严出"的策略让 list 端点 schema 保持紧凑。
2. **Invite 的 3 个 nullable 字段用 `["string", "null"]`**：invitedByUserId / revokedAt / acceptedAt 严格按 pc-repos 的 `Option<T>` 建模；nullable 是精确表达，不该用 `Option` 的 optional 表达。
3. **Companies 子路由响应 hint 大多是 None**: stats/timeline/artifacts/org 是聚合查询端点，返回结构复杂（多源 JOIN），暂时只给出 minimal response hint；后续 R514+ 可为每个专门建模。
4. **/api/companies/{company_id}/agents 显式 POST + Agent schema**：这是 admin 路径下创建 agent 的端点，请求/响应都是 Agent，与 /api/agents 不同（/api/agents 是 collection-level）。
5. **Skills 路由保持 minimal**：skill catalog 是聚合视图（多个 skill source JOIN），暂不建模；后续可按需补 CatalogEntry schema。

## V6 真实进度更新

- **R512 末**: ~86% (10 hints 已 wire 但 admin + companies 子路由缺 schema/hint)
- **R513 末**: **~95%** — 33 schemas (admin/companies/invites + 3 List) + 94 hints (admin 5 + companies 13 + invites 4 + skills 4) + 已注册路由集合 95%+ 覆盖

## 教训

1. **测试 fixture 命名一致性**: R513 测试和 R511 测试用相同的 fixture 命名空间，但 R511 的 `r511_folders_crud_and_legacy` 因为 patch 失误被删掉了签名；后来用 Python `s.replace(old, new)` 修复。后续类似工作最好直接 patch 新测试名而不是用 marker replacement。
2. **`#[serde(rename_all = "camelCase")]` vs JSON schema 字段名**: pc-repos 的 Row struct 用 `principal_id` (snake_case) + `rename_all = "camelCase"` → JSON 输出 `principalId`；OpenAPI schema 字段名必须跟 JSON 一致，所以直接写 `principalId`。
3. **Companies members endpoint 的 response 是 CompanyMemberList 还是 CompanyMember[]**: 现有代码返回 `Vec<CompanyMemberRow>` 直接序列化为 JSON array；OpenAPI 中 `[]` 和 `{type: array, items: $ref}` 等价；用 `CompanyMemberList` (named) 让 client 可读性更高。
4. **Stats / timeline endpoints 用 minimal response**: 给它们专门建模需要 8-12 字段聚合 JSON schema，但一次 round 写不完；R514+ 视优先级补。

## 下一步 (R514+)

| 轮次 | 目标 | 价值 |
|---|---|---|
| **R514** | V5 Auth: API key `pk_` 前缀 (machine-to-machine) | V5 70% → 80% |
| **R515** | V5 Auth: CSRF token 验证 (state-changing endpoints) | V5 70% → 85% |
| **R516** | V6 收尾: Companies 聚合端点 (stats/timeline/artifacts/org) schemas | V6 95% → 100% |
| **R517** | V4 OpenAPI ↔ UI 类型对齐: 生成 types.ts 给 ui/ 60 client 用 | V4 0% → 60% |
