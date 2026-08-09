# R532 / M32 — 产品遥测 fallback 与 server 生命周期

## 对齐能力

- 503/502/504 与网络错误继续尝试备用 endpoint；终止型 4xx 不 fallback。
- 同一批次复用完全相同的序列化 envelope 与 batchId。
- 可启动、可停止的周期 flush，不阻塞 server graceful shutdown。
- `pc-server` 启动时使用 instance telemetry 目录创建客户端，退出时先停止周期任务再最终 flush。
- 对齐 Node opt-out：`PAPERCLIP_TELEMETRY_DISABLED=1`、`DO_NOT_TRACK=1`、CI 环境默认禁用。

## 真实验证

- 两个真实 TCP HTTP receiver：第一个返回 503，第二个返回 202，断言两端 body 完全一致。
- 10ms 周期任务真实发送并停止，断言不会重复发送空队列。
- `cargo test -p pc-telemetry --all-targets`：21/21。
- `cargo check -p pc-server`：0 errors（工作区既有 warnings 不属于本模块）。
- `cargo test --workspace --lib`：所有 suite 均通过，4934/4934。

## 剩余差距

- 尚缺指数退避、Retry-After、pending retry 有界队列和 maxBodyBytes 递归拆分。
- Rust 业务 route/service 尚未逐项接入 Node 已有事件埋点。
