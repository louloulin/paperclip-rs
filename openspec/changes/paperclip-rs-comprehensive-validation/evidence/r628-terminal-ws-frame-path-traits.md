# R628 — terminal-ws 复刻第一轮（frame + path + traits）

> 日期：2026-08-12
> 范围：Node `server/src/realtime/environment-custom-image-terminal-ws.ts` (766 LOC) 的**核心契约层**
> 状态：✅ 28/28 单元测试通过
> 后续轮次：handler（WS 升级 + auth 桥接）+ ssh2 真实 connector

## 1. 背景

R615 gap analysis 标识 terminal-ws 为最大单一模块缺口（Node 766 LOC，Rust 0 LOC）。
R628 第一轮聚焦**核心契约**（可单测的纯函数 / 数据结构），把 SSH 桥接留到 R629。

## 2. 产出

| 文件 | 行数 | 作用 |
|---|---:|---|
| `crates/pc-realtime/src/terminal/mod.rs` | 35 | 模块入口 + 重导出 |
| `crates/pc-realtime/src/terminal/frame.rs` | 250 | JSON 帧协议（12 测试） |
| `crates/pc-realtime/src/terminal/path.rs` | 154 | URL 路径解析（8 测试） |
| `crates/pc-realtime/src/terminal/traits.rs` | 250 | SSH connector + shell trait + Fake（8 测试） |
| `crates/pc-realtime/src/lib.rs` | +5 | `pub mod terminal;` 公开 |
| `crates/pc-realtime/Cargo.toml` | +1 | `async-trait` 依赖 |

**总计**：3 个核心文件，694 行（含 28 个测试），0 unsafe。

## 3. 三个核心契约

### 3.1 帧协议（frame.rs，12 测试）

**客户端 → 服务端**：
```ts
{ type: "auth",    token: "..." }      // 首条非 resize 帧
{ type: "resize",  cols: 80, rows: 24 } // pre-auth 也接受
"echo hello\n"                          // 任意文本 → 直通 SSH stdin
```

**服务端 → 客户端**：
```ts
{ type: "ready",  setupSessionId, terminalSessionId }
{ type: "output", data: "..." }          // SSH stdout utf-8
{ type: "error",  message: "..." }      // 鉴权 / SSH / 超时
```

Rust 实现 `ClientFrame::decode(&[u8]) → ClientFrame`，1:1 对齐 Node `decodeClientMessage + parseJsonClientFrame`。
关键设计：
- `serde_json::from_str` 失败 → fallback 到 `RawText`（容错）
- 无效 UTF-8 → `RawBytes`（直通 binary 流）
- `camelCase` 字段名（`setupSessionId` 等），与 Node `{ type, ... }` literal 完全一致

### 3.2 路径解析（path.rs，8 测试）

**路径格式**：`/api/environment-custom-image-setup-sessions/{setupSessionId}/terminal/ws`

Rust 实现 `parse_terminal_path(&str) → Result<String, TerminalPathError>`，1:1 对齐 Node `parseTerminalPath`：
- 前缀匹配 `/api/environment-custom-image-setup-sessions/`
- 后缀匹配 `/terminal/ws`
- percent-decode（`%XX` → byte，错误返 `UrlDecodeError`）
- 空 id 拒绝

**测试覆盖**：canonical / UUID / percent-encoded / missing prefix / wrong suffix / no terminator / empty id / invalid percent。

### 3.3 SSH Trait（traits.rs，8 测试）

```rust
#[async_trait]
pub trait TerminalSshShell: Send {
    async fn write(&mut self, data: &str) -> Result<(), String>;
    async fn resize(&mut self, dims: TerminalDimensions) -> Result<(), String>;
    async fn close(self: Box<Self>) -> Result<(), String>;
    async fn into_data_stream(self: Box<Self>) -> Result<mpsc::Receiver<ShellEvent>, String>;
}

#[async_trait]
pub trait TerminalSshConnector: Send + Sync {
    async fn connect(
        &self,
        params: SshConnectionParams,
        verify_host_key_sha256: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    ) -> Result<Box<dyn TerminalSshShell>, String>;
}
```

**与 Node 上游差异**：
- Node 回调函数 (`onData/onClose/onError`) → Rust `mpsc::Receiver<ShellEvent>`
- Node `ssh2` 包 → Rust trait（真实 impl 留 R629）
- `Box<Self>` 消费所有权 → 单写者 shell 资源安全

