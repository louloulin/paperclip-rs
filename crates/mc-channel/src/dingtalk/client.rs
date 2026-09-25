//! `DingTalk` `OpenAPI` 客户端：令牌铸造 / 缓存、`postJSON`、机器人消息文件与 **bot 可读身份**
//! （上游 `internal/integrations/dingtalk/{client.go,token.go,bot_identity.go}` 的传输那一半）。
//!
//! - **写者**：M7-9（`docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §23 的 D1）。
//! - **与 M7-8 的 `outbound/openapi.rs` 的关系**（`docs/32` §22 的 **D3** 交接项逐字）：
//!   M7-8 把出站真正用到的那两条调用抽成端口 [`OpenApiTransport`]（端口 + 生产实现
//!   `HttpOpenApi`），并写明「M7-9 换实现即可，语义一行不动」。本文件的 [`Client`] 就是那个
//!   **完整实现**：它同样 `impl OpenApiTransport`（令牌缓存、401 ⇒ 作废 + 重试一次的语义与
//!   `HttpOpenApi` 逐条一致），另外多出**安装校验**与 **bot 名查询**这两条本片才需要的调用。
//!   `HttpOpenApi` **保留**（M7-8 已合、冻结）：宿主在 [`crate::dingtalk::DingTalkDeps::with_outbound`]
//!   处换成 `Arc::new(client::Client::new())` 即可，**语义面不动**。
//!
//! # 三条平台事实（照抄上游，别"顺手统一"）
//!
//! 1. **`AppKey`/`AppSecret` 是两件事**：`AppKey` 明文进 `config->>'app_id'`（它不是秘密），
//!    `AppSecret` **只**出现在铸令牌的请求体里，且只经 [`AppSecret::expose`] 取出一次；
//! 2. **令牌有寿命**（上游 `expireIn`，约 2 小时）⇒ 进程内按 `AppKey` 缓存，
//!    过期前 `TOKEN_SAFETY_MARGIN` 就刷新；并发未命中折叠成**一次**铸造（上游 `singleflight`）；
//! 3. **平台错误体可能回声请求体**（铸令牌的请求体里就是 `appSecret`）⇒ 本文件**只用
//!    `code`**，`message` 一律丢掉（与 M7-8 的 `DingTalkApiError` 同款，登记 `docs/32` §22 差异 3）。
//!
//! # 凭据纪律（`docs/60` §2.3）
//!
//! - [`AccessToken`] **手写 `Debug`**（它**是**一个凭据）：只打印长度类别；
//! - 本文件**没有任何** `tracing::*` 插值令牌 / `AppSecret` / 请求体；
//! - 「错误路径不回显凭据」有专门用例（`client/tests.rs`）：HTTP 401 / 非 2xx / 坏 JSON
//!   三条错误路径的 `Display` 都不含 `AppSecret`、令牌与请求体。

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::Value;

use super::outbound::openapi::{
    api_base, DingTalkApiError, OpenApiTransport, ACCESS_TOKEN_PATH, HTTP_UNAUTHORIZED,
    MAX_CACHED_TOKENS, TOKEN_MINT_TIMEOUT, TOKEN_SAFETY_MARGIN,
};
use super::stream::AppSecret;

/// 机器人消息文件换下载 URL 的路径（上游 `messageFilesDownloadPath`，**逐字**）。
pub const MESSAGE_FILES_DOWNLOAD_PATH: &str = "/v1.0/robot/messageFiles/download";

/// 群机器人清单查询的路径（上游 `bot_identity.go` 的 `groupBotsPath`，**逐字**）。
pub const GROUP_BOTS_PATH: &str = "/v1.0/robot/groups/robots/query";

// =====================================================================
// 值对象
// =====================================================================

