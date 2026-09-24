//! `Channel` trait 与装配契约（上游 `server/internal/integrations/channel/channel.go`）。
//!
//! - **写者**：M7-0 建（本 anchor）；**M7-1** 填充/收紧（`docs/60` §3.3）。
//! - **本文件是 engine 与 adapter 的编译边界**：engine 只认这五个方法，adapter 只实现它们。
//!
//! # 五方法逐条对应（上游 `channel.Channel`）
//!
//! | 上游 | 本文件 | 语义 |
//! | --- | --- | --- |
//! | `Type() Type` | [`Channel::kind`] | 平台判别式；必须等于注册时用的 kind，且实例生命周期内不变 |
//! | `Connect(ctx) error` | [`Channel::connect`] | **建立连接后阻塞跑接收循环**；ctx 取消 ⇒ `nil`，链路不可本地恢复地断开 ⇒ 非 nil（supervisor 按"这次尝试失败"退避重连） |
//! | `Disconnect(ctx) error` | [`Channel::disconnect`] | 拆链路、释放资源；`connect` 失败后调用**安全**，重复调用**安全**（已断开 ⇒ `Ok`） |
//! | `Send(ctx, out) (SendResult, error)` | [`Channel::send`] | 投递一条出站消息并返回平台消息 id；非 nil error 只留给**真的投递失败**（网络/鉴权/限流） |
//! | `Capabilities() Capability` | [`Channel::capabilities`] | 纯声明、无副作用、结果稳定；**本包不做降级**，调用方自己读位图决定怎么渲染 |
//!
//! # 入站**不**在这个 trait 上（上游注释逐字的意思）
//!
//! 平台把消息推给 adapter，adapter 在构造时拿到 [`crate::message::InboundHandler`]，
//! 由**自己的**接收循环调用它；engine **不**轮询 `Channel`。理由：轮询会强迫每个平台把
//! 长连接语义包装成队列，而 slack 的 Socket Mode / lark 的 WS / telegram 的
//! `getUpdates` 三者的"取一条"语义完全不同。
//!
//! # 取消语义（本仓与上游的**唯一**形态差异，登记 `docs/32` §10）
//!
//! 上游靠 `context.Context` 传递取消；本仓不引 `tokio-util`（锚点的依赖面一次定死），
//! 所以签名里**没有**取消令牌：取消由 supervisor 侧对承载 `connect` 的 `tokio::task`
//! 做 `abort` + 随后 `disconnect` 表达。契约不变：**取消不是错误**，`connect` 在取消后
//! 应返回 `Ok(())`（M7-1 负责把这条写进用例）。

use std::sync::Arc;

use async_trait::async_trait;
use mc_core::channel::message::{OutboundMessage, SendResult};
use mc_core::channel::ChannelKind;
use mc_core::id::Id;

use crate::capability::Capability;
use crate::message::SharedInboundHandler;

/// 渠道运行时的错误。
///
/// **凭据纪律**（`docs/60` §2.3）：任何变体**不得**携带明文 secret / 令牌 / 密文。
/// `Transport` / `Auth` / `Storage` 只带"人可读的失败描述"，这些描述由 adapter 自己
/// 保证不含凭据（各片 `DoD` 有专门的反例用例）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChannelError {
    /// `Registry::build` 没有该 kind 的工厂（上游 `ErrUnknownType`）。
    #[error("channel: no factory registered for type {kind}")]
    UnknownType { kind: String },
    /// 工厂拒绝这份配置（上游要求"返回 error 而不是半成品 Channel"）。
    #[error("channel: invalid configuration for {kind}: {reason}")]
    InvalidConfig { kind: String, reason: String },
    /// 链路层失败：拨号失败、帧格式错、连接被对端关闭且不可本地恢复。
    #[error("channel: transport failure: {message}")]
    Transport { message: String },
    /// 鉴权/授权失败（凭据过期、被撤销、权限不足）。**不得**回显凭据。
    #[error("channel: authentication failure: {message}")]
    Auth { message: String },
    /// 存储层失败（DB / 对象存储）。
    #[error("channel: storage failure: {message}")]
    Storage { message: String },
    /// 租约没能拿到（另一副本持有该 installation 的长连接，见 `docs/60` §2.5）。
    #[error("channel: installation lease is held elsewhere")]
    LeaseHeld,
    /// supervisor 已请求停机（不是错误路径，调用方按"正常收尾"处理）。
    #[error("channel: shut down")]
    Shutdown,
}

impl ChannelError {
    /// 稳定错误码（route 层映射 JSON 错误体时用；与 `docs/60` §2.4 的"未配置"语义分开）。
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnknownType { .. } => "channel_unknown_type",
            Self::InvalidConfig { .. } => "channel_invalid_config",
            Self::Transport { .. } => "channel_transport_error",
            Self::Auth { .. } => "channel_auth_error",
            Self::Storage { .. } => "channel_storage_error",
            Self::LeaseHeld => "channel_lease_held",
            Self::Shutdown => "channel_shutdown",
        }
    }

    /// 是否值得按退避重连（上游 supervisor 的判据：`Connect` 返回非 nil ⇒ 这次尝试失败）。
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Transport { .. } | Self::Storage { .. })
    }
}

/// `Result` 别名（本 crate 的统一错误）。
pub type ChannelResult<T> = Result<T, ChannelError>;

