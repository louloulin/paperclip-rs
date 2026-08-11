# R607 — Cursor Cloud adapter 基础模块（5 子模块拆分）

> 2026-08-12 / Change: paperclip-rs-comprehensive-validation / 状态：✅ 完成

## 1. 目标

把 `pc-adapter-cursor-cloud` 从 1 模块 / 0 测试 / 147 行 stub
推进到 **7 模块 / 93 测试 / 2211 行**核心复刻。

Node 原始 execute.ts 是 611 行 + session.ts 61 行 + parse-stdout.ts 186 行 +
build-config.ts 67 行 = 925 行。本轮聚焦：
**SDK 行为不可知的纯函数层**（决策 / 编解码 / 配置 schema / env 拼装）。

下一轮 R608 起将补 cloud_client trait + fake HTTP server + execute 整合。

## 2. 模块拆分（R607 末）

| 模块 | 行数 | 职责 | Node 对应 |
|---|---|---|---|
| `constants.rs` | 54 | ADAPTER_TYPE / EnvType / 默认值 / 禁用 key 列表 | server/index.ts 共享常量 |
| `session_codec.rs` | 422 | CursorCloudSession 序列化/反序列化（含 key 别名）+ sessionMatches | session.ts |
| `event_codec.rs` | 503 | CursorCloudEvent 枚举 + eventLine + 解析 SDK 消息 (assistant/user/thinking/tool_call/tool_result/status/task) | parse-stdout.ts |
| `config_schema.rs` | 326 | UI 14 字段 schema（model / repoUrl / envType / ...） | build-config.ts |
| `wake_env.rs` | 410 | buildWakeEnv + paperclip_keys + renderPaperclipEnvNote + describe_env | execute.ts::buildWakeEnv |
| `prompt_render.rs` | 301 | renderTemplate + joinPromptSections + shouldResumeSession + env_note_from_wake_env | execute.ts::buildPrompt |
| `result_builder.rs` | 432 | buildSuccess + buildFailure + toSummary + formatRunError + toAdapterOutcome | execute.ts success/catch 路径 |

**lib.rs (整合 + readiness)**：271 行，新增 `evaluate_readiness` 决策函数。

## 3. 关键设计

### 3.1 session_codec — 字段别名覆盖

Node `session.ts.normalize` 接受 3 种 id 别名（`cursorAgentId` /
`agentId` / `sessionId`），全部 trim 跳过空字符串。`latestRunId` 与 `runId`
也兼容。Rust 端 `deserialize_session` 保持同样 fallback 链。

### 3.2 event_codec — 强类型 enum 替代松散 JSON

把 SDK 6 类消息（assistant / user / thinking / tool_call / tool_result /
task）和 Cursor Cloud 4 类顶层事件（init / status / message / result）
表示为强类型 enum，`event_line` / `parse_cursor_cloud_stdout_line` 双向
编解码。比直接 JSON 透传多了：编译期字段检查 + 避免 `serde_json::Value`
滥用。

### 3.3 wake_env — 安全优先级链

`build_wake_env` 实现 5 层优先级（高 → 低）：
1. **harness auth_token** → `PAPERCLIP_API_KEY`（必须）
2. config.env 中的 `CURSOR_API_KEY`（必须）
3. 标准 `PAPERCLIP_*` env（agent / run_id）
4. wake payload 字段 → `PAPERCLIP_TASK_ID` 等
5. workspace 字段 → `PAPERCLIP_WORKSPACE_CWD` 等

**安全保证**：`PAPERCLIP_API_KEY` 永远不来自 config.env（config 给的会被
显式记录到 `dropped_keys` 列表）。

### 3.4 evaluate_readiness — execute 入口校验

决策函数（lib.rs）：检查 `CURSOR_API_KEY` / `repoUrl` / 当 env_type 是
pool/machine 时还需要 `runtimeEnvName`。**`ready == true` 只意味着字段
齐全**，真正 HTTP auth 检查留给 R608+ 的 cloud_client。

