//! Channel 领域类型：6 个内置 + 自定义。
//!
//! # 本文件的角色（M7-0 anchor 扩展，`LUM-1765` / `docs/60-M7-PLAN.md` §2.1）
//!
//! 本文件是 M7（W7 渠道面）的**领域层**：`ChannelKind` 与安装行投影。
//! 渠道的运行时（trait / registry / adapter / engine）在 `mc-channel`，仓储在
//! `mc_repos::channel`，HTTP 面在 `mc_http::routes::channels`。**三者都引用本文件**，
//! 所以跨 crate 的枚举与投影只在这里定义一次。
//!
//! ## 三个字符串口径（**别把它们混起来**，M7 最常见的坑）
//!
//! | 口径 | 出处 | `ChannelKind::Lark` 的取值 |
//! | --- | --- | --- |
//! | [`ChannelKind::as_str`] | 路由前缀 `/api/workspaces/{id}/lark/…`、`issue.origin = 'lark_chat'` | `lark` |
//! | [`ChannelKind::storage_str`] | `channel_installation.channel_type` 等渠道表的**存储**列（上游 `channel.Type`） | **`feishu`** |
//! | [`ChannelKind::secret_key_env`] | 部署密钥的 env 名（上游 `secretbox.LoadKey` 的实参） | `MULTICA_LARK_SECRET_KEY` |
//!
//! 上游把 Lark/Feishu 存成 `feishu`（`channel.TypeFeishu = "feishu"`）而 HTTP 面写
//! `lark`；本仓的 `as_str()` 历史上取的是后者（M4 的 `IssueOrigin` 与路由都已按它落），
//! 所以**存储口径必须单列一个函数**，而不是让 20 个切片各自 `if kind == Lark { "feishu" }`。
//!
//! ## 遗留投影（**不删、也别用**）
//!
//! [`ChannelInstallation`] 是 M2 期的脚手架投影：它的 `external_id` / `display_name` /
//! `enabled` 三列在真实 `channel_installation` 表里**不存在**（真实列见
//! [`installation::Installation`]）。它的 shape 本片**不动**（`docs/60` §5 的硬要求），
//! 但它**不是** M7 的真值 —— 新代码一律用 [`installation::Installation`]。
//! 收敛（删或改名）不在 M7 写集内，登记在 `docs/32` §10。

use serde::{Deserialize, Serialize};

use super::id::Id;
use super::timestamp::Timestamp;

pub mod binding;
pub mod install_session;
pub mod installation;
pub mod message;

pub use binding::{BindingToken, BindingTokenTtl, RedeemedBindingToken};
pub use install_session::{InstallSession, RegistrationSessionStatus};
pub use installation::{Installation, InstallationStatus};
pub use message::{
    ChatType, InboundMessage, MediaRef, MessageKind, OutboundMessage, ReplyCtx, SendResult, Source,
};

/// Channel 类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelKind {
    Slack,
    Lark,
    DingTalk,
    WeCom,
    Telegram,
    Custom,
}

impl ChannelKind {
    /// M7 的五个内置平台（**不含** `Custom`）：它们各自有部署密钥、路由前缀与 adapter。
    pub const BUILTIN: [ChannelKind; 5] = [
        Self::Slack,
        Self::Lark,
        Self::DingTalk,
        Self::WeCom,
        Self::Telegram,
    ];

