# M3 ws 传输层（LUM-1439）：`/api/daemon/ws` 的连接、索引、扇出与帧分派

本文件是 `docs/37-M3-W3C-PREFLIGHT.md` §4 派出的**预切片**的落地记录：把 daemon WebSocket 传输层
从 M3-7 里切出来单独交付，**0 路由 / 0 DB / 0 前置**。M3-7 的路由 handler 只 `use` 本片，不重写。

| 项 | 值 |
| --- | --- |
| issue | **LUM-1439**（parent epic LUM-1334） |
| 分支 | `feat/multica-rs-m3c-ws-transport` → PR 目标 `feat/multica-rs-initial` |
| 基线 | `feat/multica-rs-initial` @ `28e5c56` |
| 上游对照 | `louloulin/multica` clone @ `90e0bdf8`，`server/internal/daemonws/hub.go` |
| 契约来源 | `docs/16-M3-DAEMON-PROTOCOL.md` §3.3/§3.4/§3.5/§4/§11 |
| 门禁 | `bash scripts/gates.sh` **8/8 全绿**（暖跑 37s） |

## 1. 结论

1. **传输层与协议常量层完全解耦**：本片只新增 `crates/mc-ws` 的模块，**没有**新增路由、**没有**碰
   `mount.rs`、**没有**碰 M3-7 的 `routes/daemon.rs`。
2. **门 ⑦ / ⑨ 零影响**（实测）：⑦ `local 136 / implemented 122 (112 real + 10 placeholder) / known_gap 334
   / local_only 11 / regression 0`，⑨ `report matches`。本片 0 路由 ⇒ 两个快照都不必重生成。
3. **上游语义逐条对齐**（§4 的表）：索引三维、事件去重（128，空 id 绕过）、hub 级 runtime-gone 去重
   （512 + forget 分支）、16 深发送缓冲 + 慢客户端驱逐、RPC 判定顺序 503 → 429 → 404、
   读上限 64 KiB、写预算 10s、ping 54s / pong 等待 60s、`invalidateRuntime` 的四步顺序。
4. **57 个测试**（19 lib + 37 集成 + 1 doc-test）全绿，**全部走真 socket**（真 `TcpListener` + `axum::serve`
   + `tokio-tungstenite` 客户端），不是 mock 传输。
5. **Cargo.toml 增量**只有 `mc-daemon-proto` + `uuid`（两者本就在 `Cargo.lock` 里 ⇒ 锁文件只多两条依赖边，
   不引入任何新 crate）。

### 交付文件（行数实测）

| 文件 | 行数 | 可见性 |
| --- | --- | --- |
| `crates/mc-ws/src/hub.rs` | 580 | `pub mod hub` |
| `crates/mc-ws/src/frames.rs` | 525 | `pub mod frames` |
| `crates/mc-ws/src/connection.rs` | 323 | `mod connection`（私有） |
| `crates/mc-ws/src/identity.rs` | 201 | `pub mod identity` |
| `crates/mc-ws/src/pump.rs` | 346 | `mod pump`（私有） |
| `crates/mc-ws/src/lib.rs` | 114 | 改：模块声明 + crate 文档 |
| `crates/mc-ws/tests/hub_support/mod.rs` | 348 | 测试支撑（真 socket） |
| `crates/mc-ws/tests/hub_heartbeat.rs` | 286 | 集成测试 7 例 |
| `crates/mc-ws/tests/hub_limits.rs` | 240 | 集成测试 6 例 |
| `crates/mc-ws/tests/hub_reconnect.rs` | 373 | 集成测试 8 例 |
| `crates/mc-ws/tests/hub_registry.rs` | 334 | 集成测试 8 例 |
| `crates/mc-ws/tests/hub_rpc.rs` | 371 | 集成测试 8 例 |

**全部 ≤ 800 行**（门 ⑩ 硬上限）。单文件版 `hub.rs` 初稿实测 **1145 行**（超限），因此按职责拆成
`hub` / `connection` / `pump` 三个模块；`connection` 与 `pump` 是**私有**模块，公开面只在 `hub`。

## 2. 模块地图

