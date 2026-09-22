//! Multica 领域模型核心。
//!
//! 包含：
//! - 通用类型：`User`、`Workspace`、`Member` 等基础实体
//! - 不变量校验：优先级 / 状态名 / slug / handle 等
//! - 时间戳 / ID / Hash 工具
//! - actor 抽象
//!
//! 与 paperclip-rs 的 `pc-core` 在风格上保持一致，但领域类型集合是 multica 自己的。
//!
//! 设计原则：
//! - 一个文件 = 一个领域类型
//! - 实体定义公开、内部状态由 typedef + schema 校验保护
//! - 不直接依赖 sqlx（feature-gated），便于 pure logic 单元测试

pub mod id;
pub mod timestamp;
pub mod hash;
pub mod slug;
pub mod priority;
pub mod status;
pub mod actor;
pub mod pagination;

pub mod workspace;
pub mod user;
pub mod member;
pub mod agent;
pub mod runtime;
pub mod issue;
pub mod comment;
pub mod project;
pub mod autopilot;
pub mod squad;
pub mod skill;
pub mod chat;
pub mod inbox;
pub mod plugin;
pub mod channel;
pub mod wakeup;

pub use actor::{spawn_system_actor, ActorKey, ActorRegistry};
pub use hash::{ContentHash, HashAlgo};
pub use id::{Id, IdError, PrefixedId};
pub use pagination::{Page, PageRequest, PageSize, SortOrder};
pub use priority::Priority;
pub use slug::Slug;
pub use status::IssueStatus;
pub use timestamp::Timestamp;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_exports_are_consistent() {
        // sanity: types can be referenced from the crate root
        let _: Id = Id::new();
        let _: Timestamp = Timestamp::now();
        let _: Priority = Priority::default();
        let _: IssueStatus = IssueStatus::Todo;
        let _: Slug = Slug::parse("hello-world").unwrap();
    }
}