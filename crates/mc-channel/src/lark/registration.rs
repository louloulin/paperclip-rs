//! lark **设备流注册面**（RFC 8628）：`begin` 扫码 → `status` 轮询 → 终态
//! （上游 `internal/integrations/lark/{registration.go,registration_service.go,
//! install_session_store.go,install_session_redis_store.go}`，**1,716 行**）。
//!
//! - **写者**：M7-14（`docs/60-M7-PLAN.md` §3.3）。
//! - **协议**（上游注释逐字，照抄 RFC 8628 对 `accounts.feishu.cn` /
//!   `accounts.larksuite.com` 的用法）：只有两个阶段 ——
//!   1. `action=begin`：Lark 回 `device_code` + `verification_uri_complete`（QR 目标）+
//!      `interval` + 过期时间。Multica 渲染 QR，用户在 Lark app 里扫码、走完
//!      "为本账号建一个 PersonalAgent"、授权；
//!   2. `action=poll`：回 `authorization_pending` / `slow_down` /
//!      `user_info.tenant_brand`（**换云信号**）/ `client_id+client_secret`（终态成功）/
//!      `expired_token`、`access_denied`（终态失败）。
//! - **本文件的两半**：前半是**协议客户端**（[`RegistrationClient`]，对传输只依赖
//!   [`FormPoster`] ⇒ 可以对一个假服务器确定性测试，不碰数据库）；后半是**会话状态机**
//!   （[`RegistrationService`]，持会话表 + 后台轮询 + 终态落库）。
//!
//! # 会话状态存在**共享**表里，`device_code` **不**进去（上游 MUL-7340 的教训）
//!
//! 浏览器每 ~5s 轮 `GET …/install/{session_id}/status`，**任何副本**都可能收到那一枪；
//! 会话状态若只在一个进程的 map 里，另一副本就会 404、对话框在 QR 出来约 5s 后报
//! "session lost"。所以状态放 [`InstallSessionStore`]（上游是 Redis + 单进程降级
//! [`MemoryInstallSessionStore`]，本仓**没有** Redis ⇒ 单副本假设，登记 `docs/32` §30 的
//! **D2**）。`device_code` **相反**：它是持有者凭证（谁拿到谁能走完授权），只有跑轮询
//! 协程的进程需要它 ⇒ 它**只在内存**里，于是"会话不跨进程存活"这条后果是**故意**的、
//! 上游逐字写明的。
//!
//! # 终态写入是**首次写入胜**，且**重试**
//!
//! 过期截止与一次 poll 结果可能并发落地；用户必须看到他**已经看到过**的那个结局
//! ⇒ [`InstallSessionStore::mark_terminal`] 是首次写入胜，输掉的那次是 **no-op 而不是错误**。
//! 而一次成功的终态落库发生时，安装行与安装者绑定**已经在 Postgres 里提交了** ⇒ 把这次写入
//! 在第一个错误上丢掉，会让"已经绑好了"的用户盯着对话框停在 `pending` 直到 QR 过期 ——
//! 所以它按指数退避重试（上游 `terminalWriteAttempts = 10`）。
//!
//! # 凭据纪律（`docs/60` §2.3）
//!
//! - [`RegistrationCredentials`] 承载明文 `app_secret` + `client_id` ⇒ **不派生 `Debug`**；
//! - 本文件**所有** `tracing::*` 只插值 `session_id` / `workspace_id` / 平台**机器码**与
//!   描述 —— 绝不插值 `client_secret` / `device_code`；
//! - 「错误路径不回显凭据」有专门用例（`registration/tests.rs`）。
//!
//! # 与上游的形态差异（逐条登记 `docs/32` §30）
//!
//! | # | 差异 | 理由 |
//! | --- | --- | --- |
//! | **D2** | `install_session_redis_store.go`（187 行）**不落地**，只留 [`InstallSessionStore`] + 单进程实现 | 本仓无 Redis 依赖（`docs/60` §2.5 的表第 3 行：上游**本身**就有"无 Redis ⇒ 会话按进程"的降级分支与明文 warn）⇒ 换部署形态，不是伪造行为 |
//! | **D3** | `db.Queries` / `TxStarter` / `events.Bus` → 端口 [`RegistrationStore`] | 层次铁律：`mc-channel` 不写 SQL；事务边界（回收死主 + upsert + 绑定安装者）在端口实现里，**判决**留在本文件可测 |
//! | **D5** | `Now func() time.Time`（Go 的函数字段）→ `Arc<dyn Fn() -> DateTime<Utc>>` | 同一个 seam：过期边界要确定性，测试注入假时钟而不是睡觉 |

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use super::types::{OpenId, Region};

