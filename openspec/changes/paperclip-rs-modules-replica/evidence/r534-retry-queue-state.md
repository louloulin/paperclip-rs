# R534 / M34-a — RetryQueue 状态机

## 本轮完成

- 新增独立 `crates/pc-telemetry/src/retry_queue.rs` 泛型有界队列。
- 超出容量时淘汰最旧批次，并返回被淘汰 payload，调用方可记录丢失。
- `drain_due` 只取到期项，未来项保持队列内。
- 新增 `RetryBackoff`，实现指数增长、对称 jitter、最大延迟封顶。
- 产品客户端退避计算统一复用 `RetryBackoff`，不再自有第二套算法。

## 验证

- `retry_queue_contract`：3/3。
- `pc-telemetry --all-targets`：26/26。
- `cargo check -p pc-server`：0 errors。

## 未完成

`RetryQueue` 目前是纯状态模块，尚未替换产品客户端当前 flush 内同步 retry 为异步 actor/timer；pending payload 的 attempt 元数据、timer cancel、stop drain 仍是 M34-b。