| 模块 | 职责 | 上游对应（`hub.go` @ `90e0bdf8`） |
| --- | --- | --- |
| `identity.rs` | `ClientIdentity`（连接身份）+ scope 派生（`authorized_workspace_ids` / `primary_workspace_id` / `allows_workspace` / `runtime_set`）+ `validate()` | `ClientIdentity` `:23-42`，`AuthorizedWorkspaceIDs` `:103`，`PrimaryWorkspaceID` `:126`，`AllowsWorkspace` `:137` |
| `connection.rs` | `Connection`（单连接状态：runtime 集合、16 深发送队列、128 事件去重、关闭信号、在飞 RPC 配额）+ `Registry`（三维索引）+ `DedupCache` + 容 poison 的锁助手 | `client` `:150-193`，`trySend` `:181`，`markSeen` `:205`，`register` `:834`，`unregister` `:880` |
| `pump.rs` | 读泵（读上限 / pong 截止 / 关闭竞速）+ 写泵（ping 心跳 / 写预算 / 收尾 Close）+ 入站帧分派（心跳、RPC） | `readPump` `:933`，`handleFrame` `:963`，`handleRPCFrame` `:995`，`handleHeartbeatFrame` `:1068`，`writePump` `:1128` |
| `frames.rs` | 帧构造/编解码（`Message` ↔ WS 文本帧）、`RpcReply`、`RpcRequest`/`HeartbeatRequest`、`RpcCancel`、`InFlightLimiter`、`handler_error_status` | 帧构造 `:760-806`，`sendRPCResponse` `:1044` |
| `hub.rs` | 公开面：`Hub`（注册表 + 通知 API + 升级入口）、`TransportConfig`、`DeliveryOutcome` | `Hub` `:312`，`HandleWebSocket` `:412`，`Notify*` `:446-476`，`DeliverDaemonRuntime` `:594`，`notifyFrame`/`notifyWorkspaceFrame`/`notifyUserFrame` `:676-758`，`invalidateRuntime` `:548` |

**公开面只有三处**：`mc_ws::hub::{Hub, TransportConfig, DeliveryOutcome}`、
`mc_ws::frames::*`、`mc_ws::identity::{ClientIdentity, IdentityError}`。`Connection` / `Registry` /
`DedupCache` / `Index` / 读写泵都是 crate 私有 —— M3-7 无法绕过 `Hub` 直接改注册表。

## 3. 公开 API

### 3.1 升级入口（M3-7 唯一需要的调用）

```rust
pub fn handle_websocket(&self, ws: WebSocketUpgrade, identity: ClientIdentity) -> Response
```

- `identity.validate()` 失败 → **HTTP 400** + body `{"error":"runtime_ids or user identity required"}`
  （上游条件：`len(identity.RuntimeIDs) == 0 && identity.UserID == ""`，`hub.go:413-416`）。
- 成功 → `101 Switching Protocols`，并按 `TransportConfig.read_limit` 设 `max_message_size`
  （上游是 `SetReadLimit(64*1024)`，`hub.go:944`）。
- **身份由调用方注入**：`Hub` 不做 token 解析、不查库、不认 cookie。M3-7 负责把 `X-Daemon-ID` /
  `X-Runtime-Ids` / `X-Workspace-Id(s)` / `X-User-Id`（以及自己的鉴权结论）翻成 `ClientIdentity`。

```rust
async fn daemon_ws(
    State(hub): State<Arc<Hub>>,      // 可 Clone 的提取器在前
    headers: HeaderMap,               // …
    ws: WebSocketUpgrade,             // WebSocketUpgrade 必须在最后（它消费请求）
) -> Response {
    let identity = identity_from_headers(&headers); // M3-7 的鉴权产物
    hub.handle_websocket(ws, identity)
}
```

### 3.2 通知 API（写侧；全部同步、非阻塞、`&self`）

| 方法 | 扇出维度 | 帧 type | 上游 |
| --- | --- | --- | --- |
| `notify_task_available(runtime_id, task_id)` | runtime | `daemon:task_available` | `:446` / `:478` |
| `notify_runtime_profiles_changed(workspace_id, profile_id)` | workspace | `daemon:runtime_profiles_changed` | `:452` / `:494` |
| `notify_workspaces_changed(user_id)` | user | `daemon:workspaces_changed` | `:458` / `:505` |
| `notify_pending_work(runtime_id, kind)` | runtime | `daemon:pending_work` | `:466` / `:516` |
| `notify_runtime_gone(runtime_id)` | runtime + 反注册 | `daemon:heartbeat_ack`(`status=runtime_gone`) | `:474` / `:532` / `:548` |
| `deliver_daemon_runtime(scope_id, frame, event_id)` | 按帧 type 分派到上表四个维度 | 原样转发 | `:594` |

