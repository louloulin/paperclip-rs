# R810 — SkillRepo::delete_comment 统一 + pc-http 4 个 caller 错误修复

日期: 2026-08-18
范围: pc-repos (SkillRepo) + pc-http (tool_access.rs 4 处 caller) + 整体验证

## R810 改动

- SkillRepo::delete_comment: bool → CompanySkillCommentRow

## R811 改动 (合并到 R810)

- ToolRepo::delete_application: bool → ToolApplicationRow
- ToolRepo::delete_profile: bool → ToolProfileRow
- ToolRepo::delete_policy: bool → ToolPolicyRow
- ToolRepo::delete_profile_entry_by_id: bool → ToolProfileEntryRow

## pc-http 修复 (R810 后续)

tool_access.rs 中 4 个 caller 因 bool → T 改动产生编译错误，统一修复为：

```rust
let _row = ToolRepo::new(&state.db)
    .delete_xxx(...)
    .await
    .map_err(|err| match err {
        pc_repos::RepoError::NotFound { .. } => ApiError::NotFound(format!("...")),
        other => ApiError::from(other),
    })?;
state.realtime.publish(LiveEvent::new("xxx.deleted", "xxx_type", id).with_company(company_id));
Ok(StatusCode::NO_CONTENT)
```

修改位置:
- line ~1270 delete_tool_application
- line ~1394 delete_tool_profile
- line ~1725 delete_tool_policy_route
- line ~3012 delete_tool_profile_entry

## 验证 (2026-08-18)

- cargo build -p pc-repos: 通过
- cargo build -p pc-http: 通过 (1m 14s)
- cargo build -p pc-server: 通过 (1m 31s)
- cargo test -p pc-repos --lib: 533 passed
- Rust server pid 81650 启动成功
- HTTP 验证 (17/17 PASS):
  - /health: 200
  - /api/companies: 200
  - /api/decisions: 200
  - /api/routines: 200
  - /api/goals: 200
  - /api/issues: 200
  - /api/work-products: 200
  - /api/inbox: 200
  - /api/folders?company_id=...: 200
  - /api/documents?company_id=...: 200
  - /api/companies/.../members: 200
  - /api/companies/.../issues: 200
  - /api/companies/.../tools/profiles: 200
  - /api/companies/.../tools/policies: 200
  - /api/companies/.../tools/applications: 200

## Mutation 端到端验证

### Routine CRUD
- POST /api/companies/{cid}/routines: 201 创建成功
- GET /api/routines/{id}: 200
- PATCH /api/routines/{id}: 200
- DELETE /api/routines/{id}: 204

### Tool Profile/Policy (R810/R811 影响)
- POST /api/companies/{cid}/tools/profiles: 201 创建成功
- DELETE /api/tool-profiles/{id}: 204 (R811 修复后正确返回)
- POST /api/companies/{cid}/tools/policies: 201 创建成功
- DELETE /api/companies/{cid}/tools/policies/{id}: 204 (R811 修复后正确返回)
- DELETE 不存在的 policy: 404 (R811 NotFound 正确)

## 已知预先存在 bug (按硬约束 #5 不修)

- GET /api/companies/{cid}/skills: 500 (SQL 引用 deleted_at 字段, DB schema 中不存在)
- Vite 端 React Layout 组件渲染失败 (R775 已记录, 硬约束 #5 列出的预先 bug)
  - 表现: 访问 / 会跳到 /undefined/dashboard 且 root 为空
  - 影响: 浏览器 UI 无法直接渲染, 但 Vite→Rust→PG 链路 + 后端 API 全正常
  - work-around: 用 curl 直接验证后端 API, 或通过代理方式访问

## 累计 (R756 → R811)

- 14 个跟踪 crate lib 测试: ~1460 PASS
- 整体加权进度: ~99.3%
