//! lark **设备流会话状态机与服务**（上游 `registration_service.go` 的**服务那一半**，858 行里
//! 大约 400 行属此）。
//!
//! 写者 **M7-14**。拆出来是**门 ⑩**（单文件 800 行硬限）的要求；本模块是"把协议客户端与共享
//! 会话表缝起来"的那一半：开会话 → 后台轮询 → 终态落库（一个事务）。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use mc_core::id::Id;
use mc_secrets::secretbox::SecretBox;

use super::session::{
    InstallSessionOutcome, InstallSessionState, InstallSessionStore, SessionNotFound,
};
use super::{
    bot_name_preset, reason, PollResult, RegistrationClient, RegistrationError, SessionStatus,
    DEFAULT_POLL_SECONDS, DEFAULT_SESSION_TTL,
};
use crate::lark::client::{ApiClient, TokenCacheInvalidator};
use crate::lark::installation::{InstallationService, PersistOutcome};
use crate::lark::params::{AppSecret, InstallationCredentials};
use crate::lark::types::{OpenId, Region};

// =====================================================================
// 端口（落库那一步）
// =====================================================================

/// 设备流成功之后**一个事务里**要做的三件事（上游 `finishSuccess` 的 tx 段）。
///
/// 三条 SQL 的串行化在实现里；**判决**（分类）由实现调用 [`classify_live_owner`] 得到，
/// 于是判决表仍在本 crate 里可测（**D3**）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInstall {
    /// 目标 workspace。
    pub workspace_id: Id,
    /// 目标 agent。
    pub agent_id: Id,
    /// 安装者（同时是 `installer_user_id` 与绑定的 `multica_user_id`）。
    pub initiator_id: Id,
    /// 新铸的 `client_id`。
    pub app_id: String,
    /// `app_id` 的**明文** `client_secret`（实现侧负责封）。
    pub client_secret: String,
    /// Bot 的按安装 `open_id`（下一行绑定的 `lark_open_id`）。
    pub bot_open_id: OpenId,
    /// Bot 的 `union_id`（尽力而为）。
    pub bot_union_id: String,
    /// 安装所在的云。
    pub region: Region,
    /// 安装者的 Lark `open_id`（**下一行**绑定的目标；与 [`Self::bot_open_id`] 是两个不同的
    /// 身份 —— 前者是装出来的 Bot，后者是发起人自己）。
    pub installer_open_id: OpenId,
}

/// 注册服务需要的落库面。
#[async_trait]
pub trait RegistrationStore: Send + Sync {
    /// 该 agent 是不是这个 workspace 的；是 ⇒ 回它的**显示名**（预填 Bot 名用）。
    ///
    /// # Errors
    ///
    /// 存储层故障。
    async fn agent_name_in_workspace(
        &self,
        workspace_id: Id,
        agent_id: Id,
    ) -> Result<Option<String>, String>;

    /// 回收死主 + upsert 安装 + 绑定安装者，**一个事务**。
    ///
    /// # Errors
    ///
    /// 存储层故障（分类冲突走 `Ok(PersistOutcome::Conflict)`，不是 `Err`）。
    async fn commit_install(&self, params: &CommitInstall) -> Result<PersistOutcome, String>;
}

// =====================================================================
// 会话状态机
// =====================================================================

/// 注册服务的时钟（可注入 ⇒ 过期边界确定性）。
pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// 会话状态机的配置（上游 `RegistrationServiceConfig`）。
#[derive(Clone)]
pub struct RegistrationServiceConfig {
    /// 终态会话的保活窗口。
    pub session_ttl: Duration,
    /// 时钟。
    pub now: Clock,
    /// 终态写入的重试次数（上游 `terminalWriteAttempts`）。
    pub terminal_write_attempts: u32,
    /// 重试的初始退避（指数增长到 [`Self::terminal_write_max_backoff`]）。
    pub terminal_write_initial_backoff: Duration,
    /// 重试退避的上限。
    pub terminal_write_max_backoff: Duration,
}

