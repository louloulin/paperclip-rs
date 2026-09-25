//! composio SDK 的 HTTP 客户端 —— 上游 `pkg/composio`（`Options{APIKey}` 面 + 7 个端点）
//! （M8-0 anchor 落形状与 `api_base` 接缝，**M8-6 落地**：`LUM-1803`）。
//!
//! # `api_base` 是离线替身的接缝（R-M8-1，`docs/61` §4.2）
//!
//! 上游 composio SDK 的 API base 可注入 ⇒ 本地 HTTP 替身按 `toolkits` / `auth_configs` /
//! `connected_accounts` 的形状答，端到端断言链（connect → callback → 落库 → toolkits）
//! 不需要真连 composio。
//!
//! # 凭据纪律
//!
//! `COMPOSIO_API_KEY` 是 `x-api-key` 头的值，**不派生 `Debug`**（手写脱敏）。
//! 它**不**进错误值：非 2xx 的错误只带 HTTP 状态与上游的 `error.slug`（不透明枚举串，
//! 见 [`ComposioError::Upstream`]），**不带**响应体、不带请求头
//! （`docs/61` §2.4 的 redaction 第 3 条：错误路径不回显凭据 —— 由
//! `errors_never_echo_the_api_key` 钉住）。
//!
//! # 端点表（逐字来自上游 `pkg/composio`，**7 条**）
//!
//! | 方法 | 路径 | 上游 | 本文件 |
//! | --- | --- | --- | --- |
//! | GET | `/toolkits?limit=1000&sort_by=usage[&cursor=]` | `toolkits.go` `ListToolkits` | [`ComposioClient::list_toolkit_entries`] |
//! | GET | `/auth_configs?limit=1000[&cursor=]` | `auth_configs.go` `ListAuthConfigs` | [`ComposioClient::list_auth_configs`] |
//! | POST | `/connected_accounts/link` | `connected_accounts.go` `CreateLink` | [`ComposioClient::create_link`] |
//! | GET | `/connected_accounts?connected_account_ids=` | 同上 `ListConnectedAccounts` | [`ComposioClient::list_connected_accounts`] |
//! | POST | `/connected_accounts/{id}/revoke` | 同上 `RevokeConnection` | [`ComposioClient::revoke_connection`] |
//! | DELETE | `/connected_accounts/{id}` | 同上 `DeleteConnectedAccount`（404 视为成功） | [`ComposioClient::delete_connected_account`] |
//! | POST | `/tool_router/session` | `sessions.go` `CreateSession` | [`ComposioClient::create_session`] |
//!
//! 分页抓取的页数上限（`MAX_LIST_PAGES` = 20）与每页条数（`LIST_PAGE_LIMIT` = 1000）在
//! [`crate::catalog`]（上游 `maxToolkitPages` / `maxAuthConfigPages` / `listPageLimit` 逐字）。

use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::catalog::{LIST_PAGE_LIMIT, MAX_LIST_PAGES};
use crate::service::ComposioError;

/// composio API 的默认 base（上游 `pkg/composio.DefaultBaseURL` 逐字：
/// `https://backend.composio.dev/api/v3.1`）。
///
/// ⚠️ anchor 期这里是 `…/api/v3`（少一个 `.1`）；M8-6 照上游 `DefaultBaseURL` 修正
/// （登记在 `docs/32` §9.12）。本仓只钉形状：若上游 SDK 以后改端点，改这一处。
pub const DEFAULT_API_BASE: &str = "https://backend.composio.dev/api/v3.1";

/// 每个请求都带的 `User-Agent`（上游 `DefaultUserAgent` = `multica-composio-go/0.1` 的本地对应物）。
pub const DEFAULT_USER_AGENT: &str = "multica-rs-composio/0.1";

/// `x-api-key` 头名（上游 `Client.APIKeyHeader` 逐字）。
pub const API_KEY_HEADER: &str = "x-api-key";

