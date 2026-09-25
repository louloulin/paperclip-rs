//! `DingTalk` 安装面：**list / get / revoke + BYO 安装**（上游
//! `internal/integrations/dingtalk/install.go` 285 行 + `byo_install.go` 100 行）。
//!
//! - **写者**：M7-9（`docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §23 的 D1）。
//! - **BYO 模型**（上游注释逐字）：agent owner 或 workspace 管理员自己建一个 `DingTalk`
//!   **Stream 模式机器人**，把 `AppKey`（client id）+ `AppSecret`（client secret）贴进 Multica。
//!   **没有 OAuth 码交换**：活的校验就是"用这对凭据铸一枚访问令牌"，铸得出来就证明凭据是真的。
//! - **每个 BYO 机器人是一个独立的 `DingTalk` 应用**（一个独立的 bot 身份）⇒ 同一个 `DingTalk`
//!   组织里可以有几个机器人、每个 agent 一个；落库的 `config` 带 `AppKey` 当路由键。
//!
//! # 密钥保密是 [`InstallService`] 的职责（不是调用方的）
//!
//! `boxed` 是**必填**的（上游 `newInstallService` 逐字：`we refuse plaintext storage even in
//! dev`）⇒ 任何调用方都**写不出**一行带明文 `AppSecret` 的安装：`config` 列里的
//! `app_secret_encrypted` 只可能来自 [`InstallService`] 的 `seal`。
//!
//! # 三条从上游逐字搬来的判决（`byo_install.go` 的 `RegisterBYO`）
//!
//! 1. **空凭据**：`AppKey` / `AppSecret` 空 ⇒ [`InstallError::InvalidAppKey`] /
//!    [`InstallError::InvalidAppSecret`]（400，前端能给出精确提示）；
//! 2. **活校验**：铸令牌失败 ⇒ [`InstallError::CredentialValidation`]（**400**：这是用户错误，
//!    上游注释逐字 `a user error`, handler 引导他去核对凭据）。本仓**不分**
//!    "平台权威地拒了"与"够不着平台"—— 上游这一条也只有一个哨兵（与 telegram 面**不同**，
//!    那是那边自己的切分，别照抄过来）；
//! 3. **加密 / 落库失败**：一律 `Store` / `Seal` / `Encode` ⇒ **500**
//!    （上游逐字：服务端问题不该被说成"你的凭据不对"）。
//!
//! # 与上游的一处形态差异（登记 `docs/32` §23 的 D4）
//!
//! 上游 `persistInstall` 在**自己**的事务里跑四步（锁 → 回收死主 → 换机器人时退休旧行 →
//! upsert），唯一冲突时再分类"谁占着"。本仓把"这一个事务"收进 [`InstallStore::persist`]
//! **一个端口方法**（层次铁律：channel 层不碰 SQL，`docs/60` §2.6 第 1 条）⇒ 三个分类结果由
//! [`PersistOutcome`] 显式表达，于是"贴了别人的机器人"这条**产品**判决仍在本文件可测。

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_repos::channel::installation::ChannelInstallationRow;
use mc_secrets::secretbox::SecretBox;
use serde_json::Value;

use super::client::{AccessToken, CredentialProbe};
use super::config::{decode_public_config, InstallConfig, PublicConfig};
use super::stream::AppSecret;

/// 存储口径的渠道名（`ChannelKind::DingTalk.storage_str()`）。
pub const CHANNEL_TYPE: &str = "dingtalk";

/// 本 adapter 的平台判别式。
pub const KIND: ChannelKind = ChannelKind::DingTalk;

// =====================================================================
// 错误（上游 `install.go` / `byo_install.go` 的哨兵）
// =====================================================================