全部返回 `DeliveryOutcome { delivered, deduped }`（`hit()` / `miss()` / `duplicate()`），替代上游的
`(delivered, deduped)` 双返回值与 metrics 计数器。**慢客户端的驱逐发生在写锁释放之后**（上游同序）。

### 3.3 处理器注入（M3-7 的业务挂载点）

```rust
pub fn set_rpc_handler(&self, handler: RpcHandler)              // Fn(RpcRequest) -> RpcFuture
pub fn set_heartbeat_handler(&self, handler: HeartbeatHandler)  // Fn(HeartbeatRequest) -> HeartbeatFuture
```

- `RpcRequest { identity, method, body: Option<Value>, timeout_ms, cancel }` —— **故意不带 `request_id`**：
  上游 handler 看不到它，关联由传输层持有（`sendRPCResponse(requestID, …)` 在传输层闭包里）。
- `RpcReply::{ok, with_status, failed}`：`ok` 走 200，`failed` 走 `<400` 时被 `handler_error_status`
  改写成 500（对齐上游 `sendRPCResponse`：errMsg 非空且 status < 400 ⇒ 500）。
- `HeartbeatRequest { identity, runtime_id, supports_batch_import }` → `Result<Option<DaemonHeartbeatAckPayload>, String>`；
  `Ok(None)` 或 handler 报错 ⇒ **不回 ack**。**ack 载荷逐字转发**（传输层不代填 `runtime_id`，handler 自己回显）。

### 3.4 其它公开项

- `TransportConfig`：见 §6；`Hub::new()` = `TransportConfig::default()`，测试用 `Hub::with_config` 缩短窗口。
- 观测：`connection_count()` / `runtime_connection_count(id)` / `workspace_connection_count(id)` /
  `user_connection_count(id)`（上游 `:816-832`）。
- `frames::decode(&str) -> Result<Message, _>`、`frames::encode_text(&Message) -> Option<String>`：
  出入站的唯一编解码通道（M3-7 的 relay 路径可复用）。
- `frames::InFlightLimiter`、`frames::RpcCancel`：供 handler 侧理解配额与取消语义（见 §4.6/§4.7）。

## 4. 上游契约对照表

