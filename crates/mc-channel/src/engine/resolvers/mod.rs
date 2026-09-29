//! 端口与流水线词表：engine 跑入站流水线要用的**全部**接缝 + 判决词表。
//!
//! - **写者**：M7-0 建（anchor 只落 `ResolverSet` 的雏形）；**M7-1 重写**（`docs/60`
//!   §3.3 的写集表：`engine/resolvers.rs` 归 M7-1）。
//! - **上游**：`server/internal/integrations/channel/engine/resolvers.go`（433 行，
//!   `M7-1` 的上游文件之一）。逐条对应见下表。
//!
//! **平台专有的一切都在这些 trait 后面**：Router 只认 `ResolverSet`（一台平台一组端口），
//! 核心永远不长出平台分支。这份词表是 M7-2…M7-20 全部切片的**共同语言** ⇒ 必须一次落准，
//! 别让各片各写一份"什么算命中 / 什么算丢弃"。
//!
//! # 上游 → 本文件
//!
//! | 上游 | 本文件 | 备注 |
//! | --- | --- | --- |
//! | `Outcome` / `DropReason` | [`Outcome`] / [`DropReason`] | 取值**逐字**对齐（值班看板按它聚合） |
//! | `Result` | [`RouteResult`] | 改名的原因：`Result` 在 Rust 里是 `std` 的别名，同文件内会互相遮蔽 |
//! | `ResolvedInstallation` / `ResolvedIdentity` | 同名 | `Platform any` → [`ResolvedInstallation::platform`]（`Arc<dyn Any>`） |
//! | `ResolverSet` | [`ResolverSet`] | 结构体（不是 trait）：一台平台一组端口 |
//! | `ErrInstallationNotFound` … `ErrClaimLost` | [`PipelineError`] | 上游用 `errors.Is` 的哨兵；Rust 侧用**枚举变体**（可穷举、可 `match`） |
//! | `InstallationResolver` … `SessionReader` | 同名的 `trait` | 全部 `async_trait`；实现分散在 M7-2…M7-20 的各自写集 |
//! | `NewDBMediaIntentLedger` | **不落** | 它适配上游的 `db.Queries`；本仓的实现在 `mc_repos::channel::media`，由 adapter 片接上（M7-18） |
//!
//! # 三条纪律（逐条可测）
//!
//! 1. **engine 不知道平台**：本文件**不得** `use` 任何 `slack` / `lark` / `dingtalk` /
//!    `wecom` / `telegram` 类型；反向同理，adapter 只拿 [`ResolverSet`] 里注入的 `Arc<dyn …>`。
//!    `engine/mod.rs` 的测试用"源码扫描"钉住这一条。
//! 2. **凭据不进 `Debug`/日志**：本文件的结构只有 id / 词表，**没有**密钥字段；
//!    [`ResolvedInstallation::platform`] 的 `Debug` 手写成 `<opaque>`。
//! 3. **判决不是错误**：丢弃是 `RouteResult` 里的 `Outcome`，**不是** `Err`。

use std::any::Any;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use mc_core::channel::message::{InboundMessage, MediaRef};
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_core::timestamp::Timestamp;

use crate::channel::ChannelError;

// =====================================================================
// 错误：哨兵（上游 `errors.Is` 的那批）→ 枚举变体
// =====================================================================

mod errors;
pub use errors::*;

// =====================================================================
// 解析结果（上游 `ResolvedInstallation` / `ResolvedIdentity`）
// =====================================================================

mod dto;
pub use dto::*;

// =====================================================================
// 命令词表（上游 `fresh_command.go` / `issue_command.go` 的**结论**）
// =====================================================================

mod traits;
pub use traits::*;

// =====================================================================
// 端口包：一台平台一组（上游 `ResolverSet`）
// =====================================================================

mod set;
pub use set::*;

#[cfg(test)]
mod tests;
