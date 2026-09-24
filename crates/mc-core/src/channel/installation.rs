//! 通用渠道安装行：`channel_installation`（迁移 `124`）的领域投影。
//!
//! - **写者**：M7-0 anchor（本片；`docs/60-M7-PLAN.md` §2.1 / §3.1 —— `mc-core` 的
//!   channel 面锚点落地后**各片只读**）。
//! - **上游**：`migrations/upstream/124_channel_generalization.up.sql` 的
//!   `CREATE TABLE channel_installation`（列见下），消费方是 `internal/integrations/channel`
//!   与五个 adapter 的 store。
//! - **本波 0 新迁移**：22 张渠道表全部已在上游（`docs/60` §6.4）。
//!
//! # 列 → 字段（逐列对齐，**没有**"顺手加一列"）
//!
//! | 列 | 字段 | 说明 |
//! | --- | --- | --- |
//! | `id` | `id` | `UUID PRIMARY KEY DEFAULT gen_random_uuid()` |
//! | `workspace_id` | `workspace_id` | 无 FK（`124` 的硬规则 1：**无外键、无级联**，完整性在应用层） |
//! | `agent_id` | `agent_id` | 一个 agent 每种渠道至多一条安装（`UNIQUE(workspace_id, agent_id, channel_type)`） |
//! | `channel_type` | `kind` | ⚠️ **存储口径**：Lark 存 `feishu`（见 [`ChannelKind::storage_str`]） |
//! | `config` | `config` | 平台自己的标识/凭据 JSONB；feishu 的 `app_secret_encrypted` 是 **base64 字符串**（`124` 注释写明 bytea → JSON 去换行） |
//! | `status` | `status` | `CHECK (status IN ('active','revoked'))` ⇒ [`InstallationStatus`] 两态 |
//! | `ws_lease_token` / `ws_lease_expires_at` | 同名 | **长连接租约**（上游无 Redis 时的进程内租约，见 `docs/60` §2.5 R-M7-1） |
//! | `installer_user_id` | `installer_user_id` | 安装发起人；`NOT NULL` |
//! | `installed_at` / `created_at` / `updated_at` | 同名 | 三个独立时间戳（`installed_at` **不**等于 `created_at`：BYO/扫码路径会补写） |
//!
//! # 不做什么
//!
//! - **不**定义 `config` 的内部结构：它是每个平台自己的 JSON（Lark 的 `app_id` /
//!   `app_secret_encrypted` / `tenant_key` / `region`…）。按结构拆类型会让「加一个平台字段」
//!   变成动 `mc-core`；上游也把它当不透明 blob 搬运（`channel.Config.Raw`）。读取用
//!   `serde_json::Value` 的取键，**不在这里**翻译成强类型。
//! - **不**做 `status` 的状态机（`active → revoked` 的写路径在 M7 各片的仓储层）。
//! - **不**把 `channel_installation` 与遗留的 `lark_installation` 合并：上游两套表**同时
//!   在用**（`docs/60` §6.4 / R-M7-5），合并会静默丢数据。
//!
//! [`ChannelKind::storage_str`]: crate::channel::ChannelKind::storage_str

use serde::{Deserialize, Serialize};

use super::ChannelKind;
use crate::id::Id;
use crate::timestamp::Timestamp;

/// `channel_installation.status` 的两态（列的 `CHECK` 约束逐字）。
///
/// 上游没有第三态：撤销是**行级**语义（`revoked` 行保留，供审计与历史绑定查询），
/// 不是删除。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallationStatus {
    Active,
    Revoked,
}

impl InstallationStatus {
    /// 列里的字面量（`CHECK` 的两个取值）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Revoked => "revoked",
        }
    }

    /// 解回枚举；未知取值返回 `None`（调用方按"坏数据"处理，别默认成 `Active`）。
    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s {
            "active" => Some(Self::Active),
            "revoked" => Some(Self::Revoked),
            _ => None,
        }
    }

    /// 是否还能承载长连接 / 收发消息。
    pub fn is_live(self) -> bool {
        matches!(self, Self::Active)
    }
}

/// `channel_installation` 的一行（领域投影）。
///
/// ⚠️ 仓储层的行结构是**另一套**（裸 `Uuid` + 手写 `sqlx::FromRow`，见 `mc_repos::channel`）：
/// 本结构是跨 crate 的**领域**形态，转换发生在仓储→领域的那一步，**不要**在这里给字段加
/// `sqlx` 依赖（`mc-core` 对 sqlx 是 feature-gated 的）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Installation {
    pub id: Id,
    pub workspace_id: Id,
    pub agent_id: Id,
    pub kind: ChannelKind,
    /// 平台自己的 JSON（不透明；见模块文档「不做什么」）。
    pub config: serde_json::Value,
    pub status: InstallationStatus,
    /// 长连接租约令牌（`ws_lease_token`）；空 = 没有副本持有连接。
    pub ws_lease_token: Option<String>,
    /// 租约到期时间（`ws_lease_expires_at`）；配合 `ws_lease_token` 判过期接管。
    pub ws_lease_expires_at: Option<Timestamp>,
    pub installer_user_id: Id,
    pub installed_at: Timestamp,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl Installation {
    /// `channel_type` 的**存储**取值（写库/比对时用这一份，别用 `kind.as_str()`）。
    pub fn channel_type(&self) -> &'static str {
        self.kind.storage_str()
    }

    /// 撤销后是否还有效（行仍在，`revoked` 行不得再建连接）。
    pub fn is_live(&self) -> bool {
        self.status.is_live()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Installation {
        Installation {
            id: Id::new(),
            workspace_id: Id::new(),
            agent_id: Id::new(),
            kind: ChannelKind::Lark,
            config: serde_json::json!({ "app_id": "cli_x" }),
            status: InstallationStatus::Active,
            ws_lease_token: None,
            ws_lease_expires_at: None,
            installer_user_id: Id::new(),
            installed_at: Timestamp::now(),
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
        }
    }

    /// 两态与 `CHECK` 逐字一致，且往返闭合。
    #[test]
    fn status_matches_the_check_constraint() {
        assert_eq!(InstallationStatus::Active.as_str(), "active");
        assert_eq!(InstallationStatus::Revoked.as_str(), "revoked");
        for status in [InstallationStatus::Active, InstallationStatus::Revoked] {
            assert_eq!(
                InstallationStatus::from_str_opt(status.as_str()),
                Some(status)
            );
        }
        // 未知取值**不能**被当成 active（坏数据必须显形）。
        assert_eq!(InstallationStatus::from_str_opt("deleted"), None);
        assert!(InstallationStatus::Active.is_live());
        assert!(!InstallationStatus::Revoked.is_live());
    }

    /// `channel_type()` 走**存储**口径：Lark 行写 `feishu`。
    #[test]
    fn channel_type_uses_the_storage_slug() {
        assert_eq!(sample().channel_type(), "feishu");
        let mut slack = sample();
        slack.kind = ChannelKind::Slack;
        assert_eq!(slack.channel_type(), "slack");
    }
}