| # | 上游语义（`hub.go` @ `90e0bdf8`） | 本片实现 | 覆盖测试 |
| --- | --- | --- | --- |
| 1 | 无 runtime 且无 user ⇒ 400 `runtime_ids or user identity required` | `Hub::handle_websocket` 先 `validate()` | `hub_registry::missing_identity_is_rejected_before_upgrade` |
| 2 | 注册进 `byRuntime` / `byWorkspace` / `byUser` 三维索引（空 key 不入索引） | `Registry::insert` + `Identity::authorized_workspace_ids` | `upgrade_registers_every_index_dimension`、`multi_workspace_scope_is_deduped_and_all_keys_are_indexed` |
| 3 | 多工作区字段优先于遗留 `WorkspaceID` | `primary_workspace_id()` / `authorized_workspace_ids()` | `workspace_scope_prefers_list_and_dedups_in_order`、`legacy_workspace_field_becomes_the_scope_when_multi_workspace_is_absent`、`workspace_scope_falls_back_to_legacy_field` |
| 4 | 连接级事件去重 `eventDedupCapacity = 128`，**空 `eventID` 直接绕过** | `DedupCache`（FIFO 淘汰） | `dedup_cache_dedups_and_evicts_in_order`、`empty_event_id_disables_dedup`、`relay_event_dedup_is_per_connection_and_keyed_by_event_id` |
| 5 | hub 级 runtime-gone 去重 `runtimeGoneDedupCapacity = 512`（`hub.go:199`）；无连接时若已见 ⇒ `duplicate`，否则 `forget` | `Hub::invalidate_runtime` 的两条分支 | `runtime_gone_dedup_survives_an_event_that_raced_ahead_of_registration`、`direct_runtime_gone_notification_matches_the_relay_shape` |
| 6 | `invalidateRuntime` 顺序：先摘索引 + `removeRuntime`，再投递，最后驱逐慢客户端 | `invalidate_runtime` 逐步同序 | `runtime_scoped_notification_reaches_only_matching_connections`、`heartbeat_after_runtime_gone_is_no_longer_in_scope` |
| 7 | 发送队列 16 深，满 ⇒ `trySend` 返回 false ⇒ 该连接被标记驱逐 | `Connection::try_send` + `SEND_BUFFER=16` | `slow_consumer_is_evicted_while_other_connections_keep_working` |
| 8 | RPC 判定顺序：解析失败/空 `request_id` 静默丢弃 → handler 未装 503 → 配额满 429 → 未知方法 404 → spawn | `handle_rpc_frame` + `run_rpc_handler`（404 在**拿到配额之后**判，所以 429/503 优先；404 文案 `unknown rpc method "<m>"`，对齐 `handler/daemon_rpc.go:50` 的 `unknown rpc method %q`） | `rpc_with_missing_request_id_or_broken_payload_is_dropped`、`rpc_without_a_handler_is_503`、`rpc_unknown_method_is_404_and_never_reaches_the_handler`、`rpc_in_flight_is_bounded_and_unknown_method_loses_to_429` |
| 9 | 每连接在飞 RPC 上限 8（`maxInFlightRPCPerClient`）；超时预算用 `timeout_ms` | `InFlightLimiter` + `tokio::time::timeout` | `in_flight_limiter_saturates_without_queueing`、`rpc_timeout_budget_is_enforced_and_frees_the_slot` |
| 10 | RPC 响应的 `status` 与 HTTP 状态码同义；`body` 与 HTTP body 逐字节同源 | `RpcReply::into_frame` + `rpc_response_frame` | `rpc_request_reaches_the_handler_and_status_passes_through`、`rpc_frames_round_trip` |
| 11 | 心跳 ack 由 handler 决定载荷；越权 runtime / 空 `runtime_id` / 无 handler ⇒ 不回 | `handle_heartbeat_frame`（`allows_runtime` 判定） | `hub_heartbeat` 全 7 例 |
| 12 | 未知帧型、坏 JSON、非对象、Binary、Ping 一律忽略且**不断连** | `handle_frame` 的 switch 默认分支 | `invalid_and_unknown_frames_are_ignored_without_dropping_the_connection` |
| 13 | relay（`DeliverDaemonRuntime`）按**帧 type** 分派到 runtime/workspace/user 三个扇出面 | `deliver_daemon_runtime` | `relay_frames_are_dispatched_by_frame_type`、`relay_forwards_the_exact_bytes_it_received`、`relay_ignores_invalid_or_unroutable_frames` |
| 14 | 读上限 64 KiB；读截止 = `pongWait`；仅 pong 重置；超时踢连接 | `pump::read_pump`（`select!` biased） | `oversized_inbound_frame_closes_the_connection`、`connection_without_a_pong_is_kicked_after_pong_wait`、`pong_resets_the_read_deadline_so_the_connection_survives` |
| 15 | 写截止 = `writeWait`（10s）；ping 周期 = `pingPeriod`（54s） | `pump::write_pump` + `write_frame` | `transport_config_defaults_match_frozen_protocol_constants`、`stalled_write_is_bounded_by_the_write_budget` |
| 16 | 连接断开 ⇒ 反注册（三维索引 + runtime 集合） | `read_pump` 收尾 + `Hub::unregister` | `disconnect_clears_workspace_and_user_indices_too`、`reconnect_replaces_the_registration` |
| 17 | 重复 runtime id 去重；空 id 不入集合 | `Identity::runtime_set` | `runtime_set_skips_blank_ids` |

**常量全部引用协议层冻结点**（`mc_daemon_proto::rpc`），不在传输层重抄：`MAX_IN_FLIGHT_RPC_PER_CLIENT=8`、
`RPC_READ_LIMIT_BYTES=65536`、`WRITE_WAIT_MS=10000`、`PONG_WAIT_MS=60000`、`PING_PERIOD_MS=54000`。

## 5. 登记差异（deviation registry，与上游的**故意**不同）

