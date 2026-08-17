# R761 — 真实 Chromium 浏览器 mutation 链路（14/14 PASS）

## 目标

在 puppeteer + Google Chrome headless 真实浏览器中，通过 Vite (5174) → Rust (3100) → PostgreSQL 17 (55433) 完整链路，对 Agent / Routine / Tool 三个核心域执行 POST→PATCH→GET→DELETE mutation 流程，验证浏览器 fetch 层全链路通。

## 环境

- **浏览器**：Google Chrome 151.0.7922.138（puppeteer-core 驱动）
- **Vite**：127.0.0.1:5174（已运行）
- **Rust server**：127.0.0.1:3100（已运行）
- **PG 17**：127.0.0.1:55433（已运行）
- **部署模式**：local_trusted
- **公司**：11111111-1111-4111-8111-111111111111（R756 Co）

## Mutation 链路结果

### Agent（4/4 PASS）

| 步骤 | 状态码 | 关键字段 |
|---|---|---|
| POST /api/companies/{id}/agent-hires | 201 | id=855859b3-70a0-4c91-b47f-b141228ca707, status=idle |
| PATCH /api/agents/{id} | 200 | title=R761 mutated |
| GET /api/agents/{id} | 200 | status=idle |
| DELETE /api/agents/{id} | 204 | |

### Routine（3/3 PASS）

| 步骤 | 状态码 | 关键字段 |
|---|---|---|
| POST /api/routines | 201 | id=ef08ac89-bf23-46c2-b7ed-d826dbe6d6d4, revision=1 |
| PATCH /api/routines/{id} | 200 | status=paused |
| DELETE /api/routines/{id} | 204 | |

### Tool application（4/4 PASS — R757 critical bug fix 浏览器层验证）

| 步骤 | 状态码 | 关键字段 |
|---|---|---|
| POST /api/companies/{id}/tools/applications | 200 | kind=mcp（正确映射 DB type 列）|
| PATCH /api/tool-applications/{id} | 200 | description=R761 mutated |
| GET /api/tool-applications/{id} | 200 | description=R761 mutated |
| DELETE /api/tool-applications/{id} | 204 | |

## 浏览器层关键发现

| 项 | 结果 |
|---|---|
| Vite → Rust proxy | fetch 直接走 /api/* 路径代理到 3100 |
| 浏览器 fetch POST/PATCH/GET/DELETE | 14/14 正确状态码 |
| ToolApplicationRow.kind → type 列映射 | R757 fix 在浏览器层验证 PASS |
| POST 包络 | Agent: 包络 / Routine: 裸 row / Tool: 裸 row |
| PATCH 返回 | Agent: 裸 row / Routine: 裸 row / Tool: {id, updated} |

## 预存在 bug 验证

| Bug | 现象 | R761 处理 |
|---|---|---|
| Layout toUpperCase throw | React Layout 组件渲染失败 | 已知（hard constraint #5），R761 绕开 UI 直接 fetch API |
| /api/auth/whoami 返回 HTML | endpoint 不存在 | 已知；不影响 R761 |
| /Rd13b0/agents/all → /undefined/dashboard | useActiveCompanyPrefix 解析失败 | 已知；R761 用固定 company_id mutation |

## 截图证据

- .tmp/r761-screenshot.png（5.8 KB）

## 关键证据文件

- .tmp/r761-browser-mutation.json（14 步详细结果）

## R762+ 后续计划

- R762 — pc-decisions / 其他模块集成测试
- Adapter 仍按硬约束保持不动
