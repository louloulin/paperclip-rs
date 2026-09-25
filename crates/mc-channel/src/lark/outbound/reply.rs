//! `outbound.rs` 的**回复落点与回落**：`@` 目标 / 三档回复目标 / 话题守卫 / 分类过的会话层
//! 回落 / 凭据解出。
//!
//! - **写者**：M7-13（`docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §32）。
//! - 拆出本文件是**门 ⑩**（单文件 800 行硬限）的要求，切点是「**纯判决** ∥ 投递编排」：
//!   本文件里没有一条 `async fn` 之外的 I/O 决策，也没有任何结构性状态；[`super`] 的
//!   `LarkOutboundDelivery` 只负责"按这些判决投递"。上游把这几段散在 `outbound.go` 里，本仓
//!   按"它们会被**两处**调用（出站面 + 回复器）"这一事实收成一处。
//!
//! # 与 `replier.rs` 的关系（上游逐字的不变式）
//!
//! [`inbound_reply_target`](crate::lark::replier::inbound_reply_target)（回复器用**活的**
//! 入站消息）与 [`thread_reply_target`]（出站面用**按任务冻结**的绑定行）必须给出同一个落点
//! —— 上游注释逐字：*a user cannot tell whether an answer came from the synchronous replier or
//! the task patcher, so they must not place their replies differently.*

use std::future::Future;

use super::super::client::{is_thread_reply_unsupported, ApiError};
use super::super::feishu_channel::credentials::{
    installation_credentials_for, ConfigError, Decrypter,
};
use super::super::params::{InstallationCredentials, ReplyTarget};
use super::super::resolvers::LarkInstallation;
use super::super::store::{BindingConfig, ChatSessionBinding};
use super::super::types::{ChatId, ChatType};
use crate::engine::resolvers::{EngineError, EngineResult};
use serde_json::Value as Json;

// =====================================================================
// 上游的纯函数（逐字）
// =====================================================================

/// 这条回复要 `@` 的 `open_id`，或空串 = 不 `@`（上游 `mentionOpenID`）。
///
/// 上游注释逐字的两条性质，本仓保留：
///
/// - 提到的是**这次**触发的那个账号（不是最近在群里说话的人）⇒ 慢一点的回答不会 `@` 到后来的
///   发言人；
/// - 它是**平台原生身份**，不是从 Multica 成员反查出来的 —— `channel_user_binding` 只在
///   `(installation_id, channel_user_id)` 上唯一，一个成员在同一安装下可以持有多个 `open_id`，
///   按成员反查可能 `@` 错人。
///
/// **只在群聊**：p2p 回复本身就落在 1:1 会话里、自带通知，`@` 在那里纯属噪音。
#[must_use]
pub fn mention_open_id(binding: &ChatSessionBinding) -> String {
    if binding.chat_type != ChatType::Group {
        return String::new();
    }
    binding.last_sender_id.clone().unwrap_or_default()
}

/// 出站回复落到哪里（上游 `threadReplyTarget`）。
///
/// 三档：话题里 ⇒ `reply_in_thread`；普通群 ⇒ 回复触发那条消息；其余（p2p / 没记到触发 id）
/// ⇒ 会话层发送。
#[must_use]
pub fn thread_reply_target(binding: &ChatSessionBinding) -> ReplyTarget {
    let Some(message_id) = binding.last_message_id.as_deref() else {
        return ReplyTarget::default();
    };
    if message_id.is_empty() {
        return ReplyTarget::default();
    }
    if binding
        .last_thread_id
        .as_deref()
        .is_some_and(|id| !id.is_empty())
    {
        return ReplyTarget {
            message_id: message_id.to_string(),
            in_thread: true,
        };
    }
    if binding.chat_type != ChatType::Group {
        return ReplyTarget::default();
    }
    ReplyTarget {
        message_id: message_id.to_string(),
        in_thread: false,
    }
}

/// 这条任务属于**话题隔离**会话、却没有触发消息可回（上游 `topicSendWithoutTrigger`）。
///
/// 上游注释逐字的理由本仓保留：Slack / Telegram 仅凭线程 id 就能把消息放进线程
/// （`thread_ts` / `message_thread_id`），而 Lark 进话题的**唯一**路径是回复话题里的某条消息 ⇒
/// 没有触发时发送会落到 [`ChatSessionBinding::outbound_chat_id`] 解出的**整个会话**上，
/// 于是"某个话题里的提问"的答案会出现在主群。所以**拒绝发送**。
#[must_use]
pub fn topic_send_without_trigger(binding: &ChatSessionBinding) -> bool {
    let has_trigger = binding
        .last_message_id
        .as_deref()
        .is_some_and(|id| !id.is_empty());
    binding.is_topic_isolated() && !has_trigger
}

/// 出站要寻址的真实会话 id（上游 `outboundChatID`）。
#[must_use]
pub fn outbound_chat_id(binding: &ChatSessionBinding) -> ChatId {
    ChatId::new(binding.outbound_chat_id())
}