// =====================================================================
// 常量（上游 `registration.go` / `registration_service.go`，逐字）
// =====================================================================

/// 飞书（大陆）账户主机。
pub const DEFAULT_FEISHU_ACCOUNTS_DOMAIN: &str = "https://accounts.feishu.cn";
/// Lark（国际）账户主机。
pub const DEFAULT_LARK_ACCOUNTS_DOMAIN: &str = "https://accounts.larksuite.com";
/// 设备流端点（两个云同一条路径）。
pub const REGISTRATION_ENDPOINT: &str = "/oauth/v1/app/registration";
/// Lark 省略 `interval` 时的轮询节奏（5s，与上游/RFC 8628 一致）。
pub const DEFAULT_POLL_SECONDS: u64 = 5;
/// Lark 省略过期时的**兜底**窗口（10 分钟；两个真云都给 3600）。
pub const DEFAULT_EXPIRE_SECONDS: u64 = 600;
/// 终态会话的保活窗口（上游 `SessionTTL`）。
pub const DEFAULT_SESSION_TTL: Duration = Duration::from_mins(30);
/// `tenant_brand` 的"这是国际 Lark 账号"标记。
pub const TENANT_BRAND_LARK: &str = "lark";
/// `tenant_brand` 的"这是大陆飞书账号"标记。
pub const TENANT_BRAND_FEISHU: &str = "feishu";

/// 会话状态的三个取值（上游 `RegistrationSessionStatus`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    /// QR 已铸出、后台协程还在轮询。
    Pending,
    /// 设备流拿到凭据**且**安装行 + 安装者绑定已提交。
    Success,
    /// 终态失败（过期 / 用户拒绝 / 协议错 / 取 Bot 信息失败 / 落库失败）。
    Error,
}

impl SessionStatus {
    /// 序列化出去的字面量（前端按它分支，不解析散文）。
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Success => "success",
            Self::Error => "error",
        }
    }
}

/// 失败会话上的**稳定**原因码（上游 `RegistrationReason*`）。
pub mod reason {
    /// 设备码窗口在授权前过期。
    pub const EXPIRED: &str = "expired";
    /// 用户在 Lark UI 里明确拒绝。
    pub const ACCESS_DENIED: &str = "access_denied";
    /// Lark 回的某个我们没预料到的协议错。
    pub const PROTOCOL: &str = "lark_protocol_error";
    /// 成功轮询之后的 `GetBotInfo` 失败（或响应缺 `open_id`）。
    pub const BOT_INFO_FAILED: &str = "bot_info_failed";
    /// `app_id` 路由槽被活的持有者占着。
    pub const INSTALLATION_CONFLICT: &str = "installation_conflict";
    /// 安装者绑定失败。
    pub const INSTALLER_BIND_FAILED: &str = "installer_bind_failed";
    /// 封 / 开事务 / 提交一类不透明内部错误。
    pub const INTERNAL_ERROR: &str = "internal_error";
}

// =====================================================================
// 协议错误与结果
// =====================================================================

/// 带类型的 Lark 协议错误（上游 `RegistrationError`）。
///
/// [`Self::description`] 只承载**平台自己的**文案或本文件的静态句子 —— 上游会把响应体的
/// 尾巴（截断到 256）塞进来，本仓**照搬**：那是**平台的**回执正文，不是我们的凭据。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrationError {
    /// 平台机器码（`authorization_pending` / `expired_token` / `http_500` …）。
    pub code: String,
    /// 描述（可为空）。
    pub description: String,
}

