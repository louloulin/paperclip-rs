//! composio 服务 —— 上游 `integrations/composio/service.go`（709 行）+ `dispatch.go`（249 行）
//! 的**服务面**（M8-0 anchor 建桩、**M8-6 落地**：`LUM-1803`）。
//!
//! 一次连接生命周期（M8-6 的 `DoD`，`docs/61` §6.5）：
//!
//! ```text
//! connect init（签 state + 生成会话 URL）→ callback（验 state + 落 user_composio_connection）
//!   → 列表 / toolkit 目录 → 断开
//! ```
//!
//! # 落地边界（逐条对应 `docs/61` 的表格）
//!
//! | 件 | 上游 | 落点 |
//! | --- | --- | --- |
//! | 连接生命周期（begin / callback / list / disconnect） | `service.go` | 本文件 |
//! | toolkit 目录 + auth-config 解析 | `service.go` 的目录段 | [`crate::catalog`] |
//! | per-task overlay 的**构建** | `dispatch.go` | [`crate::overlay`] |
//! | state 的签发 / 校验 | `state.go` | [`crate::state`] |
//! | SDK wire（7 端点） | `pkg/composio` | [`crate::client`] |
//! | 落库 | `pkg/db/queries/composio.sql` | `mc_repos::composio::connection` |
//!
//! ⚠️ 门 ⑩ 预飞把本文件排在 600–800 行（`docs/61` §6.3）⇒ toolkit / auth-config 解析
//! **不**在本文件（在 [`crate::catalog`]），overlay 构建不在本文件（在 [`crate::overlay`]）。
//!
//! # 三个「未配置」判据别混用
//!
//! 1. [`ComposioConfig::is_configured`] —— **部署**四条件（flag + 三个密钥/基址），路由层用它
//!    决定 4 条会话路由的「未配置」语义；
//! 2. [`ComposioService::enabled`] —— 同一件事的服务侧视角（`new` 在未配置时**不** panic，
//!    只返回一个 `enabled()==false` 的服务）；
//! 3. **公开回调不看 1/2**（`docs/61` §6.5 的 M8-6 行：公开回调仍须按 state 判）——
//!    [`ComposioService::complete_callback`] 先把 state 验了再说，未配置只是让「验不过」
//!    这件事必然发生（空 secret 签不出/验不过 ⇒ `state` 拒绝 ⇒ 401）。

use std::collections::HashMap;

use mc_core::composio::ComposioToolkit;
use mc_core::Id;
use mc_repos::composio::connection::{
    ComposioConnectionRepo, ComposioConnectionRow, NewComposioConnection,
};
use mc_repos::RepoError;

use crate::catalog::{self, AuthConfigDirectory, DEFAULT_AUTH_CACHE_TTL_SECS};
use crate::client::ComposioClient;
use crate::state::{StateClaims, StateError, StateSigner, DEFAULT_STATE_TTL_SECS};

/// 回调路径（上游 `callbackPath` 常量逐字：**不可配置**，否则 SDK 的回调地址与路由会漂移）。
pub const CALLBACK_PATH: &str = "/api/integrations/composio/callback";

/// composio 面统一错误（**不得**含 API key / state secret / bearer / 响应体）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ComposioError {
    #[error("composio: transport error: {0}")]
    Transport(String),
    #[error("composio: unauthorized")]
    Unauthorized,
    #[error("composio: not configured")]
    NotConfigured,
    #[error("composio: malformed payload: {0}")]
    Malformed(String),
    /// 上游 `ErrToolkitNotSupported`：项目里没有该 toolkit 的可用 auth config。
    #[error("composio: toolkit not supported")]
    ToolkitNotSupported,
    /// 上游 `ErrConnectNotSuccessful`：Composio 回报的状态不是 `success`（**不**写活跃行）。
    #[error("composio: connection was not successful")]
    ConnectNotSuccessful,
    /// 上游 `ErrConnectionNotFound`：连接不存在，**或不属于**调用者（两者同判，不泄漏存在性）。
    #[error("composio: connection not found")]
    ConnectionNotFound,
    /// 上游 `ErrAccountVerification`：`connected_account_id` 无法被证实属于 state 里的
    /// 用户与该 toolkit 的 auth config（篡改 / 跨 toolkit 走私）⇒ **fail closed**。
    #[error("composio: connected account verification failed")]
    AccountVerification,
    /// 上游非 2xx（**只**带状态码与调用点：不回显 body、不回声请求）。
    #[error("composio: upstream error: {status} at {context}")]
    Upstream { status: u16, context: String },
    /// 本仓特有：服务没挂连接仓储（`new()` 之后没 `with_store()`）。只可能出现在单测里。
    #[error("composio: connection store is not attached")]
    StoreMissing,
    /// 本仓特有：落库失败。
    #[error("composio: store error: {0}")]
    Store(String),
}