/// 把投递行的 `config` 补上发送者平台 id（[`super::store::with_channel_sender_id`] 的显式入口）。
#[must_use]
pub fn delivery_config_with_sender(config: Json, sender_id: &str) -> Json {
    crate::lark::store::with_channel_sender_id(config, sender_id)
}

/// 绑定行的 `config` 编码（`{"chat_id": …}`；见 [`BindingConfig::encode`]）。
#[must_use]
pub fn binding_config_json(chat_id: &str) -> Json {
    BindingConfig::encode(chat_id)
}

// =====================================================================
// 分类过的会话层回落（上游 `sendWithReplyFallback`）
// =====================================================================

/// 跑一次发送，**只**在失败是"这条触发消息确实收不到回复"时回落一次。
///
/// 上游注释逐字（本仓逐条保留）：传输错误 / 5xx / 超时 / 限流 / 含义不确定的
/// "服务器可能已经收到"**一律不回落** —— 盲目回落可能重复回复，或把只该在话题里的回复泄进主群。
/// 目标本来就是会话层时没有可回落的东西，错误原样返回。
///
/// 日志只插值 `op`（静态操作名）、回复目标 id 与**错误类别**（`ApiError` 的 `Debug` 里不含
/// 平台 `msg` / URL / 请求体）。
///
/// # Errors
///
/// 回落也失败 ⇒ 带两个原因的错误（**第一个**是原始失败）。
pub async fn send_with_reply_fallback<T, F, Fut>(
    op: &'static str,
    target: ReplyTarget,
    send: F,
) -> Result<T, FallbackError>
where
    F: Fn(ReplyTarget) -> Fut,
    Fut: Future<Output = Result<T, ApiError>>,
{
    let error = match send(target.clone()).await {
        Ok(value) => return Ok(value),
        Err(error) => error,
    };
    if target.is_set() && is_thread_reply_unsupported(&error) {
        tracing::warn!(
            op,
            reply_message_id = target.message_id,
            in_thread = target.in_thread,
            class = error.class().as_str(),
            code = error.code().unwrap_or_default(),
            "lark: reply target unusable, retrying at chat level"
        );
        return match send(ReplyTarget::default()).await {
            Ok(value) => Ok(value),
            Err(fallback) => Err(FallbackError::Send {
                op,
                original: error,
                fallback: Some(fallback),
            }),
        };
    }
    if target.is_set() {
        tracing::warn!(
            op,
            reply_message_id = target.message_id,
            in_thread = target.in_thread,
            class = error.class().as_str(),
            "lark: reply failed; not falling back (non-classified error)"
        );
    }
    Err(FallbackError::Send {
        op,
        original: error,
        fallback: None,
    })
}

/// [`send_with_reply_fallback`] 的失败（上游 `%s: %w` / `%s (chat-level fallback after
/// unusable reply target: %v): %w` 两种形状的**结构化**等价物）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FallbackError {
    /// 第一次发送失败；`fallback` 为 `Some` 时表示回落**也**失败了。
    #[error("lark: {op}: {original}")]
    Send {
        /// 静态操作名。
        op: &'static str,
        /// 原始失败。
        original: ApiError,
        /// 会话层回落的失败（没回落过 ⇒ `None`）。
        fallback: Option<ApiError>,
    },
}

impl FallbackError {
    /// 静态操作名。
    #[must_use]
    pub fn op(&self) -> &'static str {
        match self {
            Self::Send { op, .. } => op,
        }
    }

    /// 原始失败。
    #[must_use]
    pub fn original(&self) -> &ApiError {
        match self {
            Self::Send { original, .. } => original,
        }
    }

    /// 回落是否发生过。
    #[must_use]
    pub fn fell_back(&self) -> bool {
        match self {
            Self::Send { fallback, .. } => fallback.is_some(),
        }
    }
}

// =====================================================================
// 凭据
// =====================================================================

/// 从安装行解出一次发送要的明文凭据（上游 `Patcher::installationCredentials`）。
///
/// # Errors
///
/// 解密器缺失或密文解不开 ⇒ [`EngineError::Infra`]（**不回显**密文）。
pub fn installation_credentials(
    installation: &LarkInstallation,
    decrypt: Option<&Decrypter>,
) -> EngineResult<InstallationCredentials> {
    let Some(decrypt) = decrypt else {
        return Err(EngineError::infra(
            "lark patcher: credentials resolver missing",
        ));
    };
    installation_credentials_for(installation, decrypt).map_err(|error| credentials_error(&error))
}

/// 把凭据面的错误映射成 engine 的错误（**只带类别**）。
#[must_use]
pub fn credentials_error(error: &ConfigError) -> EngineError {
    EngineError::infra(match error {
        ConfigError::Decrypt { .. } => "lark: decrypt app_secret failed".to_string(),
        ConfigError::MissingAppId => "lark: installation config has no app_id".to_string(),
        ConfigError::MissingSecret => {
            "lark: installation config has no app_secret_encrypted".to_string()
        }
        ConfigError::SecretNotBase64 { length } => {
            format!("lark: app_secret_encrypted is not valid base64 ({length} bytes)")
        }
    })
}
