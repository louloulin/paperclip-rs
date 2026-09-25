//! Telegram 安装面：list / get / revoke + **BYO 安装**（贴一个 `@BotFather` 的 bot token）
//! （上游 `internal/integrations/telegram/install.go`，303 行）。
//!
//! - **写者**：M7-5（`docs/60-M7-PLAN.md` §3.3；本片的写集勘误见 `docs/32` §17.1）。
//! - **BYO 模型**（上游注释逐字）：workspace 管理员用 `@BotFather` 建一个 bot，把它的 token
//!   贴进 Multica；安装键是 **bot 的数值 id**（令牌前缀），路由键落 `config->>'app_id'`。
//!   没有 OAuth 码交换，也没有 hosted app。
//! - **密钥保密是服务的职责**：本文件的 [`InstallService`] 拥有 token 的**落库加密** ——
//!   任何调用方都不可能写出一行带明文 token 的安装（`boxed` 是**必填**的，上游逐字：
//!   we refuse plaintext storage even in dev）。
//!
//! # 三条从上游逐字搬来的校验（`install.go` 的 `Register`）
//!
//! 1. **令牌形状** `<数值 id>:<secret>` ⇒ 否则 [`InstallError::InvalidBotToken`]（400）。
//! 2. **`getMe` 活的校验**：同时拿到 bot 的用户名（`@-mention` 判定要用）—— 拒了就是
//!    [`InstallError::CredentialsRejected`]（400）；**够不着**是
//!    [`InstallError::CredentialsUnverifiable`]（**503**，且明说"令牌没被保存"）。
//!    两者必须分开（上游注释逐字）：绝不能因为部署的代理 / 网络挂了，就告诉用户去轮换一个
//!    其实有效的凭据。
//! 3. **`getWebhookInfo`**：bot 当前挂着 outgoing webhook ⇒ [`InstallError::WebhookConfigured`]
//!    （400）。长轮询与 webhook **互斥**，所以**不要**在安装时偷偷删掉别的集成配的 webhook。
//!
//! # 与上游的三处形态差异（登记 `docs/32` §17.2）
//!
//! 1. **存储口是一个端口**：[`InstallStore::persist`] 收下上游 `persistInstall` 的整个事务语义
//!    （回收死主 → 按 `(workspace, agent, channel)` upsert → 唯一冲突时分类出"谁占着"），
//!    因为 channel 层不碰 SQL。三个分类结果由 [`PersistOutcome`] 显式表达，于是"贴了别人的
//!    bot"这条**产品**判决仍在本文件。
//! 2. **`apiBase` 不是结构字段**：上游为测试留了 `apiBase` 覆盖位；本仓复用
//!    [`crate::telegram::api::set_api_base`] 的进程内基址接缝（与 M8-1 的 `GITHUB_API_BASE`
//!    同款）—— 少一个贯穿三层的参数。
//! 3. **API 端口是 [`TelegramApi`]**（上游 `installQueries` + `*botAPI` 两个口）：本片只需要
//!    该 trait 的 `get_me` / `get_webhook_info` 两个方法，但**共用同一个**客户端实现，
//!    免得安装面与入站面各写一份 HTTP。
//!
//! # 凭据纪律（`docs/60` §2.3）
//!
//! - [`InstallService`] **不派生** `Debug`：它持有封装盒与端口，没有可打印的凭据。
//! - [`InstallError`] 的每个变体只带**静态文案 / Telegram 自己的错误码** —— 既不回显令牌
//!   也不回显密文。
//! - [`InstallRecord`] 的 `config` 里是**密文**；`Debug` 只打印"密文列非空"。

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_repos::channel::installation::ChannelInstallationRow;
use mc_secrets::secretbox::SecretBox;
use serde_json::Value;

use crate::telegram::api::{ApiError, TelegramApi};
use crate::telegram::config::{encode_ciphertext, InstallConfig};

/// 存储口径的渠道名（`telegram`；`ChannelKind::Telegram.storage_str()`）。
pub const CHANNEL_TYPE: &str = "telegram";

// =====================================================================
// 错误（上游的八个哨兵）
// =====================================================================