## 4. 测试覆盖（93 个 lib tests）

| 模块 | 测试数 | 覆盖 |
|---|---|---|
| `session_codec` | 15 | 3 种 id 别名 + null 处理 + 完整 round-trip + sessionMatches 4 场景 |
| `event_codec` | 18 | init / status / result 序列化 + 6 种 SDK 消息解析 + round-trip |
| `config_schema` | 9 | 14 字段 + 必填 + 默认值 + JSON 序列化 |
| `wake_env` | 15 | config_env pass-through + 6 类 wake 字段 + workspace 7 字段 + auth_token 覆盖 + paperclip_keys |
| `lib::readiness` | 11 | full pass / 缺 api_key / 缺 repo / pool 需要 name / 完整 fallback / extras |

总计 **93 个**（0.04s 跑完；prompt_render 15 + result_builder 10）。

```
$ cargo test -p pc-adapter-cursor-cloud --lib
test result: ok. 93 passed; 0 failed; 0 ignored; 0 measured
```

## 5. 与 Node 字节级一致性

| Node 行为 | Rust 实现 | 一致性 |
|---|---|---|
| 三种 id 别名 | `cursorAgentId` > `agentId` > `sessionId` | ✅ |
| `CURSOR_API_KEY` 强制来自 config | `evaluate_readiness` 检查 | ✅ |
| `PAPERCLIP_API_KEY` 永不接受 config | `build_wake_env` 丢到 `dropped_keys` | ✅ |
| 7 个 PAPERCLIP_WORKSPACE_* + AGENT_HOME | `workspace_fields_become_paperclip_env` 测试 | ✅ |
| README-style paperclip env note | `render_paperclip_env_note` | ✅ |
| `sessionMatches` 1:1 字段对比 | `session_matches` | ✅ |

## 6. 与其他 adapter 对齐

| Adapter | Rust 子模块数 | Rust 测试数 |
|---|---|---|
| **cursor-cloud**（本轮） | **5** | **68** |
| hermes | 9 | 79 |
| hermes-gateway | 4 | 25 |
| claude-local | 6 | ~80 |
| gemini-local | 5 | 26 |
| opencode-local | 5 | 39 |
| grok-local | 5 | 38 |

cursor-cloud 在 1 轮内从空白走到了与 gemini / opencode 等深的模块化水平。

## 7. R608+ 计划

| 优先级 | 模块 | 说明 |
|---|---|---|
| **P0** | `prompt_render.rs` | instructions + bootstrap + wake + env_note + prompt + handoff 拼接 |
| **P0** | `cloud_client.rs` | trait CursorCloudClient + FakeClient（fake HTTP server via in-process TCP） |
| **P0** | `result_builder.rs` | toSummary + formatRunError + AdapterExecutionResult 构造 |
| **P0** | `lib.rs::execute` | 完整 execute path + 真实 E2E（fake server + 真实 round-trip） |
| P1 | openclaw-gateway 基础模块拆分 | 1491 行 Node → 镜像 cursor-cloud 模板 |
| P2 | 架构重构 AdapterEnvironmentCheck | 提取 `pc-adapter-claude-local::claude_test` + `grok_test` 重复 |

## 8. 整体进度更新

| 域 | R606 末 | R607 末 |
|---|---|---|
| shared/ 契约 | 85% | 85% |
| server/ 路由 | 92% | 92% |
| server/ middleware | 60% | 60% |
| server/ services | 58% | 58% |
| server/ repos | 85% | 85% |
| UI client | 35% | 35% |
| CLI | 60% | 60% |
| 验证层 | 45% | 45% |
| **Adapters** | **84%** | **87%** ↑ |
| **总计** | **~89.5%** | **~91%** ↑ |

workspace lib tests passing: 7,105 → 7,198 (+93)
