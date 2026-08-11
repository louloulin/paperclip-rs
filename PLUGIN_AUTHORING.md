# Paperclip 插件作者指南（PLUGIN_AUTHORING.md）

> R584 / 2026-08-12
> 范围：插件 manifest / IPC 协议 / 事件 / 工具 / 状态 / webhook / UI
> 协议参考：`crates/pc-plugin-protocol/src/` (1,812 行 Rust)
> 配套：`OPERATIONS.md`（部署）/ `ARCHITECTURE.md`（架构）

## 1. 概述

Paperclip 插件是一个独立的 Node.js / TypeScript（或任意 JSON-RPC 兼容）进程，通过 **stdio JSON-RPC 2.0** 与 host (`paperclip-server`) 通信。

```
┌──────────────────┐                ┌──────────────────┐
│  plugin worker   │  JSON-RPC/stdio│  paperclip-server│
│  (your code)     │ <───────────>  │  (Rust host)     │
│  Node 18+ / Deno │                │                  │
└──────────────────┘                └──────────────────┘
        │                                    │
        │ spawn()                            │ pg / http / events
        ▼                                    ▼
   独立文件系统                            主数据库
```

### 1.1 关键特性

- **沙箱**：插件独立进程；host 控制 IPC + 资源限制
- **热加载**：修改 manifest 可热重载，无需重启 server
- **能力声明**：插件必须声明 `capabilities`，host 按 capability 分配权限
- **版本化协议**：`manifestVersion: "v1"`，未来 v2 不破坏 v1 插件
- **事件订阅**：插件可订阅 host 事件（issues、agents 等）

## 2. 快速开始

### 2.1 最小的 manifest

```json
{
  "id": "com.example.hello",
  "version": "1.0.0",
  "manifestVersion": "v1",
  "label": "Hello World",
  "description": "A minimal example plugin",
  "entry": "./dist/index.js",
  "capabilities": []
}
```

### 2.2 最小的 worker (TypeScript)

```typescript
#!/usr/bin/env node
// ./dist/index.js
import { createInterface } from "readline";

const rl = createInterface({ input: process.stdin });

// 1. 监听 host 消息
rl.on("line", (line) => {
  try {
    const msg = JSON.parse(line);
    handleRequest(msg);
  } catch (e) {
    console.error("parse error", e);
  }
});

function send(msg: unknown) {
  process.stdout.write(JSON.stringify(msg) + "\n");
}

function handleRequest(req: { id: string | number; method: string; params?: unknown }) {
  // 2. respond to handshake
  if (req.method === "host/ready") {
    send({
      jsonrpc: "2.0",
      id: req.id,
      result: { ready: true, manifestVersion: "v1" },
    });
    return;
  }
  // unknown method
  send({
    jsonrpc: "2.0",
    id: req.id,
    error: { code: -32601, message: "Method not found" },
  });
}

// 3. signal ready
send({ jsonrpc: "2.0", method: "worker/ready", params: { manifestVersion: "v1" } });

process.on("SIGTERM", () => process.exit(0));
process.on("SIGINT", () => process.exit(0));
```

### 2.3 安装插件

```bash
# 1. 打包插件（必须包含 manifest.json 和 entry 路径）
tar czf hello-plugin.tgz manifest.json dist/

# 2. 通过 paperclipai CLI 安装
paperclipai plugin install hello-plugin.tgz

# 3. 或者把 tgz 放到 plugins/ 目录，自动发现
cp hello-plugin.tgz ~/.paperclip/plugins/

# 4. 重启 server 加载（开发模式热重载可用）
systemctl restart paperclip-server
```

## 3. Manifest 完整字段

```typescript
interface PluginManifestV1 {
  // 必需
  id: string;                   // 唯一 ID，反向 DNS 命名（com.example.foo）
  version: string;              // semver
  manifestVersion: "v1";        // 必须是 "v1"
  label: string;                // UI 显示名
  description: string;          // 一句话描述
  entry: string;                // 入口文件路径（相对于包根）

  // 可选
  author?: {
    name: string;
    email?: string;
    url?: string;
  };

  capabilities: Array<{
    kind: "jobs" | "events" | "data" | "actions" | "tools" | "webhooks" | "ui" | "external_objects" | "environments" | "access";
    requires?: string[];        // 依赖的其他 capability
  }>;

  configSchema: object;         // JSON Schema for plugin instance config

  uiContributions: Array<{
    kind: "sidebar" | "panel" | "modal" | "page";
    entry: string;              // 子入口路径
    label?: string;
    metadata?: object;
  }>;

  metadata: object;             // 任意自定义元数据

  localFolders: Array<{
    folderKey: string;          // 插件内唯一
    displayName: string;
    description?: string;
    access?: "read" | "read_write";  // 默认 "read_write"
    requiredDirectories?: string[]; // 相对路径
    requiredFiles?: string[];
  }>;
}
```