/// 安装面的失败（上游 `install.go` 的哨兵 + HTTP 层未分类的那几条）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InstallError {
    /// 本 workspace 里没有这一行（上游 `ErrInstallationNotFound`）。
    #[error("telegram installation not found")]
    NotFound,
    /// 令牌形状不对（`<数值 id>:<secret>`）—— 400（上游 `ErrInvalidBotToken`）。
    #[error("telegram: bot token must look like 123456:ABC-DEF…")]
    InvalidBotToken,
    /// **Telegram 自己**拒了这个令牌（`getMe` 回 401）—— 400。
    #[error("telegram: Telegram rejected this bot token")]
    CredentialsRejected,
    /// **够不着** Telegram，拿不到任何判决 —— **503**，且令牌没有被保存 / 改动。
    #[error("telegram: could not reach Telegram to verify this bot")]
    CredentialsUnverifiable,
    /// 这个 bot 已连到**另一个** Multica workspace（路由索引会撞）。
    #[error("telegram: this bot is already connected to a different Multica workspace")]
    OwnedByAnotherWorkspace,
    /// 已连到**同一个** workspace 里的另一个（存活、未归档）agent。
    #[error("telegram: this bot is already connected to another agent in this workspace")]
    OwnedBySameWorkspace,
    /// 已连到同 workspace 里一个**已归档**的 agent（归档可逆 ⇒ bot 还占着）。
    #[error("telegram: this bot is connected to an archived agent in this workspace")]
    OwnedByArchivedAgent,
    /// bot 当前挂着 outgoing webhook（与长轮询互斥）—— 400。
    #[error("telegram: bot has an outgoing webhook configured")]
    WebhookConfigured,
    /// 落库加密失败（`secretbox` 拒绝；不透明）。
    #[error("telegram: sealing the pasted bot token failed")]
    Seal,
    /// 配置 blob 编码失败。
    #[error("telegram: encoding the installation config failed")]
    Encode,
    /// 存储层故障（不透明）。
    #[error("telegram: installation store failure: {message}")]
    Store { message: String },
}

impl InstallError {
    /// 稳定错误码（HTTP 层映射用；**不含**任何凭据）。
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound => "telegram_installation_not_found",
            Self::InvalidBotToken => "telegram_invalid_bot_token",
            Self::CredentialsRejected => "telegram_credentials_rejected",
            Self::CredentialsUnverifiable => "telegram_credentials_unverifiable",
            Self::OwnedByAnotherWorkspace => "telegram_bot_owned_by_another_workspace",
            Self::OwnedBySameWorkspace => "telegram_bot_owned_by_same_workspace",
            Self::OwnedByArchivedAgent => "telegram_bot_owned_by_archived_agent",
            Self::WebhookConfigured => "telegram_webhook_configured",
            Self::Seal => "telegram_seal_failed",
            Self::Encode => "telegram_encode_failed",
            Self::Store { .. } => "telegram_store_error",
        }
    }

    /// 上游 `handler/telegram.go` 的 switch 逐条状态码。
    ///
    /// 注意 [`Self::CredentialsUnverifiable`] 是 **503**（不是 400）：被关掉的**瞬时**故障
    /// 回 4xx 会招来"用户把好令牌删掉"的误导（上游注释逐字）。
    #[must_use]
    pub fn http_status(&self) -> u16 {
        match self {
            Self::NotFound => 404,
            Self::OwnedByAnotherWorkspace
            | Self::OwnedBySameWorkspace
            | Self::OwnedByArchivedAgent => 409,
            Self::CredentialsUnverifiable => 503,
            Self::Seal | Self::Encode | Self::Store { .. } => 500,
            Self::InvalidBotToken | Self::CredentialsRejected | Self::WebhookConfigured => 400,
        }
    }
}

