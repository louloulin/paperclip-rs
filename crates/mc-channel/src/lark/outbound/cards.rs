//! `outbound.rs` 的**卡片面**：卡片词表 / 渲染器 / 一次性状态卡 / 卡片 patch 的生命周期。
//!
//! - **写者**：M7-13（`docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §32）。
//! - 拆出本文件是**门 ⑩**（单文件 800 行硬限）的要求，切点取上游自己的边界：卡片渲染与
//!   `lark_outbound_card_message` 那条行生命周期（上游 `outbound.go` 的 `Renderer` /
//!   `RenderInput` / `defaultRenderer` / `Patcher` 的 card 那一半）**自成一段**，与
//!   "判决 → 发什么"（[`super`] 的 `LarkOutboundDelivery`）不共享任何状态。
//!
//! # 为什么本片把卡片生命周期落成**真的可调用**
//!
//! 上游注释逐字地写着这条生命周期**已经被缩减**（*the original "thinking → streaming → final
//! card" lifecycle was reduced to a single plain-text reply … The error path is the one survivor
//! of card rendering*），但 `PatcherQueries` 上仍然留着 `CreateLarkOutboundCardMessage` /
//! `UpdateLarkOutboundCardStatus` / `GetLarkOutboundCardByTask` 三个方法且**没有任何调用点**
//! —— 也就是说上游的卡行生命周期是**遗留物**。`docs/60` §6.5 的 M7-13 行要求的正是"卡片 patch
//! 与打字指示的生命周期"，而 [`crate::lark::feishu_channel::FeishuChannel::capabilities`] 已经
//! 声明了 `message_edit` 位 ⇒ 本片把上游缺失的那一半**补上**（登记 D5）。

use std::fmt;
use std::sync::Arc;

use mc_core::id::Id;
use serde_json::json;

use super::super::client::ApiClient;
use super::super::feishu_channel::credentials::Decrypter;
use super::super::params::{PatchCardParams, SendCardParams};
use super::super::store::{CardStatus, NewOutboundCard};
use super::send_with_reply_fallback;
use super::{
    fallback_error, installation_credentials, outbound_chat_id, thread_reply_target,
    DeliveryOutcome, PatcherQueries, SkipReason, INSTALLATION_ACTIVE,
};
use crate::engine::resolvers::{EngineError, EngineResult};

// =====================================================================
// 卡片词表（上游 `CardStatus` / `CardKind` / `CardRender` / `RenderInput`）
// =====================================================================

/// [`Renderer`] 的失败 → engine 的错误。
///
/// 留在这里（而不是 `super`）是因为**只有本文件**会渲染：出站面的错误卡路径与
/// [`LarkCardPatcher`] 的两次渲染都用它。
pub(crate) fn render_error(error: &RenderError) -> EngineError {
    EngineError::infra(error.to_string())
}

/// 卡片变体（上游 `CardKind`）。
///
/// 上游注释逐字：*The Renderer is plug-replaceable so the on-wire card template can evolve
/// without touching the patcher's transport / DB logic.*
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CardKind {
    /// 已受理、还没开始产出。
    #[default]
    Thinking,
    /// 正在产出。
    Running,
    /// 终态、带正文。
    Final,
    /// 失败终态。
    Error,
}

impl CardKind {
    /// 稳定字串（诊断用）。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Thinking => "thinking",
            Self::Running => "running",
            Self::Final => "final",
            Self::Error => "error",
        }
    }

    /// 渲染这个变体之后卡片行该落的状态（上游 `CardStatus` 的对应关系）。
    #[must_use]
    pub fn status(self) -> CardStatus {
        match self {
            Self::Thinking => CardStatus::Pending,
            Self::Running => CardStatus::Streaming,
            Self::Final => CardStatus::Final,
            Self::Error => CardStatus::Error,
        }
    }
}

/// 渲染好的卡片体（上游 `CardRender`；patcher 序列化后交给 [`ApiClient`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardRender {
    /// 整卡 JSON（Lark 的 patch 端点整卡替换 ⇒ 每次都要给完整的一份）。
    pub json: String,
}

/// 渲染器的入参（上游 `RenderInput`）。
///
/// 字段随任务生命周期逐步有值：`issue_number` 在 `/issue` 流程里才有，`content` 在完成的
/// chat 任务上才有，`error_message` 在失败时才有。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RenderInput {
    pub kind: CardKind,
    pub agent_name: String,
    pub issue_number: i64,
    pub issue_id: Option<Id>,
    pub task_id: Option<Id>,
    pub content: String,
    pub error_message: String,
}

/// 渲染失败（上游 `unknown card kind %q` —— 在 Rust 里变体是枚举，这条路径只剩"模板本身"）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RenderError {
    /// 认不出的变体（上游 `fmt.Errorf("unknown card kind %q", in.Kind)`）。
    #[error("lark: unknown card kind {kind}")]
    UnknownKind {
        /// 出错时收到的字面量。
        kind: String,
    },
}

