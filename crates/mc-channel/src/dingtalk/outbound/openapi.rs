//! `DingTalk` `OpenAPI` 的**传输面**：访问令牌缓存、`postJSON`、基址接缝与错误分类
//! （上游 `client.go` 168 行 + `token.go` 201 行里**出站真正用到**的那两条调用）。
//!
//! - **写者**：M7-8（`docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §22 的 D1 —— 本文件是
//!   门 ⑩ 的 800 行硬限逼出来的切分，边界取上游两个文件的边界）。
//! - **为什么不在 `client.rs`**：那个文件名归 **M7-9**（它要落完整的 `Client`：安装 / 吊销面
//!   也要它）。本文件是**端口 + 生产实现**，M7-9 收敛时换实现即可。
//!
//! # 凭据纪律（`docs/60` §2.3）
//!
//! - 明文 `AppSecret` 只在铸令牌那一步经 [`AppSecret::expose`] 取出，**不进**任何结构体字段
//!   之外的出口；
//! - [`DingTalkApiError`] 的变体只带**路径**与平台自己的 `code`：平台的 `message` 会**回声
//!   请求体**（铸令牌的请求体里就是 `appSecret`）⇒ 一律丢掉；
//! - 本文件没有任何 `tracing::*`。

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::Value;

use crate::channel::ChannelError;
use crate::dingtalk::stream::AppSecret;

// =====================================================================
// wire 常量（上游 `outbound_send.go` / `client.go`）
// =====================================================================

/// `DingTalk` `OpenAPI` 的基址（上游 `defaultAPIBase`）。测试把它指向本地替身。
pub const DEFAULT_API_BASE: &str = "https://api.dingtalk.com";

/// 铸企业内应用访问令牌的路径（上游 `accessTokenPath`）。
pub const ACCESS_TOKEN_PATH: &str = "/v1.0/oauth2/accessToken";

/// 把机器人收到的消息的 `downloadCode` 换成短期下载 URL 的路径（上游 `messageFilesDownloadPath`）。
pub const MESSAGE_FILES_DOWNLOAD_PATH: &str = "/v1.0/robot/messageFiles/download";

/// 两个发送端点共用的 `msgKey`（上游 `msgKeyMarkdown`）——**逐字**保持文档模板，
/// 别去猜"从收到的互动卡片载荷反推出来的另一种模板"。
pub const MSG_KEY_MARKDOWN: &str = "sampleMarkdown";

/// 直聊（1:1）的主动发送路径（上游 `pathSendP2P`）。
pub const PATH_SEND_P2P: &str = "/v1.0/robot/oToMessages/batchSend";

/// 群发送路径（上游 `pathSendGroup`）。
pub const PATH_SEND_GROUP: &str = "/v1.0/robot/groupMessages/send";

/// 令牌提前失效的余量（上游 `tokenSafetyMargin = 5m`）：吸收时钟偏移与在飞使用。
pub const TOKEN_SAFETY_MARGIN: Duration = Duration::from_secs(300);

/// 一次铸造自身的上限（上游 `tokenMintTimeout = 10s`）：调用方可以放弃等待，
/// 但**不能**取消其他调用方正在共享的那次铸造。
pub const TOKEN_MINT_TIMEOUT: Duration = Duration::from_secs(10);

/// 访问令牌的进程内缓存上限（本仓补齐：上游靠进程生命周期 + 安装数收敛）。
pub const MAX_CACHED_TOKENS: usize = 256;

/// 平台把令牌失效表达成 HTTP 401（上游 `errUnauthorized`）。
pub const HTTP_UNAUTHORIZED: u16 = 401;

// =====================================================================
// 错误
// =====================================================================