| # | 差异 | 原因 / 影响 |
| --- | --- | --- |
| D1 | 400 响应的 body 是**裸 JSON**（`content-type: application/json`，无尾随换行）；上游 `http.Error` 是 `text/plain; charset=utf-8` + `\n` | 本仓统一 JSON 错误面（`mc-errors`）；**判据是 body 字符串**，测试逐字节断言 |
| D2 | 超时错误文案 `rpc request timed out`（status 500） | 上游用 `context.DeadlineExceeded` 的字符串；语义（状态码 + 不回 body）一致 |
| D3 | 上游 metrics 计数器 → `DeliveryOutcome { delivered, deduped }` | 本仓无 metrics 设施；返回值可观测且可测 |
| D4 | `ClientIdentity::RuntimeLeases` **未实现** | 上游用它给 DB 写节流；那是 M3-7 的库内关切，传输层不持有租约 |
| D5 | 写泵退出时**总会**尝试发一帧 Close（带上写预算）；上游仅在发送通道被关闭时发 | 本仓的连接收尾是统一路径（`watch` 关闭信号），Close 让对端可观测到正常关闭 |
| D6 | 扇出顺序用 `BTreeSet<Uuid>`（确定性），上游 Go map 无序 | 测试可重复；语义上无差异（best-effort 唤醒提示） |
| D7 | 帧 body 的键序 = **字典序**（`Message{payload: Value}` 的 `serde_json::Map` 是 `BTreeMap`） | 冻结在 W3a 的信封实现里（`docs/16` §11.3 D1 / §11.5）；本片测试按字典序断言 |
| D8 | 取消语义：连接收尾时**发信号而 not abort** handler 任务 | 在飞任务最多泄漏到「每连接 8 个配额」的边界，handler 可自行观测 `RpcCancel` |

## 6. `TransportConfig`：默认值 = 冻结常量，测试可缩短

| 字段 | 默认 | 来源 |
| --- | --- | --- |
| `read_limit` | `rpc::RPC_READ_LIMIT_BYTES`（65536） | `hub.go:944` |
| `write_wait` | `rpc::WRITE_WAIT_MS`（10s） | `hub.go:17` |
| `pong_wait` | `rpc::PONG_WAIT_MS`（60s） | `hub.go:18` |
| `ping_period` | `rpc::PING_PERIOD_MS`（54s = 60s × 9/10） | `hub.go:19` |
| `send_buffer` | 16 | `hub.go:433` |
| `event_dedup_capacity` | 128 | `hub.go:195` |
| `runtime_gone_dedup_capacity` | 512 | `hub.go:199` |

`TransportConfig::default()` 与冻结常量**逐字段相等**这一点由
`hub_limits::transport_config_defaults_match_frozen_protocol_constants` 断言（含 `ping_period < pong_wait`、
`Hub::new().config() == TransportConfig::default()`）。测试里缩短 `ping_period`/`pong_wait`/`write_wait`/
`send_buffer` 来在秒级内触发真实的踢连接与驱逐路径。

## 7. 测试：全部走真 socket

支撑层 `crates/mc-ws/tests/hub_support/mod.rs` 起一个真 `TcpListener`（`127.0.0.1:0`）+ `axum::serve`，
用 `tokio-tungstenite` 作客户端：

- `TestServer::{start, url, connect}`，`Drop` 时 abort 服务任务；
- 身份经测试头（`x-test-*`）注入，路由里再翻成 `ClientIdentity`（与 M3-7 的真实形状同构）；
- `expect_text` 跳过 Ping/Pong 取文本帧；`quiet_for(QUIET)` 只在**完全没有帧**到达时才算「安静」；
- 确定性负面断言用**屏障帧**：读泵是 FIFO 的，后续一帧的应答只能在其前面所有帧处理完之后发出，
  所以「先收到应答」= 「前面的帧确实没被应答」，比 `sleep` 可靠；
- `raw_upgrade` 手写 HTTP/1.1（`Upgrade` + `Sec-WebSocket-Key` + `Sec-WebSocket-Version`）打 400 分支，
  保证那个 400 **只能**来自 `Hub::handle_websocket`，而不是 axum 提取器。

| 文件 | 用例数 | 重点 |
| --- | --- | --- |
| `hub_registry.rs` | 8 | 400 分支（逐字节 body + content-type）、三维索引、scope 优先级、越权隔离、未知帧不断连 |
| `hub_heartbeat.rs` | 7 | ack 逐字节形状（含 `server_capabilities` 键序）、handler 缺省/报错不回、越权/空 id 不回、runtime-gone 后失焦 |
| `hub_rpc.rs` | 8 | 503/429/404 优先级与配额耗尽恢复、超时预算（150ms 预算 + 30s sleep ⇒ 500，耗时 < 2s）、坏帧静默丢弃、取消信号、并发调用的 `request_id` 关联 |
| `hub_reconnect.rs` | 8 | 重连替换注册、断开清三维索引、relay 四支分派、**逐字节转发**、事件去重（空 id 绕过）、runtime-gone 竞态（先到先 dedup）、非法帧 10 种 miss |
| `hub_limits.rs` | 6 | 默认值=冻结常量、无 pong 被踢、pong 续命、超读上限断开、慢消费者被驱逐且不影响他人、写预算兜底 |
| lib 单元测试 | 19 | 帧编解码/`RpcReply` 状态改写/去重缓存/scope 派生/`InFlightLimiter`/关闭竞态 |

