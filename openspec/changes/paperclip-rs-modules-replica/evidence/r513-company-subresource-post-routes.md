# Evidence: R513 — 补齐 3 个真缺漏的公司级 POST 路由

## 目标
- 补齐 Node 端已有、Rust 端只有 GET 的 3 个公司级 POST endpoint：
  - `POST /api/companies/:company_id/approvals`（对齐 `approvals.ts:124`）
  - `POST /api/companies/:company_id/decisions`（对齐 `decisions.ts:42`）
  - `POST /api/companies/:company_id/pipelines`（对齐 `pipelines.ts:891`）
- 真实 DB TDD 验证
- 不引入新 adapter；保持 claude-local + codex-local 约束

## 之前路由覆盖率
- 97.76%（568/581 common，13 missing）

## 当前路由覆盖率
- **98.28%**（missing 13 → **10**）
- 新增 3 条路由（Rust 870 → **873**）

## 设计选择
1. **路由归位** — 三个 POST 都属于公司子资源，因此接在 `routes::companies::router()` 下，
   与 `GET /api/companies/:company_id/{approvals,decisions,pipelines}` 在同一 router 聚合
   （保留 Node 的 `/companies/:companyId/...` 嵌套命名）。
2. **Body 形状** — 与 `approvals.ts` / `decisions.ts` / `pipelines.ts` 的 Node schema 对齐，
   snake_case JSON 输入（与 Rust 现有 `/api/approvals`、`/api/decisions` 一致），但
   `company_id` 从路径获取，不再要求 body 提供。
3. **Repo 复用** — approvals POST 改用 `ApprovalRepo::create(NewApproval)` 而不是
   `create_three_args`（后者把 `requested_by_*` 写死为 `None`，会触发新增的 repo 层
   校验 "approval must be requested by agent or user"）；decisions / pipelines 直接
   复用现有 `DecisionRepo::create` + `PipelineRepo::create`。
4. **状态码** — 三个创建端点都返回 `201 CREATED`（与 Node 行为一致；Rust 旧的
   `/api/approvals` POST 仍返回 `201`，本次未触碰）。
5. **实时事件** — 三个创建都发布对应的 realtime event
   （`approval.created` / `decision.created` / `pipeline.created`），与现有
   `/api/{approvals,decisions,pipelines}` POST 一致。
6. **决策可选字段透传** — `create_company_decision_route` 接受 Node schema 的
   `options` / `inputs` / `rule_key`，落到对应列。

## 验证证据

| 命令 | 结果 |
|---|---|
| `cargo test -p pc-http --test r513_company_subresource_create_contract` | **5 passed**（approvals/decisions/pipelines 成功 + approvals 空类型 400 + pipelines 空 key 400） |
| `cargo test -p pc-http --lib` | 279 passed |
| `bash scripts/diff-routes.sh` | coverage=**98.28%** missing=10 |
| `bash scripts/e2e-full-stack.sh` | **17/17 passed**（5.5s） |
| `git diff --check` | clean |
| `rustfmt --edition 2021 --check`（新增文件） | clean |

## 测试设计（contracts）
每条路由验证 5 条断言（POST 201/400 校验 + GET 列表含新建项）：
- `company_approvals_post_creates_pending_approval`：创建 pending approval + GET 列表含
- `company_approvals_post_rejects_empty_approval_type`：空类型返回 400
- `company_decisions_post_creates_pending_decision`：创建 open decision + GET 列表含
- `company_pipelines_post_creates_pipeline`：创建 pipeline + GET 列表含
- `company_pipelines_post_rejects_empty_key`：空 key 返回 400

## 已知遗留
- `POST /api/cases/:case_id/documents/:key` 仍缺。Node `cases.ts:934` 的 PUT 包含
  baseRevisionId 校验、锁检查、新建文档生成 revision 等较复杂语义，独立到 R514。
- 既有 `approval_create_get_list_decide_delete_lifecycle` 测试在 HEAD 上已失败
  （`create_three_args` 触发 repo 校验；与本轮改动无因果关系，commit `166b1e8` 之前即
  存在），不属于本轮范围。

## 整体复刻完成度（基于 R508 + R511 + R512 + R513）

| 模块 | 复刻度 |
|---|---|
| HTTP 路由覆盖率 | **98.28%**（568/581 Node routes；13 → 10 missing） |
| M18 全栈 E2E | **17/17** |
| 远程 execution target 决策 | ✅ R512 |
| 远程 SSH bridge IPC | ✅ R492/R493/R506 |
| adapter claude-local / codex-local | 决策 + 真实 bridge ✅；managed runtime I/O 留 stub |
| 其他 adapter | stub（用户约束外） |
| 公司级 sub-resource POST 路由 | ✅ R513 |
| PUT case documents | 后续 R514 |
| Bootstrap flow / 其他路由 / UI 类型生成 | 后续轮 |
