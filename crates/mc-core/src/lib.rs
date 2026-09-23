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

pub mod actor;
pub mod hash;
pub mod id;
pub mod pagination;
pub mod priority;
pub mod slug;
pub mod status;
pub mod timestamp;

pub mod agent;
pub mod autopilot;
// M5-0 anchor（LUM-1563）：quota 两张表（`352` / `448`）与 autopilot 主表不同源、不同生命周期，
// 单独一个模块（`autopilot.rs` 已有 5 个类型组，再塞进去会逼近单文件门 ⑩）。
pub mod autopilot_quota;
pub mod channel;
pub mod chat;
pub mod comment;
pub mod inbox;
pub mod issue;
pub mod member;
pub mod plugin;
pub mod project;
pub mod runtime;
pub mod skill;
pub mod squad;
pub mod user;
pub mod wakeup;
pub mod workspace;

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
