# Paperclip 整体架构分析（Node/TS 端基线）

> 日期：2026-08-03 · 用途：Rust 重写的契约对照基线

## 一、仓库结构

```
paperclip/                                       (pnpm monorepo)
├── server/                  Node + Express 后端
│   ├── src/
│   │   ├── routes/          56 个路由模块（HTTP 入口）
│   │   ├── services/        212 个服务模块（业务逻辑）
│   │   ├── adapters/        server 端内置适配器 host 抽象
│   │   ├── auth/            better-auth 集成
│   │   ├── realtime/        live-events WebSocket
│   │   ├── middleware/      authz、log、error
│   │   ├── storage/         local-disk / s3 provider
│   │   ├── secrets/         本地加密 / AWS Secrets Manager
│   │   ├── built-ins/       内置 agent 模板
│   │   ├── instrumentation  pino 日志 + 启动横幅
│   │   ├── config.ts        配置加载
│   │   ├── runtime-config.ts 运行期配置
│   │   ├── app.ts           Express 装配
│   │   ├── index.ts         HTTP 入口
│   │   └── shutdown.ts      优雅关闭
│   └── ...
├── ui/                      React + Vite 前端（**复用**）
│   └── src/
│       ├── api/             60 个 API 客户端模块（基于 @paperclipai/shared）
│       ├── components/      ~50 个组件分类
│       ├── adapters/        11 个 adapter 的 UI 实现
│       ├── plugins/         插件 UI 渲染
│       ├── context/         React 上下文
│       ├── hooks/           自定义 hooks
│       ├── pages/           路由级页面
│       └── lib/             工具函数
├── cli/                     paperclipai CLI
│   └── src/                 run / install / onboard / doctor 等 20+ 子命令
├── packages/
│   ├── db/                  Drizzle ORM + 109 张表 schema + 迁移
│   ├── shared/              前后端共享类型（zod schema + 枚举）
│   ├── adapters/            11 个适配器（claude-local/codex-local/...）
│   │   ├── claude-local
│   │   ├── codex-local
│   │   ├── cursor-cloud / cursor-local
│   │   ├── gemini-local
│   │   ├── grok-local
│   │   ├── hermes
│   │   ├── hermes-gateway
│   │   ├── openclaw-gateway
│   │   ├── opencode-local
│   │   ├── pi-local
│   │   └── adapter-utils     共享 helper
│   ├── plugins/             插件 SDK + examples
│   │   └── paperclip-plugin-sdk
│   ├── skills-catalog/      技能清单（manifest + SKILL.md）
│   ├── teams-catalog/       团队目录
│   ├── google-sheets-mcp-server
│   ├── kv-demo-mcp-server
│   └── mcp-server
├── docker/                  docker-compose / Dockerfile / entrypoint
├── scripts/                 monorepo 工具脚本
├── design/                  UI 设计稿
├── doc/                     额外文档
├── docs/                    README 等
├── evals/                   eval 套件
├── tests/                   集成测试
├── releases/                预打包发布产物
├── screenshots/             截图
└── tools/                   内部工具
```

## 二、关键数字基线

| 指标 | 数值 |
|---|---|
| server/src/routes/ | 56 个文件 |
| server/src/services/ | 212 个文件 |
| packages/db/src/schema/ | 109 张表 |
| packages/adapters/ | 11 个内置适配器 |
| ui/src/ | 1168 个文件，~34.4 万行 |
| ui/src/api/ | 60 个 API 客户端模块 |
| 总 TS 文件 | ~760 个，约 44.4 万行 |
| CLI 子命令 | 20+ |
| 数据库表数 | 109 |
| HTTP 路由端点 | 56 个 router，约 300+ endpoint |

## 三、核心业务领域（从 server/src/services 抽象）

