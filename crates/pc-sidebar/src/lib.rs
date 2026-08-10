//! `pc-sidebar` —— sidebar 业务聚合。
//!
//! 由 2 个旧 crate 合并而来：
//! - `pc-sidebar-badges`       → [`badges`]
//! - `pc-sidebar-preferences`  → [`preferences`]
//!
//! ## 设计
//! - 高内聚：badges（计数 / dismiss 状态）+ preferences（顺序持久化）共同支撑
//!   "sidebar 渲染" 这一业务能力
//! - 低耦合：两个子模块独立、零共享状态
//! - 业务背景：两个子模块在 Node 中也是相邻 service（`sidebar-badges.ts` +
//!   `sidebar-preferences.ts`），路由层通常会同时拉两者
//!
//! ## 与 Node 的对应
//! - Node `services/sidebar-badges.ts`      → `badges`
//! - Node `services/sidebar-preferences.ts` → `preferences`

#![forbid(unsafe_code)]

pub mod badges;
pub mod preferences;

// 平铺 re-export：兼容旧 crate 的 use path。
pub use badges::*;
pub use preferences::*;