/// 单连接的请求超时：上游 SDK 的 `DefaultTimeout` 逐字（30 秒）。
pub const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// 一个 auth config（上游 `authconfigs.AuthConfig` 的**已消费子集**）。
///
/// `toolkit_slug` 是从上游嵌套的 `toolkit.slug` 里提上来的（本仓的归约键就是 slug）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthConfig {
    /// `ac_…`（**不透明配置句柄**，不是凭据）。
    pub id: String,
    /// 规范化前的原始 slug（归约时由 [`crate::catalog::normalize_slug`] 处理）。
    pub toolkit_slug: String,
    /// `true` = Composio 托管的 OAuth 应用；`false` = 客户自带（白标通路）。
    pub is_composio_managed: bool,
    /// `ENABLED` / `DISABLED`（上游原样小写化前的字面量）。
    pub status: String,
    /// RFC3339；只有字典序参与选择（上游 `betterAuthConfig` 逐字比较字符串）。
    pub last_updated_at: String,
}

/// toolkit 目录的一行（上游 `Toolkit` 的已消费子集 + 上游 `ListToolkits` 的排序语义）。
///
/// 与 [`mc_core::composio::ComposioToolkit`] 的分工：领域的那个**没有** `category`
/// （它在 `mc-core` 里，M8-0 冻结）；本结构保留上游的 `categories` 首项 ⇒ 服务面用它，
/// 领域投影走 [`ComposioClient::list_toolkits`]。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolkitEntry {
    pub slug: String,
    pub name: String,
    pub logo_url: String,
    /// 上游 `categories[0]`（空数组 ⇒ 空串）。
    pub category: String,
}

/// 一个已连接的账号（上游 `ConnectedAccount` 的已消费子集）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectedAccount {
    /// `ca_…`。
    pub id: String,
    /// 上游的 `user_id`（本仓的不变量：它等于 Multica 用户 id 的字符串形态）。
    pub user_id: String,
    /// 顶层的 `auth_config_id`（老形态）。
    pub auth_config_id: String,
    /// 嵌套的 `auth_config.id`（新形态；两者上游都返回，校验时**任一**匹配即可）。
    pub auth_config_ref_id: String,
    /// 嵌套的 `toolkit.slug`。
    pub toolkit_slug: String,
    /// `ACTIVE` / `EXPIRED` / …（上游原样）。
    pub status: String,
}

/// `POST /connected_accounts/link` 的结果（上游 `CreateLinkResponse` 的已消费子集）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedLink {
    /// 用户浏览器要去的托管授权页（上游 `redirect_url`）。
    pub redirect_url: String,
    /// 一次性 link token（**不**回显给客户端，只用于诊断）。
    pub link_token: String,
    /// 上游预分配的账号 id（callback 时由 Composio 再带回）。
    pub connected_account_id: String,
    /// RFC3339 过期时刻。
    pub expires_at: String,
}

/// `POST /tool_router/session` 的结果（上游 `CreateSessionResponse.MCP`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRouterSession {
    pub session_id: String,
    /// 流式 HTTP MCP 的 URL（上游 `mcp.url`）。
    pub mcp_url: String,
    /// 上游 `mcp.type`（逐字 `http`）。
    pub mcp_type: String,
}

/// 用 `x-api-key` 认证的 composio 客户端。
pub struct ComposioClient {
    /// `COMPOSIO_API_KEY`；未配置为 `None`（⇒ 整体不装配）。
    pub(crate) api_key: Option<String>,
    /// API base（替身接缝）。
    pub(crate) api_base: String,
    /// 连接池。
    pub(crate) http: reqwest::Client,
}

