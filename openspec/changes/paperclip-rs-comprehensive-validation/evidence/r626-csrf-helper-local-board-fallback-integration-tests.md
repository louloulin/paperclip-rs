# R626 — 回归保护 + local-board fallback 移除 + CSRF helper

> 日期：2026-08-12
> 范围：5 项 P0/P1 完成
> 状态：✅ 全部已 merged

## 1. 背景

R625 修复了 3 个 server-side bug（CSRF / principal schema / session_cookie_name）并
通过 e2e-ux-flow 跑通 7 步。R626 把这次发现**固化成回归保护**，避免未来再回归同类问题。

## 2. 产出

| # | 产出 | 文件 | 状态 |
|---|---|---|---|
| 1 | 8 个 sqlx::test! 集成测试覆盖 5 个 query 修复 | `crates/pc-repos/tests/r626_company_member_principal_id.rs` (214 行) | ✅ 8/8 pass |
| 2 | 移除 `create_company_route` 的 `"local-board"` fallback | `crates/pc-http/src/routes/companies.rs:298-304` | ✅ + e2e 验证 |
| 3 | UI `applyCsrfHeader()` 显式 helper（60 client 自动受益） | `ui/src/api/client.ts:56-89` | ✅ |
| 4 | GitHub Actions CI workflow (UX flow 跑通 fail PR) | `.github/workflows/r626-ux-flow-e2e.yml` | ✅ |
| 5 | e2e-ux-flow 脚本 paperclip-rs server 真实绑定 `:54301` 验证 | `scripts/r625-ux-flow.sh` + `.py` | ✅ 7/7 步过 |

## 3. 集成测试覆盖（防 5 个 query 回归）

```
running 8 tests
test r626_company_memberships_has_no_user_id_column ... ok
test r626_is_active_member_archived_returns_false ... ok
test r626_is_active_member_agent_principal_excluded ... ok
test r626_list_for_user_with_company_returns_membership_role ... ok
test r626_list_company_ids_for_user_returns_all_active ... ok
test r626_replace_user_companies_inserts_with_principal_columns ... ok
test r626_is_active_member_owner_returns_true ... ok
test r626_replace_user_companies_clears_all_memberships ... ok

test result: ok. 8 passed; 0 failed
```

| 测试 | 防的回归 |
|---|---|
| `r626_company_memberships_has_no_user_id_column` | schema 信息检查，确保 `user_id` 列**不再存在**（数据库约束） |
| `r626_is_active_member_owner_returns_true` | is_active_member 用 principal_type + principal_id |
| `r626_is_active_member_archived_returns_false` | status='archived' 不算 active |
| `r626_is_active_member_agent_principal_excluded` | principal_type='agent' 不算 human member（语义安全） |
| `r626_list_company_ids_for_user_returns_all_active` | list 返所有 active company |
| `r626_list_for_user_with_company_returns_membership_role` | column 名 `cm.role` → `cm.membership_role` |
| `r626_replace_user_companies_clears_all_memberships` | DELETE 用 principal_type + principal_id |
| `r626_replace_user_companies_inserts_with_principal_columns` | INSERT 列名顺序 + VALUES 顺序 |

**测试基础设施**：
- 复用现有约定：`postgres://paperclip:paperclip@127.0.0.1:5432/paperclip_repos`（pre-existing 集成测试 DB）
- 每个测试用 unique company_id + unique user_id（uuid v4），互不污染
- 走真实 SQL（包括 ON CONFLICT 唯一索引），不 mock

## 4. 移除 `local-board` fallback

### Before
```rust
// R590: 业务下沉到 CompanyService
let owner_id = match require_user_id(&state, &headers).await {
    Ok(user_id) => user_id,
    Err(ApiError::Unauthorized(_)) => "local-board".to_owned(),  // ← 掩盖鉴权 bug
    Err(error) => return Err(error),
};
```

