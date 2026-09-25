//! Slack 安装面：list / get / revoke + **BYO 安装**（自带 app token）
//! （上游 `internal/integrations/slack/install.go` 255 行 + `byo_install.go` 190 行）。
//!
//! - **写者**：M7-4（`docs/60-M7-PLAN.md` §3.3）。
//! - **BYO 模型**（上游注释逐字）：workspace 管理员**自己**建一个 Slack app、装进自己的
//!   workspace，把 **bot token**（`xoxb-`）与 **app-level token**（`xapp-`）贴进 Multica。
//!   没有 OAuth 码交换；`xapp-` 里嵌着**真实 app id**，那是每 app 的存储/路由键
//!   （⇒ 同一个 Slack workspace 里可以有多个 app，一个 agent 一个）。
//! - **密钥保密是服务的职责**：本文件的 [`InstallService`] 拥有两个令牌的**落库加密**
//!   （明文永不入库），所以**任何**调用方都不可能写出一行带明文令牌的安装。
//!
//! # 三条从上游逐字搬来的校验（`byo_install.go` 的 `RegisterBYO`）
//!
//! 1. **`xoxb-` 前缀**：否则 [`InstallError::InvalidBotToken`]（400）。
//! 2. **`xapp-1-<APP_ID>-…`**：第三段即 app id 且必须以 `A` 开头，否则
//!    [`InstallError::InvalidAppToken`]（400）。
//! 3. **两个令牌必须属于同一个 app**：`auth.test` 拿 `bot_id` →
//!    `bots.info(bot_id)` 拿**拥有该 bot 的** app id → 必须等于 `xapp-` 里的 app id。
//!    不校这一条的话，"贴 A 的 bot token + B 的 app token"会**连上但坏掉**：
//!    入站从 B 的 socket 来（按 `api_app_id=B` 路由），而提及判定与出站用 A 的身份/令牌
//!    （上游注释逐字）。
//!
//! 另外 `apps.connections.open` 会被真打一次 —— 一个**永远收不到事件**的 app token
//! 不该被存下来。
//!
//! # 与上游的两处形态差异（登记 `docs/32` §15）
//!
//! 1. **存储口是一个端口**：[`InstallStore::persist`] 收下上游 `persistInstall` 的
//!    整个事务语义（回收死主 → 按 `(workspace, agent, channel)` upsert → 唯一冲突时
//!    分类出"谁占着"），因为 channel 层不碰 SQL。三个分类结果由
//!    [`PersistOutcome`] 显式表达，于是"贴了别人的 app"这条**产品**判决仍在本文件。
//! 2. **`api_url` 不是结构字段**：上游为测试留了 `apiURL` 覆盖位；本仓复用
//!    `outbound.rs` 的进程内基址接缝（[`crate::slack::outbound::set_api_base`]），
//!    与 M8-1 的 `GITHUB_API_BASE` 同款 —— 少一个贯穿三层的参数。
//!
//! # 凭据纪律（`docs/60` §2.3）
//!
//! - [`InstallService`] **不派生** `Debug`：它持有封装盒与端口，没有可打印的凭据。
//! - [`InstallError`] 的每个变体只带**字段名 / 静态文案** —— 既不回显令牌也不回显密文。
//! - [`InstallRecord`] 的 `config` 里是**密文**；`Debug` 只打印"两个密文列非空"。

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use mc_core::channel::ChannelKind;
use mc_core::id::Id;
use mc_repos::channel::installation::ChannelInstallationRow;
use mc_secrets::secretbox::SecretBox;
use serde_json::Value;

use crate::slack::config::{encode_ciphertext, InstallConfig};
use crate::slack::outbound::{ApiResult, HttpSlackApi};

/// 存储口径的渠道名（`slack`；`ChannelKind::Slack.storage_str()`）。
pub const CHANNEL_TYPE: &str = "slack";

/// 令牌前缀（上游 `strings.HasPrefix` 的字面量）。
pub const BOT_TOKEN_PREFIX: &str = "xoxb-";
/// app-level 令牌前缀。
pub const APP_TOKEN_PREFIX: &str = "xapp-";

// =====================================================================
// 错误（上游的七个哨兵）
// =====================================================================

