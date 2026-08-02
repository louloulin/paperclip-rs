# kameo Actor 架构分析与路由优化计划

> 日期：2026-08-03 · 涉及 crate：pc-core / pc-heartbeat / pc-server

## 一、kameo 是什么

[louloulin/kameo](https://github.com/louloulin/kameo) 是一个 Rust actor 框架，提供：
- `Actor` trait：定义 actor 生命周期（`pre_start`/`post_stop`）
- `ActorRef<A>`：类型安全的 actor 引用，支持 `send`/`ask` 通信
- `Spawn` trait：通过 spawn 创建 supervised actor
- `Message` trait：异步消息分发（支持 `async_trait`）
- `WeakActorRef<A>`：弱引用（不阻止 actor 停止）
- 优雅关闭：`stop_gracefully()`

当前工作区使用 `kameo = 0.22`。

## 二、pc-core 中的 Actor 抽象层

### 2.1 `pc_core::actor_runtime::ActorRegistry`

```rust
// crates/pc-core/src/actor_runtime.rs (371 行)

/// 进程内 Actor 注册表。
#[derive(Clone, Default)]
pub struct ActorRegistry {
    actors: Arc<Mutex<HashMap<ActorKey, RegisteredActor>>>,
}

/// Actor 的稳定业务身份（不暴露 kameo 内部分配的 ActorId）。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ActorKey {
    pub kind: String,     // e.g. "heartbeat_run", "plugin_worker"
    pub id: String,       // e.g. heartbeat_run_id or plugin_id
}
```

关键方法：

| 方法 | 功能 |
|---|---|
| `register<A: Actor>(&self, key, ref)` | 注册一个 actor，检查 Alive |
| `get<A: Actor>(&self, key)` | 按 key 获取类型安全的 ActorRef，向下转型校验 |
| `unregister(&self, key)` | 移除注册项 |
| `shutdown(&self)` | 遍历所有注册项 → stop_gracefully |
| `is_empty(&self)` | 检查是否为空 |

设计哲学：
- 业务层不直接依赖 kameo（通过 `kameo_api` 重导出子模块）
- 类型擦除的 `RegisteredActor` 存储带 `dy Any` 的 ActorRef
- 边界校验：`get<A>` 通过 `downcast_ref::<ActorRef<A>>` 确保类型匹配

### 2.2 `pc_core::actor::Actor`

```rust
// crates/pc-core/src/actor.rs
pub enum Actor {
    User { id: String },
    Agent { id: Uuid },
    System,
}
```

**注意**：这是业务领域的 `Actor`（"主体"），**不是** kameo 的 `Actor` trait。命名存在潜在混淆（文档已注明）。

### 2.3 `pc_core::DomainMessage`

```rust
/// 领域消息：携带 actor 主体身份。
pub struct DomainMessage<M> {
    pub origin: MessageOrigin,
    pub payload: M,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}
```

### 2.4 当前使用情况

```bash
$ rg "ActorRegistry|ActorKey|spawn_" paperclip-rs/crates/*/src/ -l
pc-core/src/lib.rs           ← 定义
pc-core/src/actor_runtime.rs  ← 核心实现
pc-heartbeat/src/lib.rs       ← 心跳 actor（基于 kameo）
pc-http/src/state.rs          ← 注入 AppState
pc-server/src/main.rs         ← composition root
```

**核心观察**：`ActorRegistry` 是一种**间接层**（facade），它内部的 `RegisteredActor` 用 `Arc<dyn Any + Send + Sync>` 擦除具体类型，然后在 `get<A>` 时做 `downcast_ref`。这种模式类似于 Java 的 `ServiceRegistry<ActorInterface>` → `get(MyActor.class)`。

## 三、Actor 使用模式分析

### 3.1 当前 heartbeat actor

```rust
// pc-heartbeat/src/lib.rs — 每个 heartbeat_run 是一个 kameo Actor
#[derive(Debug)]
pub struct HeartbeatActor {
    run_id: Uuid,
    state: HeartbeatRunState,
    adapter: Arc<dyn Adapter + Send + Sync>,
    realtime: RealtimeHandle,
    repo: Arc<HeartbeatRepo>,
}

impl kameo::actor::Actor for HeartbeatActor {
    type Mailbox = kameo::actor::UnboundedMailbox<Self>;
    fn on_start(&mut self) -> impl Future<Output = Result<(), kameo::error::BoxError>> + Send {
        async {
            // 启动心跳 tick 循环
            Ok(())
        }
    }
    fn on_stop(&mut self, reason: ActorStopReason) -> impl Future<Output = Result<(), kameo::error::BoxError>> + Send {
        async {
            // 持久化 final state，发布 live-event
            Ok(())
        }
    }
}
```

模式：actor **按 ID 隔离**（一个 heartbeat run → 一个 actor）

### 3.2 建议扩展的 actor 模式

| 领域 | Actor 类型 | 生命周期 | 隔离键 |
|---|---|---|---|
| 心跳 | HeartbeatActor | 一次 agent run | `kind="heartbeat_run" id=<run_id>` |
| 插件 worker | PluginWorkerActor | 插件生命周期 | `kind="plugin_worker" id=<plugin_id>` |
| WebSocket 连接 | WsConnectionActor | WS 连接生命周期 | `kind="ws_conn" id=<conn_id>"` |
| 适配器 bridge | AdapterBridgeActor | per-adapter-config | `kind="adapter" id=<config_id>"` |
| Routine 执行 | RoutineRunActor | 一次 routine run | `kind="routine_run" id=<run_id>` |
| Pipeline 执行 | PipelineRunActor | 一次 pipeline run | `kind="pipeline_run" id=<run_id>` |
| GC / Cleanup | HousekeepingActor | 全局单例 | `kind="system" id="housekeeping"` |

### 3.3 关键设计约束

- **ActorRef 获取只通过 registry → 不存在裸 kameo 引用泄露到路由层**
- **每次 `get<A>` 都要做一次 downcast_ref 校验，失败 → TypeMismatch**
- **Actor shutdown 必须在 registry 层面做（保证 stop handle 可追踪）**

## 四、当前的局限与优化方向

### 4.1 当前局限

1. **kameo 版本锁定在 0.22**：若 0.23+ 有 API 变更，`ActorRegistry` 的 facade 仍可隔离变更
2. **注册表是 `Mutex<HashMap>`**：适合低频注册/注销（当前 pattern）；高频查询（每请求）性能 OK
3. **`get<A>` 的 downcast_ref 是 O(1) 指针比较**：快但类型安全靠运行时
4. **重启逻辑缺失**：当前 actor 停止后不自动重启；需要 supervisor 层
5. **多实例部署**：单进程 actor 不适合跨实例通信 → 应走 PostgreSQL + polling

### 4.2 优化路线（Phase A 后执行）

| 优化 | 动作 | 优先级 |
|---|---|---|
| Supervisor + restart policy | 在 `pc-core` 加 `Supervisor` trait，heartbeat/plugin worker 实现 | P1 |
| Actor 状态持久化 | 心跳 actor 把状态 checkpoint 到 DB，崩溃重启后恢复 | P1 |
| 注册表锁优化 | 当前 `Mutex<HashMap>` 对重负载 OK；若性能瓶颈 → `dashmap` | P2 |
| kameo multi-node | future：引入 dist-kv + network bridge（不是当前目标） | P3 |

## 五、路由计划 — 按 Actor 改造剩余模块

### 5.1 tools（工具锁 → ToolInvocationActor）

当前 `tool_access.rs` 的 items 仍返回 `[]`，且有 OAuth 流需要异步等。

计划：

```rust
/// 每个 tool invocation 是一个 actor，主持 OAuth flow + 断线重连生命周期
struct ToolInvocationActor {
    invocation_id: Uuid,
    oauth_state: Option<OAuthPendingState>,
    provider: Box<dyn ToolProvider + Send>,
}
impl Actor for ToolInvocationActor { ... }
impl Handler<StartOAuth> for ToolInvocationActor { ... }
impl Handler<CompleteOAuth> for ToolInvocationActor { ... }
```

### 5.2 secrets rotate → KeyRotationActor

```rust
struct KeyRotationActor {
    secret_id: Uuid,
    providers: Vec<Box<dyn SecretProvider + Send>>,
}
impl Actor for KeyRotationActor { ... }
impl Handler<RotateKey> for KeyRotationActor { ... }
```

### 5.3 适配器 → AdapterBridgeActor

每台适配器配置创建一个 actor，接管 stdio 子进程生命周期 + 重启。

```rust
struct AdapterBridgeActor {
    config: AdapterConfig,
    child: Option<tokio::process::Child>,
}
impl Actor for AdapterBridgeActor { ... }
impl Handler<InvokeCommand> for AdapterBridgeActor { ... }
```

### 5.4 Plugin Worker → PluginWorkerActor

```rust
struct PluginWorkerActor {
    plugin_id: Uuid,
    worker: Option<tokio::process::Child>,
    rx: broadcast::Receiver<Arc<LiveEvent>>,
}
impl Actor for PluginWorkerActor { ... }
```

## 六、测试策略

- `pc-core/src/actor_runtime.rs` 已有测试（register / get / unregister / shutdown）
- 每个实现了 kameo Actor 的 crate 需要有：
    1. unit test：消息 handler 逻辑
    2. integration test：actor spawn → send message → assert → stop
    3. 回归测试：类型擦除后 downcast 仍正确
- 在所有 `get<A>` 调用点加 `unwrap_or_else(|| panic!("wrong type for key"))`，把类型不匹配变为不可恢复的错误（安全选择）