/// composio 的**装配配置**（四个「未配置」条件缺一即不装配，`docs/61` §2.5）。
///
/// 手写 `Debug`（脱敏）：只暴露「配了哪些」。
#[derive(Clone, Default)]
pub struct ComposioConfig {
    /// `COMPOSIO_API_KEY`。
    pub api_key: Option<String>,
    /// `COMPOSIO_STATE_SECRET` 或由 `JWT_SECRET` 派生。
    pub state_secret: Option<String>,
    /// `COMPOSIO_CALLBACK_BASE_URL` 或 `MULTICA_PUBLIC_URL`。
    pub callback_base_url: Option<String>,
    /// feature flag（`mc-feature-flags`）—— 与密钥的组合，两层都过才装配。
    pub feature_enabled: bool,
    /// API base（替身接缝）。
    pub api_base: Option<String>,
    /// state 寿命（秒）覆盖；`None` ⇒ [`DEFAULT_STATE_TTL_SECS`]（上游 `Config.StateTTL`）。
    pub state_ttl_secs: Option<i64>,
}

impl ComposioConfig {
    /// 四个条件缺一即 `false`（`docs/61` §2.5 的 composio 行）。
    pub fn is_configured(&self) -> bool {
        self.feature_enabled
            && self.api_key.is_some()
            && self.state_secret.is_some()
            && self.callback_base_url.is_some()
    }

    /// 缺了哪些条件（诊断；**不**回显值）。
    pub fn missing(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if !self.feature_enabled {
            missing.push("feature_flag");
        }
        if self.api_key.is_none() {
            missing.push("COMPOSIO_API_KEY");
        }
        if self.state_secret.is_none() {
            missing.push("COMPOSIO_STATE_SECRET|JWT_SECRET");
        }
        if self.callback_base_url.is_none() {
            missing.push("COMPOSIO_CALLBACK_BASE_URL|MULTICA_PUBLIC_URL");
        }
        missing
    }
}

impl std::fmt::Debug for ComposioConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComposioConfig")
            .field("configured", &self.is_configured())
            .field("missing", &self.missing())
            .field("api_base", &self.api_base)
            .field("state_ttl_secs", &self.state_ttl_secs)
            .finish_non_exhaustive()
    }
}

/// 一条连接的**API 面视图**（上游 `Service.Connection` 逐字：不带 `connected_account_id` /
/// `auth_config_id` —— 那两个是服务端内部句柄，不是 API 面）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionView {
    pub id: String,
    pub toolkit_slug: String,
    pub status: String,
    /// RFC3339（上游 `row.ConnectedAt.Time.UTC().Format(time.RFC3339)`）。
    pub connected_at: String,
    /// RFC3339（上游 `util.TimestampToPtr`）。
    pub last_used_at: Option<String>,
}

/// toolkit 目录项的**API 面视图**（上游 `Service.ToolkitView` 逐字）。
///
/// `connectable` 自 MUL-4009 起恒 `true`（目录里只剩可连接的），字段保留是为了老桌面客户端
/// （上游注释逐字：dropping it would make them treat every entry as non-connectable and hide
/// the Connect button）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolkitView {
    pub slug: String,
    pub name: String,
    /// 上游 `logo,omitempty`（空串 = 不发该字段）。
    pub logo_url: String,
    /// 上游 `category,omitempty`（`categories[0]`）。
    pub category: String,
    pub connectable: bool,
}

/// 一个 tool-router（MCP）会话。
///
/// ⚠️ **只承载 URL**：`x-api-key` 头的取值走 [`ComposioService::client`] 的
/// `auth_headers()`（那条路径的返回值本身就是 bearer 材料，调用侧负责不进日志）。
/// 本结构没有 `Debug` 之外的内容，也不派生 `Debug` 之外的任何东西。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSession {
    /// 上游 `resp.MCP.URL`。
    pub url: String,
}