impl RegistrationError {
    /// 装配。
    #[must_use]
    pub fn new(code: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            description: description.into(),
        }
    }
}

impl fmt::Display for RegistrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.description.is_empty() {
            write!(formatter, "registration: {}", self.code)
        } else {
            write!(
                formatter,
                "registration: {}: {}",
                self.code, self.description
            )
        }
    }
}

impl std::error::Error for RegistrationError {}

/// `begin` 的产物（上游 `BeginResult`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeginResult {
    /// 设备码（**持有者凭证**：只留在内存里，不落库、不进响应）。
    pub device_code: String,
    /// QR 目标（已带上 `from=sdk&tp=sdk&source=…` 遥测参数）。
    pub qr_code_url: String,
    /// 本次会话开在哪个账户主机上（换云判定要用它做基）。
    pub domain: String,
    /// Lark 建议的轮询节奏。
    pub interval: Duration,
    /// `device_code` 的绝对寿命。
    pub expires_in: Duration,
}

/// 一次 `poll` 的判决（上游 `PollResult` 的判别联合，逐字段保留）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PollResult {
    /// 终态成功：`client_id`。
    pub client_id: String,
    /// 终态成功：`client_secret`（**明文**；由调用方立刻封好）。
    pub client_secret: String,
    /// 终态成功：安装者的 `open_id`。
    pub open_id: Option<OpenId>,
    /// 换云信号：新的账户主机（非空 = 立刻改道重投，**不**等 interval）。
    pub switched_domain: String,
    /// 与 [`Self::switched_domain`] 同步给出的新 region。
    pub switched_region: Option<Region>,
    /// 非终态协议信号（`authorization_pending` / `slow_down`）。
    pub status: String,
    /// 终态错误码。
    pub error: Option<RegistrationError>,
}

impl PollResult {
    /// 是不是"拿到凭据"的终态成功。
    #[must_use]
    pub fn is_success(&self) -> bool {
        !self.client_id.is_empty() && !self.client_secret.is_empty()
    }

    /// 是不是"换云"那一支。
    #[must_use]
    pub fn is_switch(&self) -> bool {
        !self.switched_domain.is_empty()
    }
}

// =====================================================================
// 传输
// =====================================================================

/// 设备流用的最小传输面：POST 一个 `application/x-www-form-urlencoded` 体，拿回响应字节。
///
/// 抽出来是为了让协议客户端**确定性可测**（一个假服务器 / 一个假传输），且不把 `reqwest`
/// 类型递进公开面（`docs/32` §25 已立的同款纪律）。
#[async_trait]
pub trait FormPoster: Send + Sync {
    /// 把 `body` POST 到 `endpoint`，返回响应体字节。
    ///
    /// # Errors
    ///
    /// 链路失败（DNS / 连接 / 读体）⇒ 一句**不含请求体**的描述。
    async fn post_form(&self, endpoint: &str, body: String) -> Result<Vec<u8>, String>;
}

/// 生产传输（`reqwest`）。
///
/// **不派生 `Debug`**：它持有连接池，且"能打印"没有诊断价值。
pub struct HttpFormPoster {
    client: reqwest::Client,
}

impl HttpFormPoster {
    /// 用一个 30s 超时的客户端装配（上游 `&http.Client{Timeout: 30 * time.Second}`）。
    ///
    /// # Errors
    ///
    /// 传输构建失败（TLS 后端不可用一类）。
    pub fn new() -> Result<Self, String> {
        Self::with_timeout(Duration::from_secs(30))
    }

    /// 指定超时。
    ///
    /// # Errors
    ///
    /// 同上。
    pub fn with_timeout(timeout: Duration) -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|error| format!("build lark registration http client: {error}"))?;
        Ok(Self { client })
    }
}

#[async_trait]
impl FormPoster for HttpFormPoster {
    async fn post_form(&self, endpoint: &str, body: String) -> Result<Vec<u8>, String> {
        let response = self
            .client
            .post(endpoint)
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .body(body)
            .send()
            .await
            .map_err(|error| format!("lark registration http do: {error}"))?;
        let bytes = response
            .bytes()
            .await
            .map_err(|error| format!("lark registration read body: {error}"))?;
        Ok(bytes.to_vec())
    }
}

