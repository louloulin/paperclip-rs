# R534 / M34-b — 异步 pending-retry actor

## 本轮完成

- 客户端内部新增 `pending: RetryQueue<PendingBatch>` 与 attempt 表。
- 新增 `start_background_retry_actor()`：单 worker `select!` 调度 notify + 100ms 周期 tick。
- `enqueue_retry` 在容量满时淘汰最旧批次并记录警告；attempts 超出上限后丢弃。
- `RetryActorHandle::stop()` 唤醒所有等待者并 abort worker，保证 timer 不再触发。
- `final_flush()` 在 server shutdown 时检查 pending 残留。
- `send_blocking` 路径保持原有同步重试 + fallback 行为，不破坏既有契约。

## 真实验证

- `background_retry_recovers_after_endpoint_comes_back`：真实 TCP server 依次返回 429、202；actor 驱动下 bodies 收到 2 次相同 envelope。
- `stop_cancels_pending_retry_timer`：actor 提前 stop 后 pending 批次不再 POST，bodies 长度受限。
- `cargo test -p pc-telemetry --all-targets`：28/28。
- `cargo check -p pc-server`：0 errors。
- `cargo test --workspace --lib`：4934/4934（40 suites）。

## 后续差距

当前 actor 是单进程 in-memory 队列，Node 的 bounded 计数器、timer cancel 计数器与 jitter 注入测试可继续用相同状态机覆盖；尚未做 server 接线（product_telemetry 暂未在 `pc-server` 内启动 actor），留作 M35。