/// [`ComposioService::complete_callback`] 的三态判决（路由层据此决定 401 / 302 两种重定向）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallbackOutcome {
    /// state 不合法（篡改 / 过期 / 重放 / 形态非法）⇒ 路由回 **401**。
    ///
    /// 载荷里带着具体原因**只**为内部诊断（日志）；对外的响应**不区分**这四类
    /// （上游注释逐字：we never tell the browser which check failed）。
    StateRejected(StateError),
    /// state **合法**，但本次部署未装配（flag 关 / 缺 `COMPOSIO_API_KEY` / 缺 state secret /
    /// 缺回调基址）⇒ 路由回 **403**（上游 `writeFeatureDisabled` 的语义）。
    ///
    /// ⚠️ 它排在 state 校验**之后**：公开路由的身份来源就是 state，先证身份再谈部署能力
    /// （`docs/61` §6.5 的 M8-6 行：公开回调仍须按 state 判）。
    NotConfigured { toolkit_slug: String },
    /// state 合法但这次连接没成（上游状态非 `success` / 账号没验过 / 落库失败）⇒ 路由回
    /// 「失败重定向」，**不**写活跃行。
    Rejected {
        toolkit_slug: String,
        reason: ComposioError,
    },
    /// 落库成功（幂等：同一 `(user_id, connected_account_id)` 只会有一行）。
    Connected { toolkit_slug: String },
}

/// composio 服务（5 条 HTTP 路由 + task 派发服务的共用生产者）。
pub struct ComposioService {
    client: ComposioClient,
    signer: StateSigner,
    config: ComposioConfig,
    /// 项目 auth-config 目录的进程内缓存（`begin_connect` 与 `list_toolkits` 共用）。
    directory: AuthConfigDirectory,
    /// `user_composio_connection` 的仓储（`new()` 之后由 [`ComposioService::with_store`] 挂上）。
    store: Option<ComposioConnectionRepo>,
}

impl ComposioService {
    /// 装配 —— `is_configured()==false` 时返回一个 `enabled()==false` 的**空**服务
    /// （**不** panic：路由层据此给「未配置」语义，进程照常起）。
    pub fn new(config: ComposioConfig) -> Self {
        let client = ComposioClient::new(config.api_key.clone());
        let client = match &config.api_base {
            Some(base) => client.with_api_base(base.clone()),
            None => client,
        };
        let signer = StateSigner::new(config.state_secret.clone().unwrap_or_default())
            .with_ttl(config.state_ttl_secs.unwrap_or(DEFAULT_STATE_TTL_SECS));
        Self {
            client,
            signer,
            config,
            directory: AuthConfigDirectory::new(DEFAULT_AUTH_CACHE_TTL_SECS),
            store: None,
        }
    }

    /// 挂上连接仓储（路由层的装配助手负责；单测可按需跳过 ⇒ 只有落库方法会报
    /// [`ComposioError::StoreMissing`]）。
    #[must_use]
    pub fn with_store(mut self, store: ComposioConnectionRepo) -> Self {
        self.store = Some(store);
        self
    }

    /// 是否已装配（缺任一条件 ⇒ `false` ⇒ 4 条会话路由给「未配置」语义）。
    pub fn enabled(&self) -> bool {
        self.config.is_configured()
    }

    pub fn client(&self) -> &ComposioClient {
        &self.client
    }

    pub fn signer(&self) -> &StateSigner {
        &self.signer
    }

    /// 装配配置（脱敏 `Debug`）。
    pub fn config(&self) -> &ComposioConfig {
        &self.config
    }

    /// 回调地址：`{callback_base_url}/api/integrations/composio/callback`
    /// （上游 `callbackURL = base + callbackPath`，base 去尾斜杠）。
    ///
    /// # Errors
    ///
    /// 缺回调基址 ⇒ [`ComposioError::NotConfigured`]。
    pub fn callback_url(&self) -> Result<String, ComposioError> {
        let base = self
            .config
            .callback_base_url
            .as_deref()
            .map(str::trim)
            .unwrap_or_default();
        if base.is_empty() {
            return Err(ComposioError::NotConfigured);
        }
        Ok(format!("{}{CALLBACK_PATH}", base.trim_end_matches('/')))
    }

    // -----------------------------------------------------------------------
    // 连接生命周期
    // -----------------------------------------------------------------------

