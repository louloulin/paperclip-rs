#![forbid(unsafe_code)]

//! 活动日志：记录用户 / 系统 / 集成的关键动作。
//!
//! 与原 paperclip `server/src/services/activity-log.ts` 等价：
//! - `ActivityKind`：覆盖 issue / decision / approval / agent / plugin 等枚举
//! - `ActivityEvent`：kind + actor + subject + payload + timestamp
//! - `ActivitySink`：trait 抽象，便于 in-mem / db / remote 切换
//! - `ActivityLog`：上层 handle，封装 emit / query 语义

pub mod kinds;
pub mod log;
pub mod sink;
pub mod types;

pub use kinds::ActivityKind;
pub use log::{ActivityLog, InMemoryActivityLog};
pub use sink::{ActivitySink, SharedActivitySink};
pub use types::{ActivityActor, ActivityEvent, ActivityFilter, ActivityId, ActivityQuery};
