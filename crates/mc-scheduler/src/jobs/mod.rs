//! job 注册与分发（M5-8）。
//!
//! - **写者**：M5-8（`jobs/**` 整组）。
//! - **上游**：`scheduler/jobs_autopilot.go`448（449）+ `scheduler/jobs_issue_wakeup.go`21（22）。
//! - **依赖方向**：M5-8 ← M5-7 的内核 + M5-4 的 dispatch + M5-6 的 wakeup
//!   （`docs/44` §4.3 的串行边）⇒ 本组只能在内核与两个面**合并之后**动。
//! - **注册点**：`apps/mc-server/src/main.rs` 的 spawn 块由 M5-7 写、M5-8 往注册表加 2 行
//!   （串行边，不是并发写）。
pub mod autopilot;
pub mod issue_wakeup;