/// 安装面的失败。
///
/// 每个变体只带**静态文案**或**平台自己的错误码** —— 既不回显 `AppKey` / `AppSecret`，
/// 也不回显密文（凭据纪律第 2 条）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InstallError {
    /// 本 workspace 里没有这一行（上游 `ErrInstallationNotFound`）。
    #[error("dingtalk installation not found")]
    NotFound,
    /// `AppKey`（client id）为空 —— 400（上游 `ErrInvalidAppKey`）。
    #[error("dingtalk: AppKey (client id) is required")]
    InvalidAppKey,
    /// `AppSecret`（client secret）为空 —— 400（上游 `ErrInvalidAppSecret`）。
    #[error("dingtalk: AppSecret (client secret) is required")]
    InvalidAppSecret,
    /// 活的铸令牌**拒了**这对凭据 —— 400（上游 `ErrCredentialValidation`）。
    #[error("dingtalk: could not validate credentials")]
    CredentialValidation,
    /// 这个机器人已连到**另一个** Multica workspace（`(dingtalk, app_id)` 路由索引会撞）。
    #[error("dingtalk: this DingTalk robot is already connected to a different Multica workspace")]
    OwnedByAnotherWorkspace,
    /// 已连到**同一个** workspace 里的另一个（存活、未归档）agent。
    #[error(
        "dingtalk: this DingTalk robot is already connected to another agent in this workspace"
    )]
    OwnedBySameWorkspace,
    /// 已连到同 workspace 里一个**已归档**的 agent（归档可逆 ⇒ 机器人还占着）。
    #[error("dingtalk: this DingTalk robot is connected to an archived agent in this workspace")]
    OwnedByArchivedAgent,
    /// 落库加密失败（`secretbox` 拒绝；不透明）。
    #[error("dingtalk: sealing the pasted AppSecret failed")]
    Seal,
    /// 配置 blob 编码失败。
    #[error("dingtalk: encoding the installation config failed")]
    Encode,
    /// 存储层故障（不透明：不回显 SQL / 参数）。
    #[error("dingtalk: installation store failure: {message}")]
    Store { message: String },
}

impl InstallError {
    /// 稳定错误码（HTTP 层映射用；**不含**任何凭据）。
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound => "dingtalk_installation_not_found",
            Self::InvalidAppKey => "dingtalk_invalid_app_key",
            Self::InvalidAppSecret => "dingtalk_invalid_app_secret",
            Self::CredentialValidation => "dingtalk_credential_validation_failed",
            Self::OwnedByAnotherWorkspace => "dingtalk_robot_owned_by_another_workspace",
            Self::OwnedBySameWorkspace => "dingtalk_robot_owned_by_same_workspace",
            Self::OwnedByArchivedAgent => "dingtalk_robot_owned_by_archived_agent",
            Self::Seal => "dingtalk_seal_failed",
            Self::Encode => "dingtalk_encode_failed",
            Self::Store { .. } => "dingtalk_store_error",
        }
    }

    /// 上游 `handler/dingtalk.go` 的 switch 逐条状态码。
    ///
    /// 三类凭据错误是 **400**（用户错误），三条冲突是 **409**，其余（加密 / 落库 / 意外）是
    /// **500** —— 上游逐字：`Encrypt / persist / unexpected failures are server-side, not the
    /// user's credentials`。
    #[must_use]
    pub fn http_status(&self) -> u16 {
        match self {
            Self::NotFound => 404,
            Self::InvalidAppKey | Self::InvalidAppSecret | Self::CredentialValidation => 400,
            Self::OwnedByAnotherWorkspace
            | Self::OwnedBySameWorkspace
            | Self::OwnedByArchivedAgent => 409,
            Self::Seal | Self::Encode | Self::Store { .. } => 500,
        }
    }
}

// =====================================================================
// 存储端口
// =====================================================================

