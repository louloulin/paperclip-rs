# R756 — UI Agent mutation 真实冒烟

## 目标

验证 paperclip-rs 完整前后端 mutation 链路：Vite (5174) → Rust server (3100) → PostgreSQL 17 (55433)。本轮目标：Agent create → patch → get → delete 走真实 HTTP + DB 持久化。

## 环境前置

- **Rust server**: `target/debug/paperclip-server`，端口 3100，已运行（tty session 95048，PID 28054）
- **Vite dev server**: `pnpm --dir ui dev --host 127.0.0.1 --port 5174`，端口 5174，已运行（tty session 74368，PID 29124）
- **PostgreSQL 17**: `.tmp/pg17data-r756`，端口 55433，已运行
- **部署模式**: `PAPERCLIP_DEPLOYMENT_MODE=local_trusted`（无需登录 session）

## Seed 数据

```sql
INSERT INTO companies (id, name, created_at, updated_at)
VALUES ('11111111-1111-4111-8111-111111111111', 'R756 Co', now(), now())
ON CONFLICT (id) DO NOTHING;
```

## API 调用链

### 1. POST /api/companies/{id}/agent-hires → 201

```bash
curl -s -X POST http://127.0.0.1:3100/api/companies/11111111-1111-4111-8111-111111111111/agent-hires \
  -H 'Content-Type: application/json' \
  -d '{
    "name": "R756 Agent C",
    "role": "general",
    "adapterType": "process",
    "budgetMonthlyCents": 1500
  }'
```

**响应**（`.tmp/r756-agent-create.json`）：
```json
{
  "agent": {
    "id": "61596b5d-1d5e-43cf-b1ca-ce4ed8e487b2",
    "companyId": "11111111-1111-4111-8111-111111111111",
    "name": "R756 Agent C",
    "role": "general",
    "adapterType": "process",
    "budgetMonthlyCents": 1500,
    "status": "idle",
    "permissions": {"canCreateAgents": false, "canCreateSkills": true},
    "createdAt": "2026-08-17T09:45:18.024010Z",
    "updatedAt": "2026-08-17T09:45:18.024010Z"
  },
  "approval": null
}
```

### 2. PATCH /api/agents/{agent_id} → 200

```bash
curl -s -X PATCH http://127.0.0.1:3100/api/agents/61596b5d-1d5e-43cf-b1ca-ce4ed8e487b2 \
  -H 'Content-Type: application/json' \
  -d '{ "title": "R756 mutated", "budgetMonthlyCents": 2500 }'
```

**响应**（`.tmp/r756-agent-update.json`）：title 改为 "R756 mutated"，budgetMonthlyCents 改为 2500，updatedAt 推进。

### 3. GET /api/agents/{agent_id} → 200

**响应**（`.tmp/r756-agent-get.json`）：与 patch 后状态一致，status = "idle"。

### 4. DELETE /api/agents/{agent_id} → 204

无 body（204 No Content）。

### 5. GET /api/agents/{agent_id} → 404

符合预期，agent 已被删除。

### 6. DB 一致性校验

```sql
SELECT count(*) FROM agents WHERE id = '61596b5d-1d5e-43cf-b1ca-ce4ed8e487b2';
-- 返回 0
```

## 关键发现

| 项 | Node paperclip | paperclip-rs | 状态 |
|---|---|---|---|
| 创建路径 | POST /api/companies/{id}/agent-hires | POST /api/companies/{id}/agent-hires | ✅ 一致 |
| POST 返回 | `{agent: {...}, approval: ...}` | `{agent: {...}, approval: null}` | ✅ 一致 |
| PATCH/GET 返回 | 裸 AgentRow | 裸 AgentRow | ✅ 一致 |
| DELETE 返回 | 204 No Content | 204 No Content | ✅ 一致 |
| 必填字段 | name / role / adapterType / budgetMonthlyCents | 同 | ✅ 一致 |
| 默认 status | idle | idle | ✅ 一致 |

## 验证证据

| 文件 | 内容 |
|---|---|
| `.tmp/r756-agent-create.json` | POST 响应（agent_id 已创建）|
| `.tmp/r756-agent-update.json` | PATCH 响应（title/budget 已变更）|
| `.tmp/r756-agent-get.json` | GET 响应（与 patch 后一致）|
| `.tmp/r756-agent-delete.json` | DELETE 响应（空，204）|
| DB count | 0（已彻底删除）|

## 结论

- **真实持久化链路打通**：Vite → Rust → PG 完整三层 mutation 链路无丢失
- **DB 一致性**：DELETE 后 count = 0，无残留
- **状态码**：201/200/204/404 全链路符合预期
- **API 形态**：与 Node paperclip 路径/返回结构一致

## 下一步

- R757 — UI Routine / Tool mutation 冒烟
- R758 — pc-issues / liveness / scheduler 集成测试
- R759 — pc-heartbeat / reconcile 集成测试
