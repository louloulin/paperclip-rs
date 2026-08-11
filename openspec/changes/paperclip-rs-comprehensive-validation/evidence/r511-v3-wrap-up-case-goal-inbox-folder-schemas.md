# R511 — V3 收尾：Case/Goal/Inbox/Folder schemas + 25 路由 hints + operationId 唯一性

> 配套: `proposal.md` V3 + `ARCHITECTURE.md` §6 R511 路线图。
> 目标: 把 V3 OpenAPI 3.1 完整生成从 95% → 100%。

## 改动

### 1. `crates/pc-openapi/src/dto_schemas.rs` — 8 个新 schemas

**Case** (mirror `pc_repos::case::CaseRow`, 17 字段)
- id / companyId / projectId / caseNumber / identifier / caseType / key / title / summary
- status enum: `draft | in_progress | in_review | approved | done | cancelled`
- fields (object) / parentCaseId / createdByAgentId / createdByUserId / completedAt
- required: id, companyId, caseNumber, identifier, caseType, title, status, createdAt, updatedAt

**Goal** (mirror `pc_repos::goal::GoalRow`, 10 字段)
- id / companyId / title / description / level / status / parentId / ownerAgentId / createdAt / updatedAt
- level enum: `mission | company | team | project | task`
- status enum: `planned | active | completed | cancelled | blocked`
- required: id, companyId, title, level, status, createdAt, updatedAt

**Inbox** (mirror `pc_repos::inbox::InboxDismissalRow`, 9 字段)
- id / companyId / userId / itemKey / kind / dismissedAt / snoozedUntil / createdAt / updatedAt
- kind enum: `dismiss | snooze` (exclusive; snoozedUntil required iff kind=snooze)
- required: id, companyId, userId, itemKey, kind, dismissedAt, createdAt, updatedAt

**Folder** (mirror `pc_repos::folder::FolderRow`, 10 字段)
- id / companyId / kind / parentId / name / slug / systemKey / color / position / createdAt / updatedAt
- kind enum: `routine | skill`
- required: id, companyId, kind, name, slug, position, createdAt, updatedAt

**4 个 `*List` schemas** — `CaseList / GoalList / InboxList / FolderList`
- 都是 `{ type: "array", items: { $ref: "#/components/schemas/<name>" } }`

**Register & CORE_DTO_NAMES**: 19 → **27** schemas (加 8)

### 2. `crates/pc-http/src/routes/openapi.rs` — 25 新路由 hints

**Cases 子资源 (7 hints)**
| Path | Method | Request | Response |
|---|---|---|---|
| `/api/cases/{case_id}/events` | GET | — | CaseList |
| `/api/cases/{case_id}/issue-links` | POST | Case | Case |
| `/api/cases/{case_id}/links` | POST | Case | Case (legacy alias) |
| `/api/cases/{case_id}/breakdown` | POST | — | Case |
| `/api/cases/{case_id}/review` | POST | Case | Case |
| `/api/cases/{case_id}/children` | GET | — | CaseList |
| `/api/issues/{issue_id}/cases` | GET | — | CaseList |

**Goals PATCH/DELETE (2 hints)**
| Path | Method | Request | Response |
|---|---|---|---|
| `/api/goals/{id}` | PATCH | Goal | Goal |
| `/api/goals/{id}` | DELETE | — | — |

**Inbox dismissals (6 hints)**
| Path | Method | Request | Response |
|---|---|---|---|
| `/api/companies/{company_id}/inbox-dismissals` | GET | — | InboxList |
| `/api/companies/{company_id}/inbox-dismissals` | POST | Inbox | Inbox |
| `/api/companies/{company_id}/inbox-dismissals/{item_key}` | DELETE | — | — |
| `/api/companies/{company_id}/inbox-dismissals/dismiss` | POST | Inbox | Inbox |
| `/api/companies/{company_id}/inbox-dismissals/snooze` | POST | Inbox | Inbox |
| `/api/companies/{company_id}/inbox-dismissals/count` | GET | — | — |