// =====================================================================
// 协议客户端
// =====================================================================

/// 设备流客户端的配置（上游 `RegistrationConfig`）。
#[derive(Clone)]
pub struct RegistrationConfig {
    /// 初始轮询主机（默认飞书）。
    pub domain: String,
    /// 国际租户的主机（`tenant_brand=lark` 时切过去）。
    pub lark_domain: String,
    /// QR URL 的 `source` 参数（默认 `multica`）。
    pub source: String,
}

impl Default for RegistrationConfig {
    fn default() -> Self {
        Self {
            domain: DEFAULT_FEISHU_ACCOUNTS_DOMAIN.to_string(),
            lark_domain: DEFAULT_LARK_ACCOUNTS_DOMAIN.to_string(),
            source: "multica".to_string(),
        }
    }
}

impl RegistrationConfig {
    /// 补齐默认值（上游 `withDefaults`）。
    #[must_use]
    pub fn with_defaults(mut self) -> Self {
        if self.domain.is_empty() {
            self.domain = DEFAULT_FEISHU_ACCOUNTS_DOMAIN.to_string();
        }
        if self.lark_domain.is_empty() {
            self.lark_domain = DEFAULT_LARK_ACCOUNTS_DOMAIN.to_string();
        }
        if self.source.is_empty() {
            self.source = "multica".to_string();
        }
        self
    }
}

/// 设备流协议客户端（**不**持会话状态、**不**碰数据库）。
pub struct RegistrationClient {
    config: RegistrationConfig,
    poster: Arc<dyn FormPoster>,
}

impl RegistrationClient {
    /// 装配。
    #[must_use]
    pub fn new(config: RegistrationConfig, poster: Arc<dyn FormPoster>) -> Self {
        Self {
            config: config.with_defaults(),
            poster,
        }
    }

    /// 本次会话开在哪台主机上（`region` 决定；空/未知回落飞书 —— 与
    /// [`Region::or_default`] 同一条历史不变量）。
    #[must_use]
    pub fn domain_for(&self, region: Region) -> String {
        match region {
            Region::Lark => self.config.lark_domain.clone(),
            Region::Feishu => self.config.domain.clone(),
        }
    }

    /// 开一个设备流会话。`name_preset` 预填 Lark 建 `PersonalAgent` 表单上的名字。
    ///
    /// # Errors
    ///
    /// 链路失败 / 平台回错。
    pub async fn begin(
        &self,
        name_preset: &str,
        region: Region,
    ) -> Result<BeginResult, RegistrationError> {
        let domain = self.domain_for(region);
        let body = form_body(&[
            ("action", "begin"),
            ("archetype", "PersonalAgent"),
            ("auth_method", "client_secret"),
            ("request_user_info", "open_id"),
        ]);
        let raw = self.post(&domain, body).await?;
        let resp: BeginEnvelope = serde_json::from_slice(&raw).unwrap_or_default();
        if !resp.error.is_empty() {
            return Err(RegistrationError::new(resp.error, resp.error_description));
        }
        if resp.device_code.is_empty() {
            return Err(RegistrationError::new(
                "invalid_response",
                "device_code is empty",
            ));
        }
        if resp.verification_uri_complete.is_empty() {
            return Err(RegistrationError::new(
                "invalid_response",
                "verification_uri_complete is empty",
            ));
        }
        let qr_code_url = decorate_qr_code_url(
            &resp.verification_uri_complete,
            &self.config.source,
            name_preset,
        )?;
        let interval = if resp.interval > 0 {
            resp.interval
        } else {
            DEFAULT_POLL_SECONDS
        };
        // `expires_in` 是 RFC 8628 §3.2 的字段名、也是两个真云实际发的那个；
        // `expire_in` 是上游 SDK 的类型拼法 ⇒ 两个都收，任一方向的 schema 漂移都保住真实窗口。
        let expire = if resp.expires_in > 0 {
            resp.expires_in
        } else if resp.expire_in > 0 {
            resp.expire_in
        } else {
            DEFAULT_EXPIRE_SECONDS
        };
        Ok(BeginResult {
            device_code: resp.device_code,
            qr_code_url,
            domain,
            interval: Duration::from_secs(interval),
            expires_in: Duration::from_secs(expire),
        })
    }