### After
```rust
// R590: 业务下沉到 CompanyService
// R626: 取消 "local-board" fallback — 鉴权失败必须 surface（之前会掩盖 client
//       鉴权 bug，导致创建出来的 company owner 是 "local-board" 占位 principal，
//       后续 is_active_member / WS auth / resource ACL 全部误判）。
let owner_id = match require_user_id(&state, &headers).await {
    Ok(user_id) => user_id,
    Err(error) => return Err(error),
};
```

**修复理由**：R625 期间发现，当 `require_user_id` 失败时（因为 session_cookie_name bug），
fallback 用 `"local-board"` 字符串，导致：
1. 创建出的 company `owner_principal_id = "local-board"`（非真实 user_id）
2. 后续 `is_active_member(real_user_id, company_id)` 永远 false
3. WS `/api/live-events` 返回 401
4. 用户看到「company 创建成功」但实际权限全错

**验证**：R626 e2e 跑通（合法用户 sign-in 成功后创建公司 → 真实 user_id 写入 → WS 升级成功，
`next_event_id=9`）。

## 5. UI CSRF Helper

### Before
- 60 client 依赖 better-auth SDK **隐式**注入 `X-CSRF-Token` header
- 第三方 client (CLI / 测试 / 非浏览器 fetch) 集成时一律 403 `CSRF_VALIDATION_FAILED`
- 没有显式 helper，bug 难定位

### After
- `ui/src/api/client.ts` 新增 `applyCsrfHeader(headers, method, path)` 显式 helper
- 跟 Rust CSRF middleware 行为**1:1 对齐**：
  - 同 path 白名单 (`/api/auth/*`, `/api/dev-server/*`, `/live-events`, `/openapi.json`, `/_plugins/*`, `/health`)
  - 同 method 限制 (POST/PUT/PATCH/DELETE)
  - 同 cookie name (`paperclip_csrf`) + header name (`x-csrf-token`)
- 所有 60 client 调用 `request()` 时自动注入，无需修改
- 注释引用 R625 finding，方便后续维护

## 6. CI Workflow

`/Users/louloulin/Documents/lumosaipaperclip/paperclip-rs/.github/workflows/r626-ux-flow-e2e.yml`

- **触发条件**：影响 PC server auth / session / company member / CSRF / live_events / e2e 脚本的 PR
- **步骤**：
  1. 起 PG 16 (service container)
  2. 装 Rust toolchain
  3. 装 Python + websockets 12.0
  4. 跑 8 个 sqlx::test! 集成测试（防 R625 修复回归）
  5. pc-migrate up
  6. cargo build pc-server
  7. 后台启动 pc-server
  8. 跑 7 步 Python UX 流
  9. 失败时 dump server log
- **超时**：15 分钟
- **任一阶段失败 = fail PR**

## 7. 数字

| 指标 | R625 | R626 |
|---|---:|---:|
| 集成测试数 | 0 | **8** (新增) |
| `local-board` fallback 路径 | 1 (生产) | **0** |
| UI 显式 CSRF helper | ❌ | ✅ |
| CI 回归保护 workflow | ❌ | ✅ |
| e2e 7 步响应时间 | 9.16s | ~10s (PC-server 重启 +0.5s) |
| `cargo check --workspace` | 0 errors | 0 errors |

## 8. 下一步 (R627+)

| 优先级 | 轮次 | 目标 |
|---|---|---|
| **P1** | R627 | e2e-ux-flow 扩到 13 步（issue checkout / approval / run continuation） |
| **P1** | R627 | 监控 `require_user_id` 失败频次（去除 fallback 后应能早期发现 client 鉴权问题） |
| **P2** | R628 | terminal-ws 复刻（Node 766 LOC → pc-realtime / pc-environment-support） |
| **P2** | R628 | 写 `live_events` 集成测试 (WS upgrade + welcome + resume buffer) |
| **P2** | R629 | pc-openapi 86.7% → 100% 覆盖率 |
| **P2** | R629 | V12 Playwright 跑通 (real `tests/e2e/full-stack-ui.spec.ts`) |
| **P3** | R630 | plugin-host Node SDK 互操作 |
