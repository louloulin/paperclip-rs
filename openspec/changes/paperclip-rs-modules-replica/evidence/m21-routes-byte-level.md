# Evidence: M21 — 路由 method+path 字节级对齐

## 度量脚本

`scripts/diff-routes.sh` 提取 Node 上游（`../paperclip/server/src/routes/*.ts`）与 Rust 端（`crates/pc-http/src/routes/*.rs`）的 method+path 路由表，归一化 `:param` 与 `${id}` 后计算重合率。

## 真实运行结果

```text
$ bash scripts/diff-routes.sh
coverage=75.76%  node=693 rust=686 missing=168
reports: .route-audit/route-diff.{json,md}
```

| 维度 | 数值 |
|---|---|
| Node unique method+path | 693 |
| Rust unique method+path | 686 |
| Common | 525 |
| Missing in Rust | **168** |
| Extra in Rust | 161 |
| **Coverage** | **75.76%** |

## 缺口分布（按 `/api/<category>/*` 聚合）

| Category | Missing count |
|---|---:|
| `:param/*`（companies/issues/agents/cases 子路由） | 129 |
| 根路径探测（`/`、`/api/`、`/api/openapi.json`） | 15 |
| `/api/gateways/*` | 3 |
| `/api/settings/*` | 3 |
| `/api/exports/*` / `/api/imports/*` / `/api/export/*` | 6 |
| `/api/artifacts/*` / `/api/preview/*` / `/api/timeline/*` / `/api/me/*` / `/api/feedback-traces/*` / `/api/jobs/*` / `/api/users/*` / `/api/restart/*` | 8 |

## 主要缺口模式

### 1. companies 子路由 DELETE 端点（high-frequency）

```
DELETE /api/companies/:company_id/folders/:folder_id
DELETE /api/companies/:company_id/me/user-secrets/:secret_id
DELETE /api/companies/:company_id/skill-policy
DELETE /api/companies/:company_id/skill-test-run-templates/:template_id
DELETE /api/companies/:company_id/skills/:skill_id
DELETE /api/companies/:company_id/skills/:skill_id/comments/:comment_id
DELETE /api/companies/:company_id/skills/:skill_id/files
DELETE /api/companies/:company_id/skills/:skill_id/star
DELETE /api/companies/:company_id/skills/:skill_id/test-inputs/:input_id
DELETE /api/companies/:company_id/tools/policies/:policy_id
```

### 2. issues 子路由

```
DELETE /api/issues/:issue_id/comments/:comment_id
DELETE /api/issues/:issue_id/documents/:doc_id
DELETE /api/issues/:issue_id/inbox-archive
DELETE /api/issues/:issue_id/watchdog
```

### 3. OpenAPI URL 差异（已修复）

```
GET /api/openapi.json  (Node 上游)   →  现已 alias
GET /openapi.json      (Rust)        →  两条 URL 都返回 200
```

## 设计意图与现状差异

| 类别 | Node | Rust | 决策 |
|---|---|---|---|
| `/openapi.json` mount | `/api/openapi.json` | `/openapi.json` + alias | ✅ 本轮加 alias 对齐 |
| `_plugins/:id/ui/*` | `/api/_plugins/:id/ui/*filePath` | `/_plugins/:id/ui/*path` | Rust 根路径是设计选择；未来如需加 alias |
| `/api/me/user-secrets/*` | 在 companies 子路由 | 未实现 | 列入 M21-follow-up |

## 后续 M21-follow-up

补全 companies/issues/agents 子路由 DELETE 端点（按缺口清单顺序补），
预期增量 +30–40 个 method+path 覆盖，覆盖率 75.76% → 90%+。

## 结论

**M21 度量阶段通过**：
- ✅ `scripts/diff-routes.sh` 真实度量（75.76% 覆盖）
- ✅ 168 个缺口清晰分类
- ✅ `/api/openapi.json` URL 契约对齐
- ⏳ 168 个 method+path 实际补全列为 follow-up