impl ComposioClient {
    /// 构造（未配置 api key 也可构造，只是 [`ComposioClient::enabled`] 为 `false`）。
    pub fn new(api_key: Option<String>) -> Self {
        Self {
            api_key,
            api_base: DEFAULT_API_BASE.to_string(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
                .build()
                .unwrap_or_default(),
        }
    }

    /// 自定义 base（**测试/替身唯一入口**）。
    #[must_use]
    pub fn with_api_base(mut self, api_base: impl Into<String>) -> Self {
        self.api_base = api_base.into();
        self
    }

    /// 是否配了 API key。
    pub fn enabled(&self) -> bool {
        self.api_key.is_some()
    }

    pub fn api_base(&self) -> &str {
        &self.api_base
    }

    /// 出站请求要带的认证头（上游 `MCPAuthHeaders` / `APIKeyHeader` 逐字）。
    ///
    /// ⚠️ 调用侧拿到的就是 **bearer 材料**：它只允许进两个地方 —— ① 出站请求头；
    /// ② per-task overlay 的 server 条目（M8-7 的尾账）。**不得**进日志 / 响应 / 错误值。
    ///
    /// # Errors
    ///
    /// 未配置 api key ⇒ [`ComposioError::NotConfigured`]（**不**返回空头）。
    pub fn auth_headers(&self) -> Result<Vec<(String, String)>, ComposioError> {
        let key = self.api_key()?;
        Ok(vec![(API_KEY_HEADER.to_string(), key.to_string())])
    }

    // -----------------------------------------------------------------------
    // toolkit 目录
    // -----------------------------------------------------------------------

    /// 逐页抓完 toolkit 目录（上游 `ListToolkits`，`Limit=1000` / `SortBy=usage`）。
    ///
    /// # Errors
    ///
    /// 传输失败 / 未配置 / 401 / 载荷非法 ⇒ 见 [`ComposioError`]。
    pub async fn list_toolkit_entries(&self) -> Result<Vec<ToolkitEntry>, ComposioError> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut cursor = String::new();
        for _ in 0..MAX_LIST_PAGES {
            let page: ToolkitPage = self
                .get_json(&toolkit_path(&cursor), "list toolkits")
                .await?;
            for item in page.items {
                let slug = item.slug.trim().to_string();
                if slug.is_empty() || !seen.insert(slug.clone()) {
                    continue;
                }
                out.push(ToolkitEntry {
                    slug,
                    name: item.name,
                    logo_url: item.logo,
                    category: item.categories.into_iter().next().unwrap_or_default(),
                });
            }
            if page.next_cursor.is_empty() {
                return Ok(out);
            }
            cursor = page.next_cursor;
        }
        Ok(out)
    }

    /// 列出 toolkit 目录 —— **领域投影**（anchor 的签名；`auth_config_ids` 由服务侧按
    /// 项目目录填，本方法只留空）。
    ///
    /// # Errors
    ///
    /// 同 [`ComposioClient::list_toolkit_entries`]。
    pub async fn list_toolkits(
        &self,
    ) -> Result<Vec<mc_core::composio::ComposioToolkit>, ComposioError> {
        Ok(self
            .list_toolkit_entries()
            .await?
            .into_iter()
            .map(|entry| mc_core::composio::ComposioToolkit {
                slug: entry.slug,
                name: entry.name,
                auth_config_ids: Vec::new(),
                logo_url: Some(entry.logo_url).filter(|url| !url.is_empty()),
            })
            .collect())
    }

    // -----------------------------------------------------------------------
    // auth configs
    // -----------------------------------------------------------------------

    /// 逐页抓完项目的 auth config（上游 `ListAuthConfigs`，`ShowDisabled=false` / `Limit=1000`）。
    ///
    /// 上游只在 `ShowDisabled` 为真时才发 `show_disabled=true` ⇒ 本方法**不发**该参数
    /// （默认即「只返回启用的」）。
    ///
    /// # Errors
    ///
    /// 同 [`ComposioClient::list_toolkit_entries`]。
    pub async fn list_auth_configs(&self) -> Result<Vec<AuthConfig>, ComposioError> {
        let mut out = Vec::new();
        let mut cursor = String::new();
        for _ in 0..MAX_LIST_PAGES {
            let page: AuthConfigPage = self
                .get_json(&auth_config_path(&cursor), "list auth configs")
                .await?;
            for item in page.items {
                out.push(AuthConfig {
                    id: item.id,
                    toolkit_slug: item.toolkit.slug,
                    is_composio_managed: item.is_composio_managed,
                    status: item.status,
                    last_updated_at: item.last_updated_at,
                });
            }
            if page.next_cursor.is_empty() {
                return Ok(out);
            }
            cursor = page.next_cursor;
        }
        Ok(out)
    }

    // -----------------------------------------------------------------------
    // 连接生命周期
    // -----------------------------------------------------------------------