/// 安装面的失败（上游 `install.go` / `byo_install.go` 的哨兵 + HTTP 层未分类的那条）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InstallError {
    /// 本 workspace 里没有这一行（上游 `ErrInstallationNotFound`）。
    #[error("slack installation not found")]
    NotFound,
    /// 这个 Slack app 已连到**另一个** Multica workspace（Routing 索引会撞）。
    #[error("slack: this Slack app is already connected to a different Multica workspace")]
    OwnedByAnotherWorkspace,
    /// 已连到**同一个** workspace 里的另一个（存活、未归档）agent。
    #[error("slack: this Slack app is already connected to another agent in this workspace")]
    OwnedBySameWorkspace,
    /// 已连到同 workspace 里一个**已归档**的 agent（归档可逆 ⇒ bot 还占着）。
    #[error("slack: this Slack app is connected to an archived agent in this workspace")]
    OwnedByArchivedAgent,
    /// bot token 前缀不对（400）。
    #[error("slack: bot token must start with xoxb-")]
    InvalidBotToken,
    /// app-level token 形状不对（400）。
    #[error("slack: app-level token must start with xapp- and embed an app id")]
    InvalidAppToken,
    /// 两个令牌**不属于同一个 app**（400）。
    #[error("slack: the bot token and app-level token are from different Slack apps")]
    TokenAppMismatch,
    /// `auth.test` 的响应缺 `team_id` / `user_id` / `bot_id`（上游 `errors.New`）。
    #[error("slack: auth.test response missing team_id / user_id / bot_id")]
    IncompleteAuthTest,
    /// Slack Web API 拒绝 / 传输失败（上游 `fmt.Errorf("slack auth.test: %w", err)` 的分支）。
    ///
    /// 只带**哪一步**与具体原因（Slack 拒绝时是它自己的错误码）—— 供 HTTP 层翻成 400
    /// 并提示用户重查令牌。**绝不**带令牌 / URL。
    #[error("slack: {step} failed ({code})")]
    Api { step: &'static str, code: String },
    /// 落库加密失败（`secretbox` 拒绝；不透明）。
    #[error("slack: sealing the pasted tokens failed")]
    Seal,
    /// 配置 blob 编码失败。
    #[error("slack: encoding the installation config failed")]
    Encode,
    /// 存储层故障（不透明）。
    #[error("slack: installation store failure: {message}")]
    Store { message: String },
}

impl InstallError {
    /// 稳定错误码（HTTP 层映射用；**不含**任何凭据）。
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound => "slack_installation_not_found",
            Self::OwnedByAnotherWorkspace => "slack_app_owned_by_another_workspace",
            Self::OwnedBySameWorkspace => "slack_app_owned_by_same_workspace",
            Self::OwnedByArchivedAgent => "slack_app_owned_by_archived_agent",
            Self::InvalidBotToken => "slack_invalid_bot_token",
            Self::InvalidAppToken => "slack_invalid_app_token",
            Self::TokenAppMismatch => "slack_token_app_mismatch",
            Self::IncompleteAuthTest => "slack_auth_test_incomplete",
            Self::Api { .. } => "slack_api_error",
            Self::Seal => "slack_seal_failed",
            Self::Encode => "slack_encode_failed",
            Self::Store { .. } => "slack_store_error",
        }
    }

    /// 上游把这七条里**前七类**都翻成 400（"用户贴错了东西"），三类冲突翻 409。
    #[must_use]
    pub fn http_status(&self) -> u16 {
        match self {
            Self::OwnedByAnotherWorkspace
            | Self::OwnedBySameWorkspace
            | Self::OwnedByArchivedAgent => 409,
            Self::NotFound => 404,
            Self::Store { .. } | Self::Seal | Self::Encode => 500,
            _ => 400,
        }
    }
}

// =====================================================================
// Web API 端口（上游 `InstallService` 的三条调用）
// =====================================================================

/// `auth.test` 的结论（上游 `slack.AuthTestResponse` 的三个字段）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthTest {
    pub team_id: String,
    /// bot **自己的** user id（提及判定剥的就是它）。
    pub user_id: String,
    pub bot_id: String,
}

