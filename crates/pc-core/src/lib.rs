//! Paperclip 领域核心。
//!
//! 高内聚：所有领域类型（实体、值对象、不变量）集中在此。
//! 低耦合：不依赖任何 IO crate（sqlx、tokio 等）。
//! 上层服务（pc-repos、pc-http、pc-heartbeat）依赖本 crate。

pub mod id;
pub mod timestamp;
pub mod money;
pub mod actor;
pub mod actor_runtime;
pub mod error;

pub use id::Id;
pub use timestamp::Timestamp;
pub use money::Money;
pub use actor::Actor;
pub use actor_runtime::{DomainMessage, MessageOrigin, SystemActor, spawn_system_actor};
pub use error::{CoreError, CoreResult};