    /// `POST /connect/init` 的服务侧：解析 auth config → 签 state → 问 Composio 要托管授权页。
    ///
    /// 返回的 URL 是**用户浏览器**要去的地方（上游 `BeginConnect`）。
    ///
    /// # Errors
    ///
    /// - 该 toolkit 没有可用 auth config（或 slug 为空）⇒ [`ComposioError::ToolkitNotSupported`]；
    /// - 缺 state secret / 回调基址 / API key ⇒ [`ComposioError::NotConfigured`]；
    /// - 上游拒绝或不可达 ⇒ [`ComposioError::Upstream`] / [`ComposioError::Transport`]。
    pub async fn begin_connect(
        &self,
        user_id: Id,
        toolkit_slug: &str,
    ) -> Result<String, ComposioError> {
        // 先判「未配置」与回调基址（都不花网络往返），再问项目目录。
        if !self.enabled() {
            return Err(ComposioError::NotConfigured);
        }
        let callback_url = self.callback_url()?;

        let now = now_unix();
        let slug = catalog::normalize_slug(toolkit_slug);
        let auth_config_id = self
            .directory
            .auth_config_for(&self.client, &slug, now)
            .await?
            .ok_or(ComposioError::ToolkitNotSupported)?;
        let user = user_id.to_string();

        let claims = StateClaims::new(
            user.clone(),
            slug,
            auth_config_id.clone(),
            now,
            self.signer.ttl_secs(),
        );
        // 空 secret 是「未配置」的那一格（`StateSigner::sign` 对空 secret 直接拒绝）。
        let state = self
            .signer
            .sign(&claims)
            .map_err(|_| ComposioError::NotConfigured)?;

        // `state` 是 base64url + '.'（全部 URL 安全字符）⇒ 与上游 `url.QueryEscape` 的编码**逐字相同**。
        let callback_with_state = format!("{callback_url}?state={state}");
        let link = self
            .client
            .create_link(&auth_config_id, &user, &callback_with_state)
            .await?;
        Ok(link.redirect_url)
    }

    /// `GET /callback` 的服务侧（上游 `CompleteCallback`）。
    ///
    /// **公开路由**：本方法**不**看 [`ComposioService::enabled`] —— state 是唯一身份来源，
    /// 未配置只意味着「必然验不过」（`docs/61` §6.5 的 M8-6 行）。
    ///
    /// # 语义（逐条）
    ///
    /// 1. state 不合法 ⇒ [`CallbackOutcome::StateRejected`]（四类原因**不**外传）；
    /// 2. `status` 不是 `success` ⇒ `Rejected`（**不**写活跃行，但**仍**带出 slug 供重定向）；
    /// 3. `connected_account_id` 为空 / state 里的 user 不是 UUID ⇒ `Rejected`；
    /// 4. 向 Composio 复核账号归属 ⇒ 不符 / 查无 / auth config 不匹配 ⇒ `Rejected`（fail closed）；
    /// 5. `(user_id, connected_account_id)` upsert ⇒ 重复 callback **幂等**（同一行重新激活）。
    pub async fn complete_callback(
        &self,
        state: &str,
        status: &str,
        connected_account_id: &str,
    ) -> CallbackOutcome {
        let claims = match self.signer.verify(state, now_unix()) {
            Ok(claims) => claims,
            Err(error) => return CallbackOutcome::StateRejected(error),
        };
        let slug = claims.toolkit_slug.clone();
        let rejected = |reason: ComposioError| CallbackOutcome::Rejected {
            toolkit_slug: slug.clone(),
            reason,
        };

        // 身份已确认，轮到「本部署打开了这个功能吗」（`slug` 到这一支为止就交出去了）。
        if !self.enabled() {
            return CallbackOutcome::NotConfigured { toolkit_slug: slug };
        }

        if !status.trim().eq_ignore_ascii_case("success") {
            return rejected(ComposioError::ConnectNotSuccessful);
        }
        let account_id = connected_account_id.trim();
        if account_id.is_empty() {
            return rejected(ComposioError::Malformed(
                "callback is missing connected_account_id".into(),
            ));
        }
        let Ok(user_id) = Id::parse(claims.user_id.trim()) else {
            return rejected(ComposioError::Malformed(
                "state carries a non-uuid user id".into(),
            ));
        };
        let store = match self.require_store() {
            Ok(store) => store,
            Err(error) => return rejected(error),
        };
        if let Err(error) = self.verify_account_ownership(account_id, &claims).await {
            return rejected(error);
        }

        let upserted = store
            .upsert(NewComposioConnection {
                user_id,
                toolkit_slug: claims.toolkit_slug.clone(),
                auth_config_id: claims.auth_config_id.clone(),
                connected_account_id: account_id.to_string(),
                composio_user_id: claims.user_id.clone(),
            })
            .await;
        match upserted {
            Ok(_) => CallbackOutcome::Connected { toolkit_slug: slug },
            Err(error) => rejected(ComposioError::from(error)),
        }
    }

