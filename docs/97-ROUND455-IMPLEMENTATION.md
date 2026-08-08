# R455 — PC-HTTP executionTarget 注入

## 目标

打通 `pc-http` → `AdapterExecutionContext` → adapter 远程执行路径。需要：
1. 在 `pc-adapter-api` 的 `AdapterExecutionContext` 加 `execution_target` 字段
2. 在 `pc-http` `routes/agents.rs` 构建 `ExecutionContext` 时从 `agent_config` + `runtime_config` 注入
3. 不引入 `pc-adapter-api` → `pc-acpx` 循环依赖（用 JSON 形式存储）

### 三大设计目标

1. **零循环依赖**：`pc-adapter-api` 存 `execution_target` 为 `serde_json::Value`，避免依赖 `pc-acpx::AdapterExecutionTarget` 类型
2. **fallback 链保留**：`runtime_config.executionTarget` > `adapter_config.executionTarget` > legacy `remoteExecution`，对齐 Node `readAdapterExecutionTarget`
3. **解耦消费**：adapter 端需要时通过 `pc_acpx::execution_target` 模块自行 `serde_json::from_value`，不破坏既有接口

---

## `AdapterExecutionContext` 字段新增

```rust
pub struct AdapterExecutionContext {
    pub run_id: Uuid,
    pub agent_id: Uuid,
    pub prompt: String,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
    pub session_id: Option<String>,
    pub session_params: Option<serde_json::Value>,
    pub adapter_config: serde_json::Value,
    pub runtime_config: serde_json::Value,
    /// 执行目标（local / ssh / sandbox），由 route 层从 agent config 解析并
    /// 注入。adapter 端需要 ssh/sandbox 行为时通过 `pc_acpx::execution_target`
    /// 下的 `AdapterExecutionTarget` 解码。存为 JSON 以避免 pc-adapter-api
    /// 与 pc-acpx 形成循环依赖。
    pub execution_target: Option<serde_json::Value>,
    pub cancellation: CancellationToken,
}
```

附带 fluent API：
```rust
impl AdapterExecutionContext {
    pub fn with_execution_target(mut self, target: serde_json::Value) -> Self { ... }
    pub fn execution_target_json(&self) -> Option<&serde_json::Value> { ... }
}
```

---

## `resolve_execution_target_for_agent` 助手

```rust
pub fn resolve_execution_target_for_agent(
    adapter_config: &serde_json::Value,
    runtime_config: &serde_json::Value,
) -> Option<serde_json::Value> {
    use pc_acpx::execution_target::{
        is_adapter_execution_target_instance, read_adapter_execution_target,
    };
    let from_cfg = adapter_config.get("executionTarget");
    let from_rt = runtime_config.get("executionTarget");
    let from_legacy = runtime_config.get("remoteExecution");

    // 1. 已类型化实例（typed instance）优先
    if let Some(v) = from_cfg {
        if is_adapter_execution_target_instance(v) {
            return Some(v.clone());
        }
    }
    if let Some(v) = from_rt {
        if is_adapter_execution_target_instance(v) {
            return Some(v.clone());
        }
    }
    // 2. 解析（runtime_config > adapter_config）
    let parsed = read_adapter_execution_target(from_rt, from_legacy)
        .or_else(|| read_adapter_execution_target(from_cfg, from_legacy));
    parsed.map(|target| serde_json::to_value(target).unwrap_or(serde_json::Value::Null))
}
```

对齐 Node `readAdapterExecutionTarget` 三段优先级：
1. **typed instance**：`is_adapter_execution_target_instance(v)` 直接 clone
2. **parsed JSON**：`read_adapter_execution_target` 解析，`kind` 字段决定类型
3. **legacy remoteExecution**：`adapter_execution_target_from_remote_execution`

---

## 注入现场

```rust
// crates/pc-http/src/routes/agents.rs
execution_context.adapter_config = agent.adapter_config.clone();
execution_context.runtime_config = agent.runtime_config.clone();
// ... env 注入 ...
execution_context.execution_target =
    resolve_execution_target_for_agent(&agent.adapter_config, &agent.runtime_config);
```

`resolve_execution_target_for_agent` 在 `routes/agents.rs` 末尾，是 `pub` 助手，便于其他路由模块复用。

---

## 测试覆盖（7 个新增）

### `resolve_execution_target_for_agent` 单元
- 无 source → None
- 来自 `adapter_config.executionTarget` → local
- 来自 `runtime_config.executionTarget` → local
- 双 source 都存在时 adapter_config 胜（typed instance 优先）
- legacy `remoteExecution` fallback → SSH remote
- 非法输入（字符串 / 数字）→ None
- typed instance 短路：直接 clone

---

## 文件清单

- **修改**：`crates/pc-adapter-api/src/lib.rs`（新增 `execution_target` 字段 + 2 个 API）
- **修改**：`crates/pc-http/src/routes/agents.rs`（注入点 + 助手 + 7 个测试）
- **修改**：`crates/pc-http/Cargo.toml`（添加 `pc-acpx` 依赖）

## 测试结果

```
pc-http: 236 passed (229 prior + 7 new)
pc-acpx: 883 passed
pc-adapter-codex-local: 260 passed
pc-adapter-claude-local: 153 passed
pc-adapter-process: 6 passed
pc-activity: 14 passed
pc-adapter-quota: 39 passed (上次验证)
合计: 1591 passed (was 1584, +7)
```

---

## 后续 R456-R459

- **R456** 其他 adapter（按用户约束延后）
- **R457** quota.ts 完整复刻
- **R458** test.ts 完整复刻
- **R459** pc-repos / pc-heartbeat 深化

## 当前差距

| 维度 | 已经实现 | 后续 |
|---|---|---|
| codex 适配器 | ~98% | （接近完成） |
| claude 适配器 | ~92% | （优先其他） |
| pc-acpx 核心 | ~95% | （少量边界） |
| **pc-http** routes | **~96%** | R456 |
| quota / heartbeat | ~85% | R457 |
| 其他 adapter | 0% | R456（延后） |

## 关键设计权衡

1. **JSON 存 vs 强类型**：`pc-adapter-api` 是 leaf crate（无 `pc-acpx` 等依赖），强类型会引入循环依赖。JSON 形式 + 自描述字段让 adapter 端自主消费（`serde_json::from_value`）。
2. **typed instance 优先**：保留 Node 的两级 fallback 语义——以 `is_adapter_execution_target_instance` 区分「已经是 typed instance」与「需要 JSON 解析」。
3. **三段优先级**：typed instance → parsed JSON → legacy remoteExecution。adapter_config 在 typed instance 短路中先于 runtime_config（与 Node 一致）。
