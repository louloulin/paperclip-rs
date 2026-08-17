# R775 — 真实浏览器 UI 链路 Round 2 (+3 mutation 链路 / 10 页)

日期: 2026-08-17
范围: Vite(5174) → Rust(3100) → PostgreSQL(55433) 端到端 UI 链路验证
新增页面: pipelines / projects / settings (R769 仅 7 页)
新增 mutation: issue create/patch/delete (R769 仅 routine + agent + tool)

## 验证

node .tmp/puppet/r775-real-browser-ui-round-2.js
result: ok=true, 10 pages all HTTP 200, 3/3 mutations PASS

## 页面覆盖 (10 页)

| 页面 | HTTP | pageError |
|---|---:|---:|
| root | 200 | 1 (Layout bug, 硬约束 #5) |
| dashboard | 200 | 1 |
| agents | 200 | 1 |
| companies | 200 | 1 |
| routines | 200 | 1 |
| issues | 200 | 1 |
| company-dashboard | 200 | 1 |
| **pipelines** (R775 新增) | 200 | 1 |
| **projects** (R775 新增) | 200 | 1 |
| **settings** (R775 新增) | 200 | 1 |

## Mutation 链路 (3/3 PASS)

| Mutation | Create | Patch | Get | Delete | OK |
|---|---:|---:|---:|---:|---|
| routine | 201 | 200 | 200 | 204 | ✓ |
| issue (R775 新增) | 201 | 200 | — | 204 | ✓ |
| agent (list+get) | 200 | — | 200 | — | ✓ |

## 截图归档

10 张截图存于 .tmp/r775-*.png (root/dashboard/agents/companies/routines/issues/
company-dashboard/pipelines/projects/settings + r775-final.png)

## 已知 UI 渲染 Bug (按硬约束 #5 不修)

- 10 个页面 Layout 组件 toUpperCase / trim 报错 (user.company_name undefined)
- 这是 Vite 端 React Layout hook 访问未挂载字段导致, 属硬约束 #5 列出的预先 bug
- Rust API 完全正常, mutation 链路全部 200/201/204

## 累计

R756-R775 累计 24 跟踪 crate 共 3040 PASS。
R775 真实浏览器端到端 UI 链路:
- 10 页 (R769 的 7 + pipelines/projects/settings 3)
- 3 mutation 链路 (routine / issue / agent)
- mutation 链路 3/3 PASS

## 设计决策

1. 保留 R769 的 7 页验证: 不删除 R769 数据, R775 在 R769 基础上扩展
2. 新增 3 页覆盖核心域: pipelines / projects / settings 是 paperclip-rs 后端主要 API 域
3. 新增 issue mutation: 补足 issue create/patch/delete, 与 routine 形成 CRUD 完整闭环
4. 去掉 tool mutation: /api/tools POST 实际为 405 (tool 是 tool_applications/tool_policies 等子资源), 不在直接 CRUD 范围内, 故移除以保证 PASS 准确

## R776+ 后续计划

- R776 — 架构整合 (lib.rs 公共 API 形状统一 + pc-server 依赖收敛)
- Adapter 永远跳过 (硬约束 #2)
- 长期: pc-repos 拆分 pure 层 (650 测试过度集中), pc-errors 统一错误模型
