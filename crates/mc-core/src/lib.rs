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
// M9 anchor scaffold（LUM-1815 / docs/62-M9-PLAN.md §2.1 / §5）：M9 的四个领域面一次性声明，
// 让 M9-1..M9-11 不再同时编辑本文件。四个模块的**完整类型形状**在 anchor 落定
// （各切片的仓储/HTTP 面只读引用）：
// - `cloud`：云侧四个请求 DTO + 口径常量（**没有**响应结构体 —— billing/subscriptions 是
//   纯出站代理，云侧响应原样透传，建了就是第二个真相源）；
// - `onboarding`：完成路径 / 问卷答案（含 `stringOrSlice` 宽容）/ 5 个上游列的投影；
// - `notification`：通知偏好分组词表（7 组 × 2 值，上游 `validNotifGroups`）；
// - `dashboard`：dashboard 6 条的**行形状**与 cutoff/tz/days 口径。
// ⚠️ `mc_core::cloud` 与 `mc_repos::cloud`（后者本波不建）不是一回事：本波 cloud 面无表。
pub mod cloud;
pub mod comment;
// M8 anchor scaffold（LUM-1797 / docs/61-M8-PLAN.md §3.1 / §5）：M8 的四个领域面一次性声明，
// 让 M8-1..M8-7 不再同时编辑本文件。四个模块的**完整类型形状**在 anchor 落定（各切片的
// 运行时/仓储/HTTP 面只读引用）：
// - `vcs`：`VcsProviderKind` / `VcsConnection` / `VcsPullRequest` / `VcsCommitStatus`；
// - `github`：`GitHubInstallation` / `GitHubPullRequest` / `IssuePrLink` / `PullRequestSnapshot`；
// - `mcp`：`WorkspaceMcpServer` / `McpTransport` / `McpBinding` + `mcp::overlay`（M8-3 填）；
// - `composio`：`ComposioConnection` / `ComposioToolkit`。
// ⚠️ `vcs` 与 `github` **两套并列、不合并**（docs/61 §1.6）；MCP 面**复用** daemon 侧语义、
// **不**与 `mc-mcp`（remote MCP 客户端）混同（docs/61 §2.3）。
pub mod composio;
// M9-1（LUM-1816）的 dashboard 6 条行形状（见文档：`mc_core::dashboard` 的模块头）。
pub mod dashboard;
pub mod github;
pub mod inbox;
pub mod issue;
pub mod mcp;
pub mod member;
// M9-3（LUM-1818）的 onboarding 形状。
pub mod notification;
pub mod onboarding;
pub mod plugin;
pub mod project;
pub mod runtime;
pub mod skill;
pub mod squad;
pub mod user;
pub mod vcs;
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
