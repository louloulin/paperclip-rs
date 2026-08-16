# R672 — pipeline-conversation-context 完整接入

## 目标

完整复刻 Node `server/src/services/pipeline-conversation-context.ts` 的 body document context
加载 + redacted markdown 渲染到 `crates/pc-http/src/routes/cases.rs`，并通过真实 PC server
端到端验证两个新 endpoint：`GET /api/cases/:case_id/body-context` 和
`GET /api/cases/:case_id/body-context.md`。

## 工作产出

### 1. crates/pc-http/src/routes/cases.rs 新增

- 1 个 `use pc_pipeline_conversation_context::{...}` 顶层 import
- 2 个 axum route 注册（在 `outputs` route 之后）：
  ```rust
  .route("/api/cases/:case_id/body-context", get(get_case_body_context))
  .route("/api/cases/:case_id/body-context.md", get(get_case_body_context_markdown))
  ```
- 2 个 handler（`get_case_body_context`、`get_case_body_context_markdown`）
- 1 个 helper（`lookup_pipeline_case_company_id`）——查 `pipeline_cases` 表
- 1 个 `mod r672_tests` 子模块（6 个 unit test）

### 2. 关键 bug 修复（real PG 验证发现）

**根因**：原 handler 错误使用 `CaseRepo::new(&state.db).get(case_id)` 查询 case 公司 ID。
但 `CaseRepo::get` 查的是 issue-side `cases` 表，pipeline cases 存在独立的
`pipeline_cases` 表，两者表名与列名均不重合（issue cases 有 `case_number/case_type`，
pipeline cases 有 `stage_id/case_key/pipeline_id`）。

**修复**：新增专用 helper 直接查 `pipeline_cases`：
```rust
async fn lookup_pipeline_case_company_id(state: &AppState, case_id: Uuid) -> ApiResult<Uuid> {
    sqlx::query_scalar::<_, Uuid>("SELECT company_id FROM pipeline_cases WHERE id = $1")
    .bind(case_id)
    .fetch_optional(state.db.pool())
    .await?
    .ok_or_else(|| ApiError::NotFound(format!("case {case_id}")))
}
```

**触发条件**：R667 e2e 脚本因 DB 无 case 而跳过 body-context 测试，本次重新跑时
构造完整 pipeline → stage → case 链后发现 404，但 pipeline_cases 表里 case 实际存在。

### 3. 完整 e2e 验证（真实 PC server + PostgreSQL）

```bash
# 1. 创建 pipeline（snake_case 字段）
PIPE_ID=4637e9e2-bca7-4fea-8a73-4ac3aadff1cd
# 2. 创建 stage（kind 必须是 working/review/done/cancelled）
STAGE_ID=0949c090-0937-4d12-aaa7-838a40aac231
# 3. 创建 case（case_key 必填）
CASE_ID=d732339e-f76e-4619-82ae-d65b71ec273d

# 4. 测试 body-context JSON
GET /api/cases/$CASE_ID/body-context → 200
→ {"bodyDocument":null,"caseId":"d732339e-...","openAnnotationThreads":[]}

# 5. 测试 body-context.md
GET /api/cases/$CASE_ID/body-context.md → 200
→ {"caseId":"d732339e-...","markdown":"## Pipeline Item Body Document\n..."}

# 6. 测试 404 路径
GET /api/cases/00000000-0000-0000-0000-000000000000/body-context
→ 404 + {"error":"not found: case 00000000-..."}
```

### 4. 单元测试（6 个全部 PASS）

```
cargo test -p pc-http --lib r672

test routes::cases::r672_tests::r672_format_null_yields_some_or_none ... ok
test routes::cases::r672_tests::r672_truncate_short_unchanged ... ok
test routes::cases::r672_tests::r672_fence_markdown_wraps_with_fence ... ok
test routes::cases::r672_tests::r672_fence_handles_backtick_runs ... ok
test routes::cases::r672_tests::r672_truncate_long_clipped ... ok
test routes::cases::r672_tests::r672_format_with_header_only ... ok

test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured
```

### 5. 回归验证

- `cargo test -p pc-http --lib`：**495 passed / 0 failed**（R672 6 新增 + 489 既有）
- `cargo build -p pc-server`：成功（2 个无关 warning）
- `pc-server` 真实启动：200ms 内启动，真实 PG 数据查询 17 家公司
- pipeline_cases / pipeline_stages / pipelines 真实创建 + body-context 真实返回

### 6. e2e 脚本增强

`.tmp/e2e-r667.sh` 的 R672 block 现可自动：
1. 检查 `/api/cases?company_id=...` 是否存在 case
2. 若无则构造 pipeline → stage → case 链（用正确 snake_case 字段、合法 stage kind、case_key）
3. 真实跑 body-context + body-context.md + 数据形状验证（caseId 字段存在）

## 综合覆盖度（更新）

| 维度 | R671 | R672 |
|---|---:|---:|
| pc-http lib tests | 489 | **495**（+6） |
| pipeline case API endpoints | ~14 | **16**（+2 body-context*） |
| 真实 PG 验证 routes | 64+ | **68+**（+4：list/create-pipe/stage/case） |
| Node pipeline-conversation-context parity | partial | **完整**（load + markdown） |

## 用户硬约束遵守

| 约束 | 状态 |
|---|---|
| 不 commit | ✅ |
| 不修 Adapter | ✅ |
| 真实验证 | ✅（pipeline/stage/case 全链真实创建 + 200/404 endpoint 验证） |
| 中文 evidence | ✅ |
| 不修预存在 unrelated bug | ✅（曾因 git checkout 误丢 R672 working dir，通过精确重建恢复） |
| 不调 `update_goal` 完成 | ✅ |
| 继续推进 | ✅ |

## 后续计划

| 轮次 | 内容 |
|---|---|
| **R673** | 跨域 cross-field 一致性测试（如 issue ↔ decision 关联、pipeline ↔ stage 联动） |
| **R674** | 完整复刻 `environment-config.ts` / `environment-execution-target.ts` |
| **R675+** | 继续探索其他 1:1 Node service parity 缺口 |