impl Default for RegistrationServiceConfig {
    fn default() -> Self {
        Self {
            session_ttl: DEFAULT_SESSION_TTL,
            now: Arc::new(Utc::now),
            terminal_write_attempts: 10,
            terminal_write_initial_backoff: Duration::from_millis(200),
            terminal_write_max_backoff: Duration::from_secs(15),
        }
    }
}

/// 把本平台挂进 `Registry` 之外，本片真正的"服务"：设备流安装的生命周期。
///
/// 它是唯一同时做这四件事的地方：开会话 / 在共享表里跟踪状态 / 跑后台轮询 / 成功时
/// 用新铸的凭据取 Bot 信息并**一个事务**写安装行 + 安装者绑定。
pub struct RegistrationService {
    config: RegistrationServiceConfig,
    client: RegistrationClient,
    api: Arc<dyn ApiClient>,
    /// 令牌缓存失效口（上游用类型断言拿的那个 `TokenCacheInvalidator`）。
    ///
    /// 生产接线把同一个 `HttpApiClient` 作为**两个** `Arc` 递进来；替身给了也没有物可忘。
    /// 形态差异登记 `docs/32` §30 的 **D7**：上游 `if invalidator, ok := s.api.(TokenCacheInvalidator)`
    /// 在 Go 里不需要额外的依赖位，Rust 的 `dyn` 没有类型断言 ⇒ 把那个可选能力显式化，
    /// 而不是把 `ApiClient` trait（M7-10 已合、**冻结**）再加一个方法。
    invalidator: Option<Arc<dyn TokenCacheInvalidator + Send + Sync>>,
    store: Arc<dyn RegistrationStore>,
    installs: Arc<InstallationService>,
    sessions: Arc<dyn InstallSessionStore>,
}

impl RegistrationService {
    /// 装配（六个必需项；缺任一 ⇒ 调用点自己决定不装配，本仓是值类型 ⇒ 没有"半成品"状态）。
    #[must_use]
    pub fn new(
        config: RegistrationServiceConfig,
        client: RegistrationClient,
        api: Arc<dyn ApiClient>,
        invalidator: Option<Arc<dyn TokenCacheInvalidator + Send + Sync>>,
        store: Arc<dyn RegistrationStore>,
        installs: Arc<InstallationService>,
        sessions: Arc<dyn InstallSessionStore>,
    ) -> Self {
        Self {
            config,
            client,
            api,
            invalidator,
            store,
            installs,
            sessions,
        }
    }

    /// 借出封装盒（路由层封 `client_secret` 用；与 [`InstallationService::boxed`] 同一个）。
    #[must_use]
    pub fn boxed(&self) -> &SecretBox {
        self.installs.boxed()
    }

