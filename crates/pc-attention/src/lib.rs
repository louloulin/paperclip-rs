#![forbid(unsafe_code)]
//! R632: Attention feed aggregation service.
//!
//! 跨多个 repo 聚合"需要关注"的资源（issue blockers, failed runs, pending approvals,
//! open decisions, budget incidents 等）为统一 feed。
//!
//! 设计目标：
//! - 高内聚：单一 crate 暴露所有 attention 数据
//! - 低耦合：上游 service 只需调用 `list_for_company`，无需直接接触各 repo
//! - 可测：service 单元测试不依赖 HTTP 层

mod service;

pub use service::{
    AttentionCounts, AttentionItem, AttentionItemKind, AttentionService, AttentionSeverity,
};