/// 一枚刚铸出来的访问令牌（上游 `accessTokenResponse` 的两个字段**合并**成一个值对象：
/// `ExpireIn <= 0` 时本文件按 [`TOKEN_SAFETY_MARGIN`] 兜底，与上游 `token.go` 的判据一致）。
#[derive(Clone, PartialEq, Eq)]
pub struct AccessToken {
    /// 令牌本体（**凭据**）。
    pub value: String,
    /// 平台给的寿命（秒）。
    pub expire_in: i64,
}

impl fmt::Debug for AccessToken {
    /// 手写脱敏：只报告"有没有 / 多长"，**绝不**打印令牌。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AccessToken")
            .field("value", &redaction_of(&self.value))
            .field("expire_in", &self.expire_in)
            .finish()
    }
}

impl AccessToken {
    /// 可用的缓存寿命（上游 `token.go`：`ttl < 2*margin ⇒ ttl = 2*margin`，再减一个 margin）。
    #[must_use]
    pub fn usable_lifetime(&self) -> Duration {
        let ttl = Duration::from_secs(u64::try_from(self.expire_in.max(0)).unwrap_or(0));
        ttl.max(TOKEN_SAFETY_MARGIN * 2)
            .saturating_sub(TOKEN_SAFETY_MARGIN)
    }
}

/// 只报告"这个凭据字段**有没有**".
fn redaction_of(value: &str) -> &'static str {
    if value.is_empty() {
        "<empty>"
    } else {
        "<redacted>"
    }
}

// =====================================================================
// 端口（用例注入替身；生产 = [`Client`]）
// =====================================================================

/// "拿这堆凭据去铸一枚令牌" —— 安装校验用的**端口**（上游 `fetchAccessToken` 的调用面）。
///
/// 抽成端口只有一个理由：`POST …/dingtalk/install/byo` 的"凭据活的校验"必须能在**没有网络**
/// 的用例里被逐条钉住（凭证形状 → 拒 → 400；够不着 → 500），而那条判据在
/// [`super::install::InstallService::register_byo`] 里。
#[async_trait]
pub trait CredentialProbe: Send + Sync {
    /// **不查缓存**地铸一次（上游 `fetchAccessToken`；安装校验要的就是"平台现在认不认这对
    /// `AppKey`/`AppSecret`"，缓存会掩盖一次已被撤销的凭据）。
    ///
    /// # Errors
    ///
    /// 传输失败 / 平台拒了 / 响应形状不对。
    async fn fetch_access_token(
        &self,
        app_key: &str,
        app_secret: &AppSecret,
    ) -> Result<AccessToken, DingTalkApiError>;
}

/// 群机器人的**可读名字**查询（上游 `bot_identity.go` 的 `botNameInGroup`）。
///
/// 名字的唯一用途是"把群里的 `@bot` 提及剥掉"（`jobs::BotNameSource` 的文档逐字）⇒ 拿不到
/// 名字的正确行为是**失败关闭**（保留每一个可见提及），不是猜跨度。
#[async_trait]
pub trait BotNameApi: Send + Sync {
    /// 取 `robot_code` 这个机器人在 `conversation_id` 群里的**可读名字**。
    ///
    /// # Errors
    ///
    /// 权限不足（上游 `qyapi_chat_manage`）、机器人不在该群、传输失败 —— 三类都必须让调用方
    /// 退回"失败关闭"，所以这里不把它们压成一个 `Option`。
    async fn bot_name_in_group(
        &self,
        app_key: &str,
        app_secret: &AppSecret,
        robot_code: &str,
        conversation_id: &str,
    ) -> Result<String, DingTalkApiError>;
}

// =====================================================================
// 生产实现
// =====================================================================

/// 一条缓存的令牌。
struct CachedToken {
    value: String,
    expires_at: Instant,
}

