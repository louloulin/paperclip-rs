# R535 / M35 — Telemetry 接入 pc-server 生命周期

## 本轮完成

- `pc-server` 在 telemetry 客户端创建后立即启动后台 retry actor。
- shutdown 时按顺序：停周期 flush → 停 actor → `final_flush()`（包含 pending 残留检查）→ 关闭 actors。
- 引入 `RetryActorHandle` 到 server import。

## 验证

- `cargo check -p pc-server`：0 errors。
- `pc-telemetry --all-targets`：28/28。
- `cargo test --workspace --lib`：4934/4934（40 suites）。

## 剩余差距

Telemetry 子系统在 Rust 端核心链路已完整：状态持久化、事件入队、字节分批、retry-after、退避、fallback、周期任务、后台 actor、shutdown 顺序与残留报告。下一步应转向核心业务模块（业务事件埋点、`pc-authz` 完整复刻、远程 bridge IPC）。
