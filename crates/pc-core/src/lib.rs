//! Paperclip 领域核心。
//!
//! 高内聚：所有领域类型（实体、值对象、不变量）集中在此。
//! 低耦合：不依赖任何 IO crate（sqlx、tokio 等）。
//! 上层服务（pc-repos、pc-http、pc-heartbeat）依赖本 crate。

pub mod actor;
pub mod actor_runtime;
pub mod error;
pub mod id;
pub mod money;
pub mod timestamp;

pub use actor::Actor;
pub use actor_runtime::{spawn_system_actor, DomainMessage, MessageOrigin, SystemActor};
pub use error::{CoreError, CoreResult};
pub use id::Id;
pub use money::Money;
pub use timestamp::Timestamp;