    /// 起一次托管授权（上游 `CreateLink`，`POST /connected_accounts/link`）。
    ///
    /// # Errors
    ///
    /// 入参为空 ⇒ [`ComposioError::Malformed`]；其余同
    /// [`ComposioClient::list_toolkit_entries`]。
    pub async fn create_link(
        &self,
        auth_config_id: &str,
        user_id: &str,
        callback_url: &str,
    ) -> Result<CreatedLink, ComposioError> {
        if auth_config_id.is_empty() || user_id.is_empty() {
            return Err(ComposioError::Malformed(
                "create link needs an auth config id and a user id".into(),
            ));
        }
        let body = LinkRequest {
            auth_config_id,
            user_id,
            callback_url,
        };
        let response: LinkResponse = self
            .post_json("/connected_accounts/link", &body, "create connect link")
            .await?;
        Ok(CreatedLink {
            redirect_url: response.redirect_url,
            link_token: response.link_token,
            connected_account_id: response.connected_account_id,
            expires_at: response.expires_at,
        })
    }

    /// 按 `connected_account_id` 查连接（上游 `ListConnectedAccounts` 的单元素特例）。
    ///
    /// # Errors
    ///
    /// 同 [`ComposioClient::list_toolkit_entries`]。
    pub async fn list_connected_accounts(
        &self,
        connected_account_ids: &[String],
    ) -> Result<Vec<ConnectedAccount>, ComposioError> {
        let mut path = format!("/connected_accounts?limit={LIST_PAGE_LIMIT}");
        for id in connected_account_ids {
            if !id.is_empty() {
                path.push_str("&connected_account_ids=");
                path.push_str(&percent_encode(id));
            }
        }
        let page: ConnectedAccountPage = self.get_json(&path, "list connected accounts").await?;
        Ok(page
            .items
            .into_iter()
            .map(|item| ConnectedAccount {
                id: item.id,
                user_id: item.user_id,
                auth_config_id: item.auth_config_id,
                auth_config_ref_id: item.auth_config.id,
                toolkit_slug: item.toolkit.slug,
                status: item.status,
            })
            .collect())
    }

    /// 撤销上游 grant（上游 `RevokeConnection`，`POST …/revoke`）。
    ///
    /// # Errors
    ///
    /// 空 id ⇒ [`ComposioError::Malformed`]；其余同
    /// [`ComposioClient::list_toolkit_entries`]（**404 也算失败** —— 幂等由服务侧决定）。
    pub async fn revoke_connection(&self, connected_account_id: &str) -> Result<(), ComposioError> {
        if connected_account_id.is_empty() {
            return Err(ComposioError::Malformed(
                "revoke needs a connected account id".into(),
            ));
        }
        let path = format!(
            "/connected_accounts/{}/revoke",
            percent_encode(connected_account_id)
        );
        self.post_empty(&path, "revoke connected account").await
    }

    /// 删除 Composio 侧的连接记录（上游 `DeleteConnectedAccount`，`DELETE …/{id}`）。
    ///
    /// **404 视为成功**（上游 SDK 的注释逐字：Returns nil for 404 so callers can treat the
    /// operation as idempotent）。
    ///
    /// # Errors
    ///
    /// 空 id ⇒ [`ComposioError::Malformed`]；其余非 404 的失败同
    /// [`ComposioClient::list_toolkit_entries`]。
    pub async fn delete_connected_account(
        &self,
        connected_account_id: &str,
    ) -> Result<(), ComposioError> {
        if connected_account_id.is_empty() {
            return Err(ComposioError::Malformed(
                "delete needs a connected account id".into(),
            ));
        }
        let path = format!(
            "/connected_accounts/{}",
            percent_encode(connected_account_id)
        );
        match self.delete_empty(&path, "delete connected account").await {
            Err(ComposioError::Upstream { status: 404, .. }) => Ok(()),
            other => other,
        }
    }

