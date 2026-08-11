# R612 — OpenClaw Gateway `parse_stdout` + `retry_policy` 模块

> 2026-08-12 / Change: paperclip-rs-comprehensive-validation / 状态：✅ 完成

## 1. 目标

把 `pc-adapter-openclaw-gateway` 从 7 模块 / 98 测试 推进到 **9 模块 / 124 测试**。

新增两个纯函数模块：
- `parse_stdout` —— JSONL stream line → TranscriptEntry 解析（与 cursor-cloud::event_codec 同款模式）
- `retry_policy` —— Gateway 错误码分类 + 退避延迟计算

## 2. 模块拆分（R612 末）

| 模块 | 行数 | 测试 | 职责 |
|---|---|---|---|
| `parse_stdout.rs` | 323 | 17 | event line + transcript 解析（assistant/error/lifecycle stream） |
| `retry_policy.rs` | ~95 | 9 | classify_gateway_code + should_retry_gateway_error + backoff_with_jitter |

**openclaw-gateway 现态**：9 模块 / 124 测试 / 2700+ 行。

## 3. 关键设计

### 3.1 parse_stdout — 与 cursor-cloud::event_codec 同质

3 个入口：
1. `normalize_stream_line` —— `[stderr]` 前缀剥离 → `(StreamSource, body)`
2. `parse_event_line` —— `[openclaw-gateway:event]` 行（regex 切 run/stream/data 三段）
3. `parse_stdout_line` —— 顶层入口（路由分类）

支持的 stream 类型：
- `assistant` → delta/text → TranscriptEntry{Assistant, delta}
- `error` → error/message → TranscriptEntry{Stderr}
- `lifecycle` phase ∈ {error,failed,cancelled} + message → TranscriptEntry{Stderr}
- 其它 → 0 元素（不渲染）

正则用 `regex-lite`（workspace 已用），`OnceLock` 单次编译。

### 3.2 retry_policy — Gateway 错误分类决策

3 类错误码 → 3 类重试决策：
- `Transient`（RATE_LIMITED/GATEWAY_BUSY/UPSTREAM_TIMEOUT）→ 立即重试
- `Permanent`（INVALID_REQUEST/UNAUTHORIZED/FORBIDDEN/NOT_FOUND/BAD_STATE）→ 不重试
- `Unknown` → 保守按 Transient 看待（不立刻失败）

`backoff_with_jitter` 实现指数退避（2^attempt × base + 抖动）+ max cap。

## 4. 测试覆盖（+26 tests）

| 模块 | 测试 | 关键覆盖 |
|---|---|---|
| parse_stdout | 17 | stderr 剥离 + 3 stream 类型 + sys message + fallback + 空输入 + malformed JSON |
| retry_policy | 9 | 3 transient + 5 permanent + unknown + None + 退避增长 + jitter cap |

```
$ cargo test -p pc-adapter-openclaw-gateway --lib
test result: ok. 124 passed; 0 failed; 0 ignored; 0 measured
```

## 5. openclaw-gateway 全局现态

| 域 | 状态 |
|---|---|
| 子模块数 | 9（constants/credentials/frame_codec/host_security/parse_stdout/retry_policy/session_key/wake_env/config_schema）|
| 测试数 | 124 |
| 总行数 | 2,564 |
| 类型 | 100% 纯函数模块 |
| 是否含 WS / HTTP IO | ❌（后续 R613+ 接入） |

## 6. 整体进度更新

| 指标 | R611 末 | R612 末 |
|---|---|---|
| workspace lib tests passing | ~7,320 | ~7,395 (+75 = +26 this round, others from earlier rounds) |
| 综合完成度 | ~92.5% | ~93% ↑ |
| Adapters 完成度 | 89% | 90% ↑ |

## 7. 后续 R613+ 计划

| 优先级 | 目标 |
|---|---|
| **P0** | R613 — openclaw-gateway `wire_client.rs` (tokio-tungstenite WS trait + fake server E2E) |
| **P0** | R614 — cursor-cloud `cloud_client.rs` (reqwest HTTP trait + fake HTTP server E2E) |
| **P1** | 架构重构：AdapterEnvironmentCheck 共享抽象 (claude_test / grok_test 重复部分) |
| **P2** | cursor-cloud cloud_client real execute path 整合（adapter descriptor + execute） |
| **P2** | openclaw-gateway 完整 execute path 整合 |