/// 每条安装的归一化配置 —— 工厂消费它来造 [`Channel`]（上游 `channel.Config`）。
///
/// `raw` 是平台自己的凭据/配置 blob（Lark 的 `app_id` / 已封装 `app_secret` / `tenant_key` /
/// `region`，Slack 的 bot/app token…），**不透明**搬运：基础层不因此长出每平台字段。
/// 它直接对应 `channel_installation.channel_type` + `config` JSONB（`MUL-3515` 决策 §3）。
#[derive(Clone)]
pub struct ChannelConfig {
    /// 平台判别式（= 注册该工厂时用的 key）。
    pub kind: ChannelKind,
    /// 平台自己的 JSON blob（见结构文档）。
    pub raw: serde_json::Value,
    /// 造这条 Channel 的 `channel_installation.id`。
    ///
    /// `None` = 该构造路径没有安装行（上游注释：树内目前没有这种路径，但工厂应当容忍，
    /// 而不是假设它一定存在）。WeCom 用它把 per-connection 发送器键进共享注册表；
    /// Feishu / Slack 目前不读它。
    pub installation_id: Option<Id>,
    /// engine 注入的**共享**入站入口（见 [`crate::message::InboundHandler`]）；工厂捕获它，
    /// 在自己的接收循环里调用。纯出站路径（不需要入站投递）可以为 `None`。
    pub handler: Option<SharedInboundHandler>,
}

impl std::fmt::Debug for ChannelConfig {
    /// ⚠️ **手写脱敏**：`raw` 里可能有已封装的凭据（甚至 BYO 明文），所以只打印 kind 与
    /// 有没有 handler/installation，**绝不**打印 `raw`。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChannelConfig")
            .field("kind", &self.kind)
            .field("raw", &"<redacted>")
            .field("installation_id", &self.installation_id)
            .field("handler", &self.handler.is_some())
            .finish()
    }
}

/// 从配置造一个 [`Channel`] 的工厂（上游 `channel.Factory`）。
///
/// 每个 adapter 只在自己的 kind 下注册**一个**工厂；[`crate::registry::Registry`] 调它
/// 实例化 per-installation 的 Channel。工厂应当**校验** `raw` 并返回 `Err`，而不是交出
/// 半成品（上游注释逐字）。
pub type Factory =
    Arc<dyn Fn(ChannelConfig) -> ChannelResult<Arc<dyn Channel>> + Send + Sync + 'static>;

/// 平台中立的渠道契约（上游 `channel.Channel`）。五方法见模块文档的表。
#[async_trait]
pub trait Channel: Send + Sync {
    /// 平台判别式；必须等于注册时用的 kind。
    fn kind(&self) -> ChannelKind;

    /// 建立链路并**阻塞跑接收循环**，直到链路结束。
    ///
    /// - 取消（supervisor 停机）⇒ `Ok(())`；
    /// - 链路掉落且不可本地恢复 ⇒ `Err(_)`（supervisor 按"这次尝试失败"退避重连）。
    ///
    /// 运行期间 adapter 通过构造时捕获的 handler 投递入站消息；[`Channel::send`] 可能在
    /// 另一任务里并发调用。实现必须容忍"在不同任务上重复 connect"（supervisor 会
    /// connect → 返回 → 退避后再 connect）。
    async fn connect(&self) -> ChannelResult<()>;

    /// 拆链路并释放资源。`connect` 失败后调用安全；重复调用安全（已断开 ⇒ `Ok`）。
    async fn disconnect(&self) -> ChannelResult<()>;

    /// 投递一条出站消息，返回平台给的消息 id。非 `Ok` 只留给**真的投递失败**。
    async fn send(&self, out: OutboundMessage) -> ChannelResult<SendResult>;

    /// 本 Channel 支持什么（纯声明，无副作用）。**本包不做降级**：调用方读位图自己决定。
    fn capabilities(&self) -> Capability;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ChannelError` 的稳定码与可重试分类（M7-1 的 route 层要按这两张表映射）。
    #[test]
    fn error_codes_and_retryability_are_stable() {
        let cases = [
            (
                ChannelError::UnknownType {
                    kind: "slack".into(),
                },
                "channel_unknown_type",
                false,
            ),
            (
                ChannelError::InvalidConfig {
                    kind: "lark".into(),
                    reason: "no app_id".into(),
                },
                "channel_invalid_config",
                false,
            ),
            (
                ChannelError::Transport {
                    message: "reset".into(),
                },
                "channel_transport_error",
                true,
            ),
            (
                ChannelError::Auth {
                    message: "revoked".into(),
                },
                "channel_auth_error",
                false,
            ),
            (
                ChannelError::Storage {
                    message: "db down".into(),
                },
                "channel_storage_error",
                true,
            ),
            (ChannelError::LeaseHeld, "channel_lease_held", false),
            (ChannelError::Shutdown, "channel_shutdown", false),
        ];
        for (error, code, retryable) in cases {
            assert_eq!(error.code(), code);
            assert_eq!(error.is_retryable(), retryable, "{code} 的可重试分类");
        }
    }

    /// 配置的 `Debug` **不**回显 `raw`（凭据纪律）。
    #[test]
    fn channel_config_debug_redacts_raw() {
        let config = ChannelConfig {
            kind: ChannelKind::Lark,
            raw: serde_json::json!({ "app_secret_encrypted": "SECRET-B64" }),
            installation_id: Some(Id::new()),
            handler: None,
        };
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("SECRET-B64"), "回显了凭据：{rendered}");
        assert!(rendered.contains("<redacted>"));
        assert!(rendered.contains("Lark"));
    }
}
