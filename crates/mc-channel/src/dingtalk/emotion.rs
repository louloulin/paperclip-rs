//! `DingTalk` 机器人的**表情回应**（上游 `emotion.go` 67 行）。
//!
//! - **写者**：M7-7（`docs/60-M7-PLAN.md` §3.3；本片的写集勘误见 `docs/32` §19）。
//!
//! # 本片只落**平台契约**，调用接缝是端口（为什么）
//!
//! 上游把 `setEmojiReaction` 实现成 `*sender` 的一个方法，而 `sender` 属于
//! **M7-8**（`outbound.go` / `outbound_send.go` / `ack.go`）。写集是"一格 = 一个文件 =
//! 一个写者" ⇒ 本片**不能**定义那个类型，也不该为了一个方法去动 M7-8 的文件。
//!
//! 所以这里落的是这条能力里**与出站实现无关**的那一半（全是可测的判据）：
//!
//! 1. **两个平台枚举名**：`收到`（acknowledge）与 `Done`（完成）—— 它们是 `DingTalk` 内置
//!    表情的**平台名**（`OpenAPI` 的 enum 值，`emotion_167` / `emotion_193`），**不是**产品文案；
//! 2. **两条路径**：`/v1.0/robot/emotion/reply` 与 `/v1.0/robot/emotion/recall`；
//! 3. **请求体**：`robotCode` / `openConversationId` / `openMsgId` / `emotionType`(=1) /
//!    `emotionName`；
//! 4. **三条前置校验**（顺序逐字照上游）：会话 id 与消息 id 都在、`robotCode` 非空、
//!    **名字必须是那两个之一**（别的名字直接拒，不发给平台）；
//! 5. **401 失效 ⇒ 作废令牌缓存并重试一次**（再失败就把那次失败原样返回 —— 上游的循环
//!    在两次尝试后 `return errUnauthorized`）。
//!
//! 调用方（M7-8 的 `sender`）实现 [`EmotionTransport`]（拿它的 `client` 令牌缓存 + `postJSON`），
//! 其余一律走本文件的 [`set_emoji_reaction`] ⇒ 重试与校验只有一份。
//!
//! # 凭据面
//!
//! 本文件**没有**任何凭据字段：访问令牌在 [`EmotionTransport`] 的实现里（M7-8 的
//! `token.go` 缓存面）。[`EmotionError`] 的每个变体都只携带**人可读的失败类别** —— 可见性
//! 用例钉住"错误路径不回显凭据"（`docs/60` §2.3 第 3 条）。

use async_trait::async_trait;
use serde::Serialize;

/// `收到` —— `DingTalk` 内置的 acknowledge 表情（平台 enum 值，**不是**产品文案）。
pub const EMOTION_ACKNOWLEDGED: &str = "收到";
/// `Done` —— `DingTalk` 内置的完成表情（平台 enum 值）。
pub const EMOTION_DONE: &str = "Done";

/// 贴表情的路径（上游 `pathReplyEmotion`）。
pub const PATH_REPLY_EMOTION: &str = "/v1.0/robot/emotion/reply";
/// 撤表情的路径（上游 `pathRecallEmotion`）。
pub const PATH_RECALL_EMOTION: &str = "/v1.0/robot/emotion/recall";

/// `emotionType` 的固定取值（上游逐字写 `1`）。
pub const EMOTION_TYPE_DEFAULT: u8 = 1;

/// 两个受支持的内置表情（上游只认这两个名字）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Emotion {
    /// 收到（`收到`）：入站被受理的"我知道了"。
    Acknowledged,
    /// 完成（`Done`）。
    Done,
}

impl Emotion {
    /// 全部取值（用例与文档遍历用）。
    pub const ALL: [Emotion; 2] = [Emotion::Acknowledged, Emotion::Done];

    /// 平台 enum 名（发给 `DingTalk` 的就是它）。
    #[must_use]
    pub fn platform_name(self) -> &'static str {
        match self {
            Self::Acknowledged => EMOTION_ACKNOWLEDGED,
            Self::Done => EMOTION_DONE,
        }
    }

    /// 从平台名解回枚举（不认识的取值 ⇒ `None`：**绝不**把别的东西当表情发出去）。
    #[must_use]
    pub fn from_platform_name(name: &str) -> Option<Self> {
        match name {
            EMOTION_ACKNOWLEDGED => Some(Self::Acknowledged),
            EMOTION_DONE => Some(Self::Done),
            _ => None,
        }
    }
}

/// 撤表情的路径选择（纯函数：用例不需要网络就能钉住两条路径）。
#[must_use]
pub fn emotion_path(recall: bool) -> &'static str {
    if recall {
        PATH_RECALL_EMOTION
    } else {
        PATH_REPLY_EMOTION
    }
}