/// `DingTalk` `OpenAPI` 的完整客户端（上游 `Client` + `client.go` 的两条调用）。
///
/// 一个实例**跨安装共享**（缓存按 `AppKey` 分格，`docs/60` §2.2 的"一个 crate 一个 Runtime"）。
/// 线程安全。
pub struct Client {
    http: reqwest::Client,
    tokens: Mutex<HashMap<String, CachedToken>>,
    /// 按 `AppKey` 的铸造闸门（上游 `singleflight.Group` 的等价物）。
    minting: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// 一次铸造自身的上限（上游 `tokenMintTimeout`；用例可缩短）。
    mint_timeout: Duration,
}

impl fmt::Debug for Client {
    /// 派生本来也安全（没有凭据字段）；手写是为了让"缓存里有多少条 / 超时多久"可诊断。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cached = self.tokens.lock().map_or(0, |map| map.len());
        formatter
            .debug_struct("Client")
            .field("api_base", &api_base())
            .field("cached_tokens", &cached)
            .field("mint_timeout", &self.mint_timeout)
            .finish_non_exhaustive()
    }
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    /// 装配（令牌缓存初始为空；基址取进程内的 [`api_base`] 接缝）。
    #[must_use]
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
            tokens: Mutex::new(HashMap::new()),
            minting: Mutex::new(HashMap::new()),
            mint_timeout: TOKEN_MINT_TIMEOUT,
        }
    }

    /// 换一次铸造的超时（用例用；上游是常量 10s）。
    #[must_use]
    pub fn with_mint_timeout(mut self, timeout: Duration) -> Self {
        self.mint_timeout = timeout;
        self
    }

    /// 缓存里**未过期**的令牌。
    fn cached(&self, app_key: &str) -> Option<String> {
        let map = self.tokens.lock().ok()?;
        let entry = map.get(app_key)?;
        if entry.expires_at > Instant::now() {
            return Some(entry.value.clone());
        }
        None
    }

    /// 写入缓存（有界：满了先丢一个任意条目；上游靠进程生命周期收敛）。
    fn store(&self, app_key: &str, token: &AccessToken) {
        let Ok(mut map) = self.tokens.lock() else {
            return;
        };
        if map.len() >= MAX_CACHED_TOKENS && !map.contains_key(app_key) {
            if let Some(victim) = map.keys().next().cloned() {
                map.remove(&victim);
            }
        }
        map.insert(
            app_key.to_string(),
            CachedToken {
                value: token.value.clone(),
                expires_at: Instant::now() + token.usable_lifetime(),
            },
        );
    }

    /// 本 `AppKey` 的铸造闸门（并发未命中折叠成一次铸造）。
    fn mint_gate(&self, app_key: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut gates = self.minting.lock().expect("minting map");
        Arc::clone(
            gates
                .entry(app_key.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
    }

    /// 缓存命中的令牌；未命中则铸一次（折叠并发）。上游 `Client.accessToken`。
    ///
    /// # Errors
    ///
    /// 铸造失败（传输 / 平台拒绝 / 形状不对 / 超时）。
    pub async fn access_token(
        &self,
        app_key: &str,
        app_secret: &AppSecret,
    ) -> Result<String, DingTalkApiError> {
        if let Some(token) = self.cached(app_key) {
            return Ok(token);
        }
        let gate = self.mint_gate(app_key);
        let _guard = gate.lock().await;
        // 排队期间别人可能已经铸好了（上游 `singleflight` 里的二次检查）。
        if let Some(token) = self.cached(app_key) {
            return Ok(token);
        }
        let mint = self.fetch_access_token(app_key, app_secret);
        let minted = tokio::time::timeout(self.mint_timeout, mint)
            .await
            .map_err(|_| DingTalkApiError::Transport {
                path: ACCESS_TOKEN_PATH,
            })??;
        self.store(app_key, &minted);
        Ok(minted.value)
    }

    /// 作废缓存里 `AppKey` 的令牌（上游 `invalidate`；401 之后调用）。
    pub fn invalidate(&self, app_key: &str) {
        if let Ok(mut map) = self.tokens.lock() {
            map.remove(app_key);
        }
    }

    /// 带令牌 `POST` 一次，并在 **401** 时作废缓存 + 重铸 + 重试**一次**。
    ///
    /// 这条形状逐字来自上游 `bot_identity.go` / `token.go` 的两处调用（`messageFileDownloadURL`
    /// 与 `botNameInGroup`）：401 只重试一次，第二次还 401 就把错误交给调用方。
    ///
    /// # Errors
    ///
    /// 重铸失败 ⇒ 铸造的错误；两次都失败 ⇒ 第二次的错误。
    pub async fn post_json_authorized(
        &self,
        path: &'static str,
        app_key: &str,
        app_secret: &AppSecret,
        body: Value,
    ) -> Result<Value, DingTalkApiError> {
        let token = self.access_token(app_key, app_secret).await?;
        match self.post_json(path, &token, body.clone()).await {
            Err(DingTalkApiError::Unauthorized) => {
                self.invalidate(app_key);
                let fresh = self.access_token(app_key, app_secret).await?;
                self.post_json(path, &fresh, body).await
            }
            other => other,
        }
    }

    /// 上游 `Client.messageFileDownloadURL`：`downloadCode` → 临时下载 URL。
    ///
    /// 返回的 URL 是**短期签名链**：立刻取、立刻下，**不要**落库、**不要**进日志
    /// （上游注释逐字：`never persist or log either value`）。
    ///
    /// # Errors
    ///
    /// 平台拒绝 / 响应缺 `downloadUrl` / 传输失败。
    pub async fn message_file_download_url(
        &self,
        app_key: &str,
        app_secret: &AppSecret,
        robot_code: &str,
        download_code: &str,
    ) -> Result<String, DingTalkApiError> {
        let body = serde_json::json!({
            "robotCode": robot_code,
            "downloadCode": download_code,
        });
        let value = self
            .post_json_authorized(MESSAGE_FILES_DOWNLOAD_PATH, app_key, app_secret, body)
            .await?;
        match value.get("downloadUrl").and_then(Value::as_str) {
            Some(url) if !url.is_empty() => Ok(url.to_string()),
            _ => Err(DingTalkApiError::Malformed {
                path: MESSAGE_FILES_DOWNLOAD_PATH,
            }),
        }
    }
}

#[async_trait]
impl CredentialProbe for Client {
    async fn fetch_access_token(
        &self,
        app_key: &str,
        app_secret: &AppSecret,
    ) -> Result<AccessToken, DingTalkApiError> {
        let body = serde_json::json!({
            "appKey": app_key,
            "appSecret": app_secret.expose(),
        });
        let url = format!("{}{ACCESS_TOKEN_PATH}", api_base());
        let response = self.http.post(&url).json(&body).send().await.map_err(|_| {
            DingTalkApiError::Transport {
                path: ACCESS_TOKEN_PATH,
            }
        })?;
        let status = response.status().as_u16();
        let parsed: Option<Value> = response.json().await.ok();
        let Some(value) = parsed else {
            return Err(if (200..300).contains(&status) {
                DingTalkApiError::Malformed {
                    path: ACCESS_TOKEN_PATH,
                }
            } else {
                DingTalkApiError::Http {
                    path: ACCESS_TOKEN_PATH,
                    status,
                }
            });
        };
        if !(200..300).contains(&status) {
            return Err(envelope_error(ACCESS_TOKEN_PATH, status, &value));
        }
        let token = value
            .get("accessToken")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if token.is_empty() {
            return Err(DingTalkApiError::Malformed {
                path: ACCESS_TOKEN_PATH,
            });
        }
        Ok(AccessToken {
            value: token.to_string(),
            expire_in: value.get("expireIn").and_then(Value::as_i64).unwrap_or(0),
        })
    }
}

#[async_trait]
impl BotNameApi for Client {
    /// 上游 `Client.botNameInGroup`：查**这个群里所有机器人**，再按 `robotCode` **精确匹配**。
    ///
    /// 上游注释逐字：`callers must never infer identity from list position or persist another
    /// bot's metadata` ⇒ 只认 `robotCode` 完全相等的那一条，找不到就是错误。
    async fn bot_name_in_group(
        &self,
        app_key: &str,
        app_secret: &AppSecret,
        robot_code: &str,
        conversation_id: &str,
    ) -> Result<String, DingTalkApiError> {
        if robot_code.trim().is_empty() || conversation_id.trim().is_empty() {
            return Err(DingTalkApiError::InvalidTarget {
                reason: "bot-name lookup requires robot code and conversation id",
            });
        }
        let body = serde_json::json!({ "openConversationId": conversation_id });
        let value = self
            .post_json_authorized(GROUP_BOTS_PATH, app_key, app_secret, body)
            .await?;
        let bots = value
            .get("chatbotInstanceVOList")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for bot in bots {
            if bot.get("robotCode").and_then(Value::as_str) != Some(robot_code) {
                continue;
            }
            let name = bot
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if name.is_empty() {
                return Err(DingTalkApiError::InvalidTarget {
                    reason: "robot has no readable name in this group",
                });
            }
            return Ok(name);
        }
        Err(DingTalkApiError::InvalidTarget {
            reason: "robot is absent from the group bot list",
        })
    }
}

#[async_trait]
impl OpenApiTransport for Client {
    async fn access_token(
        &self,
        app_key: &str,
        app_secret: &AppSecret,
    ) -> Result<String, DingTalkApiError> {
        Client::access_token(self, app_key, app_secret).await
    }

    fn invalidate(&self, app_key: &str) {
        Client::invalidate(self, app_key);
    }

    async fn post_json(
        &self,
        path: &'static str,
        access_token: &str,
        body: Value,
    ) -> Result<Value, DingTalkApiError> {
        let url = format!("{}{path}", api_base());
        let response = self
            .http
            .post(&url)
            .header("Content-Type", "application/json")
            .header("x-acs-dingtalk-access-token", access_token)
            .json(&body)
            .send()
            .await
            .map_err(|_| DingTalkApiError::Transport { path })?;
        let status = response.status().as_u16();
        if status == HTTP_UNAUTHORIZED {
            return Err(DingTalkApiError::Unauthorized);
        }
        let parsed: Option<Value> = response.json().await.ok();
        match parsed {
            Some(value) if (200..300).contains(&status) => Ok(value),
            Some(value) => Err(envelope_error(path, status, &value)),
            None if (200..300).contains(&status) => Err(DingTalkApiError::Malformed { path }),
            None => Err(DingTalkApiError::Http { path, status }),
        }
    }
}

/// 把 [`Client`] 交给工厂（上游 `DingTalkInstall` / 出站共用**一个**客户端实例）。
///
/// 宿主装配点（`apps/mc-server/src/channels.rs`，anchor 写集）：
/// `DingTalkDeps::default().with_outbound(Arc::new(client::Client::new()))`
/// —— `DingTalkDeps` 的**其余**字段（解密器 / 拨号 / bot 名源）也各有一条 `with_*`。
#[must_use]
pub fn shared_client() -> Arc<Client> {
    Arc::new(Client::new())
}

/// 非 2xx 的错误信封（**只取 `code`**，见模块文档第 3 条）。
///
/// 与 `outbound/openapi.rs` 里的同名私有函数**同形**（各切片各自持有本地副本，本仓既有
/// 约定 `routes/agents.rs` 的注释逐字）：平台错误体可能回声请求体，所以 `message` 一律丢掉。
fn envelope_error(path: &'static str, status: u16, value: &Value) -> DingTalkApiError {
    match value.get("code").and_then(Value::as_str) {
        Some(code) if !code.is_empty() => DingTalkApiError::Refused {
            path,
            code: code.to_string(),
        },
        _ => DingTalkApiError::Http { path, status },
    }
}

#[cfg(test)]
mod tests;