## 8. 门禁证据（全绿）

```
$ bash scripts/gates.sh
  ①  fmt                   0     0s  PASS
  ②  build                 0     7s  PASS       # cargo build --workspace --all-targets --locked
  ③  clippy                0     3s  PASS       # --workspace --all-targets -D warnings
  ④  clippy-test-util      0     2s  PASS
  ⑤  test                  0    22s  PASS       # 70 suites / 513 passed
  ⑦  route-parity          0     0s  PASS       # local 136 / implemented 122 / known_gap 334 / regression 0
  ⑨  conformance           0     3s  PASS       # report matches
  ⑩  file-size             0     0s  PASS
  overall: PASS — 8/8 gate(s) green in 37s
```

`Cargo.lock` 的增量**只有两条依赖边**（`mc-ws` 的 dependencies 里多 `mc-daemon-proto` 与 `uuid`），
没有新 crate、没有版本漂移：

```bash
git diff --stat Cargo.lock          # Cargo.lock | 2 ++
```

## 9. 复算命令

```bash
cd <workdir>/paperclip-rs
export PATH="$HOME/.cargo/bin:$PATH"

# 本片测试（唯一新增的测试目标）
cargo test -p mc-ws                       # 19 lib + 37 集成（5 个二进制）+ 1 doc-test

# 全量门禁
bash scripts/gates.sh                     # 8/8；--with-db 追加 ⑥ ⑧
bash scripts/gates.sh --only file-size    # ⑩ 单跑：新文件都 ≤ 800 行

# 路由账与一致性快照（本片 0 路由 ⇒ 应完全不变）
python3 scripts/route_parity.py --quiet
cargo run -q -p mc-conformance -- --no-db --check crates/mc-conformance/report.json

# 行数复核
wc -l crates/mc-ws/src/*.rs crates/mc-ws/tests/hub_*.rs crates/mc-ws/tests/hub_support/mod.rs

# 上游交叉核对（只读 clone）
#   <upstream>/server/internal/daemonws/hub.go @ 90e0bdf8
#   grep -n "SetReadLimit\|pingPeriod\|writeWait\|pongWait\|maxInFlightRPCPerClient" hub.go
```

## 10. M3-7 的消费方式（本片不做的事）

M3-7 需要补的是**传输层之外**的部分（`docs/37` §4.2 已列）：

1. `/api/daemon/ws` 路由注册 + `mount.rs` 的 `mount_slice_daemon()`（锚点**已接好**，无需预删）；
2. 从请求头构造 `ClientIdentity`（含 token/鉴权与 runtime 批量授权查询）；
3. RPC handler = **复用 HTTP handler 的同一份 dispatch**（`docs/16` §4：WS body 字节 ≡ HTTP body 字节）；
4. `tasks.claim` 之后调 `notify_task_available` 等 `Notify*`；
5. runtime 下线路径调 `notify_runtime_gone`；
6. 上游 `hub.go` 里**传输层已覆盖**的 17 条语义（§4）不要再写第二遍。

## 11. 本片没做什么（边界）

- **0 路由**：`crates/mc-http` 一个字节没改；`mount.rs` / `docs/15` 的注册表都没动。
- **0 DB**：不引 `sqlx`、不建迁移、不碰 `mc-repos`。
- **未改 `/live-events`**：`lib.rs` 里既有的 `live_events_handler` 与 `ClientMessage` 语义原样保留
  （两个 ws 是两条独立的通道，`docs/37` §4.1 专门区分过）。
- **不改 M3-7 / M3-8 的文件**：`routes/daemon.rs`、`mc-daemon-proto`、execenv/adapters 都没碰。
- **⑦/⑨ 快照不重生成**：本片 0 路由 ⇒ 基线不该动；重生成会把别的切片的路由混进来。
- **`RuntimeLeases` 与 metrics 不迁入**（D3/D4）：留给 M3-7 按自己的库内设施处理。