    /// 跑**一次**轮询。主机由调用方给（会话状态机是"下一次打哪台"的唯一出处）。
    ///
    /// # Errors
    ///
    /// 链路失败（**非终态**：会话状态机据此重试下一次 tick）。
    pub async fn poll(
        &self,
        domain: &str,
        device_code: &str,
    ) -> Result<PollResult, RegistrationError> {
        if device_code.is_empty() {
            return Err(RegistrationError::new(
                "invalid_argument",
                "device_code is required",
            ));
        }
        let domain = if domain.is_empty() {
            self.config.domain.clone()
        } else {
            domain.to_string()
        };
        let body = form_body(&[("action", "poll"), ("device_code", device_code)]);
        let raw = self.post(&domain, body).await?;
        let resp: PollEnvelope = serde_json::from_slice(&raw).unwrap_or_default();

        // 换云：Lark 在"授权账号与 begin 主机不是一个云"的**过渡那一枪**上只发一次
        // `tenant_brand`，下一个 poll 必须落到匹配的主机才拿得到凭据。两个方向都认
        // （feishu→lark 与 lark→feishu），因为分 CTA 的 UI 也会直接在 larksuite 上 begin。
        // 判据挂在**当前**主机上，避免在同一档上打转。
        if let Some(user_info) = resp.user_info.as_ref() {
            match user_info.tenant_brand.as_str() {
                TENANT_BRAND_LARK if !domain.starts_with(&self.config.lark_domain) => {
                    return Ok(PollResult {
                        switched_domain: self.config.lark_domain.clone(),
                        switched_region: Some(Region::Lark),
                        ..PollResult::default()
                    });
                }
                TENANT_BRAND_FEISHU if !domain.starts_with(&self.config.domain) => {
                    return Ok(PollResult {
                        switched_domain: self.config.domain.clone(),
                        switched_region: Some(Region::Feishu),
                        ..PollResult::default()
                    });
                }
                _ => {}
            }
        }

        // 成功：`client_id` 与 `client_secret` 与安装者 `open_id` **三个都**要有 ——
        // 半个响应当协议错处理，于是状态机**永远**不会写出一行半填的安装。
        if !resp.client_id.is_empty() && !resp.client_secret.is_empty() {
            let open_id = resp.user_info.as_ref().map(|info| info.open_id.clone());
            let has_open_id = open_id.as_ref().is_some_and(|id| !id.is_empty());
            if !has_open_id {
                return Err(RegistrationError::new(
                    "invalid_response",
                    "success response missing installer open_id",
                ));
            }
            return Ok(PollResult {
                client_id: resp.client_id,
                client_secret: resp.client_secret,
                open_id,
                ..PollResult::default()
            });
        }

        match resp.error.as_str() {
            "authorization_pending" | "slow_down" => Ok(PollResult {
                status: resp.error,
                ..PollResult::default()
            }),
            "access_denied" | "expired_token" => Ok(PollResult {
                error: Some(RegistrationError::new(resp.error, resp.error_description)),
                ..PollResult::default()
            }),
            // 空 error 且空凭据 = 继续轮询（上游 SDK 在 authorize 重定向窗口里的宽容处理）。
            "" => Ok(PollResult {
                status: "authorization_pending".to_string(),
                ..PollResult::default()
            }),
            other => Ok(PollResult {
                error: Some(RegistrationError::new(other, resp.error_description)),
                ..PollResult::default()
            }),
        }
    }

