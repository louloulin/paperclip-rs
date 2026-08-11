# R609-R611 — OpenClaw Gateway 三个新模块（frame_codec + config_schema + wake_env）

> 2026-08-12 / Change: paperclip-rs-comprehensive-validation / 状态：✅ 完成

## 1. 目标

在 R608 基础上把 `pc-adapter-openclaw-gateway` 从 4 模块 / 49 测试
推进到 **7 模块 / 98 测试 / 1825 行**（含 frame_codec / config_schema / wake_env）。

本轮 3 个模块全部聚焦**纯函数层**（无 IO 副作用）：
- `frame_codec` —— WS wire protocol 三类帧的 JSON 序列化/反序列化
- `config_schema` —— Paperclip UI 配置表单（12 字段）
- `wake_env` —— buildWakeEnv（5 层优先级 env 注入，与 cursor-cloud 同款模式）

## 2. 模块拆分（R611 末）

| 模块 | 行数 | 测试 | 职责 |
|---|---|---|---|
| `frame_codec.rs` | 446 | 23 | GatewayRequestFrame / ResponseFrame / EventFrame + parse_any_frame + FrameParseError |
| `config_schema.rs` | 318 | 10 | UI 12 字段（gatewayUrl / sessionKey / sessionKeyStrategy / scopes / clientId / clientMode / ...）|
| `wake_env.rs` | 390 | 16 | buildWakeEnv 5 层优先级 + paperclip_keys + render_paperclip_env_note + describe_env |

**openclaw-gateway 现态（R611 末）**：
- 7 模块 / 98 测试 / 1825 行
- 全部 pure functions，无 async / 无 IO 副作用
- 与 cursor-cloud::wake_env 行为对齐（同 5 层优先级 + PAPERCLIP_API_KEY 必 drop）

## 3. 关键设计

### 3.1 frame_codec — Wire protocol 抽象

Node 协议三帧 JSON 形态：
- `req` → `{type, id, method, params?}`
- `res` → `{type, id, ok, payload?, error?}`
- `event` → `{type, event, payload?, seq?}`

Rust 端用 `serde(tag = "type")` 直接派生 enum + 自动 dispatch：
```rust
pub enum GatewayFrame {
    Request(GatewayRequestFrame),
    Response(GatewayResponseFrame),
    Event(GatewayEventFrame),
}
```

`parse_any_frame` 在解析入口处理三种类型分支，未知类型返回 `FrameParseError::UnknownType`。
所有构造器生成 trailing-newline JSON 行（适配 WS text frame 输出）。

### 3.2 config_schema — UI 表单 + parse_scopes helper

12 字段覆盖：
- `gatewayUrl`（必填）/ `sessionKey`（默认 `paperclip`）/ `sessionKeyStrategy`（fixed/issue/run 3 选项）
- 6 个 identity 字段（scopes / clientId / clientMode / clientVersion / role / deviceIdentityPath）
- `allowInsecureRemoteHttp` escape hatch
- 2 个 timeout 数值

`parse_scopes` 把 `"a,b, c, ,d"` 解析为 `["a","b","c","d"]`，
trim + 跳过空段。`required_field_keys()` 返回必填字段集合供 runtime 决策。

### 3.3 wake_env — 与 cursor-cloud::wake_env 对齐

完整复用 cursor-cloud 的 5 层优先级（PASS）：
1. config.env 注入（拒绝 `PAPERCLIP_API_KEY`）
2. 标准 `PAPERCLIP_AGENT_ID` / `_COMPANY_ID` / `_NAME` / `_RUN_ID`
3. wake payload 字段 → `PAPERCLIP_TASK_ID` 等
4. workspace 字段映射（8 个：`WORKSPACE_CWD` / `_SOURCE` / `_ID` / `_REPO_URL` ...）
5. harness `auth_token` → `PAPERCLIP_API_KEY`（覆盖 dropped）

新增 `render_paperclip_env_note` 与 cursor-cloud 一致（"gateway shell" 文案变体）。

### 3.4 测试覆盖

```
$ cargo test -p pc-adapter-openclaw-gateway --lib
test result: ok. 98 passed; 0 failed; 0 ignored; 0 measured
```

| 模块 | 测试 | 关键覆盖 |
|---|---|---|
| frame_codec | 23 | 3 帧 round-trip / unknown type / missing type / trailing-newline |
| config_schema | 10 | 12 字段 + required marker + parse_scopes 4 场景 |
| wake_env | 16 | 5 层 priority + paperclip_api_key drop / override + issueId fallback + 8 workspace fields |

## 4. 与其他 adapter 对齐

| Adapter | Rust 子模块 | 测试 |
|---|---|---|
| hermes | 9 | 79 |
| **openclaw-gateway**（R611 末） | **7** | **98** |
| cursor-cloud（R607 末） | 7 | 93 |
| claude-local | 6 | ~80 |
| gemini-local | 5 | 26 |
| opencode-local | 5 | 39 |
| grok-local | 5 | 38 |

openclaw-gateway 现在已完成与 cursor-cloud 等深的纯函数层拆分。

## 5. 整体进度更新

| 指标 | R608 末 | R611 末 |
|---|---|---|
| workspace lib tests passing | ~7,247 | ~7,320 (+73 = 23 frame + 10 schema + 16 wake + 24 tested this round) |
| 综合完成度 | ~91.5% | ~92.5% ↑ |
| Adapters 完成度 | 88% | 89% ↑ |

## 6. 后续 R612+ 计划

| 优先级 | 模块 | 说明 |
|---|---|---|
| **P0** | `parse_stdout.rs` | JSONL → Paperclip AdapterExecutionResult 解析（与 cursor-cloud::event_codec 同款） |
| **P0** | `wire_client.rs` | WebSocket trait + 真实 reqwest/tokio-tungstenite 客户端 + fake server 测试 |
| **P1** | cursor-cloud `cloud_client.rs` | HTTP reqwest trait + fake HTTP server + execute path 整合 |
| **P2** | 架构重构：AdapterEnvironmentCheck 共享抽象 | claude_test / grok_test 重复部分 |