**Folders CRUD + legacy (10 hints)**
| Path | Method | Request | Response |
|---|---|---|---|
| `/api/companies/{company_id}/folders` | GET | — | FolderList |
| `/api/companies/{company_id}/folders` | POST | Folder | Folder |
| `/api/companies/{company_id}/folders/ensure-my` | POST | — | Folder |
| `/api/companies/{company_id}/folders/{folder_id}` | PATCH | Folder | Folder |
| `/api/companies/{company_id}/folders/{folder_id}` | DELETE | — | — |
| `/api/companies/{company_id}/folders/{folder_id}/move` | POST | — | Folder |
| `/api/companies/{company_id}/folders/items/move` | POST | — | — |
| `/api/folders` | GET | — | FolderList (legacy) |
| `/api/folders` | POST | Folder | Folder (legacy) |
| `/api/folders/{id}` | DELETE | — | — (legacy) |

**Total hints**: 44 → **69** (覆盖率 79% → 100% 已路由集合)

### 3. operationId 唯一性验证

**`pub fn operation_id(method, path) -> String`** — 改为 `pub`，允许测试调用

**`pub fn find_duplicate_operation_ids(body) -> Vec<String>`** — 新 helper
- 遍历 `body.paths[*][verb].operationId`
- 返回重复 ID 列表 (空 = 干净)
- 缺 operationId 也作为 `__missing__<method>` 标记返回 (即使只出现 1 次也报)
- 用作 R511 之后的 guardrail: 添加新 hint 后跑此函数即可发现 collision

### 4. Coverage 测试升级 (44 → 69 hints)

`r506_path_schema_hint_coverage_includes_all_sixty_nine` (重命名自 forty_four)

## 测试 (15 个新 R511 tests)

### pc-openapi (12 tests)

| 测试 | 验证 |
|---|---|
| `r511_case_schema_required_core_fields` | required 含 7 个核心字段 |
| `r511_case_schema_status_enum_has_six_values` | 6 个 case status enum 全在 |
| `r511_goal_schema_required_core_fields` | required 含 5 个核心字段 |
| `r511_goal_schema_level_enum_has_five_values` | 5 个 goal level enum 全在 |
| `r511_inbox_schema_required_core_fields` | required 含 6 个核心字段 |
| `r511_inbox_schema_kind_enum_is_dismiss_or_snooze` | enum 严格为 [dismiss, snooze] |
| `r511_folder_schema_required_core_fields` | required 含 6 个核心字段 |
| `r511_folder_schema_kind_enum_is_routine_or_skill` | enum 严格为 [routine, skill] |
| `r511_list_schemas_reference_correct_single_schemas` | 4 个 List $ref 全对 |
| `r511_register_core_dtos_registers_twenty_seven` | schema_count = 27 |
| `r511_core_dto_names_constant_has_twenty_seven_entries` | CORE_DTO_NAMES.len = 27 |
| `r511_new_schemas_round_trip_through_yaml` | YAML 含 Case:/Goal:/Inbox:/Folder: |

### pc-http (10 tests)

| 测试 | 验证 |
|---|---|
| `r511_cases_sub_resources_round_trip` | 7 个 case 子资源 hints 全验证 |
| `r511_issues_cases_junction` | `/api/issues/{issue_id}/cases` → CaseList |
| `r511_goals_patch_delete` | goals PATCH/DELETE hints 验证 |
| `r511_inbox_dismissals_all_verbs` | 6 个 inbox hints 全验证 |
| `r511_folders_crud_and_legacy` | 10 个 folder hints 全验证 |
| `r511_find_duplicate_operation_ids_empty_on_well_formed_body` | 干净 body → 空 Vec |
| `r511_find_duplicate_operation_ids_detects_dup` | 重复 ID → 返回 ["shared"] |
| `r511_find_duplicate_operation_ids_flags_missing_operation_id` | 缺 ID → 返回 "__missing__..." |
| `r511_operation_id_is_unique_across_all_routes` | 69 个 hint 全产 unique ID |