    /// `GET /connections` 的服务侧：调用者的活跃连接（上游 `ListConnections`）。
    ///
    /// # Errors
    ///
    /// 落库查询失败 ⇒ [`ComposioError::Store`]；没挂仓储 ⇒ [`ComposioError::StoreMissing`]。
    pub async fn list_connections(
        &self,
        user_id: Id,
    ) -> Result<Vec<ConnectionView>, ComposioError> {
        let rows = self
            .require_store()?
            .list_active(user_id)
            .await
            .map_err(ComposioError::from)?;
        Ok(rows.iter().map(connection_view).collect())
    }

    /// `DELETE /connections/{id}` 的服务侧：撤销上游 grant → 删上游记录 → 本地标 `revoked`
    /// （上游 `Disconnect`）。
    ///
    /// **幂等**（上游注释逐字）：本地已经不是 `active` ⇒ 纯 no-op；上游已经没了（404）⇒ 视为成功。
    ///
    /// # Errors
    ///
    /// 连接不存在/不属于调用者 ⇒ [`ComposioError::ConnectionNotFound`]；其余见 [`ComposioError`]。
    pub async fn disconnect(&self, user_id: Id, connection_id: Id) -> Result<(), ComposioError> {
        let store = self.require_store()?;
        let Some(row) = store
            .get(connection_id, user_id)
            .await
            .map_err(ComposioError::from)?
        else {
            return Err(ComposioError::ConnectionNotFound);
        };
        if !row.status.eq_ignore_ascii_case("active") {
            // 已经断开过 ⇒ 第二次 DELETE 是 no-op（上游 404-idempotent 契约的一半）。
            return Ok(());
        }
        match self
            .client
            .revoke_connection(&row.connected_account_id)
            .await
        {
            Ok(()) | Err(ComposioError::Upstream { status: 404, .. }) => {}
            Err(error) => return Err(error),
        }
        // `delete_connected_account` 自己把 404 当成功（上游 SDK 逐字）。
        self.client
            .delete_connected_account(&row.connected_account_id)
            .await?;
        store
            .mark_revoked(connection_id, user_id)
            .await
            .map_err(ComposioError::from)
    }

    /// `GET /toolkits` 的服务侧（上游 `ListToolkits`）：**只**返回项目可连接的 toolkit。
    ///
    /// 顺序 = 上游目录顺序（`sort_by=usage`），每个条目的 `auth_config_ids` 收窄成项目里
    /// 可用的那一份。解析失败**不**回落成「空目录」（上游注释逐字：that would render as a
    /// silent "no apps configured" empty state, which is misleading）⇒ 上抛错误让路由回 502。
    ///
    /// # Errors
    ///
    /// auth-config 解析失败 / 上游失败 ⇒ [`ComposioError`]。
    pub async fn list_toolkits(&self) -> Result<Vec<ToolkitView>, ComposioError> {
        let connectable = self.directory.resolve(&self.client, now_unix()).await?;
        if connectable.is_empty() {
            // 项目里一个可用的 auth config 都没有 ⇒ 目录为空（**不**为此再打一次上游）。
            return Ok(Vec::new());
        }
        let entries = self.client.list_toolkit_entries().await?;

        let mut categories: HashMap<String, String> = HashMap::new();
        let mut all: Vec<ComposioToolkit> = Vec::with_capacity(entries.len());
        for entry in entries {
            let slug = catalog::normalize_slug(&entry.slug);
            if slug.is_empty() {
                continue;
            }
            categories.insert(slug.clone(), entry.category);
            all.push(ComposioToolkit {
                slug: slug.clone(),
                name: entry.name,
                auth_config_ids: connectable.get(&slug).cloned().into_iter().collect(),
                logo_url: Some(entry.logo_url).filter(|url| !url.is_empty()),
            });
        }

        let available: Vec<String> = connectable.values().cloned().collect();
        Ok(catalog::visible_toolkits(all, &available)
            .into_iter()
            .map(|toolkit| ToolkitView {
                category: categories.get(&toolkit.slug).cloned().unwrap_or_default(),
                slug: toolkit.slug,
                name: toolkit.name,
                logo_url: toolkit.logo_url.unwrap_or_default(),
                // 目录里只剩可连接的 ⇒ 恒 true（上游 `ToolkitView.Connectable` 的注释逐字）。
                connectable: true,
            })
            .collect())
    }

