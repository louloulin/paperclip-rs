//! 用户绑定令牌：`channel_binding_token`（迁移 `124`）的领域投影。
//!
//! - **写者**：M7-0 anchor（本片；`docs/60-M7-PLAN.md` §2.1）。落地后**各片只读**。
//! - **上游**：`migrations/upstream/124_channel_generalization.up.sql` 的
//!   `CREATE TABLE channel_binding_token`，以及 `internal/integrations/{dingtalk,wecom,
//!   slack}/binding.go` 的 `BindingToken` / `BindingTokenService`。
//!
//! # 流（为什么令牌长这样）
//!
//! 一个**未绑定**的平台用户给 bot 发消息 ⇒ adapter 侧 mint 一个一次性令牌，把兑换链接
//! 发给他（"link your account" 卡片）⇒ 用户在 web 端点 `/api/{platform}/binding/redeem`
//! 兑换 ⇒ 落一行 `channel_user_binding`。这条链路**在 workspace 上下文之前**发生
//! （兑换者是会话身份 + 令牌里的外部 user id），所以五条 redeem 路由**没有** workspace
//! 前缀（`docs/60` §1.1 的三簇之一）。
//!
//! # 三条硬口径（别"顺手放宽"）
//!
//! 1. **只存哈希**：明文令牌只在 mint 的那一刻返回一次（写进链接），库里只有
//!    `token_hash`（`TEXT PRIMARY KEY`）。所以 [`BindingToken`] 里的 `raw` **不落库**。
//! 2. **TTL 上限 15 分钟**：`channel_binding_token_ttl_cap` 的 `CHECK
//!    (expires_at <= created_at + INTERVAL '15 minutes')` 与上游 `channel.BindingTokenTTL`
//!    必须同步；本文件用 [`BindingTokenTtl`] 把上限钉在一个常量上，**不许**各平台各写一个。
//! 3. **一次性**：`consumed_at` 非空即已消费。契约是"未知 / 已消费 / 过期"三种情形
//!    **返回同一个错误**（避免用错误码做重放计时侧信道，上游 `ErrBindingTokenInvalid`
//!    的注释逐字如此）—— 所以 [`RedeemedBindingToken`] 只在成功路径出现。
//!
//! # 不做什么
//!
//! - **不**实现 mint / redeem 的 SQL 与事务（那是各平台片的仓储层：消费令牌与插入绑定行
//!   必须同事务提交，失败的绑定不能烧掉令牌）；
//! - **不**做 workspace 成员校验（那是 redeem handler 的 403 映射）；
//! - **不**在这里给令牌生成随机数（`rand` 依赖不进 `mc-core`）。

use serde::{Deserialize, Serialize};

use super::ChannelKind;
use crate::id::Id;
use crate::timestamp::Timestamp;

/// 绑定令牌的 TTL 上限（`channel_binding_token_ttl_cap` 的 15 分钟）。
///
/// 新类型而不是裸 `i64`：列上的 `CHECK`、上游的常量、以及各平台片的 mint 调用点
/// **共用同一个值**，避免三处各写 `900` 然后飘。构造只允许 `<= MAX`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BindingTokenTtl(u64);

impl BindingTokenTtl {
    /// 上限（秒）：15 分钟 —— 与 `channel_binding_token_ttl_cap` 逐字一致。
    pub const MAX_SECONDS: u64 = 15 * 60;

    /// 构造：超过上限返回 `None`（**不截断** —— 截断会把"配置写错了"变成"静默生效"）。
    pub fn from_seconds(seconds: u64) -> Option<Self> {
        (seconds <= Self::MAX_SECONDS).then_some(Self(seconds))
    }

    /// 上限本身（各平台片的默认值）。
    pub fn max() -> Self {
        Self(Self::MAX_SECONDS)
    }

    pub fn as_seconds(self) -> u64 {
        self.0
    }
}

impl Default for BindingTokenTtl {
    fn default() -> Self {
        Self::max()
    }
}

/// 一条**刚 mint 出来**的绑定令牌。
///
/// - [`BindingToken::raw`] 是明文：只在 mint 处存在一次（嵌进链接），**绝不落库、绝不进日志**；
/// - [`BindingToken::token_hash`] 是唯一持久化的那一半（`token_hash TEXT PRIMARY KEY`）；
/// - `raw` 与 `token_hash` **都不进序列化输出**（`serde` 跳过）：响应只需要明文令牌与
///   过期时间，而哈希是服务端比对用的内部值 —— 少一个出口就少一个泄露面。
///
/// 手写 `Debug`：脱敏实现（`docs/60` §2.3 的凭据纪律 —— 承载令牌的类型不派生 `Debug`）。
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingToken {
    /// 明文令牌（**响应里出现一次**，之后只剩哈希）。
    #[serde(skip)]
    pub raw: String,
    /// 持久化的哈希（`channel_binding_token.token_hash`）。
    #[serde(skip_serializing)]
    pub token_hash: String,
    pub workspace_id: Id,
    pub installation_id: Id,
    pub kind: ChannelKind,
    /// 平台原生、安装内稳定的用户 id（`channel_user_id`）。
    pub channel_user_id: String,
    pub expires_at: Timestamp,
    /// 已消费时间（`consumed_at`）；`None` = 未消费。
    pub consumed_at: Option<Timestamp>,
    pub created_at: Timestamp,
}

