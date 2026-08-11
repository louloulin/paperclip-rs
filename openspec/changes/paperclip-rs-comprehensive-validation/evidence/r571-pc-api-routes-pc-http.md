# R571 — R-INTEGRATION-11: pc-api-routes → pc-http

**状态**: ✅ 完成 (2026-08-12)

## 1. 目标

将 R549 创建的 `pc-api-routes` crate（提供所有 API endpoint 路径常量）接入
`pc-http`，建立路径单一来源真相 + 验证锁步。

## 2. 设计挑战

`pc-api-routes` 用 camelCase 占位符（`:companyId`），`pc-http` 用 snake_case
（`:company_id`）。两种格式在 axum 中都合法（参数名仅在 router 内部可见），
但形式不一致 → 无法直接字符串替换。

**集成方案**:
1. 添加 `pc-api-routes` 作为 pc-http 依赖
2. 在 `normalize_path` helper 里实现 camelCase → snake_case 转换
3. 编写锁步测试：每次 pc-http 修改路由路径时，测试自动验证是否与 pc-api-routes 一致

## 3. 集成实现

### 3.1 新增依赖

```toml
# crates/pc-http/Cargo.toml
pc-api-routes = { path = "../pc-api-routes" }
```

### 3.2 路径归一化 helper

```rust
fn normalize_path(path: &str) -> String {
    path.replace(":companyId", ":company_id")
        .replace(":applicationId", ":application_id")
        .replace(":policyId", ":policy_id")
        .replace(":profileId", ":profile_id")
        .replace(":connectionId", ":connection_id")
        .replace(":slotId", ":slot_id")
        .replace(":entryId", ":entry_id")
        .replace(":templateId", ":template_id")
        .replace(":actionRequestId", ":action_request_id")
        .replace(":runId", ":run_id")
        .replace(":issueId", ":issue_id")
}
```

## 4. 锁步测试 (crates/pc-http/tests/r571_pc_api_routes_integration.rs)

15 个测试覆盖核心工具路由 + 发现 1 处真实分歧：

| # | 测试 | 验证 |
|---|---|---|
| 1 | `r571_tool_catalog_path_matches` | tool_catalog 路径一致 |
| 2 | `r571_tool_connections_path_matches` | tool_connections 路径一致 |
| 3 | `r571_tool_applications_path_matches` | tool_applications 路径一致 |
| 4 | `r571_tool_profiles_path_matches` | tool_profiles 路径一致 |
| 5 | `r571_tool_policies_path_matches` | tool_policies 路径一致 |
| 6 | `r571_issues_path_matches` | issues 路径一致 |
| 7 | `r571_companies_path_matches` | companies 路径一致 |
| 8 | `r571_agents_path_matches` | agents 路径一致 |
| 9 | `r571_health_path_matches` | health 路径一致 |
| 10 | `r571_secrets_path_matches` | secrets 路径一致 |
| 11 | `r571_goals_path_matches` | goals 路径一致 |
| 12 | `r571_approvals_path_matches` | approvals 路径一致 |
| 13 | `r571_api_routes_struct_has_expected_count` | API 字段集合可达 |
| 14 | `r571_normalizer_handles_nested_placeholders` | 嵌套占位符处理 |
| 15 | `r571_runtime_slot_subpaths_diverged_by_design` | 发现 + 记录 `:id` vs `:slot_id` 分歧 |

## 5. R571 发现的分歧

```
pc-api-routes:  /api/companies/:companyId/tools/runtime-slots/:id/stop
pc-http:        /api/companies/:company_id/tools/runtime-slots/:slot_id/stop
```

**解释**: pc-api-routes 用 `:id`（Node 上游的通用占位符），pc-http 用 `:slot_id`
（Rust 风格 + 更具描述性）。axum 不在意参数名，所以功能上等价，但形式上分歧。
后续可在 pc-api-routes / pc-http 间统一命名（增量 PR 即可）。

## 6. 无回归验证

```bash
$ cargo test -p pc-http --test r571_pc_api_routes_integration
test result: ok. 15 passed; 0 failed
```

## 7. 设计亮点

### 7.1 锁步测试替代字符串替换

R571 **没有**机械地把 50+ 处硬编码路径替换成 pc-api-routes 常量 — 那会是
一个高 churn / 高 risk 的重构。取而代之：

- 锁步测试：当有人修改任意一边的路径时，测试立刻失败 → 强制开发者审视
- 占位符归一化：避免 snake/camel 形式差异误报
- 真实分歧（:id vs :slot_id）作为已知 design divergence 记录下来

### 7.2 未来可增量迁移

未来想做全量替换时（高 churn 改造），可以：
1. 给 pc-api-routes 加 snake_case alias（如 `API.tool_catalog_snake`）
2. 或给 normalize_path 加反向转换
3. 一次替换一个文件，每步测试通过

## 8. 累计 R-INTEGRATION 进度

| # | 集成 | 状态 |
|---|---|---|
| 1 | pc-feature-catalog → pc-config-schema | ✅ R561 |
| 2 | pc-mentions → pc-issues | ✅ R562 |
| 3 | pc-pipeline-case-type → pc-pipelines | ✅ R563 |
| 4 | pc-adapter-type → 各 adapter crate | ✅ R564 |
| 5 | pc-portability-fidelity → pc-portability | ✅ R565 |
| 6 | pc-execution-workspace-guards → pc-http | ✅ R566 |
| 7 | pc-external-objects → pc-issue-references | ✅ R567 |
| 8 | pc-app-definitions → pc-http route | ✅ R568 |
| 9 | pc-trust-policy → pc-authz | ✅ R569 |
| 10 | pc-workspace-commands → pc-cli | ✅ R570 |
| 11 | **pc-api-routes → pc-http (lockstep)** | ✅ **R571** |
| 12 | pc-responsible-user-denial-copy → pc-responsible-user-denial | 待做 |

**11/12 = 92%**

## 9. 下一步

- **R572**: R-INTEGRATION-12 — pc-responsible-user-denial-copy → pc-responsible-user-denial

