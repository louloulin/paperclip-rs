# R531 / M31 — 产品遥测客户端核心复刻

## Node 对比结论

上游 `packages/shared/src/telemetry/` 提供匿名产品事件客户端；此前 `pc-telemetry` 仅有结构化日志、OTLP 和 feedback trace 上传，缺少持久 install identity、事件队列、ingest envelope、batchId 与 flush 生命周期。

## 本轮实现

- 新增 `crates/pc-telemetry/src/product.rs`，与日志/OTLP 保持独立高内聚边界。
- 持久化并复用 `state.json`：`installId`、salt、createdAt、firstSeenVersion。
- 实现事件入队、最多 50 条分批、Node 兼容 camelCase envelope、SHA-256 128-bit `batchId`。
- 实现真实 `reqwest` POST、非 2xx 报错及失败批次回填，避免事件静默丢失。
- 实现 salt 私有引用哈希，避免上传原始敏感标识。

## 真实验证

- `cargo test -p pc-telemetry --test product_telemetry_e2e`：2/2。
- 测试真实监听本机 TCP，接收 HTTP POST 并校验 envelope 字段与 batchId。
- `cargo test -p pc-telemetry --lib`：16/16。
- `cargo test --workspace --lib`：4934/4934（40 suites）。
- `cargo clippy -p pc-telemetry --all-targets -- -D warnings`：被既有 `feedback_share.rs` 7 个风格 lint 阻塞；M31 新增文件无 clippy 报错。

## 尚未完成

- 尚未把各业务 route/service 的 Node `track(...)` 埋点逐项接入 Rust。
- 尚未实现 Node 的指数退避、双 endpoint fallback、Retry-After、body byte cap 和周期 flush task。
- 尚未在 `pc-server` graceful shutdown 中持有客户端并执行 stop + flush。

因此 M31 完成的是产品遥测“客户端核心”，不是整个 telemetry 业务域 100% 完成。