/// 安装期要打的三个 Slack Web API 方法。
#[async_trait]
pub trait InstallApi: Send + Sync {
    /// `auth.test`：校验 bot token 并拿到 `(team, bot user, bot id)`。
    async fn auth_test(&self, bot_token: &str) -> ApiResult<AuthTest>;
    /// `bots.info(bot=…)`：拿**拥有该 bot 的** app id（令牌 → app id 的唯一路径）。
    async fn bot_app_id(&self, bot_token: &str, bot_id: &str) -> ApiResult<String>;
    /// `apps.connections.open`：证明这个 `xapp-` **真的能**开一条 Socket Mode 连接。
    async fn validate_app_token(&self, app_token: &str) -> ApiResult<()>;
}

/// 生产实现：三个方法直连（基址见 [`crate::slack::outbound::api_base`]）。
#[derive(Debug, Default, Clone)]
pub struct HttpInstallApi;

#[async_trait]
impl InstallApi for HttpInstallApi {
    async fn auth_test(&self, bot_token: &str) -> ApiResult<AuthTest> {
        let body = HttpSlackApi::call("auth.test", bot_token, serde_json::json!({})).await?;
        let field = |name: &str| {
            body.get(name)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        Ok(AuthTest {
            team_id: field("team_id"),
            user_id: field("user_id"),
            bot_id: field("bot_id"),
        })
    }

    async fn bot_app_id(&self, bot_token: &str, bot_id: &str) -> ApiResult<String> {
        let body = HttpSlackApi::call("bots.info", bot_token, serde_json::json!({ "bot": bot_id }))
            .await?;
        body.get("bot")
            .and_then(|bot| bot.get("app_id"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or(crate::slack::outbound::SlackApiError::Malformed {
                method: "bots.info",
            })
    }

    async fn validate_app_token(&self, app_token: &str) -> ApiResult<()> {
        // 返回的 `url` 自带票据（等价于凭据）⇒ 只判 `ok`，**绝不**回显 / 记录它
        // （`socket.rs` 的 `open_connection` 是同一条纪律）。
        HttpSlackApi::call("apps.connections.open", app_token, serde_json::json!({}))
            .await
            .map(|_| ())
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
    /// 平台配置 blob（含两个**密文**令牌）。
    pub config: Value,
    pub installed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl fmt::Debug for InstallRecord {
    /// 手写脱敏：只说明两个密文列**配没配**（运维需要知道），值一律不打印。
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
    /// 存在 `config->>'app_id'` 的那个真实 app id（**必须**等于 config 里的 `app_id`）。
    pub app_id: String,
    /// 完整配置 blob（两个密文令牌已在里面）。
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
    /// workspace 的**全部** Slack 安装（**含 revoked**；上游 `ListChannelInstallationsByWorkspace`）。
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
    api: Arc<dyn InstallApi>,
    boxed: SecretBox,
}

impl InstallService {
    /// 装配。`boxed` 是**必填**的（上游逐字：we refuse plaintext storage even in dev）。
    #[must_use]
    pub fn new(store: Arc<dyn InstallStore>, api: Arc<dyn InstallApi>, boxed: SecretBox) -> Self {
        Self { store, api, boxed }
    }

    /// 列 workspace 的全部 Slack 安装（含 revoked）。
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

    /// BYO 安装（上游 `RegisterBYO`，三步校验 + 密封 + 落库）。
    pub async fn register_byo(
        &self,
        params: &RegisterByoParams,
    ) -> Result<InstallRecord, InstallError> {
        let bot_token = params.bot_token.trim();
        let app_token = params.app_token.trim();
        if !bot_token.starts_with(BOT_TOKEN_PREFIX) {
            return Err(InstallError::InvalidBotToken);
        }
        let app_id = parse_slack_app_id(app_token)?;

        let auth = self
            .api
            .auth_test(bot_token)
            .await
            .map_err(|error| InstallError::Api {
                step: "auth.test",
                code: error.detail(),
            })?;
        if auth.team_id.is_empty() || auth.user_id.is_empty() || auth.bot_id.is_empty() {
            return Err(InstallError::IncompleteAuthTest);
        }

        // 证明两个令牌属于**同一个** app（见模块文档校验 3）。
        let bot_app_id = self
            .api
            .bot_app_id(bot_token, &auth.bot_id)
            .await
            .map_err(|error| InstallError::Api {
                step: "bots.info",
                code: error.detail(),
            })?;
        if bot_app_id != app_id {
            return Err(InstallError::TokenAppMismatch);
        }

        // 证明这个 app token **真的能**开连接（否则会存下一个永远收不到事件的行）。
        self.api
            .validate_app_token(app_token)
            .await
            .map_err(|error| InstallError::Api {
                step: "apps.connections.open",
                code: error.detail(),
            })?;

        let sealed_bot = self
            .boxed
            .seal(bot_token.as_bytes())
            .map_err(|_| InstallError::Seal)?;
        let sealed_app = self
            .boxed
            .seal(app_token.as_bytes())
            .map_err(|_| InstallError::Seal)?;
        let config = InstallConfig {
            app_id: app_id.clone(),
            team_id: auth.team_id.clone(),
            bot_user_id: auth.user_id.clone(),
            bot_token_encrypted: encode_ciphertext(&sealed_bot),
            app_token_encrypted: encode_ciphertext(&sealed_app),
        };
        let config = serde_json::to_value(&config).map_err(|_| InstallError::Encode)?;

        let outcome = self
            .store
            .persist(&PersistInstall {
                workspace_id: params.workspace_id,
                agent_id: params.agent_id,
                installer_user_id: params.initiator_user_id,
                app_id,
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
/// **不派生 `Debug`**（见下面的手写实现）：结构体里有两个**明文**令牌。
#[derive(Clone, PartialEq, Eq)]
pub struct RegisterByoParams {
    pub workspace_id: Id,
    pub agent_id: Id,
    pub initiator_user_id: Id,
    /// `xoxb-…`（出站 Web API）。
    pub bot_token: String,
    /// `xapp-…`（本 app **自己**的 Socket Mode 连接）。
    pub app_token: String,
}

impl RegisterByoParams {
    /// 装配（令牌按值给；本结构**不派生 `Debug` 的默认实现** —— 见下）。
    #[must_use]
    pub fn new(
        workspace_id: Id,
        agent_id: Id,
        initiator_user_id: Id,
        bot_token: impl Into<String>,
        app_token: impl Into<String>,
    ) -> Self {
        Self {
            workspace_id,
            agent_id,
            initiator_user_id,
            bot_token: bot_token.into(),
            app_token: app_token.into(),
        }
    }
}

impl fmt::Debug for RegisterByoParams {
    /// 手写脱敏（凭据纪律第 1 条）：结构体里有两个明文令牌，默认 `Debug` 会把它们
    /// 原样写进任何 `assert_eq!` 失败 / `{:?}` 插值 / panic 回显。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RegisterByoParams")
            .field("workspace_id", &self.workspace_id)
            .field("agent_id", &self.agent_id)
            .field("initiator_user_id", &self.initiator_user_id)
            .field("bot_token", &redact(&self.bot_token))
            .field("app_token", &redact(&self.app_token))
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

/// 从 app-level 令牌里解出**真实** Slack app id（上游 `parseSlackAppID`，逐字）。
///
/// 形态是 `xapp-1-<APP_ID>-<gen>-<secret>`（`SplitN(…, "-", 5)`）⇒ app id 是**第三段**，
/// 且必须以 `A` 开头。它就是每 app 的存储 / 路由键，让同一个 Slack workspace 里的多个
/// BYO app 互不干扰。
pub fn parse_slack_app_id(app_token: &str) -> Result<String, InstallError> {
    if !app_token.starts_with(APP_TOKEN_PREFIX) {
        return Err(InstallError::InvalidAppToken);
    }
    let parts: Vec<&str> = app_token.splitn(5, '-').collect();
    match parts.get(2) {
        Some(part) if !part.is_empty() && part.starts_with('A') && parts.len() >= 4 => {
            Ok((*part).to_string())
        }
        _ => Err(InstallError::InvalidAppToken),
    }
}

/// 本 adapter 的平台判别式（`slack`）。
#[must_use]
pub fn kind() -> ChannelKind {
    ChannelKind::Slack
}

#[cfg(test)]
mod tests;