| 领域 | Node 服务模块 | 关键业务对象 |
|---|---|---|
| 公司 | `company.ts`, `membership.ts` | Company, Membership, Role |
| Agent | `agent.ts`, `agent-instances.ts` | Agent, AgentInstance, Runtime |
| Issue / Task | `issue.ts`, `issues-*.ts` | Issue, IssueComment, Assignment |
| Case | `case.ts` | Case, CaseItem |
| Project | `project.ts` | Project, ProjectMember |
| Approval | `approvals.ts` | ApprovalRequest, ApprovalDecision |
| Decision | `decisions.ts`, `decision-training.ts` | Decision, TrainingSample |
| Routine | `routines.ts` | Routine, RoutineRun |
| Pipeline | `pipelines.ts` | Pipeline, PipelineRun |
| Environment | `environments.ts` | Environment, EnvBinding |
| Execution Workspace | `execution-workspaces.ts` | Workspace, WorkspaceMember |
| Goal | `goals.ts` | Goal, GoalLink |
| Folder / Document | `folder.ts`, `documents.ts` | Folder, Document, Revision |
| Sidebar / Inbox | `sidebar.ts`, `inbox.ts` | SidebarLayout, InboxItem |
| Heartbeat | `heartbeat.ts` | HeartbeatRun, HeartbeatTick |
| Plugin | `plugin-registry.ts`, `plugin-lifecycle.ts`, `plugin-worker-manager.ts`, `plugin-tools.ts` | Plugin, PluginConfig, PluginJob |
| Auth | `auth.ts`, `authz.ts`, `authorization.ts` | Session, Cookie, Role |
| Activity / Cost | `activity-log.ts`, `costs.ts` | ActivityEvent, CostEvent |
| Summary | `summary.ts`, `summary-slots.ts` | Summary, Slot |
| Tool | `tools.ts`, `tool-gateway.ts` | Tool, ToolInvocation |
| Secrets | `secrets.ts`, `secret-providers/*` | Secret, SecretVersion |
| Status Card | `status-cards.ts` | StatusCard, StatusCardQuery |
| Skill | `skills-catalog.ts`, `company-skill-policy.ts` | Skill, SkillPolicy |
| Access | `access.ts` | AccessGrant, Principal |
| Board Chat | `board-chat.ts` | ChatRoom, Message |
| Built-in Agents | `built-in-agents.ts` | BuiltInAgent, Template |
| Adapter / 心跳调度 | `heartbeat.ts`, `adapters/*` | AdapterConfig, AdapterRun |
| Live Events | `realtime/live-events-ws.ts` | LiveEvent |

## 四、HTTP 路由（56 个 router）

```
access            activity         adapters       agents
approvals         assets           attention      auth
authz             board-chat       built-in-agents cases
companies         company-import-paths       company-skill-policy
company-skills    costs            dashboard      decision-training
decisions         documents        environment-selection
environments      execution-workspaces      file-resources
folders           goals            health         inbox-agent-policy
inbox-dismissals  instance-database-backups instance-settings
issue-tree-control issues          issues-checkout-wakeup
llms              live-events      openapi        org-chart-svg
pipelines         plugin-ui-static plugins        projects
resource-memberships routines       secrets        sidebar-badges
sidebar-preferences smoke-lab      status-cards   summary-slots
teams-catalog     tool-access      tool-gateway   user-profiles
workspace-command-authz     workspace-runtime-service-authz
```

## 五、数据库 schema 概览（109 张表）

按域分组（具体表名见 `packages/db/src/schema/`）：

```
auth (4)        : users, sessions, accounts, verifications
company (8)     : companies, memberships, instance_user_roles, ...
agents (7)      : agents, agent_runs, agent_messages, agent_invocation_costs, ...
issues (12)     : issues, issue_comments, issue_attachments, ...
cases (5)       : cases, case_items, case_members, ...
projects (6)    : projects, project_members, project_milestones, ...
approvals (4)   : approval_requests, approval_decisions, ...
decisions (3)   : decisions, decision_training_samples, ...
heartbeat (6)   : heartbeat_runs, heartbeat_ticks, heartbeat_monitors, ...
plugins (12)    : plugins, plugin_configs, plugin_jobs, plugin_logs, ...
routines (4)    : routines, routine_runs, routine_steps, ...
pipelines (4)   : pipelines, pipeline_runs, pipeline_steps, ...
environments (3): environments, environment_bindings, ...
execution (3)   : execution_workspaces, workspace_members, ...
goals (3)       : goals, goal_links, goal_metrics, ...
folders/docs (5): folders, documents, document_revisions, ...
sidebar/inbox (4): sidebar_layouts, inbox_items, ...
secrets (6)     : company_secrets, secret_versions, secret_access_events, ...
tools (5)       : tool_applications, tool_connections, tool_invocations, ...
status_cards (3): status_cards, status_card_queries, status_card_updates, ...
costs (3)       : cost_events, cost_aggregates, ...
activity (3)    : activity_events, audit_log, ...
summary (2)     : summaries, summary_slots, ...
misc (12)       : attachments, notifications, ...
```