    /// 一条 POST + body 解码（**唯一的**网络出口）。
    ///
    /// RFC 8628 的服务器会用**非 2xx** 回 `authorization_pending` / `slow_down`（HTTP 400，
    /// 不是 2xx）⇒ 先解 body、再让调用方按 `error` 字段分流 —— 上游把"任何非 2xx 都是硬协议错"
    /// 当成 bug 修过一次（那会让每个会话在第一次 poll 就死，因为那时用户还没扫）。
    async fn post(&self, domain: &str, body: String) -> Result<Vec<u8>, RegistrationError> {
        let endpoint = format!("{}{REGISTRATION_ENDPOINT}", domain.trim_end_matches('/'));
        let raw = self
            .poster
            .post_form(&endpoint, body)
            .await
            .map_err(|error| RegistrationError::new("transport", error))?;
        if raw.is_empty() {
            return Err(RegistrationError::new("http_0", "empty body"));
        }
        Ok(raw)
    }
}

/// 把表单键值对编码成 `application/x-www-form-urlencoded`（`url.Values.Encode` 的等价物）。
#[must_use]
pub fn form_body(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| format!("{}={}", percent_encode(key), percent_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

/// 表单值的百分号编码（`url.QueryEscape`：空格 → `+`，`A-Za-z0-9-_.~` 之外一律编码）。
#[must_use]
pub fn percent_encode(raw: &str) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            b' ' => out.push('+'),
            other => {
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}

/// 给 QR URL 补上 SDK 风格的遥测参数（上游 `decorateQRCodeURL` 逐字）。
///
/// # Errors
///
/// `raw` 不是一个 URL。
pub fn decorate_qr_code_url(
    raw: &str,
    source: &str,
    name_preset: &str,
) -> Result<String, RegistrationError> {
    let mut url = reqwest::Url::parse(raw)
        .map_err(|error| RegistrationError::new("invalid_response", error.to_string()))?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("from", "sdk");
        query.append_pair("tp", "sdk");
        query.append_pair("source", &format!("go-sdk/{source}"));
        if !name_preset.is_empty() {
            query.append_pair("name", name_preset);
        }
    }
    Ok(url.to_string())
}

/// Lark 建 `PersonalAgent` 表单上预填的名字（上游 `botNamePreset`）。
///
/// 空 agent 名（防御性）降级成光秃秃的 `Multica`，而不是一个悬空的 `" - Multica"`。
#[must_use]
pub fn bot_name_preset(agent_name: &str) -> String {
    let name = agent_name.trim();
    if name.is_empty() {
        return "Multica".to_string();
    }
    format!("{name} - Multica")
}

/// `begin` 的响应信封（本文件只关心它用到的键；缺失一律当空 —— 与 Go 的零值同义）。
#[derive(Debug, Default, serde::Deserialize)]
struct BeginEnvelope {
    #[serde(default)]
    device_code: String,
    #[serde(default)]
    verification_uri_complete: String,
    #[serde(default)]
    interval: u64,
    #[serde(default)]
    expires_in: u64,
    #[serde(default)]
    expire_in: u64,
    #[serde(default)]
    error: String,
    #[serde(default)]
    error_description: String,
}

/// `poll` 的响应信封。
#[derive(Debug, Default, serde::Deserialize)]
struct PollEnvelope {
    #[serde(default)]
    client_id: String,
    #[serde(default)]
    client_secret: String,
    #[serde(default)]
    user_info: Option<PollUserInfo>,
    #[serde(default)]
    error: String,
    #[serde(default)]
    error_description: String,
}

/// `poll` 响应里的 `user_info`（两个键：安装者 `open_id` 与租户品牌）。
#[derive(Debug, Default, serde::Deserialize)]
struct PollUserInfo {
    #[serde(default)]
    open_id: OpenId,
    #[serde(default)]
    tenant_brand: String,
}

// =====================================================================
// 子模块（本片自己的切分：门 ⑩ 的单文件 800 行硬限）
// =====================================================================

/// 会话状态机与服务（上游 `registration_service.go` 的那一半）。
pub mod service;
/// 共享会话表（上游 `install_session_store.go` 的那一半）。
pub mod session;

pub use service::{
    random_session_id, BeginInstallResult, Clock, CommitInstall, RegistrationService,
    RegistrationServiceConfig, RegistrationStore,
};
pub use session::{
    InstallSessionOutcome, InstallSessionState, InstallSessionStore, MemoryInstallSessionStore,
    SessionNotFound,
};

#[cfg(test)]
pub(crate) mod tests;