    /// 开一个 tool-router（MCP）会话（上游 `CreateSession`，`POST /tool_router/session`）。
    ///
    /// `connected_accounts` 是 `(toolkit_slug, connected_account_id)` 对（本产品一个 toolkit
    /// 至多一条活跃连接）⇒ 这里展开成上游的两层 map 形状
    /// `{"<slug>": ["<account id>"]}`，并同时把 slug 列表塞进 `toolkits.enable` 收窄会话。
    ///
    /// # Errors
    ///
    /// 空 `user_id` ⇒ [`ComposioError::Malformed`]；其余同
    /// [`ComposioClient::list_toolkit_entries`]。
    pub async fn create_session(
        &self,
        user_id: &str,
        connected_accounts: &[(String, String)],
    ) -> Result<ToolRouterSession, ComposioError> {
        if user_id.is_empty() {
            return Err(ComposioError::Malformed(
                "create session needs a user id".into(),
            ));
        }
        let mut pinned: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (slug, account_id) in connected_accounts {
            if slug.is_empty() || account_id.is_empty() {
                continue;
            }
            pinned
                .entry(slug.clone())
                .or_default()
                .push(account_id.clone());
        }
        let slugs: Vec<String> = pinned.keys().cloned().collect();
        let body = SessionRequest {
            user_id,
            toolkits: SessionToolkits { enable: &slugs },
            connected_accounts: pinned,
        };
        let response: SessionResponse = self
            .post_json("/tool_router/session", &body, "create tool router session")
            .await?;
        Ok(ToolRouterSession {
            session_id: response.session_id,
            mcp_url: response.mcp.url,
            mcp_type: response.mcp.kind,
        })
    }

    // -----------------------------------------------------------------------
    // 传输（唯一的出站口）
    // -----------------------------------------------------------------------

    /// `GET` → JSON。
    async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        context: &'static str,
    ) -> Result<T, ComposioError> {
        let request = self
            .http
            .get(self.url(path))
            .header(API_KEY_HEADER, self.api_key()?)
            .header(reqwest::header::USER_AGENT, DEFAULT_USER_AGENT)
            .header(reqwest::header::ACCEPT, "application/json");
        self.send(request, context).await
    }

    /// `POST`（带 JSON body）→ JSON。
    async fn post_json<B: Serialize + ?Sized, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
        context: &'static str,
    ) -> Result<T, ComposioError> {
        let request = self
            .http
            .post(self.url(path))
            .header(API_KEY_HEADER, self.api_key()?)
            .header(reqwest::header::USER_AGENT, DEFAULT_USER_AGENT)
            .header(reqwest::header::ACCEPT, "application/json")
            .json(body);
        self.send(request, context).await
    }

    /// `POST`（无 body）→ 只关心成败。
    async fn post_empty(&self, path: &str, context: &'static str) -> Result<(), ComposioError> {
        let request = self
            .http
            .post(self.url(path))
            .header(API_KEY_HEADER, self.api_key()?)
            .header(reqwest::header::USER_AGENT, DEFAULT_USER_AGENT)
            .header(reqwest::header::ACCEPT, "application/json");
        let response = self.execute(request, context).await?;
        check_status(response.status(), context)
    }

    /// `DELETE` → 只关心成败。
    async fn delete_empty(&self, path: &str, context: &'static str) -> Result<(), ComposioError> {
        let request = self
            .http
            .delete(self.url(path))
            .header(API_KEY_HEADER, self.api_key()?)
            .header(reqwest::header::USER_AGENT, DEFAULT_USER_AGENT)
            .header(reqwest::header::ACCEPT, "application/json");
        let response = self.execute(request, context).await?;
        check_status(response.status(), context)
    }

    /// 发请求，非 2xx 先判错，再解 JSON。
    async fn send<T: DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
        context: &'static str,
    ) -> Result<T, ComposioError> {
        let response = self.execute(request, context).await?;
        check_status(response.status(), context)?;
        let bytes = response
            .bytes()
            .await
            .map_err(|error| ComposioError::Transport(format!("{context}: {error}")))?;
        serde_json::from_slice(&bytes)
            .map_err(|_| ComposioError::Malformed(format!("{context}: response is not JSON")))
    }

    /// 真正出站（传输错误统一映射成 [`ComposioError::Transport`]）。
    async fn execute(
        &self,
        request: reqwest::RequestBuilder,
        context: &'static str,
    ) -> Result<reqwest::Response, ComposioError> {
        request
            .send()
            .await
            .map_err(|error| ComposioError::Transport(format!("{context}: {error}")))
    }

    /// API key（未配置 ⇒ `NotConfigured`，**不**发匿名请求）。
    fn api_key(&self) -> Result<&str, ComposioError> {
        self.api_key.as_deref().ok_or(ComposioError::NotConfigured)
    }

    /// base + path（base 去尾斜杠，与上游 `strings.TrimRight(baseURL, "/")` 同款）。
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.api_base.trim_end_matches('/'))
    }
}