/// 贴 / 撤一次表情的请求体（上游那个匿名 `map[string]any` 的**收窄**形态）。
///
/// 它**不带凭据**（访问令牌在 `Authorization` 头里，由 [`EmotionTransport`] 的实现加）⇒
/// `Debug` 可以照常派生。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EmotionRequest {
    #[serde(rename = "robotCode")]
    pub robot_code: String,
    #[serde(rename = "openConversationId")]
    pub conversation_id: String,
    #[serde(rename = "openMsgId")]
    pub message_id: String,
    #[serde(rename = "emotionType")]
    pub emotion_type: u8,
    #[serde(rename = "emotionName")]
    pub emotion_name: String,
}

/// 表情面的失败（**没有**任何变体携带凭据）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EmotionError {
    /// 目标会话 id 缺失（上游第一条守卫）。
    #[error("dingtalk: emoji reaction requires conversation id")]
    MissingConversationId,
    /// 目标消息 id 缺失（上游第二条守卫）。
    #[error("dingtalk: emoji reaction requires message id")]
    MissingMessageId,
    /// `robotCode` 缺失（上游第三条守卫）。
    #[error("dingtalk: emoji reaction requires robot code")]
    MissingRobotCode,
    /// 名字不是那两个内置表情（上游逐字：`unsupported emoji reaction %q`）。
    #[error("dingtalk: unsupported emoji reaction")]
    UnsupportedEmotion,
    /// 401：访问令牌失效（调用方作废缓存并重试**一次**）。
    #[error("dingtalk: access token is invalid")]
    Unauthorized,
    /// 平台回 `success=false`（上游逐字：`%s returned success=false`）。
    #[error("dingtalk: emoji reaction was rejected by the platform")]
    Rejected,
    /// 链路失败（文案由实现保证不含凭据 / URL）。
    #[error("dingtalk: emoji reaction transport failure: {message}")]
    Transport { message: String },
}

/// 组装 + 校验一次表情请求（上游四条守卫的**顺序**照抄：
/// 会话 id → 消息 id → `robotCode` → 名字）。
///
/// # Errors
///
/// 上面四条守卫任一不成立 ⇒ 对应的 [`EmotionError`] 变体。
pub fn emotion_request(
    robot_code: &str,
    conversation_id: &str,
    message_id: &str,
    emotion: Emotion,
) -> Result<EmotionRequest, EmotionError> {
    if conversation_id.is_empty() {
        return Err(EmotionError::MissingConversationId);
    }
    if message_id.is_empty() {
        return Err(EmotionError::MissingMessageId);
    }
    if robot_code.is_empty() {
        return Err(EmotionError::MissingRobotCode);
    }
    Ok(EmotionRequest {
        robot_code: robot_code.to_string(),
        conversation_id: conversation_id.to_string(),
        message_id: message_id.to_string(),
        emotion_type: EMOTION_TYPE_DEFAULT,
        emotion_name: emotion.platform_name().to_string(),
    })
}

/// 贴 / 撤表情的调用接缝（实现归 **M7-8** 的 `sender`：它握着 `client` 的令牌缓存与
/// `postJSON`）。
#[async_trait]
pub trait EmotionTransport: Send + Sync {
    /// 用本安装的访问令牌 POST 一个表情请求。
    ///
    /// 实现要做两件事（上游在循环体里做）：
    /// 1. 取（或铸造）本安装的访问令牌，放进 `Authorization` 头；
    /// 2. 平台回 401 ⇒ [`EmotionError::Unauthorized`]；回 `success=false` ⇒
    ///    [`EmotionError::Rejected`]。
    ///
    /// # Errors
    ///
    /// 见上；链路上的其它失败用 [`EmotionError::Transport`]（文案**不得**带令牌或 URL）。
    async fn post_emotion(&self, path: &str, request: &EmotionRequest) -> Result<(), EmotionError>;

    /// 作废本安装的访问令牌缓存（上游 `s.client.invalidate(s.appKey)`）。
    fn invalidate(&self);
}

/// 贴 / 撤一次表情，**带**那条"401 ⇒ 作废令牌 + 重试一次"的规则（上游 `setEmojiReaction`）。
///
/// 两次都 401 ⇒ 回 [`EmotionError::Unauthorized`]（上游循环末尾的 `return errUnauthorized`）。
///
/// # Errors
///
/// 校验失败、平台拒绝、链路失败、或两次都 401。
pub async fn set_emoji_reaction(
    transport: &dyn EmotionTransport,
    robot_code: &str,
    conversation_id: &str,
    message_id: &str,
    emotion: Emotion,
    recall: bool,
) -> Result<(), EmotionError> {
    let request = emotion_request(robot_code, conversation_id, message_id, emotion)?;
    let path = emotion_path(recall);
    for attempt in 0..2 {
        match transport.post_emotion(path, &request).await {
            Ok(()) => return Ok(()),
            Err(EmotionError::Unauthorized) if attempt == 0 => {
                transport.invalidate();
            }
            Err(error) => return Err(error),
        }
    }
    Err(EmotionError::Unauthorized)
}

#[cfg(test)]
mod tests;