**FakeSshShell + FakeSshConnector**：测试用，注入 verifier 返回值、connect error、pre-recorded data_script（验证 stdout drain）。

## 4. 28 个测试

```
running 28 tests
test terminal::frame::tests::decode_auth_token ... ok
test terminal::frame::tests::decode_auth_token_with_whitespace_trimmed ... ok
test terminal::frame::tests::decode_resize_valid ... ok
test terminal::frame::tests::decode_resize_zero_falls_through_as_raw_zero ... ok
test terminal::frame::tests::decode_unknown_json_type_falls_through ... ok
test terminal::frame::tests::decode_invalid_json_is_raw_text ... ok
test terminal::frame::tests::decode_invalid_utf8_is_raw_bytes ... ok
test terminal::frame::tests::server_frame_ready_round_trip ... ok
test terminal::frame::tests::server_frame_output_round_trip ... ok
test terminal::frame::tests::server_frame_error_round_trip ... ok
test terminal::frame::tests::e2e_auth_flow_round_trip ... ok
test terminal::frame::tests::e2e_resize_then_output_flow ... ok
test terminal::frame::tests::e2e_raw_passthrough_flow ... ok
test terminal::path::tests::parses_canonical_path ... ok
test terminal::path::tests::parses_uuid_id ... ok
test terminal::path::tests::percent_decoded_id ... ok
test terminal::path::tests::rejects_missing_prefix ... ok
test terminal::path::tests::rejects_wrong_suffix ... ok
test terminal::path::tests::rejects_no_terminator ... ok
test terminal::path::tests::rejects_empty_id ... ok
test terminal::path::tests::rejects_invalid_percent_encoding ... ok
test terminal::traits::tests::fake_shell_write_appends ... ok
test terminal::traits::tests::fake_shell_resize_appends ... ok
test terminal::traits::tests::fake_shell_close_marks_closed ... ok
test terminal::traits::tests::fake_shell_into_data_stream_drains_script ... ok
test terminal::traits::tests::fake_connector_accepts_valid_host_key ... ok
test terminal::traits::tests::fake_connector_rejects_bad_host_key ... ok
test terminal::traits::tests::fake_connector_propagates_connect_error ... ok

test result: ok. 28 passed; 0 failed; 0 ignored
```

## 5. 设计选择（高内聚低耦合）

| 维度 | 选择 | 理由 |
|---|---|---|
| 帧解析 | 枚举 + serde_json | 编译期防字段拼错，1:1 对齐 Node schema |
| Shell 资源所有权 | `Box<Self>` | 单写者，RAII，drop 兜底关 SSH |
| 数据流 | `mpsc::Receiver<ShellEvent>` | async 友好，可多 consumer 协同 |
| SSH 库选择 | trait 抽象，impl 留 R629 | 不锁定具体库（`russh` vs `ssh2`），先定契约 |
| Host key 验证 | 注入 `Arc<dyn Fn>` | caller 决定 verify 或 pin 策略，trait 无 DB 依赖 |
| Path 解析 | 纯函数 + 极简 percent-decode | 零依赖，可单测，URL semantics 显式 |

## 6. 数字

| 指标 | R627 末 | R628 末 |
|---|---:|---:|
| terminal-ws 复刻 LOC | 0 | **694** (含测试) |
| 单元测试 | 0 | **28** (terminal 模块) |
| 真实 SSH connector | 0 | 0 (留 R629) |
| WS 升级 handler | 0 | 0 (留 R629) |
| `cargo test -p pc-realtime --lib terminal` | n/a | 28/28 |

## 7. 下一轮 (R629)

| 优先级 | 目标 | 估时 |
|---|---|---|
| P0 | 选 `russh` 或 `ssh2-rs`，写 `RealSshConnector`（feature-gated） | 1 轮 |
| P0 | 写 `handler.rs` — WS upgrade + auth 桥接 + 帧循环 | 1-2 轮 |
| P1 | 与 `pc-repos::environment_terminal_session_store` 集成 | 0.5 轮 |
| P1 | 真实 sshd e2e（用 `sshd` container 启 ephemeral SSH server） | 1 轮 |
| P2 | UI terminal 组件 (xterm.js) 集成测试 | 1 轮 |
| P2 | pc-openapi 86.7% → 100% (terminal-ws 端点加 OpenAPI 描述) | 0.5 轮 |
