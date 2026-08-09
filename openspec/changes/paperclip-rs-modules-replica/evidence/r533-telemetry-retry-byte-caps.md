# R533 / M33 — Telemetry Retry-After 与字节分批

## 完成能力

- `429` 进入 retryable 分支，读取数字秒格式 `Retry-After`。
- 未提供服务端提示时使用指数退避，所有延迟受 30 秒上限约束，最多 5 次。
- 重试复用首次构造的完全相同 HTTP body，保持 events 与 batchId 幂等。
- `maxBodyBytes` 默认 512 KiB；超限批次递归二分，单事件仍超限时记录警告并丢弃。
- `400/413` 等终止响应不会进入 retry/fallback。

## 真实验证

- 真实 TCP server 依次返回 `429 Retry-After: 0` 与 `202`，断言两次 body 完全一致。
- 两个真实 TCP 接收请求，验证双事件批次按字节上限拆为两个单事件 envelope。
- `cargo test -p pc-telemetry --all-targets`：23/23。
- `cargo check -p pc-server`：0 errors。
- `cargo test --workspace --lib`：4934/4934（40 suites）。

## 后续差距

当前重试发生在单次 `flush()` 生命周期内。Node 的异步有界 pending-retry store、最旧批次淘汰、独立 retry timer 取消与 jitter RNG 尚未复刻，归入 M34。