## 4. JSON-RPC 协议

### 4.1 协议版本

`manifestVersion: "v1"` 对应 JSON-RPC 2.0 over stdio。

### 4.2 消息格式

```typescript
// Request (host → worker)
{
  "jsonrpc": "2.0",
  "id": "req-123",
  "method": "host/issue/created",
  "params": { ... }
}

// Response (worker → host)
{
  "jsonrpc": "2.0",
  "id": "req-123",
  "result": { ... }
}

// Notification (host → worker, no response)
{
  "jsonrpc": "2.0",
  "method": "host/event",
  "params": { type: "issue.created", payload: { ... } }
}
```

### 4.3 内置方法（host → worker）

| Method | 说明 |
|---|---|
| `host/ready` | host 询问 worker 是否就绪 |
| `host/issue.created` | issue 创建事件 |
| `host/issue.updated` | issue 更新事件 |
| `host/agent.run.completed` | agent 心跳运行完成 |
| `host/event` | 通用事件通道（payload 自定义） |
| `host/config.update` | host 推送配置变更 |
| `host/shutdown` | 优雅关闭请求 |

### 4.4 内置方法（worker → host）

| Method | 说明 |
|---|---|
| `worker/ready` | worker 启动后通知 host |
| `worker/log` | 写日志（host 转发到 tracing） |
| `worker/state.read` | 读持久状态 |
| `worker/state.write` | 写持久状态 |
| `worker/tool.invoke` | 调用 host 工具 |
| `worker/webhook.send` | 发送 webhook |
| `worker/job.schedule` | 调度定时作业 |
| `worker/job.cancel` | 取消作业 |
| `worker/external_object.publish` | 发布外部对象 |
| `worker/ui.action` | UI 行为上报 |

## 5. Capabilities 详解

### 5.1 jobs（定时作业）

```json
{
  "kind": "jobs",
  "requires": []
}
```

worker 可调用 `worker/job.schedule`：

```typescript
send({
  jsonrpc: "2.0",
  id: "req-1",
  method: "worker/job.schedule",
  params: {
    name: "daily-cleanup",
    schedule: "0 2 * * *",   // cron
    payload: { type: "cleanup" },
  },
});
```

### 5.2 events（订阅事件）

```json
{ "kind": "events" }
```

host 推 `host/event`，worker 应答：

```typescript
send({
  jsonrpc: "2.0",
  id: req.id,
  result: { handled: true },
});
```

### 5.3 tools（暴露工具给 agent）

```json
{ "kind": "tools" }
```

worker 接收 `tool/invoke`：

```typescript
if (req.method === "tool/invoke") {
  const { toolName, input } = req.params;
  if (toolName === "hello") {
    send({ jsonrpc: "2.0", id: req.id, result: { greeting: `Hi ${input.name}` } });
  }
}
```

agent 调用时：

```yaml
# 在 issue prompt 中
use tool hello with { name: "world" }
```

### 5.4 webhooks

```json
{ "kind": "webhooks" }
```

```typescript
send({
  jsonrpc: "2.0",
  id: "req-1",
  method: "worker/webhook.send",
  params: {
    url: "https://example.com/hook",
    method: "POST",
    body: { event: "issue.created", id: "ISS-123" },
    headers: { "X-Custom": "value" },
  },
});
```

host 会异步发送并记录到 `plugin_webhooks` 表。

### 5.5 ui（贡献 UI）

```json
{ "kind": "ui", "uiContributions": [
  { "kind": "sidebar", "entry": "./dist/sidebar.js", "label": "My Panel" }
]}
```

UI 通过 `pc-plugin-ui-static` crate 静态服务插件的 bundle。

### 5.6 data（数据库视图）

```json
{ "kind": "data" }
```

worker 可通过受限的数据库视图查询（仅暴露给插件的视图，不直接访问原始表）。

### 5.7 external_objects / environments / actions / access

其他 capability 类型，类似模式；详见 `crates/pc-plugin-protocol/src/types.rs`。

## 6. 状态持久化

```typescript
// 写
send({
  jsonrpc: "2.0",
  id: "req-1",
  method: "worker/state.write",
  params: {
    key: "user:123:last-seen",
    value: { ts: Date.now() },
    scope: "plugin",   // "plugin" | "company" | "global"
  },
});

// 读
send({
  jsonrpc: "2.0",
  id: "req-2",
  method: "worker/state.read",
  params: { key: "user:123:last-seen", scope: "plugin" },
});
```

状态存储在 `plugin_state` 表，host 保证原子性。

## 7. 日志

```typescript
send({
  jsonrpc: "2.0",
  method: "worker/log",
  params: { level: "info", msg: "Plugin started", fields: { version: "1.0.0" } },
});
```

host 将日志转发到 `tracing`（生产 JSON 格式，开发 pretty 格式）。

## 8. 错误处理