/// 卡片模板（上游 `Renderer` trait）。
///
/// 端口而不是具体类型：模板可以独立演进（或 A/B），而不必拽着 patcher 的传输 / DB 逻辑。
pub trait Renderer: Send + Sync {
    /// 把一个快照渲染成整卡 JSON。
    ///
    /// # Errors
    ///
    /// 见 [`RenderError`]。
    fn render(&self, input: &RenderInput) -> Result<CardRender, RenderError>;
}

/// 生产默认渲染器（上游 `defaultRenderer` + `NewDefaultRenderer`）。
///
/// 上游注释逐字：*minimal text-only cards that work against Lark's generic interactive-card
/// schema* —— 布局会在真正的产品卡设计落地后细化；本默认值**保持接线是真的**（JSON 能对
/// Lark 的 schema 反序列化），同时不把产品绑在某个模板上。
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultRenderer;

/// 卡片头部的默认标题（上游 `"Multica"`，agent 名字缺失时的回落）。
pub const DEFAULT_CARD_HEADER: &str = "Multica";

impl DefaultRenderer {
    /// 生产默认渲染器。
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Renderer for DefaultRenderer {
    fn render(&self, input: &RenderInput) -> Result<CardRender, RenderError> {
        let header = if input.agent_name.is_empty() {
            DEFAULT_CARD_HEADER
        } else {
            input.agent_name.as_str()
        };
        let body = match input.kind {
            CardKind::Thinking => "Thinking…".to_string(),
            CardKind::Running => "Working on it…".to_string(),
            CardKind::Final => {
                if input.content.is_empty() {
                    "Done.".to_string()
                } else {
                    input.content.clone()
                }
            }
            CardKind::Error => {
                if input.error_message.is_empty() {
                    "Run failed.".to_string()
                } else {
                    format!("Run failed: {}", input.error_message)
                }
            }
        };
        // `update_multi` 必须在**每一种**变体上都出现 —— 见模块文档的第 1 条。
        let doc = json!({
            "config": { "wide_screen_mode": true, "update_multi": true },
            "header": {
                "template": "blue",
                "title": { "tag": "plain_text", "content": header },
            },
            "elements": [
                {
                    "tag": "div",
                    "text": { "tag": "plain_text", "content": body },
                },
            ],
        });
        Ok(CardRender {
            json: doc.to_string(),
        })
    }
}

/// 一次性状态卡（上游 `renderNoticeCard`，落在 [`super::replier`] 的离线 / 归档告知路径）。
///
/// 与 [`DefaultRenderer`] 的**生命周期不同**：这些卡是**一次性**的，不会被 patch ⇒
/// `update_multi` 留 `false`（上游注释逐字：*these notice cards are one-shot, so `update_multi`
/// is left false (the card stays as-is)*）。放在这里而不是 `replier.rs`，是因为"卡片 JSON 只
/// 有一个生产者"这件事比"哪个文件调它"更重要。
///
/// 返回值**不可失败**：上游那个 `error` 返回是 `json.Marshal` 的形状，本仓用 `serde_json::Value`
/// 直接构造 ⇒ 没有错误路径。登记为 D4。
#[must_use]
pub fn render_notice_card(header: &str, body: &str) -> String {
    let doc = json!({
        "config": { "wide_screen_mode": true },
        "header": {
            "template": "grey",
            "title": { "tag": "plain_text", "content": header },
        },
        "elements": [
            {
                "tag": "div",
                "text": { "tag": "plain_text", "content": body },
            },
        ],
    });
    doc.to_string()
}

// =====================================================================
// 卡片 patch 的生命周期（专属验收：`message_edit` 能力位）
// =====================================================================

/// 卡片行的开 / 改 / 收口（上游 `lark_outbound_card_message` 那条行生命周期的**可调用**形态）。
///
/// 上游注释逐字地写着这条生命周期**已经被缩减**（*the original "thinking → streaming →
/// final card" lifecycle was reduced to a single plain-text reply … The error path is the one
/// survivor of card rendering*），但它同时把 `CreateLarkOutboundCardMessage` /
/// `UpdateLarkOutboundCardStatus` / `GetLarkOutboundCardByTask` 三个方法**留在**
/// `PatcherQueries` 上，且**没有任何调用点** —— 也就是说上游的卡行生命周期是**遗留物**。
///
/// 本片把它落成**真的可调用、可测**的一段（[`crate::lark::feishu_channel::FeishuChannel`] 的
/// `capabilities()` 已经声明了 `message_edit` 位，`docs/60` §6.5 的 M7-13 行要求的正是"卡片
/// patch 与打字指示的生命周期"）⇒ **补上**上游缺失的那一半，而不是照抄一个死接口。登记为 D5。
pub struct LarkCardPatcher {
    queries: Arc<dyn PatcherQueries>,
    client: Arc<dyn ApiClient>,
    decrypt: Option<Decrypter>,
    renderer: Arc<dyn Renderer>,
}

impl fmt::Debug for LarkCardPatcher {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LarkCardPatcher")
            .field("client", &"<dyn ApiClient>")
            .field("has_decrypter", &self.decrypt.is_some())
            .field("renderer", &"<dyn Renderer>")
            .finish_non_exhaustive()
    }
}