impl std::fmt::Debug for BindingToken {
    /// 手写脱敏：只打印能安全进日志的字段（哈希是**只验证不泄露**的那一半，但仍不打印，
    /// 免得被人拿去做离线比对）。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BindingToken")
            .field("raw", &"<redacted>")
            .field("token_hash", &"<redacted>")
            .field("workspace_id", &self.workspace_id)
            .field("installation_id", &self.installation_id)
            .field("kind", &self.kind)
            .field("expires_at", &self.expires_at)
            .field("consumed_at", &self.consumed_at)
            .finish_non_exhaustive()
    }
}

impl BindingToken {
    /// 是否已消费（`consumed_at IS NOT NULL`）。
    pub fn is_consumed(&self) -> bool {
        self.consumed_at.is_some()
    }

    /// 在 `now` 时刻是否已过期（**严格**大于：`expires_at == now` 视为仍有效，
    /// 与上游 `time.Now().After(expiresAt)` 的边界一致）。
    pub fn is_expired_at(&self, now: Timestamp) -> bool {
        now > self.expires_at
    }

    /// 可兑换 = 未消费 **且** 未过期。
    pub fn is_redeemable_at(&self, now: Timestamp) -> bool {
        !self.is_consumed() && !self.is_expired_at(now)
    }

    /// `channel_type` 的**存储**取值（别用 `kind.as_str()`，Lark 存 `feishu`）。
    pub fn channel_type(&self) -> &'static str {
        self.kind.storage_str()
    }
}

/// 兑换成功后的结果（上游 `dingtalk.RedeemedBindingToken`）。
///
/// 兑换是**事务性**的：消费令牌与插入 `channel_user_binding` 一起提交 —— 失败的绑定不
/// 烧令牌。所以本结构只在事务已提交时构造。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedeemedBindingToken {
    pub workspace_id: Id,
    pub installation_id: Id,
    pub kind: ChannelKind,
    /// 被绑定的平台用户 id（写进 `channel_user_binding.channel_user_id`）。
    pub channel_user_id: String,
}

impl RedeemedBindingToken {
    /// `channel_type` 的**存储**取值。
    pub fn channel_type(&self) -> &'static str {
        self.kind.storage_str()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// TTL 上限与列的 `CHECK` 逐字一致：15 分钟，**超一点都不行**（构造返回 `None`）。
    #[test]
    fn ttl_cap_matches_the_check_constraint() {
        assert_eq!(BindingTokenTtl::MAX_SECONDS, 900);
        assert_eq!(BindingTokenTtl::max().as_seconds(), 900);
        assert_eq!(BindingTokenTtl::default().as_seconds(), 900);
        assert_eq!(
            BindingTokenTtl::from_seconds(900).map(BindingTokenTtl::as_seconds),
            Some(900)
        );
        assert_eq!(
            BindingTokenTtl::from_seconds(901).map(BindingTokenTtl::as_seconds),
            None,
            "超上限必须拒，而不是截断成 900"
        );
        assert_eq!(BindingTokenTtl::from_seconds(0).unwrap().as_seconds(), 0);
    }

    fn token(expires_at: Timestamp, consumed_at: Option<Timestamp>) -> BindingToken {
        BindingToken {
            raw: "plaintext-DO-NOT-LOG".into(),
            token_hash: "sha256:deadbeef".into(),
            workspace_id: Id::new(),
            installation_id: Id::new(),
            kind: ChannelKind::Lark,
            channel_user_id: "ou_1".into(),
            expires_at,
            consumed_at,
            created_at: Timestamp::now(),
        }
    }

    /// 可兑换性的三个边界：未过期未消费 / 已消费 / 已过期（含 `expires_at == now` 仍有效）。
    #[test]
    fn redeemability_covers_the_three_outcomes() {
        let now = Timestamp::now();
        let fresh = token(now, None);
        assert!(fresh.is_redeemable_at(now), "expires_at == now 仍有效");
        assert!(!fresh.is_expired_at(now));

        // 过一秒即过期。
        let later = Timestamp::from(now.as_datetime() + chrono::Duration::seconds(1));
        assert!(fresh.is_expired_at(later));
        assert!(!fresh.is_redeemable_at(later));

        // 已消费：未过期也不能再兑换（一次性）。
        let consumed = token(later, Some(now));
        assert!(consumed.is_consumed());
        assert!(!consumed.is_redeemable_at(now));
    }

    /// 存储口径：Lark 令牌写 `feishu`。
    #[test]
    fn binding_rows_use_the_storage_slug() {
        let now = Timestamp::now();
        assert_eq!(token(now, None).channel_type(), "feishu");
        let redeemed = RedeemedBindingToken {
            workspace_id: Id::new(),
            installation_id: Id::new(),
            kind: ChannelKind::WeCom,
            channel_user_id: "u1".into(),
        };
        assert_eq!(redeemed.channel_type(), "wecom");
    }

    /// 凭据纪律：`Debug` 不回显明文令牌，也不回显哈希；`serde` 序列化不带 `raw`。
    #[test]
    fn debug_and_serde_never_expose_the_raw_token() {
        let now = Timestamp::now();
        let value = token(now, None);
        let rendered = format!("{value:?}");
        assert!(
            !rendered.contains("plaintext-DO-NOT-LOG"),
            "回显了明文：{rendered}"
        );
        assert!(!rendered.contains("deadbeef"), "回显了哈希：{rendered}");
        assert!(rendered.contains("<redacted>"));

        let encoded = serde_json::to_string(&value).expect("serialize");
        assert!(!encoded.contains("plaintext-DO-NOT-LOG"));
        assert!(!encoded.contains("deadbeef"));
    }
}