/// `DingTalk` `OpenAPI` 失败（上游 `*apiRequestError` / `errUnauthorized` / 传输错误的合并投影）。
///
/// 每个变体只带**路径**（我们自己给的常量）与平台自己的 `code`；**不带** `message`、不带 URL、
/// 不带请求体（见模块文档差异 3）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DingTalkApiError {
    /// 链路层失败（DNS / 连接 / 超时 / 响应体读不出来）。
    #[error("dingtalk: {path} request failed")]
    Transport { path: &'static str },
    /// 令牌失效（HTTP 401）⇒ 调用方作废缓存并重试**一次**。
    #[error("dingtalk: unauthorized (access token expired or invalid)")]
    Unauthorized,
    /// 2xx 但不是我们认得的 JSON 形状。
    #[error("dingtalk: {path} returned a malformed body")]
    Malformed { path: &'static str },
    /// 平台给了错误信封（只带机器可读的 `code`）。
    #[error("dingtalk: {path} refused: code={code}")]
    Refused { path: &'static str, code: String },
    /// 非 2xx 且没有可解析的信封。
    #[error("dingtalk: {path} returned http {status}")]
    Http { path: &'static str, status: u16 },
    /// 目标不完整（直聊缺收件人 / 群发缺会话 id）——**上游在发之前就拒**。
    #[error("dingtalk: {reason}")]
    InvalidTarget { reason: &'static str },
    /// 序列化后的 `msgParam` 连最小的分片预算都装不下（上游 `Markdown metadata exceeds
    /// payload byte budget`）。
    #[error("dingtalk: Markdown metadata exceeds payload byte budget")]
    PayloadBudget,
}

impl DingTalkApiError {
    /// 稳定错误码（路由 / 看板聚合用的**类别**；不含凭据）。
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Transport { .. } => "transport",
            Self::Unauthorized => "unauthorized",
            Self::Malformed { .. } => "malformed",
            Self::Refused { .. } => "refused",
            Self::Http { .. } => "http_status",
            Self::InvalidTarget { .. } => "invalid_target",
            Self::PayloadBudget => "payload_budget",
        }
    }

    /// 映射成本 crate 的渠道错误（`send` 的返回面）。
    ///
    /// 401 落 [`ChannelError::Auth`]（凭据面）——不许被 supervisor 当成可重试的传输失败。
    #[must_use]
    pub fn into_channel_error(self) -> ChannelError {
        match self {
            Self::Unauthorized => ChannelError::Auth {
                message: "dingtalk: access token is invalid".to_string(),
            },
            other => ChannelError::Transport {
                message: other.to_string(),
            },
        }
    }
}

// =====================================================================
// 基址接缝（测试可注入；生产**不得**调用 setter）
// =====================================================================

/// 进程内可注入的 `OpenAPI` 基址（`None` = [`DEFAULT_API_BASE`]）。
///
/// 与 `crate::slack::outbound` / `crate::telegram::api` 同款：本文件与 `outbound/tests.rs`
/// 的用例跑**真** `reqwest` 对本地替身，而不是把端口换成一个返回常量的假实现。
/// 进程全局 ⇒ 依赖它的用例必须串行。
static API_BASE: std::sync::OnceLock<Mutex<Option<String>>> = std::sync::OnceLock::new();

fn api_base_slot() -> &'static Mutex<Option<String>> {
    API_BASE.get_or_init(|| Mutex::new(None))
}

/// 当前生效的基址（末尾无 `/`）。
#[must_use]
pub fn api_base() -> String {
    api_base_slot()
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
        .unwrap_or_else(|| DEFAULT_API_BASE.to_string())
        .trim_end_matches('/')
        .to_string()
}

/// 注入基址（**只给测试用**；生产代码不得调用）。
pub fn set_api_base(base: impl Into<String>) {
    if let Ok(mut guard) = api_base_slot().lock() {
        *guard = Some(base.into());
    }
}

/// 清掉注入的基址。
pub fn reset_api_base() {
    if let Ok(mut guard) = api_base_slot().lock() {
        *guard = None;
    }
}

// =====================================================================
// 端口
// =====================================================================

/// `OpenAPI` 的两条调用（上游 `Client.accessToken` + `Client.postJSON`）。
///
/// 抽成端口有两个理由（都不是为了好看）：
/// 1. 用例要能**只**钉住"401 ⇒ 作废 + 重试一次"这条规则，而不必起 HTTP 服务端；
/// 2. M7-9 落的完整 `Client`（安装 / 吊销面也要它）可以换掉 [`HttpOpenApi`]，而发送端不动。
#[async_trait]
pub trait OpenApiTransport: Send + Sync {
    /// 取（或铸造）本安装在 `appKey` 上的访问令牌。
    ///
    /// # Errors
    ///
    /// 铸造失败 ⇒ 平台错误 / 传输错误。
    async fn access_token(
        &self,
        app_key: &str,
        app_secret: &AppSecret,
    ) -> Result<String, DingTalkApiError>;

    /// 作废 `appKey` 的令牌缓存（上游 `invalidate`；401 之后调用）。
    fn invalidate(&self, app_key: &str);

    /// 带令牌 POST 一个 JSON 体并解回 JSON（平台无响应体时回 `Null`）。
    ///
    /// # Errors
    ///
    /// HTTP 401 ⇒ [`DingTalkApiError::Unauthorized`]（**唯一**会触发重试的分支）。
    async fn post_json(
        &self,
        path: &'static str,
        access_token: &str,
        body: Value,
    ) -> Result<Value, DingTalkApiError>;
}

/// 一条缓存的令牌。
struct CachedToken {
    value: String,
    expires_at: Instant,
}

/// 生产实现：`reqwest` 直连 `OpenAPI` + 进程内令牌缓存（上游 `Client` 的那两条调用）。
pub struct HttpOpenApi {
    http: reqwest::Client,
    tokens: Mutex<HashMap<String, CachedToken>>,
    /// 按 `AppKey` 的铸造互斥（上游 `singleflight` 的等价物）。
    minting: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl fmt::Debug for HttpOpenApi {
    /// 派生本来也安全（没有凭据字段）；手写是为了让"缓存里有多少条"可诊断。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let cached = self.tokens.lock().map_or(0, |map| map.len());
        formatter
            .debug_struct("HttpOpenApi")
            .field("api_base", &api_base())
            .field("cached_tokens", &cached)
            .finish_non_exhaustive()
    }
}

impl Default for HttpOpenApi {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpOpenApi {
    /// 装配（令牌缓存初始为空）。
    #[must_use]
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
            tokens: Mutex::new(HashMap::new()),
            minting: Mutex::new(HashMap::new()),
        }
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

    /// 本 `AppKey` 的铸造闸门（并发未命中折叠成一次铸造）。
    fn mint_gate(&self, app_key: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut gates = self.minting.lock().expect("minting map");
        Arc::clone(
            gates
                .entry(app_key.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
    }

    /// 一次铸造（上游 `fetchAccessToken`）。
    async fn mint(
        &self,
        app_key: &str,
        app_secret: &AppSecret,
    ) -> Result<(String, i64), DingTalkApiError> {
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
        let expire_in = value.get("expireIn").and_then(Value::as_i64).unwrap_or(0);
        Ok((token.to_string(), expire_in))
    }
}

/// 非 2xx 的错误信封（只取 `code`，见模块文档差异 3）。
fn envelope_error(path: &'static str, status: u16, value: &Value) -> DingTalkApiError {
    match value.get("code").and_then(Value::as_str) {
        Some(code) if !code.is_empty() => DingTalkApiError::Refused {
            path,
            code: code.to_string(),
        },
        _ => DingTalkApiError::Http { path, status },
    }
}

#[async_trait]
impl OpenApiTransport for HttpOpenApi {
    async fn access_token(
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
        let mint = self.mint(app_key, app_secret);
        let (token, expire_in) = tokio::time::timeout(TOKEN_MINT_TIMEOUT, mint)
            .await
            .map_err(|_| DingTalkApiError::Transport {
                path: ACCESS_TOKEN_PATH,
            })??;
        let ttl = Duration::from_secs(u64::try_from(expire_in.max(0)).unwrap_or(0));
        // 上游：`ttl < 2*margin ⇒ ttl = 2*margin`，再减一个 margin ⇒ 有效寿命 ≥ 一个 margin。
        let usable = ttl
            .max(TOKEN_SAFETY_MARGIN * 2)
            .saturating_sub(TOKEN_SAFETY_MARGIN);
        if let Ok(mut map) = self.tokens.lock() {
            if map.len() >= MAX_CACHED_TOKENS && !map.contains_key(app_key) {
                // 缓存有界：丢一个任意条目（本仓补齐；上游靠进程生命周期收敛）。
                if let Some(victim) = map.keys().next().cloned() {
                    map.remove(&victim);
                }
            }
            map.insert(
                app_key.to_string(),
                CachedToken {
                    value: token.clone(),
                    expires_at: Instant::now() + usable,
                },
            );
        }
        Ok(token)
    }

    fn invalidate(&self, app_key: &str) {
        if let Ok(mut map) = self.tokens.lock() {
            map.remove(app_key);
        }
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