impl LarkCardPatcher {
    /// 装配。
    #[must_use]
    pub fn new(queries: Arc<dyn PatcherQueries>, client: Arc<dyn ApiClient>) -> Self {
        Self {
            queries,
            client,
            decrypt: None,
            renderer: Arc::new(DefaultRenderer),
        }
    }

    /// 接上解密器。
    #[must_use]
    pub fn with_decrypter(mut self, decrypt: Decrypter) -> Self {
        self.decrypt = Some(decrypt);
        self
    }

    /// 开一张卡：渲染 → 发送 → 落 `pending` 行（上游 `CreateLarkOutboundCardMessage` 的调用点）。
    ///
    /// # Errors
    ///
    /// 渲染 / 发送 / 落行任一失败。
    pub async fn begin(
        &self,
        installation_id: Id,
        session_id: Id,
        task_id: Id,
        kind: CardKind,
        input: &RenderInput,
    ) -> EngineResult<DeliveryOutcome> {
        let Some(installation) = self.queries.installation(installation_id).await? else {
            return Ok(DeliveryOutcome::Skipped(SkipReason::InstallationMissing));
        };
        if installation.status != INSTALLATION_ACTIVE {
            return Ok(DeliveryOutcome::Skipped(SkipReason::InstallationRevoked));
        }
        let Some(binding) = self.queries.binding_for_task(task_id).await? else {
            return Ok(DeliveryOutcome::Skipped(SkipReason::NoDeliveryRow));
        };
        let credentials = installation_credentials(&installation, self.decrypt.as_ref())?;
        let render = self
            .renderer
            .render(&RenderInput {
                kind,
                ..input.clone()
            })
            .map_err(|error| render_error(&error))?;
        let params = SendCardParams {
            credentials: credentials.clone(),
            chat_id: outbound_chat_id(&binding),
            card_json: render.json,
            reply_target: thread_reply_target(&binding),
        };
        let target = params.reply_target.clone();
        let client = Arc::clone(&self.client);
        let card_message_id = send_with_reply_fallback("send card", target, |reply_target| {
            let client = Arc::clone(&client);
            let mut params = params.clone();
            params.reply_target = reply_target;
            async move { client.send_interactive_card(params).await }
        })
        .await
        .map_err(|error| fallback_error(&error))?;
        let card = self
            .queries
            .upsert_card(&NewOutboundCard {
                chat_session_id: session_id,
                task_id,
                channel_chat_id: binding.outbound_chat_id(),
                channel_card_message_id: card_message_id,
                status: kind.status(),
            })
            .await?;
        Ok(DeliveryOutcome::Patched {
            card_id: card.id,
            status: CardStatus::from_str_opt(&card.status).unwrap_or(CardStatus::Pending),
        })
    }

    /// 原地 patch 一次（Lark 的 patch 端点**整卡替换** ⇒ 每次都渲染完整的一份）。
    ///
    /// 已收口的行（`final` / `error`）⇒ [`SkipReason::CardSettled`]，**不**patch：
    /// 上游注释逐字地担心"静默 no-op"，本仓把同一件事放到**状态**上判。
    ///
    /// # Errors
    ///
    /// 渲染 / 发送 / 翻状态任一失败。
    pub async fn patch(
        &self,
        installation_id: Id,
        card_id: Id,
        kind: CardKind,
        input: &RenderInput,
    ) -> EngineResult<DeliveryOutcome> {
        let Some(card) = self
            .queries
            .card_by_task(input.task_id.unwrap_or(card_id))
            .await?
        else {
            return Ok(DeliveryOutcome::Skipped(SkipReason::NoCard));
        };
        if card.is_terminal() {
            return Ok(DeliveryOutcome::Skipped(SkipReason::CardSettled));
        }
        let Some(installation) = self.queries.installation(installation_id).await? else {
            return Ok(DeliveryOutcome::Skipped(SkipReason::InstallationMissing));
        };
        let credentials = installation_credentials(&installation, self.decrypt.as_ref())?;
        let render = self
            .renderer
            .render(&RenderInput {
                kind,
                ..input.clone()
            })
            .map_err(|error| render_error(&error))?;
        self.client
            .patch_interactive_card(PatchCardParams {
                credentials,
                card_message_id: card.channel_card_message_id.clone(),
                card_json: render.json,
            })
            .await
            .map_err(|error| EngineError::infra(format!("lark: patch card: {}", error.class())))?;
        let status = kind.status();
        let changed = self.queries.mark_card_status(card.id, status).await?;
        if !changed {
            // 行在这两个往返之间被别的路径收口了（同一任务的另一个副本 / 收尾路径）。
            return Ok(DeliveryOutcome::Skipped(SkipReason::CardSettled));
        }
        Ok(DeliveryOutcome::Patched {
            card_id: card.id,
            status,
        })
    }
}