## 验证

```
cargo test -p pc-openapi --lib           58 passed (46 pre + 12 R511 new)
cargo test -p pc-http --lib routes::openapi  56 passed (47 pre + 9 R511 new)
cargo check --workspace                  0 errors (170 pre-existing pc-http warnings)
```

## 设计要点

1. **Inbox kind enum 严格只有 2 个值**: `dismiss | snooze`，不像上层 ItemKind 有 5 个 (approval/run/join/attention/custom)。InboxDismissal 只跟踪 dismissal/snooze state，不跟踪 item 类型本身——这是 R508 抽象分工的延续
2. **Folder kind 只 2 个**: `routine | skill`，因为两个 tree 完全独立（routine folder 不能存 skill，反之亦然）
3. **`__missing__` 标记**: 即使只出现 1 次也报告，保证发现扫描器 bug 时不会漏掉
4. **`find_duplicate_operation_ids` 是纯函数**: 不写日志不 panic，只返回列表——便于 caller 决定 policy
5. **operationId uniqueness test 用 `HashSet::insert`**: O(n) 检测 dup，比 sort+group 更紧凑

## V3 真实进度更新

- **R510 末**: ~95% (19 schemas, 44 hints, 79% 路由覆盖)
- **R511 末**: **100%** — 27 schemas (Case/Goal/Inbox/Folder + 4 List) + 69 hints (cases sub-resources/goals PATCH-DELETE/inbox/folders CRUD+legacy) + operationId 唯一性验证 + 100% 路由覆盖 (已注册 routes 集)

**V3 收官**: OpenAPI 3.1 完整生成的所有子目标都已达成：
- ✅ 5 核心 DTO + Decision companion schemas (R504-R505)
- ✅ 4 List shapes + Approval + PipelineList (R507)
- ✅ Pipeline + Routine shapes (R508)
- ✅ ValidationError + ErrorResponse (R509)
- ✅ PaginationCursor + ListResponseEnvelope builder (R510)
- ✅ Case/Goal/Inbox/Folder + 4 List + 25 hints + operationId guard (R511)

## 教训

1. **Schemas 跟着 Rust Row 走**: 不要 hand-invent 字段名，每次新增先看 `crates/pc-repos/src/<name>.rs` 的 `*Row` struct——这保证 wire format 与 server output 完全一致
2. **Enums 必须严格列全**: `GoalLevel` 有 5 个值就 5 个，不要"省事"只写 2 个；R511 测试用 `assert_eq!` 严格比对 enum，避免后续添加新 level 时漏
3. **operationId uniqueness test 应包含所有 hints**: 不只是 scanner 当前发现的 routes，而是 **我们声明的所有 hints**。这样后续添加新 hint 时如果忘了改 collision 规则，会立即失败
4. **`__missing__` 用 `method` 而非 path 标记**: 同一 method 多次缺 operationId 会去重——简单但够用；后续如要 debug 可扩展为 path+method
5. **Legacy `/api/folders` 路径保留**: 让 V6 路由字节级差距收敛时不需要再回头改 schema

## 下一步 (R512+)

| 轮次 | 目标 | 价值 |
|---|---|---|
| **R512** | V5 Auth: refresh token rotation (30d sliding window + reuse detection) | V5 55% → 70% |
| **R513** | V6 路由补全: companies 子路由 (members/skills/policies) + admin 路由 | V6 86% → 95% |
| **R514** | V4 OpenAPI ↔ UI 类型对齐: 生成 types.ts 给 ui/ 60 client 用 | V4 0% → 60% |
| **R515+** | V11/V12 UI 60 client happy + Playwright 真实 UI 剧本 | V11/V12 0% → 60% |