/// 上游 `classifyCredentialVerificationError`：把 API 失败分成"Telegram 权威地拒了"与
/// "根本没拿到判决"两类。
///
/// `getMe` 与 `getWebhookInfo` 共用同一个分类器 ⇒ 任一步的代理故障都给出同一条用户动作。
#[must_use]
pub fn classify_credential_verification_error(error: &ApiError) -> InstallError {
    if error.http_code() == Some(401) {
        InstallError::CredentialsRejected
    } else {
        InstallError::CredentialsUnverifiable
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
    /// 平台配置 blob（含 **密文** bot token）。
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

    /// 解出对外可见的**非密**身份列（上游 `telegramInstallationToResponse`）。
    #[must_use]
    pub fn public_config(&self) -> crate::telegram::config::PublicConfig {
        crate::telegram::config::decode_public_config(&self.config)
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
    /// 存在 `config->>'app_id'` 的那个 bot 数值 id（**必须**等于 config 里的 `app_id`）。
    pub app_id: String,
    /// 完整配置 blob（密文 token 已在里面）。
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

/// 安装行的读写口（上游那六条语句 + `persistInstall` 的事务）。
#[async_trait]
pub trait InstallStore: Send + Sync {
    /// workspace 的**全部** Telegram 安装（**含 revoked**；上游
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

    /// 落一行（上游 `persistInstall` 的整个事务语义，见模块文档差异 1）。
    async fn persist(&self, params: &PersistInstall) -> Result<PersistOutcome, String>;
}

// =====================================================================
// 服务
// =====================================================================

/// BYO 安装服务（上游 `InstallService`）。
///
/// **不派生 `Debug`**：它持有封装盒，而"能被打印"本身就是一个不该存在的通路。
pub struct InstallService {
    store: Arc<dyn InstallStore>,
    api: Arc<dyn TelegramApi>,
    boxed: SecretBox,
}

impl InstallService {
    /// 装配。`boxed` 是**必填**的（上游逐字：we refuse plaintext storage even in dev）。
    #[must_use]
    pub fn new(store: Arc<dyn InstallStore>, api: Arc<dyn TelegramApi>, boxed: SecretBox) -> Self {
        Self { store, api, boxed }
    }

    /// 列 workspace 的全部 Telegram 安装（含 revoked）。
    pub async fn list(&self, workspace_id: Id) -> Result<Vec<InstallRecord>, InstallError> {
        self.store
            .list_by_workspace(workspace_id)
            .await
            .map_err(|message| InstallError::Store { message })
    }

    /// workspace 收窄的单条（越权读 = 与不存在同一结果）。
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

    /// BYO 安装（上游 `Register`：形状 → `getMe` → `getWebhookInfo` → 密封 → 落库）。
    pub async fn register(&self, params: &RegisterParams) -> Result<InstallRecord, InstallError> {
        let token = params.bot_token.trim();
        let bot_id = parse_bot_id_or_invalid(token)?;

        let me = self
            .api
            .get_me(token)
            .await
            .map_err(|error| classify_credential_verification_error(&error))?;
        if !me.is_bot || me.username.is_empty() {
            // 响应不是一个带用户名的 bot ⇒ 令牌确实不对（上游 `%w: response is not a bot…`）。
            return Err(InstallError::CredentialsRejected);
        }

        let webhook = self
            .api
            .get_webhook_info(token)
            .await
            .map_err(|error| classify_credential_verification_error(&error))?;
        if !webhook.url.is_empty() {
            return Err(InstallError::WebhookConfigured);
        }

        let sealed = self
            .boxed
            .seal(token.as_bytes())
            .map_err(|_| InstallError::Seal)?;
        let config = InstallConfig {
            app_id: bot_id.clone(),
            bot_username: me.username,
            bot_token_encrypted: encode_ciphertext(&sealed),
        };
        let config = serde_json::to_value(&config).map_err(|_| InstallError::Encode)?;

        let outcome = self
            .store
            .persist(&PersistInstall {
                workspace_id: params.workspace_id,
                agent_id: params.agent_id,
                installer_user_id: params.initiator_user_id,
                app_id: bot_id,
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

/// `parse_bot_id` 的失败映射（上游 `ErrInvalidBotToken`）。
fn parse_bot_id_or_invalid(token: &str) -> Result<String, InstallError> {
    crate::telegram::config::parse_bot_id(token).ok_or(InstallError::InvalidBotToken)
}

/// 安装的入参（上游 `RegisterParams`）。
///
/// **不派生 `Debug`**（见下面的手写实现）：结构体里有一个**明文** bot token。
#[derive(Clone, PartialEq, Eq)]
pub struct RegisterParams {
    pub workspace_id: Id,
    pub agent_id: Id,
    pub initiator_user_id: Id,
    /// `123456:ABC-DEF…`（`@BotFather` 给的令牌）。
    pub bot_token: String,
}

impl RegisterParams {
    /// 装配（令牌按值给）。
    #[must_use]
    pub fn new(
        workspace_id: Id,
        agent_id: Id,
        initiator_user_id: Id,
        bot_token: impl Into<String>,
    ) -> Self {
        Self {
            workspace_id,
            agent_id,
            initiator_user_id,
            bot_token: bot_token.into(),
        }
    }
}

impl fmt::Debug for RegisterParams {
    /// 手写脱敏（凭据纪律第 1 条）：默认 `Debug` 会把令牌原样写进任何 `assert_eq!` 失败 /
    /// `{:?}` 插值 / panic 回显。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RegisterParams")
            .field("workspace_id", &self.workspace_id)
            .field("agent_id", &self.agent_id)
            .field("initiator_user_id", &self.initiator_user_id)
            .field("bot_token", &redact(&self.bot_token))
            .finish()
    }
}

/// 凭据的脱敏说明（`<empty>` / `<redacted>`）。
fn redact(value: &str) -> &'static str {
    if value.trim().is_empty() {
        "<empty>"
    } else {
        "<redacted>"
    }
}

/// 本 adapter 的平台判别式（`telegram`）。
#[must_use]
pub fn kind() -> ChannelKind {
    ChannelKind::Telegram
}

#[cfg(test)]
mod tests;