    /// 开一个设备流会话并起后台轮询。返回喂给 QR 对话框的那一包。
    ///
    /// # Errors
    ///
    /// workspace / agent / initiator 缺一 / agent 不属于该 workspace / `begin` 失败 /
    /// 会话登记失败。
    pub async fn begin_install(
        self: &Arc<Self>,
        workspace_id: Id,
        agent_id: Id,
        initiator_id: Id,
        region: Region,
    ) -> Result<BeginInstallResult, RegistrationError> {
        if workspace_id.0.is_nil() || agent_id.0.is_nil() || initiator_id.0.is_nil() {
            return Err(RegistrationError::new(
                "invalid_argument",
                "workspace, agent, and initiator are required",
            ));
        }
        // agent↔workspace 预检：没有它，调用方猜一个 UUID 就能对**别的** workspace 的 agent
        // 开会话，而对着 Lark 铸出的 `device_code` **照样**能换到凭据。
        // 同时把 agent 名留下 —— 它预填 Lark 建 PersonalAgent 表单上的名字。
        let agent_name = self
            .store
            .agent_name_in_workspace(workspace_id, agent_id)
            .await
            .map_err(|message| RegistrationError::new("store", message))?
            .ok_or_else(|| RegistrationError::new("invalid_argument", "agent not in workspace"))?;

        let region = Region::or_default(region.as_str());
        let begin = self
            .client
            .begin(&bot_name_preset(&agent_name), region)
            .await
            .map_err(|error| RegistrationError::new("begin", error.to_string()))?;
        let (now, session_id) = ((self.config.now)(), random_session_id());
        let expires_at = now + chrono::Duration::from_std(begin.expires_in).unwrap_or_default();

        // **先**登记会话、**再**起协程。这一步失败 ⇒ 整个 begin 失败：一个浏览器永远读不到的
        // 会话比没有 QR 更坏 —— 它会渲染出一张能扫的码，然后在第一次轮询报 "session lost"，
        // 正是这张表要防的那个 bug。
        //
        // 保活覆盖 QR 窗口 **加上** 终态读窗口，所以最后一秒才结束的会话在结束后仍读得到。
        // 按 Lark 的 `expires_in`（现在 1h）算而不是写死常数，正是为了两者不漂。
        let ttl = begin.expires_in + self.config.session_ttl;
        self.sessions
            .create(
                InstallSessionState {
                    id: session_id.clone(),
                    workspace_id,
                    initiator_id,
                    status: SessionStatus::Pending,
                    installation_id: None,
                    error_reason: String::new(),
                    error_message: String::new(),
                    expires_at,
                },
                ttl,
            )
            .await
            .map_err(|message| RegistrationError::new("store", message))?;

        let session = PollingSession {
            id: session_id.clone(),
            workspace_id,
            agent_id,
            initiator_id,
            device_code: begin.device_code,
            domain: begin.domain,
            interval: begin.interval,
            expires_at,
            region,
        };
        // 轮询协程比请求上下文活得久 ⇒ 它拿 `Arc` 的一份，而不是借 `&self`。
        let this = Arc::clone(self);
        tokio::spawn(async move { this.run_polling(session).await });

        Ok(BeginInstallResult {
            session_id,
            qr_code_url: begin.qr_code_url,
            expires_in_seconds: begin.expires_in.as_secs(),
            poll_interval_seconds: begin.interval.as_secs(),
        })
    }

    /// 读一条在飞 / 刚结束的会话。
    ///
    /// workspace 必需：一个 workspace 发起的会话不能被另一个轮询。
    ///
    /// # Errors
    ///
    /// 未知 / 过期出队 / 属于别的 workspace ⇒ [`SessionNotFound`]。
    pub async fn get_session(
        &self,
        workspace_id: Id,
        session_id: &str,
    ) -> Result<InstallSessionState, SessionNotFound> {
        if session_id.trim().is_empty() {
            return Err(SessionNotFound);
        }
        // 读**共享表**、不读进程内 map：拥有这个会话的协程可能在另一个后端进程里。
        let mut state = self.sessions.get(workspace_id, session_id).await?;
        // 过了 Lark 的窗口还是 `pending` ⇒ 拥有轮询协程的那个进程死在记录过期之前，
        // 没有别的副本能补完那一枪。从时间戳报过期，而不是让对话框永远轮一个不会动的会话。
        if state.status == SessionStatus::Pending && state.expires_at <= (self.config.now)() {
            state.status = SessionStatus::Error;
            state.error_reason = reason::EXPIRED.to_string();
            state.error_message = "QR expired before authorization".to_string();
        }
        Ok(state)
    }

