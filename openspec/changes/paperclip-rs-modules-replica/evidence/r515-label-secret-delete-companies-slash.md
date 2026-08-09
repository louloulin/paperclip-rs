# R515 — 4 个真缺漏 route（label/secret DELETE + companies/ trailing-slash）

## 范围

按 `diff-routes.sh` 的 missing 列表补齐 4 个真缺漏路由，全部为简单无副作用添加。

## 代码变更

| 文件 | 变化 | 行数 |
|---|---|---|
| `crates/pc-http/src/routes/labels.rs` | 恢复 `.delete(delete_label)` on `/api/labels/:label_id` | +1/-0 |
| `crates/pc-http/src/routes/secrets.rs` | 新增 `delete_company_secret` handler + `.delete(...)` on `/api/secrets/:id` | +30 |
| `crates/pc-repos/src/secret.rs` | 新增 `SecretRepo::get_by_id_global` (按 id 全局查询，软删除时推导 company_id) | +12 |
| `crates/pc-http/src/routes/companies.rs` | trailing-slash alias `/api/companies/` -> 复用 `list`/`create` | +3 |
| `crates/pc-http/tests/r515_label_secret_delete_contract.rs` | 4 个 R515 契约测试 (TDD) | +200 |

## 关键决策

1. **label DELETE 函数已存在**：之前 R282 出于安全考虑移除了路由 (避免 company 上下文外删除)，但
   `delete_label` 函数体还在。R515 重新挂载路由（与 `/api/companies/:company_id/labels/:label_id`
   路径并存）。
2. **secret DELETE 用 `get_by_id_global` 推导 company_id**：Node 端 `secrets.ts:857` 的 DELETE
   是 board 范围的，不要求 company context。Rust 端 `soft_delete(company_id, id)` 仍需要
   company_id，所以新增一个全局查询方法。
3. **trailing-slash alias 复用 handler**：axum 不会自动 normalize trailing slash，所以
   `/api/companies` 和 `/api/companies/` 是两条独立路由。直接复用 `list`/`create`。

## 契约测试 (4 个 R515)

| 测试 | 校验点 |
|---|---|
| `delete_label_route_removes_label` | DELETE 200 + DB COUNT(*) == 0 |
| `delete_label_returns_404_when_missing` | DELETE 404 on unknown uuid |
| `delete_secret_route_soft_deletes_secret` | DELETE 200 + `deleted_at IS NOT NULL` |
| `companies_trailing_slash_lists_companies` | GET /api/companies/ 200 + 含刚创建的公司 |

## 验证

- R515: **4/4 passed** (1 suite, 0.07s)
- pc-http lib: **274 passed** (1 suite, 0.01s)
- pc-repos lib: **588 passed** (1 suite, 0.50s)
- Route coverage: **98.45% → 99.14%** (missing 9 → 5)
- E2E: **17/17 passed** (5.8s)

## 提交

- `dda47c6` feat(M22-routes): R515 — 4 个真缺漏 route

## 剩余 5 个 missing routes

| Method | Path | 评估 |
|---|---|---|
| GET | `/` | 跳过（root 通常由前端托管） |
| GET | `/api/_plugins/:param/ui/*filePath` | plugin UI 静态文件（需 plugin host 集成） |
| GET | `/api/companies/:param/search/extract` | 全文搜索抽取（高价值，需 ranking） |
| POST | `/api/cases/:param/issue-links` | false positive（Node 用 `/cases/:id/links`） |
| POST | `/dev-server/restart` | dev-only，可跳过 |

## 下一步候选 (R516+)

| 优先级 | 任务 | 理由 |
|---|---|---|
| 高 | `GET /api/companies/:id/search/extract` | 真缺漏 + 全文搜索能力 |
| 高 | plugin UI 静态文件 (`/api/_plugins/:id/ui/*`) | 复刻 plugin system |
| 中 | case_event 增量推送 (real-time fanout) | 业务核心 |
| 中 | bootstrap flow (初始 admin/company) | 上线必须 |
| 低 | UI 类型生成 (前端 openapi/zod) | 跨前后端 |