```typescript
// 协议错误
send({
  jsonrpc: "2.0",
  id: req.id,
  error: {
    code: -32603,        // 标准 JSON-RPC 错误码
    message: "Internal error",
    data: { detail: "..." },
  },
});

// 业务错误（host 定义）
// -32001: Unauthorized
// -32002: Not found
// -32003: Conflict
// -32004: Validation failed
// -32005: Rate limited
// -32006: Capability not granted
// -32007: Plugin instance disabled
// -32008: Plugin timed out
```

完整错误码：`crates/pc-plugin-protocol/src/error_codes.rs`。

## 9. 生命周期

```
spawn → ready → (running) → shutdown → exit
   │         │                  │          │
   │         │                  │          └── SIGTERM → exit 0
   │         │                  └── host/shutdown → cleanup → exit
   │         └── worker/ready → host/ready → running
   └── manifest validated → spawn
```

- spawn: host 启动 worker 子进程（`node ./dist/index.js`）
- ready: 双向握手（host/ready + worker/ready）
- running: 长期运行
- shutdown: 收到 SIGTERM，30s 内退出则正常，否则 SIGKILL

## 10. 测试

### 10.1 单元测试

```typescript
import { PluginHarness } from "@paperclipai/plugin-sdk/test";

const harness = new PluginHarness("./dist/index.js");
await harness.start();

const res = await harness.call("worker/state.read", { key: "x" });
expect(res.result).toBeNull();

await harness.stop();
```

### 10.2 集成测试（host）

```bash
# 启动 host with 测试 plugin
PAPERCLIP_PLUGIN_PATH=./test-plugin.tgz \
  cargo run -p pc-server -- --plugins-dir ./test-plugins/

# 触发事件
curl -X POST http://localhost:8080/api/issues \
  -H "Content-Type: application/json" \
  -d '{"title": "test"}'

# 验证 plugin worker 收到事件
# (检查 pc-plugin-host 日志)
```

### 10.3 e2e 测试

```typescript
// @paperclipai/plugin-sdk/test/e2e
import { fullStackHarness } from "@paperclipai/plugin-sdk/test";

test("plugin handles issue.created", async () => {
  const stack = await fullStackHarness.start({
    plugin: "./my-plugin.tgz",
  });
  
  await stack.api.createIssue({ title: "test" });
  
  // 等 plugin 处理
  await stack.waitForPluginEvent("issue.created", { timeout: 5000 });
  
  await stack.stop();
});
```

## 11. 最佳实践

### 11.1 优雅关闭

```typescript
let shuttingDown = false;

process.on("SIGTERM", async () => {
  if (shuttingDown) return;
  shuttingDown = true;
  await flushPendingJobs();   // 等正在处理的作业完成
  process.exit(0);
});
```

### 11.2 资源限制

- 内存：worker 进程应 < 500MB
- 文件描述符：< 1000
- CPU：单次方法调用 < 30s（host 默认超时）

### 11.3 错误恢复

```typescript
async function callHost(method: string, params: unknown) {
  for (let i = 0; i < 3; i++) {
    try {
      return await send(method, params);
    } catch (e) {
      if (i === 2) throw e;
      await sleep(100 * (i + 1));
    }
  }
}
```

### 11.4 状态设计

- 状态 key 用 `:` 分层（`user:123:settings`）
- 频繁写的小状态 → in-memory cache + periodic flush
- 大状态（>1MB）→ 存文件系统，state store 只存引用

## 12. 调试

### 12.1 启用 plugin debug 日志

```bash
RUST_LOG=paperclip_plugin=debug paperclip-server
```

### 12.2 直接运行 worker

```bash
# 1. 拿 manifest 的 entry
ENTRY=$(jq -r .entry manifest.json)

# 2. 设置 host fake stdin（需要 plugin-sdk debug tool）
npx @paperclipai/plugin-sdk run $ENTRY
```

### 12.3 追踪 IPC 流量

```bash
# 用 socat 代理 stdio
socat - EXEC:"node ./dist/index.js",stderr PTY,rawer
```

## 13. 真实示例

参见 `crates/pc-plugin-host/tests/` 下的完整 plugin 实例：

- `tests/fixtures/hello-plugin/` — 最小示例
- `tests/fixtures/event-handler/` — 订阅 issue 事件
- `tests/fixtures/tool-plugin/` — 暴露工具给 agent
- `tests/fixtures/job-scheduler/` — 定时作业

## 14. 常见问题

### Q1: worker 启动后立即退出？

- 检查 `worker/ready` 是否发送
- 检查 manifest 的 `entry` 路径是否正确
- 看 host 日志：`paperclip_plugin::supervisor`

### Q2: 事件没收到？

- 确认 capability 包含 `events`
- 确认事件类型拼写正确（`issue.created` vs `issues.created`）

### Q3: state.write 失败？

- key 长度 ≤ 256 字符
- value JSON 序列化 ≤ 1MB
- scope 必须存在
