# R608 — OpenClaw Gateway adapter 基础模块（4 子模块拆分）

> 2026-08-12 / Change: paperclip-rs-comprehensive-validation / 状态：✅ 完成（核心纯函数层）

## 1. 目标

把 `pc-adapter-openclaw-gateway` 从 1 模块 / 9 测试 / 147 行 stub
推进到 **4 模块 + lib / 49 测试**核心纯函数复刻。

Node 原始 execute.ts 是 1491 行单文件（最大 stub）。本轮聚焦**无副作用可独立测试**
的纯函数模块——wire protocol / 帧解析 / WebSocket 层留待 R609+。

## 2. 模块拆分（R608 末）

| 模块 | 行数 | 职责 | Node 对应 |
|---|---|---|---|
| `constants.rs` | 82 | PROTOCOL_VERSION / scopes / 默认值 / 敏感日志 key 列表 / 错误码分类 | execute.ts 顶部 const |
| `session_key.rs` | 272 | SessionKeyStrategy 枚举 + resolveSessionKey + agent: 前缀 | execute.ts::resolveSessionKey |
| `credentials.rs` | 293 | isSensitiveLogKey（多 token 匹配）+ redact_value + 设备身份 fingerprint | execute.ts::isSensitiveLogKey + createEphemeralDeviceIdentity |
| `host_security.rs` | 199 | isLoopbackHost + validate_gateway_url + escape hatch | execute.ts::isLoopbackHost + transport 校验 |
| `lib.rs` | 238 | 4 模块暴露 + 保留 stub execute（用 ProcessExecution 包装） | execute.ts 完整路径（后续 round） |

## 3. 关键设计

### 3.1 session_key — 3 策略解析

Node `normalizeSessionKeyStrategy` + `resolveSessionKey`：
- `fixed` → configuredSessionKey → DEFAULT_SESSION_KEY (`"paperclip"`)
- `issue` → issueId → configuredSessionKey → DEFAULT_SESSION_KEY
- `run`   → runId (优先)

`prefix_session_key_for_agent` 避免 sessionKey 跨 agent 串扰——已经在
`agent:` 前缀时不再添加。

### 3.2 credentials — 多 token 匹配算法

`is_sensitive_log_key` 4 阶段：
1. 整名命中（exact match in SENSITIVE_LOG_KEY_BRANCHES）
2. `x-openclaw-auth` / `x-openclaw-token` 特殊头
3. 分词后单 token 命中（`auth` / `password` 等）
4. 相邻 token 拼成复合形式（`api_key` / `api-key` / `apikey`）

✅ 处理输入：
- `my_auth_token`（命中 token `auth`）
- `api-key-value`（命中复合 `api_key`）
- `privatekey`（整名命中）
- `X-OpenClaw-Auth`（特殊）
- `user-password`（命中 token `password`）
- `totally_safe` → false（无误报）

### 3.3 host_security — transport 校验

与 Hermes-gateway (`pc-adapter-hermes-gateway::transport_security`)
同款模式：loopback 允许任何 scheme；远端 HTTP 拒绝除非 escape hatch
开启。

支持 12 种 escape hatch 别名（`true` / `yes` / `on` / `enabled` / `1` /
`allow` / `false` / `no` / `off` / `disabled` / `0` / `deny`）。

### 3.4 fingerprint_public_key — 零依赖实现

16 字符 SHA256-like fingerprint（取公钥前 8 字节 hex）。包含**纯 Rust
手写 base64url 解码**避免引入额外 crypto 依赖。失败时 fallback `"0000…0"`。

## 4. 测试覆盖（49 lib tests）

| 模块 | 测试数 | 覆盖 |
|---|---|---|
| `session_key` | 13 | 3 策略 × 4 场景（agent / no-agent / prefixed / no-prefix）+ 归一化 + 默认值 |
| `credentials` | 13 | 18 case 矩阵（auth/auth-orization/api-key/private-key/x-openclaw-*）+ redact_headers + fingerprint + summarize |
| `host_security` | 14 | loopback 6 host + 5 scheme 校验 + escape hatch 6 alias 启用/禁用 |
| `lib` | 9 | adapter_descriptor / default_command / parse_stdout（4 场景） |

合计 **49 个**。

```
$ cargo test -p pc-adapter-openclaw-gateway --lib
test result: ok. 49 passed; 0 failed; 0 ignored; 0 measured
```

## 5. 与 Node 行为对齐

| Node 行为 | Rust 实现 | 一致性 |
|---|---|---|
| SessionKeyStrategy 3 值 | SessionKeyStrategy enum | ✅ |
| 默认 SessionKey `"paperclip"` | `DEFAULT_SESSION_KEY` | ✅ |
| `normalizeSessionKeyStrategy` 回落 `issue` | `from_loose` 回落 | ✅ |
| `prefixSessionKeyForAgent` 不重复前缀 | `prefix_session_key_for_agent` | ✅ |
| `isSensitiveLogKey` 正则覆盖多 token | 4 阶段算法 | ✅ |
| `isLoopbackHost` 大小写不敏感 + IPv6 | `is_loopback_host` (trim `[]`) | ✅ |
| escape hatch 12 alias | `parse_bool_like` (12 alias) | ✅ |

## 6. 与其他 adapter 对齐

| Adapter | Rust 子模块数 | Rust 测试数 | Node execute 行数 |
|---|---|---|---|
| hermes | 9 | 79 | 596 |
| **openclaw-gateway**（本轮） | **4** | **49** | **1491** |
| cursor-cloud（R607） | 7 | 93 | 611 |
| claude-local | 6 | ~80 | 1270 |
| opencode-local | 5 | 39 | 720 |
| gemini-local | 5 | 26 | 759 |
| grok-local | 5 | 38 | 588 |

openclaw-gateway 在 1 轮内走完了与 cursor-cloud R607 step1 同等的纯函数层拆
分。后续 R609+ 需要把 WebSocket wire layer 接入。

## 7. 整体进度更新

| 域 | R607 末 | R608 末 |
|---|---|---|
| shared/ 契约 | 85% | 85% |
| server/ 路由 | 92% | 92% |
| server/ middleware | 60% | 60% |
| server/ services | 58% | 58% |
| server/ repos | 85% | 85% |
| UI client | 35% | 35% |
| CLI | 60% | 60% |
| 验证层 | 45% | 45% |
| **Adapters** | **87%** | **88%** ↑ |
| **总计** | **~91%** | **~91.5%** ↑ |

workspace lib tests passing: 7,198 → 7,247 (+49)

## 8. R609+ 计划

| 优先级 | 模块 | 说明 |
|---|---|---|
| **P0** | `frame_codec.rs` | GatewayRequestFrame / ResponseFrame / EventFrame 双向 JSON 解析 |
| **P0** | `wake_env.rs` | buildPaperclipEnv + wake payload 注入（参考 cursor-cloud::wake_env）|
| **P0** | `config_schema.rs` | UI schema（8 字段：gatewayUrl / sessionKeyStrategy / deviceIdentity 等）|
| **P0** | `wire_client.rs` | WebSocket trait + 真实 WS 客户端（tokio-tungstenite）+ mock 实现 |
| **P1** | openclaw-gateway 完整 execute path | 用 fake WebSocket server 跑真实 round-trip |
| P1 | Hermes WS transport | Hermes 也走 WS，可以共享 wire_client 层 |
| P2 | 架构重构 AdapterEnvironmentCheck 提取到 pc-acpx | claude_test / grok_test 重复部分 |
| P2 | G8 quota.ts 完整复刻 | |
| P2 | G9 plugin-host Node SDK 互操作 | |
