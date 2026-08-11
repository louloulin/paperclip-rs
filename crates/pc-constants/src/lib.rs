//! `pc-constants` — 从 paperclip `packages/shared/src/constants.ts` 精选 port 的常量。
//!
//! 设计原则：
//! - 按业务域分模块（company / agent / issue / heartbeat / budget / workflow）
//! - 每个常量 `pub const FOO: &[&str]` 与 Node TS 的 `as const` 数组 1:1 对齐
//! - 不重复 port 已经在域 crate 里的常量（DEPLOYMENT_MODES → pc-network-bind；
//!   AGENT_ADAPTER_TYPES → pc-adapter-type；AGENT_STATUSES → pc-agent）
//! - 数字常量保留单位说明
//!
//! R560 首批 port（~50 个常量），后续轮次按需扩展。

pub mod agent;
pub mod budget;
pub mod company;
pub mod heartbeat;
pub mod issue;
pub mod workflow;

pub use agent::*;
pub use budget::*;
pub use company::*;
pub use heartbeat::*;
pub use issue::*;
pub use workflow::*;
