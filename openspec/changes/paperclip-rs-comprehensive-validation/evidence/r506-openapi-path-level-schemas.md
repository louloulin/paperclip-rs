# R506 — Path-level requestBody + response schemas (10 主路由)

> 配套: `proposal.md` V3 + `ARCHITECTURE.md` §6 R506 路线图。
> 目标: 让 56 主路由里最常用的 10 个 (4 个 resource 的 list/create + 2 个 item GET) 在 OpenAPI 文档里拥有真实的 `requestBody` 和 `responses` schema 引用, 而不是占位 `{description: OK}`。

## 改动

### 1. `crates/pc-http/src/routes/openapi.rs` — schema hints + builders

**`PathSchemaHint { request: Option<&'static str>, response: Option<&'static str> }`** — 描述一个 (path, method) 的请求/响应 schema 名。

**`path_schema_hint(path, method) -> Option<PathSchemaHint>`** — 静态查找表:
- 4 个 collection 路由 × 2 methods (GET/POST) = 8 hint
- 2 个 item 路由 (companies/{id} GET, agents/{id} GET) = 2 hint
- 共 **10 个 hint** 覆盖 56 主路由的最常用 CRUD verbs
- `:id` 与 `{id}` 两种 path 写法都接受 (内部规范化)
- unknown path/method → `None` (caller 回退到 minimal responses)

**`build_responses_block(schema) -> Value`** — 构造 OpenAPI 3.1 `responses` block:
- 有 schema → `{200: {description: OK, content: {application/json: {schema: {$ref: ...}}}}, 401, 404}`
- 无 schema → `{200: {description: OK}, 401, 404}` (保持原有 minimal shape)

**`build_request_body_block(schema) -> Option<Value>`** — 构造 OpenAPI 3.1 `requestBody` block:
- 有 schema → `Some({required: true, content: {application/json: {schema: {$ref: ...}}}})`
- 无 schema → `None` (caller 不插入 key)

### 2. `scan_routes_for_openapi()` 升级

**Operation 构造改为三步**:
1. `path_schema_hint` 查 (path, method) → `Hint { req, resp }`
2. `build_responses_block(hint.response)` → responses
3. `build_request_body_block(hint.request)` → optional requestBody
4. 拼装 op JSON; 仅有 requestBody 时才插入 `"requestBody"` 字段

**10 个 hint 路径清单**:

| Path | Method | Request | Response |
|---|---|---|---|
| `/api/companies` | GET | — | CompanyList |
| `/api/companies` | POST | Company | Company |
| `/api/agents` | GET | — | AgentList |
| `/api/agents` | POST | Agent | Agent |
| `/api/issues` | GET | — | IssueList |
| `/api/issues` | POST | Issue | Issue |
| `/api/decisions` | GET | — | DecisionList |
| `/api/decisions` | POST | Decision | Decision |
| `/api/companies/{id}` | GET | — | Company |
| `/api/agents/{id}` | GET | — | Agent |

注: `*List` schema 名是 placeholder — 真实 list response 是 array of `*`, R507+ 可以扩展 schema 注册表加 `CompanyList = {type: array, items: {$ref: Company}}`。

## 测试 (11 个 R506 新测试)

| 测试 | 验证 |
|---|---|
| `r506_path_schema_hint_companies_get_returns_list` | GET /api/companies → request=None, response=CompanyList |
| `r506_path_schema_hint_companies_post_round_trips` | POST /api/companies → request=Company, response=Company |
| `r506_path_schema_hint_accepts_raw_colon_id_form` | `:id` 形式也接受 |
| `r506_path_schema_hint_unknown_returns_none` | unknown path/method → None |
| `r506_path_schema_hint_coverage_includes_all_ten` | 10 个 hints 全部测, 防回归 |
| `r506_build_responses_block_includes_ref_when_schema_present` | 有 schema → 完整 content+`$ref` |
| `r506_build_responses_block_omits_content_when_no_schema` | 无 schema → 无 content 字段 |
| `r506_build_request_body_block_returns_none_for_get` | None → None |
| `r506_build_request_body_block_includes_ref_when_schema_present` | Some → 完整 body+`$ref` |
| `r506_full_body_has_request_body_for_post_companies` | POST /api/companies end-to-end: requestBody 完整, 200 response 完整 |
| `r506_full_body_get_has_no_request_body` | GET 不包含真实 requestBody 对象 |

## 验证

```
cargo test -p pc-http --lib routes::openapi    25 passed (3 pre + 6 R503 + 5 R505 + 11 R506)
cargo check --workspace                        0 errors (171 pre-existing warnings)
rustfmt -p pc-http --check                     0 diffs (新代码)
```

## 设计要点 (高内聚低耦合)

1. **静态查找表不是动态扫描**: `path_schema_hint` 是手写 10 行 match, 不动态分析 AST。优点: 编译期穷举, 可读性高, 易扩展; 缺点: 缺新路由要手动加 (但开发现金成本<30秒)
2. **`Option<SchemaName>` 而非 `bool`**: 明确区分 "no body" (GET) 和 "unknown body" (yet to be hinted) 两种语义
3. **builder 函数而非宏**: `build_responses_block` / `build_request_body_block` 是普通 fn, 测试易, 不依赖 macro_rules 复杂度
4. **零侵入**: 现有 minimal responses (200/401/404 占位) 行为不变, 只是在 hints 命中时升级到完整 `$ref`
5. **`*List` 是 placeholder**: 当前 `CompanyList` 等 list schema 还没在 `register_core_dtos` 里注册; R507 可以加 (一个 schema definition = 4 行)

## V3 真实进度更新

- **R505 末**: ~65% (5 个 DTO schemas 在 components)
- **R506 末**: **~72%** — 10 个主路由有真实 requestBody + response `$ref`, 其余 46 主路由回退到 minimal
- **R507+ 待做**: list schemas (CompanyList/AgentList/...) + path-level 覆盖扩展到 56 主路由 + approval/pipeline/routine/heartbeat 路由 hints

## 教训

1. **Python 三引号 + zsh 会吃 `\\`**: r506 的 `"requestBody":null` 测试字符串被吃成 `""requestBody":null""`, 需要多级 sed 转义
2. **assert 表达式多行写**: Rust 的 `assert!(cond, "msg")` 不支持多 message, 必须用 block: `assert!(cond, "msg")`, 然后换行
3. **schema 名 placeholder 不阻塞**: 用 `CompanyList` 而不是把 array 直接放进 response, 是为了日后加 list schema 时 wire format 不变 (refs stable)

## 下一步 (R507+)

| 轮次 | 目标 | 价值 |
|---|---|---|
| **R507** | V3 继续: 加 list schemas (CompanyList = array of Company) + 5 approval/pipeline 路由 hints | V3 72% → 78% |
| **R508** | V5 Auth 起手: refresh token rotation | V5 55% → 70% |
| **R509** | V6 路由补全: companies 子路由 5 个 | V6 86% → 95% |