    /// 后台轮询循环：等 → 投 → 按结果分支（与上游 SDK 同形）。
    async fn run_polling(self: Arc<Self>, mut session: PollingSession) {
        let mut interval = if session.interval.is_zero() {
            Duration::from_secs(DEFAULT_POLL_SECONDS)
        } else {
            session.interval
        };
        let mut region = session.region;
        loop {
            let remaining = session.expires_at - (self.config.now)();
            if remaining <= chrono::Duration::zero() {
                tracing::info!(session_id = %session.id, "lark registration: session expired");
                self.mark_error(&session, reason::EXPIRED, "QR expired before authorization")
                    .await;
                return;
            }
            let wait = interval.min(remaining.to_std().unwrap_or_default());
            tokio::time::sleep(wait).await;

            let polled = self
                .client
                .poll(&session.domain, &session.device_code)
                .await;
            let result = match polled {
                Ok(result) => result,
                Err(error) => {
                    // 带类型的协议错 ⇒ 终态；纯传输错（DNS / 网络）⇒ 下一个 tick 重试，
                    // 于是 30 秒的跨区抖动能自愈。
                    if error.code == "transport" {
                        tracing::warn!(session_id = %session.id, "lark registration: transport error");
                        continue;
                    }
                    tracing::warn!(session_id = %session.id, code = %error.code, "lark registration: protocol error");
                    self.mark_error(&session, reason::PROTOCOL, &error.to_string())
                        .await;
                    return;
                }
            };

            if result.is_switch() {
                // 换云 —— **不**等 interval 就改道重投（Lark 只在过渡那一枪发一次 brand）。
                session.domain = result.switched_domain.clone();
                if let Some(next) = result.switched_region {
                    region = next;
                }
                tracing::info!(session_id = %session.id, "lark registration: switched cloud");
                continue;
            }
            if result.is_success() {
                self.finish_success(&session, &result, region).await;
                return;
            }
            if let Some(error) = result.error.as_ref() {
                let code = match error.code.as_str() {
                    "access_denied" => reason::ACCESS_DENIED,
                    "expired_token" => reason::EXPIRED,
                    _ => reason::PROTOCOL,
                };
                tracing::info!(session_id = %session.id, code = %error.code, "lark registration: terminal error");
                self.mark_error(&session, code, &error.to_string()).await;
                return;
            }
            if result.status == "slow_down" {
                // 认 Lark 的退避：+5s（RFC 8628 §3.5）。
                interval += Duration::from_secs(5);
            }
            // `authorization_pending` 与其余情况：保持 interval，接着转。
        }
    }

    /// 轮询之后的收尾：取 Bot 信息 → **一个事务**写安装行 + 安装者绑定。
    async fn finish_success(&self, session: &PollingSession, result: &PollResult, region: Region) {
        let Some(installer_open_id) = result.open_id.clone() else {
            self.mark_error(
                session,
                reason::BOT_INFO_FAILED,
                "missing installer open_id",
            )
            .await;
            return;
        };
        // 重新注册一个既有的 Bot 会在**同一个** `client_id` 下发一把新的 `client_secret`，
        // Lark 随即吊销用旧密钥铸出的**每一个**令牌 —— 而客户端的缓存键（`app_id`）没变
        // ⇒ 先叫它忘掉，否则下面第一次调用（以及之后每一次出站）都在重放一枚已被拒的令牌。
        if let Some(invalidator) = self.invalidator.as_ref() {
            invalidator.invalidate_token_cache(&result.client_id);
        }
        let credentials = InstallationCredentials::new(
            result.client_id.clone(),
            AppSecret::new(result.client_secret.clone()),
        )
        .with_region(region);
        let info = match self.api.get_bot_info(credentials).await {
            Ok(info) => info,
            Err(error) => {
                tracing::warn!(session_id = %session.id, "lark registration: bot info failed");
                self.mark_error(session, reason::BOT_INFO_FAILED, &error.to_string())
                    .await;
                return;
            }
        };
        if info.open_id.is_empty() {
            self.mark_error(session, reason::BOT_INFO_FAILED, "bot info missing open_id")
                .await;
            return;
        }

        let commit = CommitInstall {
            workspace_id: session.workspace_id,
            agent_id: session.agent_id,
            initiator_id: session.initiator_id,
            app_id: result.client_id.clone(),
            client_secret: result.client_secret.clone(),
            bot_open_id: info.open_id.clone(),
            bot_union_id: info.union_id.clone(),
            region,
            installer_open_id,
        };
        match self.store.commit_install(&commit).await {
            Ok(PersistOutcome::Stored(install)) => {
                self.mark_success(session, Some(install.id)).await;
                tracing::info!(
                    session_id = %session.id,
                    workspace_id = %session.workspace_id,
                    agent_id = %session.agent_id,
                    installation_id = %install.id,
                    "lark registration: install complete"
                );
            }
            Ok(PersistOutcome::Conflict(error)) => {
                self.mark_error(session, reason::INSTALLATION_CONFLICT, &error.to_string())
                    .await;
            }
            Ok(PersistOutcome::UnclassifiedConflict) => {
                let error = crate::lark::installation::InstallError::ConflictUnclassified;
                self.mark_error(session, reason::INSTALLATION_CONFLICT, &error.to_string())
                    .await;
            }
            Err(message) => {
                tracing::warn!(session_id = %session.id, "lark registration: commit install failed");
                self.mark_error(session, reason::INTERNAL_ERROR, &message)
                    .await;
            }
        }
    }