    /// 开一个**用户级** MCP 会话（上游 `CreateMCPSession`）：用户没有活跃连接 ⇒ `None`。
    ///
    /// `connected_accounts` 按 toolkit 钉死成**用户自己**的账号 id（一个 toolkit 至多一条活跃
    /// 连接；行按 `connected_at DESC` 到货 ⇒ **最新者胜**，上游对此有逐字注释）。
    ///
    /// ⚠️ **本片只交付这条可注入的取数口**：把它的 URL 喂给 [`crate::overlay::build_task_overlay`]
    /// 的「3 处 enqueue 接线」**不在本波写集内**（R-M8-9，登记在 `docs/32` §9.12）。
    ///
    /// # Errors
    ///
    /// 落库查询失败 / 上游会话创建失败 ⇒ [`ComposioError`]。
    pub async fn create_mcp_session(
        &self,
        user_id: Id,
    ) -> Result<Option<McpSession>, ComposioError> {
        let rows = self
            .require_store()?
            .list_active(user_id)
            .await
            .map_err(ComposioError::from)?;
        if rows.is_empty() {
            return Ok(None);
        }
        let mut pinned: Vec<(String, String)> = Vec::new();
        for row in &rows {
            let slug = catalog::normalize_slug(&row.toolkit_slug);
            if slug.is_empty() || pinned.iter().any(|(seen, _)| *seen == slug) {
                continue;
            }
            pinned.push((slug, row.connected_account_id.clone()));
        }
        if pinned.is_empty() {
            return Ok(None);
        }
        let session = self
            .client
            .create_session(&user_id.to_string(), &pinned)
            .await?;
        // 上游 gate 4：上游 200 但没有 URL ⇒ 当作「没有 overlay」（每个 runtime 都会在空 URL 上炸）。
        if session.mcp_url.is_empty() {
            return Ok(None);
        }
        Ok(Some(McpSession {
            url: session.mcp_url,
        }))
    }

    // -----------------------------------------------------------------------
    // 内部
    // -----------------------------------------------------------------------

    /// 复核 `connected_account_id` 的归属（上游 `verifyAccountOwnership`，**fail closed**）。
    ///
    /// 三条必须同时成立：账号在 Composio 侧存在、`user_id` 与 state 里的一致、
    /// auth config 与 state 里签的那个**逐字相等**（空值也拒绝 —— 跳过就是那条
    /// cross-toolkit 绑定缺口，上游 PR 4608 review 点名的 fail-open 洞）。
    async fn verify_account_ownership(
        &self,
        connected_account_id: &str,
        claims: &StateClaims,
    ) -> Result<(), ComposioError> {
        let accounts = self
            .client
            .list_connected_accounts(&[connected_account_id.to_string()])
            .await?;
        let account = accounts
            .iter()
            .find(|account| account.id == connected_account_id)
            .ok_or(ComposioError::AccountVerification)?;
        if account.user_id != claims.user_id {
            return Err(ComposioError::AccountVerification);
        }
        let account_auth_config = if account.auth_config_id.is_empty() {
            account.auth_config_ref_id.as_str()
        } else {
            account.auth_config_id.as_str()
        };
        if claims.auth_config_id.is_empty() || account_auth_config != claims.auth_config_id {
            return Err(ComposioError::AccountVerification);
        }
        Ok(())
    }

    /// 连接仓储（没挂上 ⇒ [`ComposioError::StoreMissing`]）。
    fn require_store(&self) -> Result<&ComposioConnectionRepo, ComposioError> {
        self.store.as_ref().ok_or(ComposioError::StoreMissing)
    }
}

impl std::fmt::Debug for ComposioService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComposioService")
            .field("enabled", &self.enabled())
            .field("config", &self.config)
            .field("store", &self.store.is_some())
            .finish_non_exhaustive()
    }
}

/// 当前 Unix 秒（唯一的时钟读取点，服务面只在 state 的 `exp` 与缓存 TTL 上用它）。
fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

/// 库里那行 → API 面视图（上游 `rowToConnection`）。
fn connection_view(row: &ComposioConnectionRow) -> ConnectionView {
    ConnectionView {
        id: row.id.to_string(),
        toolkit_slug: row.toolkit_slug.clone(),
        status: row.status.clone(),
        connected_at: row
            .connected_at
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        last_used_at: row
            .last_used_at
            .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
    }
}

impl From<RepoError> for ComposioError {
    /// 仓储错误 → 服务错误（**只**带 `RepoError` 的 Display：不含凭据）。
    fn from(error: RepoError) -> Self {
        Self::Store(error.to_string())
    }
}

#[cfg(test)]
mod tests;