/// 一行安装的**完整**投影（含三个时间戳 —— 管理列表要渲染它们）。
#[derive(Clone, PartialEq)]
pub struct InstallRecord {
    pub id: Id,
    pub workspace_id: Id,
    pub agent_id: Id,
    pub installer_user_id: Id,
    pub status: String,
    /// 平台配置 blob（含 **密文** `AppSecret`）。
    pub config: Value,
    pub installed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl fmt::Debug for InstallRecord {
    /// 手写脱敏：只说明密文列**配没配**，值一律不打印。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InstallRecord")
            .field("id", &self.id)
            .field("workspace_id", &self.workspace_id)
            .field("agent_id", &self.agent_id)
            .field("installer_user_id", &self.installer_user_id)
            .field("status", &self.status)
            .field("config", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl InstallRecord {
    /// 是否还能承载消息（`status = 'active'`）。
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.status == "active"
    }

    /// 解出对外可见的**非密**身份列（上游响应的 `app_id` / `robot_code`）。
    #[must_use]
    pub fn public_config(&self) -> PublicConfig {
        decode_public_config(&self.config)
    }
}

impl From<&ChannelInstallationRow> for InstallRecord {
    fn from(row: &ChannelInstallationRow) -> Self {
        Self {
            id: row.id(),
            workspace_id: row.workspace_id(),
            agent_id: Id(row.agent_id),
            installer_user_id: Id(row.installer_user_id),
            status: row.status.clone(),
            config: row.config.clone(),
            installed_at: row.installed_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

/// 落一行 BYO 安装的入参（上游 `installPersist`）。
#[derive(Debug, Clone, PartialEq)]
pub struct PersistInstall {
    pub workspace_id: Id,
    pub agent_id: Id,
    pub installer_user_id: Id,
    /// 存在 `config->>'app_id'` 的那个 `AppKey`（**必须**等于 config 里的 `app_id`）。
    pub app_id: String,
    /// 完整配置 blob（密文已在里面）。
    pub config: Value,
}

/// 落库的判决（上游 `persistInstall` 的返回值或它翻出来的冲突哨兵）。
#[derive(Debug, Clone, PartialEq)]
pub enum PersistOutcome {
    /// 落好了（新建**或更新**：再连同一个 agent 是原地更新它的行）。
    Stored(Box<InstallRecord>),
    OwnedByAnotherWorkspace,
    OwnedBySameWorkspace,
    OwnedByArchivedAgent,
}

/// 安装行的读写口（上游 `installQueries` 那几条语句 + `persistInstall` 的事务）。
#[async_trait]
pub trait InstallStore: Send + Sync {
    /// workspace 的**全部** `DingTalk` 安装（**含 revoked**；上游
    /// `ListChannelInstallationsByWorkspace`）。
    async fn list_by_workspace(&self, workspace_id: Id) -> Result<Vec<InstallRecord>, String>;

    /// workspace 收窄的单条（跨工作区读 = 越权 ⇒ `Ok(None)`）。
    async fn get_in_workspace(
        &self,
        installation_id: Id,
        workspace_id: Id,
    ) -> Result<Option<InstallRecord>, String>;

    /// 撤销（`active → revoked`）；返回是否真的改了一行。
    async fn revoke(&self, workspace_id: Id, installation_id: Id) -> Result<bool, String>;

    /// 落一行（上游 `persistInstall` 的整个事务语义，见模块文档的形态差异）。
    async fn persist(&self, params: &PersistInstall) -> Result<PersistOutcome, String>;
}

// =====================================================================
// 服务
// =====================================================================

/// BYO 安装服务（上游 `InstallService`）。
///
/// **不派生 `Debug`**：它持有封装盒（`SecretBox` 自己脱敏，但"能被打印"本身就不该存在）。
pub struct InstallService {
    store: Arc<dyn InstallStore>,
    probe: Arc<dyn CredentialProbe>,
    boxed: SecretBox,
}

impl InstallService {
    /// 装配。`boxed` 是**必填**的（上游逐字：we refuse plaintext storage even in dev）。
    #[must_use]
    pub fn new(
        store: Arc<dyn InstallStore>,
        probe: Arc<dyn CredentialProbe>,
        boxed: SecretBox,
    ) -> Self {
        Self {
            store,
            probe,
            boxed,
        }
    }

    /// 列 workspace 的全部 `DingTalk` 安装（含 revoked）。
    ///
    /// # Errors
    ///
    /// 存储层故障。
    pub async fn list(&self, workspace_id: Id) -> Result<Vec<InstallRecord>, InstallError> {
        self.store
            .list_by_workspace(workspace_id)
            .await
            .map_err(|message| InstallError::Store { message })
    }

    /// workspace 收窄的单条（越权读 = 与不存在同一结果）。
    ///
    /// # Errors
    ///
    /// 不存在 ⇒ [`InstallError::NotFound`]；存储层故障。
    pub async fn get_in_workspace(
        &self,
        installation_id: Id,
        workspace_id: Id,
    ) -> Result<InstallRecord, InstallError> {
        self.store
            .get_in_workspace(installation_id, workspace_id)
            .await
            .map_err(|message| InstallError::Store { message })?
            .ok_or(InstallError::NotFound)
    }

    /// 撤销（行保留供审计；重装会把状态翻回 `active`）。
    ///
    /// # Errors
    ///
    /// 存储层故障。
    pub async fn revoke(
        &self,
        workspace_id: Id,
        installation_id: Id,
    ) -> Result<bool, InstallError> {
        self.store
            .revoke(workspace_id, installation_id)
            .await
            .map_err(|message| InstallError::Store { message })
    }

    /// BYO 安装（上游 `RegisterBYO`：形状 → 活校验 → 密封 → 落库）。
    ///
    /// # Errors
    ///
    /// 空凭据 / 凭据被拒 / 密封失败 / 编码失败 / 冲突三类 / 存储层故障。
    pub async fn register_byo(
        &self,
        params: &RegisterByoParams,
    ) -> Result<InstallRecord, InstallError> {
        let app_key = params.app_key.trim().to_string();
        if app_key.is_empty() {
            return Err(InstallError::InvalidAppKey);
        }
        if params.app_secret.is_empty() {
            return Err(InstallError::InvalidAppSecret);
        }

        // 活的校验：铸得出访问令牌就证明这对凭据是真的、且机器人已安装
        // （上游逐字：a successful access_token mint proves the AppKey/AppSecret pair is real）。
        let _token: AccessToken = self
            .probe
            .fetch_access_token(&app_key, &params.app_secret)
            .await
            .map_err(|_| InstallError::CredentialValidation)?;

        let sealed = self
            .boxed
            .seal(params.app_secret.expose().as_bytes())
            .map_err(|_| InstallError::Seal)?;
        let config = InstallConfig::byo(&app_key, &sealed)
            .to_config_value()
            .map_err(|_| InstallError::Encode)?;

        let outcome = self
            .store
            .persist(&PersistInstall {
                workspace_id: params.workspace_id,
                agent_id: params.agent_id,
                installer_user_id: params.initiator_user_id,
                app_id: app_key,
                config,
            })
            .await
            .map_err(|message| InstallError::Store { message })?;
        match outcome {
            PersistOutcome::Stored(record) => Ok(*record),
            PersistOutcome::OwnedByAnotherWorkspace => Err(InstallError::OwnedByAnotherWorkspace),
            PersistOutcome::OwnedBySameWorkspace => Err(InstallError::OwnedBySameWorkspace),
            PersistOutcome::OwnedByArchivedAgent => Err(InstallError::OwnedByArchivedAgent),
        }
    }
}

/// BYO 安装的入参（上游 `RegisterBYOParams`）。
///
/// **手写 `Debug`**：结构体里有一个**明文** `AppSecret`（凭据纪律第 1 条）——
/// 默认 `Debug` 会把它原样写进任何 `{:?}` 插值、`assert_eq!` 失败与 panic 回显。
#[derive(Clone)]
pub struct RegisterByoParams {
    pub workspace_id: Id,
    pub agent_id: Id,
    pub initiator_user_id: Id,
    /// `AppKey`（client id）—— 机器人码，也是路由键；**不是**秘密。
    pub app_key: String,
    /// `AppSecret`（client secret）—— **秘密**（手写脱敏类型）。
    pub app_secret: AppSecret,
}

impl fmt::Debug for RegisterByoParams {
    /// 手写脱敏（凭据纪律第 1 条）。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RegisterByoParams")
            .field("workspace_id", &self.workspace_id)
            .field("agent_id", &self.agent_id)
            .field("initiator_user_id", &self.initiator_user_id)
            .field("app_key", &self.app_key)
            .field("app_secret", &self.app_secret)
            .finish()
    }
}

impl RegisterByoParams {
    /// 装配（两个凭据按值给；`AppSecret` 只经 [`AppSecret::new`]）。
    #[must_use]
    pub fn new(
        workspace_id: Id,
        agent_id: Id,
        initiator_user_id: Id,
        app_key: impl Into<String>,
        app_secret: impl Into<String>,
    ) -> Self {
        Self {
            workspace_id,
            agent_id,
            initiator_user_id,
            app_key: app_key.into(),
            app_secret: AppSecret::new(app_secret),
        }
    }
}

/// 本 adapter 的平台判别式（诊断 / 注册用）。
#[must_use]
pub fn kind() -> ChannelKind {
    KIND
}

#[cfg(test)]
mod tests;
