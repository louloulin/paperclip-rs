//! 入站判决驱动的**出站面**（脱离 connector 的 ACK 路径）。
//!
//! - **写者**：M7-1（`docs/60` §3.3）。上游 `router.go` 的 `scheduleReply` +
//!   "on ingest 点亮打字指示"两段。
//! - 两份都在 `tokio::spawn` 里跑：adapter 的 `route()` 返回（= ACK）**不等**它们。
//!   `None` 端口 = 该平台没有这一面（关掉即可），不是一个"待实现"的 `todo!()`。
//! - 拆出本文件是门 ⑩ 的要求（`router.rs` 超 800 行 ⇒ 拆）。

use std::sync::Arc;

use mc_core::channel::message::InboundMessage;

use crate::engine::resolvers::{Outcome, ResolverSet, RouteResult};

use super::Router;

impl Router {
    /// 打字指示器（脱离式；`None` 端口 = 平台没这一面）。
    pub(super) fn emit_typing(
        set: &Arc<ResolverSet>,
        installation: &crate::engine::resolvers::ResolvedInstallation,
        message: &InboundMessage,
        result: &RouteResult,
    ) {
        let Some(typing) = set.typing.clone() else {
            return;
        };
        if result.outcome != Outcome::Ingested || !result.run_scheduled {
            return;
        }
        let Some(session_id) = result.chat_session_id else {
            return;
        };
        let (installation, message) = (installation.clone(), message.clone());
        tokio::spawn(async move {
            typing.on_ingested(&installation, &message, session_id);
        });
    }

    /// 出站回复（脱离式；`None` 端口 = 不回）。
    pub(super) fn emit_reply(
        set: &Arc<ResolverSet>,
        installation: &crate::engine::resolvers::ResolvedInstallation,
        message: &InboundMessage,
        result: &RouteResult,
    ) {
        let Some(replier) = set.replier.clone() else {
            return;
        };
        let (installation, message, result) =
            (installation.clone(), message.clone(), result.clone());
        tokio::spawn(async move {
            replier.reply(&installation, &message, &result);
        });
    }
}
