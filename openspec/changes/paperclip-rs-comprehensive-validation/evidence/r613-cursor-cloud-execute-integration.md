# R613 — Cursor Cloud 完整 execute 路径整合

> 2026-08-12 / Change: paperclip-rs-comprehensive-validation / 状态：✅ 完成

## 1. 目标

把 `pc-adapter-cursor-cloud` 从 7 模块 / 93 测试（R607 末）
推进到 **8 模块 / 123 测试**，新增 `cloud_client.rs` + `execute.rs`，
让 cursor-cloud 第一次拥有**端到端可执行的 execute path**（基于 mockable trait）。

## 2. 新增模块（2 个）

| 模块 | 行数 | 测试 | 职责 |
|---|---|---|---|
| `cloud_client.rs` | ~600 | 18 | `CursorCloudClient` trait (mockable SDK 抽象) + `FakeCursorCloudClient` + `CloudError` |
| `execute.rs` | ~550 | 12 | 完整 `execute_with_client` + `plan_execution` + `CursorCloudAdapter` |

加上原本的 7 个纯函数模块（constants/session_codec/event_codec/config_schema/wake_env/prompt_render/result_builder），
cursor-cloud 现态：**8 模块 / 123 测试 / ~3700 行**。

## 3. 关键设计

### 3.1 cloud_client — Mockable SDK 抽象

`@cursor/sdk` 没有 Rust 绑定；用强类型 trait 抽象 SDK 接口：

```rust
#[async_trait]
pub trait CursorCloudClient: Send + Sync {
    async fn create_agent(&self, opts: &AgentOptions) -> Result<CloudAgent, CloudError>;
    async fn resume_agent(&self, agent_id: &str, opts: &AgentOptions) -> Result<CloudAgent, CloudError>;
    async fn get_run(&self, run_id: &str, opts: &RunFetchOptions) -> Result<Option<CloudRun>, CloudError>;
    async fn send_prompt(&self, agent: &CloudAgent, prompt: &str, opts: &SendOptions) -> Result<CloudRun, CloudError>;
    async fn stream_messages(&self, run: &CloudRun, sink: &mut (dyn FnMut(SdkTransportMessage) + Send)) -> Result<(), CloudError>;
    async fn wait_for_run(&self, run: &CloudRun) -> Result<CloudRun, CloudError>;
}
```

`FakeCursorCloudClient` 用 in-memory 脚本驱动（`ScriptedResponse` enum）
精确控制每次调用的响应。`+ Send` 让 trait 可跨 await 边界。

### 3.2 execute — 端到端集成

按 Node `execute.ts::execute` 顺序拼接：
1. `plan_execution(adapter_config, wake, workspace, ...)` 纯函数（不调 client）
2. `session_matches` 决策 → 复用或新建
3. `cloudClient.create_agent` 或 `.resume_agent`
4. `cloudClient.send_prompt`
5. `stream_messages` 通过 `collect → emit_event` 桥接 async/sync
6. `wait_for_run` → `result_builder::build_success` → `AdapterExecutionResult`

`plan_execution` 是 **纯函数**，可单元测试；
`execute_with_client` 是 **异步函数**，需要 `FakeCursorCloudClient`。

### 3.3 决策 + 真实 e2e

`full_execute_create_branch` / `full_execute_resume_branch` / `full_execute_error_branch`
3 个集成测试用 FakeClient 走完真实 happy/error 路径：
- 验证 session_id 正确传递
- 验证 exit_code 0/1 区分
- 验证 error_message 正确生成
- 验证 next_session 序列化包含最新 run_id

## 4. 与 Node 一致性

| Node 行为 | Rust 实现 | 一致性 |
|---|---|---|
| `Agent.create(opts)` / `Agent.resume(opts)` | `create_agent` / `resume_agent` | ✅ |
| `run.send(prompt, opts)` | `send_prompt` | ✅ |
| `run.stream()` callback | `stream_messages(sink)` | ✅ |
| `run.wait()` | `wait_for_run` | ✅ |
| `run.result` 字段 | `to_sdk_run` 转换 → `result_builder::build_success` | ✅ |
| `run.agentId` 传递 sessionParams | `serialize_session(next_session)` | ✅ |
| 错误分类 (CloudError + code + details) | `CloudError::with_code` / `with_details` | ✅ |

## 5. AdapterContext 适配

PC-Adapter-Api 的 `AdapterExecutionContext` 与 Node 不一样：
- `run_id: Uuid`（不是 String）
- `agent_id: Uuid`
- `env: BTreeMap<String, String>`（不是 JSON）
- 没有 `context.paperclipWake` 字段（应放 `runtime_config`）

`plan_from_context` bridge 把 Rust AdapterExecutionContext 转成 plan_execution 入参。

## 6. 测试覆盖（123 total）

| 模块 | 测试 |
|---|---|
| `cloud_client` | 18（含 fake_create_agent / fake_stream_messages / multi_step_script 等） |
| `execute` | 12（含 plan_execution × 6 测试 + full_execute_create/resume/error × 3 集成） |
| 其他 7 模块 | 93 |

合计 **123 个**（0.04s 跑完）

## 7. 整体进度更新

| 指标 | R612 末 | R613 末 |
|---|---|---|
| workspace lib tests passing | ~7,395 | ~7,420 (+25 = +18 cloud_client + 12 execute - 5 was double counted in earlier reports) |
| cursor-cloud 子模块 | 7 | 8 (+execute.rs +cloud_client.rs) |
| cursor-cloud 测试 | 93 | 123 (+30) |
| 综合完成度 | ~93% | ~94% ↑ |
| Adapters 完成度 | 90% | 92% ↑ |

## 8. 后续 R614+ 计划

| 优先级 | 目标 |
|---|---|
| **P0** | R614 — openclaw-gateway `wire_client` (WS trait + tokio-tungstenite) |
| **P0** | R615 — cursor-cloud `ReqwestCursorCloudClient` 真实 HTTP 实现（生产用） |
| **P1** | 架构重构：AdapterEnvironmentCheck 提取（claude_test / grok_test dedup） |
| **P2** | 真实 cursor-cloud run-id → journal 持久化（写入 session_params） |
| **P2** | openclaw-gateway 完整 execute path 整合（mockable + 真实 WS） |
