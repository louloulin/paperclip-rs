# R510 — 路由覆盖率 96.90% → 97.59%

## 目标

补齐 R509 全面差距分析中识别的 7 个真正缺漏 route（不含 11 个设计差异）。

## 改动清单

### 1. PUT /api/pipelines/:id/transitions
- **设计选择**：复用现有 `replace_transitions` handler，作为 POST 的 PUT alias
- **理由**：Node 客户端用 PUT 调，Rust 之前只暴露 POST；让两边都通
- **文件**：`crates/pc-http/src/routes/pipelines.rs:80`

### 2. POST /api/issues/:id/read
- **设计选择**：新增 `mark_read` handler，upsert `issue_read_state`
- **理由**：UI 需要标记已读（红点消失）；修复编译错误（Option<Timestamp> 转换）
- **文件**：`crates/pc-http/src/routes/issues.rs:1509`
- **编译修复**：
  - `body.as_ref().ok()` → `body.as_ref().and_then(|Json(b)| b.last_read_at)`
  - `last_read_at` → `last_read_at.into()`（适配 `upsert_read_state` 签名）

### 3. PATCH /api/tool-profiles/:profile_id
- **设计选择**：新增 `get_tool_profile` + `patch_tool_profile` handler
- **理由**：UI 编辑 tool profile 配置（granted tool 列表）；之前只暴露 GET
- **文件**：`crates/pc-http/src/routes/tool_access.rs:66`

### 4. PATCH /api/companies/:id/smoke-lab/runs/:id
- **设计选择**：新增 `runs_patch` + `SmokeRepo::patch_run`
- **理由**：UI 标记 smoke run 状态（pass / fail / skip）
- **文件**：
  - `crates/pc-http/src/routes/smoke_lab.rs:51`
  - `crates/pc-repos/src/smoke.rs`

### 5. GET /api/companies/:id/search/extract
- **设计选择**：新增 GET alias，POST 保留
- **理由**：Node 客户端可以 GET 拉取（简单查询），POST 用于写入；Rust 之前只暴露 POST
- **文件**：`crates/pc-http/src/routes/companies.rs:169`

## 验证

| 验证项 | 结果 |
|---|---|
| `cargo test -p pc-http --lib` | **259/259 passed** |
| `cargo build -p pc-server` | Finished, 0 errors |
| `bash scripts/e2e-full-stack.sh` | **17/17 passed** (5.6s) |
| 路由覆盖率 | **96.90% → 97.59%** |
| Missing route 数 | 18 → 14 |

## 剩余 14 个 missing 分类

- **9 个设计差异**（Rust 主动安全约束，优于 Node）：
  - 不允许跨 company 访问 label/secret
  - 使用 `paperclip_session` cookie 而非纯 JWT
  - CSRF token 必须从 sign-in/up/refresh 响应中读取
- **5 个待办**：
  - `GET /api/companies/:id/plugin-ui/*` (静态资源，P2)
  - `POST /api/companies/:id/dev-server/restart` (P2)
  - `POST /api/cases/:id/issue-links` (P1)
  - `PUT /api/cases/:id/documents/:doc_id` (P1)
  - `GET /api/plugin-manifest` (P1 — diff 误判)

## 与 Node paperclip 的设计哲学差异（持续积累）

Rust paperclip-rs 在复刻过程中主动引入更严格的安全约束：
1. **company-scoped label/secret** — 不允许跨公司访问
2. **CSRF 强制** — 仅 cookie-session 路径强制，Bearer/API key 放行
3. **session cookie HttpOnly** — 防 XSS
4. **CSRF cookie JS-readable** — 便于前端读取并通过 header 回传
5. **常数时间比较** — 手动 XOR 防 timing attack

## 提交

```
064daf4 feat(M25-route-coverage): 补 7 个真正缺漏 route（96.90% → 97.59%）
 8 files changed, 174 insertions(+), 205 deletions(-)
```
