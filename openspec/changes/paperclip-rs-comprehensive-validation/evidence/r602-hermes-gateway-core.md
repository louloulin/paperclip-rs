# R602 — Hermes gateway adapter 核心架构组件

> 2026-08-12 / Change: paperclip-rs-comprehensive-validation / 状态：✅ 完成（核心架构）

## 1. 目标

把 `pc-adapter-hermes-gateway` 从 147 行 stub 推进到包含核心架构组件
（non-HTTP 部分），为后续完整 SSE/HTTP 集成打基础。

完整 Hermes gateway Node execute.ts 是 959 行 + 703 行 tests，本次聚焦
**核心可独立测试的模块**：

| 模块 | 行数 | 状态 |
|---|---|---|
| `constants.rs` | 75 | ✅ 完整（ADAPTER_TYPE / ADAPTER_LABEL / 4 个常量 / SessionKeyStrategy 枚举） |
| `config_schema.rs` | 220 | ✅ 完整（9 字段 UI schema） |
| `transport_security.rs` | 215 | ✅ 完整（loopback 检测 / 远端 HTTP 拒绝 / escape hatch） |
| `lib.rs` | 260 | ✅ 加 session key 构造 + apiBaseUrl 入口校验 |

**未覆盖**（后续 round 计划）：
- HTTP/SSE 客户端
- dashboard REST 集成
- 重新连接 / 退避

## 2. 设计要点

1. **`SessionKeyStrategy` enum** 代替字符串常量：编译期检查 4 个策略值
2. **`build_session_key` 纯函数**：4 种策略对应 4 种 key 构造，字段缺失优雅降级
3. **`transport_security::validate_api_base_url`** 入口校验：loopback 始终
   允许；远端必须 HTTPS；显式 escape hatch 才允许远端 HTTP
4. **`parse_boolean_like`** 接受 bool / "1" / "true" / "yes" / "on" 等 8 种别名

## 3. 测试

```
$ cargo test -p pc-adapter-hermes-gateway --lib
test result: ok. 25 passed; 0 failed
```

| 模块 | 测试数 | 覆盖 |
|---|---|---|
| `constants` | 1 | SessionKeyStrategy 解析 |
| `config_schema` | 5 | 9 字段 + 必填 + 选项 + 默认值 |
| `transport_security` | 9 | loopback / boolean 别名 / 远端 HTTP / escape hatch / invalid URL |
| `lib.rs` | 10 | descriptor / resolve_command / session key 4 策略 / parse_stdout 3 场景 |

合计 **25 个 hermes-gateway 测试 0 失败**（R602 末；之前 stub 阶段只有 6 个）。

## 4. 与 Node 一致性

| Node 行为 | Rust 实现 |
|---|---|
| `INSECURE_REMOTE_HTTP_ESCAPE_HATCH = "dangerouslyAllowInsecureRemoteHttp"` | 同名常量 |
| `isLoopbackHostname` (Node 正则) | `is_loopback_hostname` Rust 字符串匹配 |
| `isRemotePlainHttp` | `is_remote_plain_http` |
| `allowsInsecureRemoteHttp(config)` | `allows_insecure_remote_http(config)` |
| `remotePlainHttpDeniedMessage(hostname)` | `remote_plain_http_denied_message(hostname)` |
| `SessionKeyStrategy = "issue" \| "agent" \| "run" \| "none"` | `enum SessionKeyStrategy` |
| `buildHermesSessionKey` | `build_session_key` 纯函数 |
| 9 字段 config schema | `get_config_schema` 9 个字段 |

## 5. 架构价值

1. **transport_security 是真正的安全边界**：loopback vs 远端的硬区分避免
   误把开发用 HTTP gateway 暴露到生产
2. **SessionKeyStrategy enum**：把字符串分支转为编译期枚举，避免运行时
   拼写错误
3. **可独立测试**：每个模块都用纯函数 + serde_json 跑测试，不需要 mock
   HTTP server