    /// 记录终态成功。
    async fn mark_success(&self, session: &PollingSession, installation_id: Option<Id>) {
        self.mark_terminal(
            session,
            InstallSessionOutcome {
                status: SessionStatus::Success,
                installation_id,
                error_reason: String::new(),
                error_message: String::new(),
            },
        )
        .await;
    }

    /// 记录终态失败。
    async fn mark_error(&self, session: &PollingSession, reason_code: &str, message: &str) {
        self.mark_terminal(
            session,
            InstallSessionOutcome {
                status: SessionStatus::Error,
                installation_id: None,
                error_reason: reason_code.to_string(),
                error_message: message.to_string(),
            },
        )
        .await;
    }

    /// 写终态。写入按指数退避**重试**，会话在它落定之前一直留着。
    async fn mark_terminal(&self, session: &PollingSession, outcome: InstallSessionOutcome) {
        let mut delay = self.config.terminal_write_initial_backoff;
        let attempts = self.config.terminal_write_attempts.max(1);
        for attempt in 1..=attempts {
            if self
                .sessions
                .mark_terminal(&session.id, outcome.clone(), self.config.session_ttl)
                .await
                .is_ok()
            {
                return;
            }
            if attempt == attempts {
                break;
            }
            tracing::warn!(
                session_id = %session.id,
                attempt,
                "lark registration: recording session outcome failed, retrying"
            );
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(self.config.terminal_write_max_backoff);
        }
        // 重试耗尽。成功那一支的安装本身是活的、事件也已经发了 ⇒ 工作区看到的是 Bot 已连上，
        // 只有这一个对话框会超时。把结局记下来，能人工找回来。
        tracing::error!(
            session_id = %session.id,
            status = outcome.status.as_str(),
            reason = %outcome.error_reason,
            "lark registration: gave up recording session outcome"
        );
    }
}

/// 轮询协程的工作态（上游 `registrationSession`）。
///
/// `device_code` 故意留在这里、**从不**持久化：它是持有者凭证，只有跑协程的进程有用。
struct PollingSession {
    id: String,
    workspace_id: Id,
    agent_id: Id,
    initiator_id: Id,
    device_code: String,
    domain: String,
    interval: Duration,
    expires_at: DateTime<Utc>,
    region: Region,
}

/// `begin` 的公开产物（上游 `BeginInstallResult`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeginInstallResult {
    /// 浏览器用来轮询状态的不透明句柄。
    pub session_id: String,
    /// QR 目标。
    pub qr_code_url: String,
    /// 设备码寿命（秒）。
    pub expires_in_seconds: u64,
    /// 建议轮询节奏（秒）。
    pub poll_interval_seconds: u64,
}

/// 会话 id：两个 v4 UUID 拼出的 32 字节随机（本 crate 无 `rand` 依赖，
/// 与 `wecom::binding::random_binding_token` 同一手法）。
#[must_use]
pub fn random_session_id() -> String {
    use base64::Engine as _;

    let mut bytes = [0_u8; 32];
    bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}