    /// 路由前缀 / `issue.origin` 口径的小写 slug（**不是**存储口径，见模块文档）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Slack => "slack",
            Self::Lark => "lark",
            Self::DingTalk => "dingtalk",
            Self::WeCom => "wecom",
            Self::Telegram => "telegram",
            Self::Custom => "custom",
        }
    }

    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "slack" => Some(Self::Slack),
            "lark" => Some(Self::Lark),
            "dingtalk" => Some(Self::DingTalk),
            "wecom" => Some(Self::WeCom),
            "telegram" => Some(Self::Telegram),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }

    /// `channel_*` 表的 `channel_type` 列口径（上游 `channel.Type`）。
    ///
    /// 与 [`ChannelKind::as_str`] **只差一个值**：Lark/Feishu 存 `feishu`，路由与
    /// `issue.origin` 用 `lark`（见模块文档的表）。
    pub fn storage_str(self) -> &'static str {
        match self {
            Self::Lark => "feishu",
            other => other.as_str(),
        }
    }

    /// 从 `channel_type` 列的取值解回枚举（`from_str_opt` 的存储口径，**接受**
    /// `feishu`）。
    pub fn from_storage_str(s: &str) -> Option<Self> {
        match s {
            "feishu" => Some(Self::Lark),
            other => Self::from_str_opt(other),
        }
    }

    /// 该平台部署密钥的 env 名（上游 `cmd/server/router.go` 的 `secretbox.LoadKey` 实参）。
    ///
    /// 唯一读取口在 `mc_http::state::ChannelKeys`：这张表放在 `mc-core` 是**故意的**
    /// （`mc-server` 的装配点与 `mc-http` 的配置入口都要它，两处各写一张表就会漂移）。
    /// `None` = 该 kind 没有部署密钥（只有 `Custom`）。
    pub fn secret_key_env(self) -> Option<&'static str> {
        match self {
            Self::Slack => Some("MULTICA_SLACK_SECRET_KEY"),
            Self::Lark => Some("MULTICA_LARK_SECRET_KEY"),
            Self::DingTalk => Some("MULTICA_DINGTALK_SECRET_KEY"),
            Self::WeCom => Some("MULTICA_WECOM_SECRET_KEY"),
            Self::Telegram => Some("MULTICA_TELEGRAM_SECRET_KEY"),
            Self::Custom => None,
        }
    }

    pub fn all() -> [ChannelKind; 6] {
        [
            Self::Slack,
            Self::Lark,
            Self::DingTalk,
            Self::WeCom,
            Self::Telegram,
            Self::Custom,
        ]
    }
}

impl std::fmt::Display for ChannelKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Channel 主体（installation）。
///
/// ⚠️ **遗留投影**，见模块文档「遗留投影」一节：M7 的真值是
/// [`installation::Installation`]，新代码不要读本结构。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelInstallation {
    pub id: Id,
    pub workspace_id: Id,
    pub kind: ChannelKind,
    pub external_id: String,
    pub display_name: String,
    pub config: serde_json::Value,
    pub enabled: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_round_trip() {
        for k in ChannelKind::all() {
            assert_eq!(ChannelKind::from_str_opt(k.as_str()), Some(k));
        }
    }

    #[test]
    fn unknown_kind_returns_none() {
        assert!(ChannelKind::from_str_opt("discord").is_none());
    }

    /// 三个字符串口径**互不相等**且各自稳定（M7-0 anchor 立的判据）。
    #[test]
    fn lark_has_three_distinct_slugs() {
        assert_eq!(ChannelKind::Lark.as_str(), "lark");
        assert_eq!(ChannelKind::Lark.storage_str(), "feishu");
        assert_eq!(
            ChannelKind::Lark.secret_key_env(),
            Some("MULTICA_LARK_SECRET_KEY")
        );
        // 存储口径双向：`feishu` **必须**能解回 Lark（上游只写这一个值）。
        assert_eq!(
            ChannelKind::from_storage_str("feishu"),
            Some(ChannelKind::Lark)
        );
        assert_eq!(
            ChannelKind::from_storage_str("lark"),
            Some(ChannelKind::Lark)
        );
    }

    /// 其余四家的两个字符串口径**相同**（只有 Lark/Feishu 有别名）。
    #[test]
    fn other_platforms_share_one_slug() {
        for kind in [
            ChannelKind::Slack,
            ChannelKind::DingTalk,
            ChannelKind::WeCom,
            ChannelKind::Telegram,
            ChannelKind::Custom,
        ] {
            assert_eq!(kind.as_str(), kind.storage_str());
        }
        assert_eq!(ChannelKind::BUILTIN.len(), 5);
        assert_eq!(ChannelKind::Custom.secret_key_env(), None);
    }

    /// 部署密钥表的完整性：五个内置平台各一条、env 名唯一且带 `MULTICA_` 前缀。
    #[test]
    fn every_builtin_platform_declares_its_secret_key() {
        let mut names: Vec<&str> = ChannelKind::BUILTIN
            .iter()
            .map(|kind| kind.secret_key_env().expect("内置平台都有部署密钥"))
            .collect();
        assert!(names.iter().all(|name| name.starts_with("MULTICA_")));
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 5, "五个平台各自一把密钥，不能重复");
    }

    /// 遗留投影的既有 shape 不被本片改动（`docs/60` §5 的硬要求）。
    #[test]
    fn legacy_installation_projection_shape_is_stable() {
        let legacy = ChannelInstallation {
            id: Id::new(),
            workspace_id: Id::new(),
            kind: ChannelKind::Slack,
            external_id: "T123".into(),
            display_name: "team".into(),
            config: serde_json::json!({}),
            enabled: true,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
        };
        assert_eq!(legacy.kind, ChannelKind::Slack);
        assert_eq!(InstallationStatus::Active.as_str(), "active");
    }
}
