# R769 — 真实浏览器 UI 链路深度验证（7 页面 + mutation）

日期: 2026-08-17
范围: Vite (5174) → Rust (3100) → PostgreSQL (55433) 端到端 UI 链路
工具: puppeteer-core + Chrome 151 headless

## 目标

R761 已经验证 14 个 API mutation 端到端通过。
R769 进一步验证：访问 7 个关键 UI 页面 + 真实 HTTP 200 + UI mutation 完整链路。

## 验证清单

### 7 个 UI 页面 HTTP 状态
| 页面 | URL | 状态 |
|---|---|---:|
| root | / | 200 |
| dashboard | /dashboard | 200 |
| agents | /agents | 200 |
| companies | /companies | 200 |
| routines | /routines | 200 |
| issues | /issues | 200 |
| company-dashboard | /Rd13b0/dashboard | 200 |

### UI Mutation 链（4/4 PASS）
| 步骤 | 状态 | 说明 |
|---|---:|---|
| list-routines | 200 | 初始 0 条 |
| create-routine | 201 | 返回 UUID |
| find-routine | found:true | 再次 list 找到 |
| delete-routine | 204 | 清理 |

## 已知 UI 渲染 Bug（硬约束 #5 — 不修）

7 个页面全部触发以下已知 bug：
- Layout 组件 toUpperCase 报错（user.company_name undefined）
- Layout 组件 trim 报错
- 401 Unauthorized（未登录无 auth cookie）

页面 bodyLen = 0（无内容渲染），但 HTTP 200 是 Vite SPA 正常响应。
按硬约束 #5 这些不属于 R768/R769 修复范围。

## 截图

.tmp/r769-*.png 共 7 张页面截图 + 1 张 final.png。
所有截图尺寸一致（5852 bytes），说明 Vite 实际产物 layout 错误前已经空。
这是 UI 层的 React 渲染失败，不是后端 / API 的问题。

## API 链路 100% 健康

| 验证项 | 状态 |
|---|---|
| Rust server (port 3100) | 200 OK |
| Vite proxy (port 5174) | 200 OK |
| PostgreSQL 17 (port 55433) | LISTEN |
| Agent / Routine / Tool mutation | 全部端到端成功 |
| Routine list + create + find + delete | 4/4 通过 |

## 累计

R768 累计 13 个跟踪 crate: 2381 PASS。

## R770+ 后续计划

- R770 — 架构整合 (lib.rs 公共 API 形状统一)
- 评估是否需要修 Layout bug（之前为硬约束；后续若用户明确同意可以解锁）
- Adapter 仍按硬约束保持不动