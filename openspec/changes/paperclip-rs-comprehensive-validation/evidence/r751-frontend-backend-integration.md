# R751 — Vite / Rust / PostgreSQL 前后端真实集成验证

## 目标

验证已完成的 UI 是否与 Rust server 真实连通，并完成一条可回溯的业务写链路：

```text
Chrome
  ↓
Vite :5174 /api proxy
  ↓
Rust paperclip-server :3100
  ↓
PostgreSQL 17 :55433 / paperclip_repos
```

本轮不修改 Adapter，不修改 UI 实现，不修改业务代码；只验证现有集成链路。

## 环境

- Rust binary：`target/debug/paperclip-server`
- Vite：`ui`，监听 `127.0.0.1:5174`
- Rust API：监听 `127.0.0.1:3100`
- PostgreSQL：Homebrew PostgreSQL `17.7`
- 数据库：`postgres://paperclip:paperclip@127.0.0.1:55433/paperclip_repos`
- 部署模式：`local_trusted`
- 浏览器：agent-browser / Chrome

## 验证步骤

### 1. 服务与健康检查

```text
GET http://127.0.0.1:5174/api/health
→ HTTP 200
→ Vite proxy 成功转发到 Rust server
```

返回关键字段：`status=ok`、`db.ok=true`、`bootstrapStatus=ready`。

### 2. 真实 Issue mutation

通过 Vite `/api` 代理调用 Rust server：

```text
POST /api/companies/11111111-1111-4111-8111-111111111111/issues
  body: { title: "R751 integration issue", status: "todo" }
→ HTTP 成功

PATCH /api/issues/ecfdb5c5-19bb-4db8-b9b7-e823bd848d64
  body: { status: "in_progress", description: "updated through Vite proxy" }
→ HTTP 成功
→ status = "in_progress"
→ description = "updated through Vite proxy"

DELETE /api/issues/ecfdb5c5-19bb-4db8-b9b7-e823bd848d64
→ HTTP 204
```

数据库最终确认：`issues` 中该 id 记录数为 `0`。删除后 GET 返回 HTTP 404。

### 3. 浏览器页面

agent-browser 真实访问 `http://127.0.0.1:5174/onboarding`，完成：

- `Name your company`：输入 `R751 Browser Company`
- `Define your mission`：输入 `Build reliable agent workflows`
- 页面进入 mission 步骤并保持交互状态
- 截图：`/Users/louloulin/.agent-browser/tmp/screenshots/screenshot-1786922990769.png`

## 结论

1. UI 页面可以真实加载到 Vite。
2. Vite `/api` 代理可以真实访问 Rust server。
3. Rust server 可以在 PostgreSQL 17 上完成真实 migration、连接和 API 读写。
4. POST → PATCH → DELETE → 404 → DB 清空 全链路通过。
5. UI 的 mutation API bridge 已通过真实网络验证。

## 额外环境发现

首次使用 Homebrew PostgreSQL 14.18 时，迁移在 `UNIQUE NULLS NOT DISTINCT` 处失败；这是 PostgreSQL 14 与项目当前迁移语法不兼容，不是 Rust 代码或 UI 集成错误。改用 PostgreSQL 17.7 后服务正常启动。后续真实验证统一记录此版本前置条件。