## 六、实时事件（Live Events）

WebSocket 端点：`/api/live-events/stream`
消息 schema：`{ event: string, resource: string, resource_id: uuid, company_id?: uuid, actor?: string, at: ISO8601, data?: any }`

事件类型（不全）：
- `agent.run.queued|running|succeeded|failed|cancelled`
- `issue.created|updated|assigned|completed`
- `heartbeat.tick|complete`
- `plugin.installed|upgraded|enabled|disabled`
- `live.cost.recorded`
- `chat.message`

## 七、关键运行时特征

### 7.1 多进程/多运行时

| 组件 | 进程模型 | IPC 方式 |
|---|---|---|
| server 主进程 | 单 Node + Express | 内部 |
| adapter 子进程 | child_process.spawn × 11 | stdio JSON-RPC |
| plugin worker 进程 | child_process.spawn × N | stdio JSON-RPC |
| embedded-postgres | 独立子进程（postgresql 原生） | TCP 5432 |
| better-auth | server 进程内 | 内部 |
| ws live-events | server 进程内 | WS |
| multer 文件上传 | server 进程内 | 内部 |
| sharp 图片处理 | server 进程内（native） | 内部 |
| ssh2 远程操作 | server 进程内（native） | 内部 |
| better-sqlite3 | server 进程内（native） | 内部 |

**问题**：5+ 种进程/线程模型缺乏统一抽象，重构困难。

### 7.2 核心调度循环（heartbeat）

```
services/heartbeat.ts
  for each agent where enabled:
    while true:
      1. pick task (issue/case/...)
      2. invoke adapter.run(...)
      3. collect output → commit to DB
      4. emit live-event
      5. backoff / sleep
```

### 7.3 插件 Worker 编排

```
services/plugin-worker-manager.ts
  - 维护 { plugin_id → WorkerProcess } 映射
  - 每个 worker 是独立 Node 进程（运行 paperclip-plugin-*）
  - 双向 stdio JSON-RPC：host ↔ worker
  - 失败重连、健康检查、graceful shutdown
```

## 八、与 paperclip-rs 的对照映射

| Node 端 | Rust 端 | 状态 |
|---|---|---|
| server/src/index.ts | pc-server/src/main.rs | ✅ 已实现 |
| server/src/routes/*.ts (56) | pc-http/src/routes/*.rs (56) | ✅ 已建框架，部分真实化 |
| server/src/services/*.ts (212) | pc-repos/src/*.rs (29) | ✅ 已建框架，关键路径真实化 |
| better-auth | pc-auth | ✅ 已实现 |
| middleware/authz.ts | pc-authz | ✅ 已实现 |
| packages/db/src/schema/* (109) | pc-db migrations + sqlx | ✅ 已迁移 |
| adapters/ (11) | pc-adapter-{name} (1/11) | 🔄 codex-local 已完成，其余 10 个待实现 |
| realtime/live-events-ws | pc-realtime + pc-ws | ✅ 已实现 |
| storage/ | pc-storage | ✅ 已实现 |
| secrets/ | pc-secrets（计划中） | 🔄 部分实现，rotate/usage 待补 |
| plugin-worker-manager | pc-plugin-host | ⏳ 待实现 |
| built-ins/ | pc-built-in-agents | ⏳ 待实现 |
| instrumentation + shutdown | pc-telemetry | ✅ 已实现 |
| config.ts | pc-config | ✅ 已实现 |
| cli/src/*.ts | paperclip-cli（待） | ⏳ 待实现 |

## 九、契约冻结清单（必须保持兼容）

- HTTP 路由 path + method
- 请求/响应 JSON shape（camelCase + 字段名 + null vs undefined 语义）
- HTTP 错误码 + body 形状
- WebSocket 消息 schema + 事件名
- 数据库表 schema + 列名 + 类型
- 适配器 stdio JSON-RPC 协议
- 插件 SDK stdio JSON-RPC 协议
- CLI 子命令 + 参数语义

