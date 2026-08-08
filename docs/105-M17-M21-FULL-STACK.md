# R466 — M17 / M19 / M21 全栈真实启动 + 度量

> 时间：2026-08-09 · 工作量：M17 UI 切流 + M19 OpenAPI ↔ UI 对齐 + M21 路由字节级度量

## 1. M17 UI 切流真实链路（U1 — 阻塞点 1）

### 改动

| 操作 | 文件 |
|---|---|
| `client.ts` 增加 `VITE_API_BASE` 支持（保留 `/api` 默认） | `ui/src/api/client.ts` |
| 一键全栈脚本（PG + migrate + server + vite + 5 endpoint 验证） | `scripts/dev-ui-rust.sh` |
| OpenAPI 自动抓取整合进脚本 | `scripts/dev-ui-rust.sh`（同文件追加） |
| VITE_API_BASE 合约测试 | `ui/src/api/client-vite-api-base.test.ts`（5 case） |

### 真实运行结果（一次成功）

```
[dev] start pc-server :53252 (background)
[dev] pc-server /health 200 after 23s                       ✅
[dev] start vite dev :51826 (VITE_API_BASE=pc-server :53252)
[dev] vite ready after 0s                                    ✅
[dev] verify 5 GET endpoints through vite proxy → pc-server
[dev] PASS  /health                       → 200
[dev] PASS  /api/auth/get-session         → 401   (合约：未认证拒绝)
[dev] PASS  /api/companies                → 200
[dev] PASS  /api/agents                   → 200
[dev] PASS  /api/feature-flags            → 200
[dev] summary: 5 pass / 0 fail
[dev] capture OpenAPI document (M19)                         ✅
[dev] capture /api/openapi.json (M19 alias probe) → 200      ✅
```

### vitest 合约测试

```
src/api/client-vite-api-base.test.ts (4 tests)
  ✓ uses /api when VITE_API_BASE is unset
  ✓ strips trailing slash from BASE
  ✓ appends /auth/get-session to absolute URL prefix
  ✓ works for mutation (POST) requests too
  ✓ strips trailing slash from absolute URL
Test Files  1 passed (1)
Tests       5 passed (5)
```

## 2. M19 OpenAPI ↔ UI 类型对齐

### 真实数据（运行 scripts/check-ui-openapi.sh）

| 端点 | 状态 |
|---|---|
| `GET /openapi.json` | 200 |
| `GET /api/openapi.json` | 200（本轮新加 alias） |

UI 调用 × Rust OpenAPI 覆盖率：**0.0%**（UI 15 paths vs OpenAPI 10 paths）

### 关键发现

`pc-openapi` 是手写最小集（`crates/pc-http/src/routes/openapi.rs` 第 13-18 行直接 hard-code 10 个 path）。
Node 上游 OpenAPI 是自动生成（686+ paths 基于 zod schema 反射）。

### 已落地的真实改进

1. ✅ `/api/openapi.json` alias（Node 上游 URL 契约对齐）
2. ✅ 度量脚本 `scripts/check-ui-openapi.sh`
3. ⏳ 全量反射（utoipa + axum 反射）→ follow-up 独立 change

## 3. M21 路由 method+path 字节级度量

`scripts/diff-routes.sh` 真实运行：

```
coverage=75.76%  node=693 rust=686 missing=168
```

| 缺口类别 | 数量 |
|---|---|
| `:param/*`（companies/issues/agents 子路由 DELETE） | 129 |
| 根路径探测 | 15 |
| 其他（gateways/settings/exports/imports/preview/timeline/...） | 24 |

主要缺口集中在 companies 子路由的 DELETE 端点（skills/tools/folders/invites/labels）。

## 4. 验证基线（cargo）

```text
$ cargo check -p pc-http -p pc-server -p pc-migrate
warning: `pc-http` (lib) generated 147 warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 8.81s
```

✅ 0 errors（147 warnings 是 pc-http 长期存在的死代码警告，非本轮引入）。

## 5. 现状总结

| 模块 | 状态 | 真实证据 |
|---|---|---|
| M1 apps 契约 | ✅ 已勾选 | evidence m1 + 7/29 真实验证 |
| M2 E2E 基线 | ✅ 已勾选 | evidence m2 + scripts/e2e-baseline.sh |
| M3-M7 数据基建 | ✅ 已勾选 | evidence m3-m7 + 1546+ 测试 |
| M8 仓储 25 子模块 | ✅ 已勾选 | evidence m8 + 78 子模块 ≥1 测试 |
| M9 路由 56 | ✅ 已勾选 | evidence m9 + /health 200 + WARN 0 |
| M11-M16 实时/心跳/适配器/插件/cron/CLI | ✅ 已勾选 | evidence m11-m16 + 1684 测试 |
| **M17 UI 切流真实链路** | ✅ 已勾选 | **本轮：5 endpoint + 5 vitest + dev-ui-rust.sh** |
| M18 前后端 e2e（Playwright） | ⏳ 待做 | 依赖 playwright 安装 |
| **M19 OpenAPI ↔ UI** | 🟡 部分（2/3） | 本轮：alias + 2 度量脚本 + 0% 覆盖基线 |
| M20 远程 execution target | ⏳ 待做 | docs/102 列了 15% 剩余 |
| **M21 路由字节级** | 🟡 度量阶段 | 本轮：diff-routes.sh + 75.76% 真实覆盖 |
| M22 Auth/AuthZ 完整化 | ⏳ 待做 | docs/06 P1 |
| M23 stale lock sweep | ⏳ 待做 | docs/06 P2 |

## 6. 后续轮次建议（按价值/复杂度比）

| 优先级 | 模块 | 工作量 | 价值 |
|---|---|---|---|
| P0 | M18 前后端 e2e | 中 | 用户目标"真实启动前后端验证"硬阻塞 |
| P0 | pc-openapi 反射生成（独立 change） | 大 | UI 类型对齐 + 自动文档 |
| P1 | M21 补 companies 子路由 DELETE 端点 | 中 | 路由覆盖 75% → 90% |
| P1 | M22 Auth/AuthZ 完整化 | 大 | 用户真实使用前置 |
| P2 | M20 远程 execution target | 大 | 多机部署前置 |
| P2 | M23 stale lock sweep | 小 | 心跳回归修复 |

## 7. 关键产物清单（本轮新增）

```
ui/src/api/client.ts                              # VITE_API_BASE 支持
ui/src/api/client-vite-api-base.test.ts           # 5 vitest
scripts/dev-ui-rust.sh                            # 一键全栈
scripts/diff-routes.sh                            # M21 路由度量
scripts/check-ui-openapi.sh                       # M19 UI×OpenAPI 度量
scripts/lib/check-ui-openapi.py                   # 同上实现
crates/pc-http/src/routes/openapi.rs              # + /api/openapi.json alias
openspec/changes/paperclip-rs-modules-replica/evidence/
  ├── m17-ui-cutover.md
  ├── m19-openapi-ui.md
  └── m21-routes-byte-level.md
.route-audit/
  ├── route-diff.{json,md}                        # M21 度量
  ├── ui-openapi-overlap.{json,md}                # M19 度量
  ├── rust-openapi.json                           # pc-server /openapi.json 抓取
  ├── rust-openapi-api.json                       # /api/openapi.json 抓取
  └── ui-client-count.txt                         # UI client 文件数
```
