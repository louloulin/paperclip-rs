//! 入站入口：`InboundHandler`（上游 `server/internal/integrations/channel/handler.go`）。
//!
//! - **写者**：M7-0 建（本 anchor）；**M7-1** 填实现方（engine 的单一 handler）。
//!
//! # 上游契约（逐字，别简化）
//!
//! 上游是 `func(ctx, InboundMessage) error`，由 engine **单点注入**给每个 adapter
//! （`Config.Handler`）：engine 的入站处理只写一遍，所有平台汇进它。adapter 拥有自己的
//! 接收循环并调用它；核心**从不**轮询 Channel。两条判据：
//!
//! 1. **非 nil error = 基础设施失败**（核心根本处理不了这条消息：DB 挂了、dispatcher 配错…）。
//!    adapter 应当把它当"投递失败"上报，让 supervisor 的退避/重连接管。
//!    **不得**用于产品性结果。
//! 2. **nil = 消息已被接受并分类**。它仍可能因**正当的产品理由**被丢弃（dedup 命中、
//!    发件人未绑定、群过滤）—— 那**不是**错误。判决带来的任何出站回复（绑定卡 / 离线提示 /
//!    打字指示）是 handler 自己的责任，**脱离 adapter 的 ACK 路径**。
//!
//! `fire-and-classify`：除了 error 没有别的返回值 —— adapter 不因结果分支，把它耦合到
//! 平台专有的结果类型就会毁掉这层抽象。
//!
//! # Rust 形态（本仓的等价物，登记 `docs/32` §10）
//!
//! 上游是函数值（闭包 + 捕获上下文）；本仓用**对象安全的 trait**（`Arc<dyn InboundHandler>`）
//! 承载同一个契约：
//! - 生命周期（谁持有 engine、谁在 drop 时收尾）在 Rust 里必须显式，`Arc<dyn …>` 是最直白的
//!   表达，也让"同一个 handler 注入给 5 个 adapter"变成一次 `Arc::clone`；
//! - `&self` + 内部可变性：上游 handler 闭包捕获的 DB/服务句柄天然是共享的；
//! - **取消语义**：本 crate 不引 `tokio-util`（依赖面一次定死）⇒ 取消由持有 `connect` 的
//!   tokio 任务被 `abort` 表达，handler 只处理"已经收到的"消息。

use std::sync::Arc;

use async_trait::async_trait;
use mc_core::channel::message::InboundMessage;

use crate::channel::ChannelResult;

/// 每个 adapter 都会调用的**共享、平台无关**入站入口（见模块文档的契约）。
///
/// 判决语义：`Ok(())` = 已接受并分类（**包含**"按产品理由丢弃"）；`Err(_)` = 基础设施失败。
#[async_trait]
pub trait InboundHandler: Send + Sync {
    /// 处理一条归一化入站消息。
    async fn handle(&self, message: InboundMessage) -> ChannelResult<()>;
}

/// 注入进 [`crate::channel::ChannelConfig::handler`] 的共享句柄。
///
/// `Arc` 而不是 `Box`：同一个 handler 注入给 5 个平台的 adapter，且 engine 自己也要留一份
/// 引用（出站订阅/握手路径）。
pub type SharedInboundHandler = Arc<dyn InboundHandler>;
