//! `pc-feedback` —— Feedback 业务层聚合。
//!
//! 由 5 个旧 crate 合并而来：
//! - `pc-feedback-redaction`  → [`redaction`]
//! - `pc-feedback-share`      → [`share`]
//! - `pc-feedback-share-client` → [`share::client`]
//! - `pc-feedback-trace`      → [`trace`]
//! - `pc-feedback-vote`       → [`vote`]
//!
//! ## 设计
//! - 高内聚：5 个子模块都属于 feedback lifecycle（redact → record → share → vote）
//! - 低耦合：每个子模块独立，可单独测试 / 替换
//! - 底层 HTTP 实现仍在 `pc_telemetry::feedback_share`（不归本 crate 管）
//!
//! ## 与 Node 的对应
//! - Node `services/feedback-redaction.ts`         → `redaction`
//! - Node `services/feedback-share.ts`             → `share`
//! - Node `services/feedback-share-client.ts`      → `share::client`
//! - Node `services/feedback-trace.ts` (DB trace)  → `trace`
//! - Node `services/feedback.ts` (vote logic)      → `vote`

#![forbid(unsafe_code)]

pub mod pure;
pub mod redaction;
pub mod share;
pub mod trace;
pub mod vote;

// 平铺 re-export：兼容旧 crate 的 use path
pub use redaction::*;
pub use share::*;
pub use trace::*;
pub use vote::*;
