# R629 — Terminal-WS Handler 集成 + 服务端注入

> 2026-08-12  ·  Change: `paperclip-rs-comprehensive-validation`  ·  作者: Codex CLI

## 范围

| 改动 | 文件 |
|---|---|
| Server 启动注入 terminal runtime | `apps/pc-server/src/main.rs` |
| terminal 模块导出 `FakeSshConnector` + `Default` impl | `crates/pc-realtime/src/terminal/{mod,traits}.rs` |
| 集成测试（WS upgrade + frame loop + error paths） | `crates/pc-http/tests/r629_terminal_ws_contract.rs` (232 行) |

## 关键决策

1. **生产路径先 FakeSshConnector / InMemoryStore** — 与 R624 同款 "feature-gate 真实 transport" 策略，
   后续 R630 接真实 `russh` 时只需切换 connector，不动 handler 逻辑。
2. **Default impl for FakeSshConnector** — 让 `Arc::new(FakeSshConnector::default())` 在 server 启动时
   一行完成注入，符合 OpenClaw Gateway / Cursor Cloud 的生产 starter 风格。
3. **集成测试用真实 axum + tokio_tungstenite** — 验证 WS handshake + frame loop 全链路，
   不只是单测 frame codec。

## 验证结果

### 单元测试 (terminal module)

```bash
$ CARGO_BUILD_JOBS=1 cargo test -p pc-realtime --lib terminal
test result: ok. 34 passed; 0 failed; 0 ignored; 51 filtered out
```

12 frame + 8 path + 4 session_store + 7 trait + 3 handler = **34/34** 通过。

### 集成测试 (WS handshake + frame loop)

```bash
$ CARGO_BUILD_JOBS=1 cargo test --test r629_terminal_ws_contract --test-threads=1
running 3 tests
test terminal_ws_full_lifecycle ... ok                  # WS upgrade → ready → output × 2 → resize/raw → close
test terminal_ws_rejects_missing_query_params ... ok    # 400 拒绝缺 terminal_session_id
test terminal_ws_returns_503_when_runtime_missing ... ok # 503 拒绝未配置 runtime

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

测试 1 验证全链路：
- 真实 axum server 监听 `127.0.0.1:0` + environments router
- 真实 WS upgrade → 收到 101 Switching Protocols
- 服务端发送 `ready` 帧 `{type,setupSessionId,terminalSessionId}`
- 真实 FakeSshConnector data_script 推 2 条 `output` 帧
- 客户端发 `resize{cols:120,rows:40}` 和 `raw{data:"ls\\n"}` 帧
- 关闭后 server 日志输出 `reason=ws_closed`

### 编译验证

```bash
$ CARGO_BUILD_JOBS=1 cargo check -p pc-server
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.57s
```

仅 2 个 pre-existing unused variable warnings（与 R629 改动无关）。

## 设计要点（与 OpenClaw Gateway / Cursor Cloud 同款模式）

- **trait 抽象**：所有 IO / 时间副作用通过 `TerminalSshShell` + `TerminalSshConnector` trait，
  可单测 + fake + 未来真实
- **零 unsafe**：纯 safe Rust
- **错误模型**：`ServerFrame::Error` 帧发客户端 + close code 区分 not_found / expired / auth_required
- **expiry timer**：session expires_at → tokio::time::sleep pin 后 drop on scope exit 取消
- **host key verify**：`HostKeyVerifier` trait 注入，FakeSshConnector 默认接受，生产 stub 留接口

## 真实启动验证

`apps/pc-server/src/main.rs` 现在启动时自动注入：

```rust
.with_terminal_runtime(
    Arc::new(InMemoryStore::new()),
    Arc::new(FakeSshConnector::default()),
)
```

`curl -i http://127.0.0.1:PORT/api/environment-custom-image-setup-sessions/<uuid>/terminal/ws?terminal_session_id=t1&token=test`
应返回 426 Upgrade Required（缺少 WebSocket 头）；用 `tokio_tungstenite::connect_async` 可成功升级。

## 下一轮 (R630)

- **RealSshConnector**：`russh` 或 `ssh2-rs` 真实实现（feature-gated）
- **与 pc-repos::environment_terminal_session_store 集成**（真实 DB 查询）
- **真实 sshd container e2e**：起 docker sshd → 跑完整 client 端交互
- **pc-openapi 86.7% → 100%**：补 terminal-ws 路径描述

## 修复细节

1. `terminal/traits.rs` 给 `FakeSshConnector` 加 `Default` 实现（之前没有 `::default()`）
2. `terminal/mod.rs` 导出 `FakeSshConnector` + `FakeSshShell`（之前只在 traits 模块私有）
3. `pc-server/src/main.rs` 加 `use pc_realtime::terminal::{FakeSshConnector, InMemoryStore};` + `.with_terminal_runtime(...)` 调用