impl std::fmt::Debug for ComposioClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComposioClient")
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("api_base", &self.api_base)
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// 传输 helper（无状态）
// ---------------------------------------------------------------------------

/// 非 2xx ⇒ 错误（**只**看状态码：不回显 body、不回声请求）。
fn check_status(status: reqwest::StatusCode, context: &'static str) -> Result<(), ComposioError> {
    if status.is_success() {
        return Ok(());
    }
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(ComposioError::Unauthorized);
    }
    Err(ComposioError::Upstream {
        status: status.as_u16(),
        context: context.to_string(),
    })
}

/// `url.QueryEscape` 的等价物（path / query 段用：只编码必然要编码的字节）。
fn percent_encode(value: &str) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

fn toolkit_path(cursor: &str) -> String {
    let mut path = format!("/toolkits?limit={LIST_PAGE_LIMIT}&sort_by=usage");
    if !cursor.is_empty() {
        path.push_str("&cursor=");
        path.push_str(&percent_encode(cursor));
    }
    path
}

fn auth_config_path(cursor: &str) -> String {
    let mut path = format!("/auth_configs?limit={LIST_PAGE_LIMIT}");
    if !cursor.is_empty() {
        path.push_str("&cursor=");
        path.push_str(&percent_encode(cursor));
    }
    path
}

// ---------------------------------------------------------------------------
// wire 形状（私有）
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ToolkitPage {
    #[serde(default)]
    items: Vec<ToolkitWire>,
    #[serde(default)]
    next_cursor: String,
}

#[derive(Debug, Default, Deserialize)]
struct ToolkitWire {
    #[serde(default)]
    slug: String,
    #[serde(default)]
    name: String,
    /// 上游 `Toolkit.LogoURL` 的 JSON 键逐字是 `logo`（不是 `logo_url`）。
    #[serde(default)]
    logo: String,
    #[serde(default)]
    categories: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AuthConfigPage {
    #[serde(default)]
    items: Vec<AuthConfigWire>,
    #[serde(default)]
    next_cursor: String,
}

#[derive(Debug, Deserialize)]
struct AuthConfigWire {
    #[serde(default)]
    id: String,
    #[serde(default)]
    toolkit: ToolkitWire,
    #[serde(default)]
    is_composio_managed: bool,
    #[serde(default)]
    status: String,
    #[serde(default)]
    last_updated_at: String,
}

#[derive(Debug, Deserialize)]
struct ConnectedAccountPage {
    #[serde(default)]
    items: Vec<ConnectedAccountWire>,
}

#[derive(Debug, Deserialize)]
struct ConnectedAccountWire {
    #[serde(default)]
    id: String,
    #[serde(default)]
    user_id: String,
    #[serde(default)]
    auth_config_id: String,
    #[serde(default)]
    auth_config: AuthConfigRefWire,
    #[serde(default)]
    toolkit: ToolkitWire,
    #[serde(default)]
    status: String,
}

#[derive(Debug, Default, Deserialize)]
struct AuthConfigRefWire {
    #[serde(default)]
    id: String,
}

#[derive(Debug, Serialize)]
struct LinkRequest<'a> {
    auth_config_id: &'a str,
    user_id: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    callback_url: &'a str,
}

#[derive(Debug, Deserialize)]
struct LinkResponse {
    #[serde(default)]
    redirect_url: String,
    #[serde(default)]
    link_token: String,
    #[serde(default)]
    connected_account_id: String,
    #[serde(default)]
    expires_at: String,
}

#[derive(Debug, Serialize)]
struct SessionRequest<'a> {
    user_id: &'a str,
    toolkits: SessionToolkits<'a>,
    connected_accounts: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Serialize)]
struct SessionToolkits<'a> {
    enable: &'a [String],
}

#[derive(Debug, Deserialize)]
struct SessionResponse {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    mcp: SessionMcp,
}

#[derive(Debug, Default, Deserialize)]
struct SessionMcp {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    url: String,
}

#[cfg(test)]
mod tests;
