//! `POST /api/contact-sales` —— **写者 M9-5**（`LUM-1820`，`docs/62` §4.1 第 6 行）。
//!
//! 上游：`internal/handler/contact_sales.go`（323 行）。表 = `contact_sales_inquiry`
//! （仓储 = [`mc_repos::contact_sales::ContactSalesRepo`]）。形态 = **plain 单形态**
//! （补尾斜杠 = `EXTRA_ALIAS` 硬失败）。
//!
//! # 四条 `DoD`（`docs/62` §6.5 的 M9-5 行）
//!
//! 1. 🔴 **「公开面」的一种**：上游用**无会话**请求验它 ⇒ 本路由**不挂** [`AuthUser`]
//!    提取器；`workspace_id` 也不参与（`docs/62` §4.2 的 contact-sales 行）；
//! 2. **企业邮箱域名拒绝**（公网邮箱域名 ⇒ 400，反例必测）；
//! 3. **`company_size` 枚举校验**（闭合 6 值）；
//! 4. **限流 5/h ⇒ 429**（`RATE_LIMIT_CONTACT_SALES`，缺省用默认值）：同样**复用**
//!    [`SlidingWindowLimiter`]（M5-5 的 `webhook/ratelimit.rs`），**禁止**新写。
//!
//! # 「不挂鉴权提取器」在 axum 里的含义
//!
//! axum 的提取器是**按 handler 挂**的、不是全局的 ⇒ 本 handler 的参数表里**没有**
//! `AuthUser` 就没有 401 这一档，**天然**满足「公开面」（与 `probes/*` 的根路径路由同理）。
//! ⚠️ 但它**必须**自己挡住滥用：上游的三道 spam 闸本仓**全都有**——
//! 企业邮箱校验（`DoD` 2）、per-IP 5/h（`DoD` 4）、per-email 3/h（上游
//! `contactSalesHourlyEmailCap`，**独立**于 per-IP 那道）。
//!
//! # 三道 spam 闸的分工（**不要合并**）
//!
//! | 闸 | 键 | 上游常量 | 位置 |
//! |---|---|---|---|
//! | 企业邮箱域名 | 邮箱域名 | 29 个免费域名表 | 本文件（`DoD` 2） |
//! | per-IP 5/h | 远端 IP | `RATE_LIMIT_CONTACT_SALES`，**缺省 5** | 本文件（`DoD` 4） |
//! | per-email 3/h | 规范化邮箱 | `contactSalesHourlyEmailCap = 3` | 路由 + [`ContactSalesRepo`] |
//!
//! 前一道是「不接受垃圾来源」，后两道是「就算过了也别重放」⇒ 折叠成一道会丢掉另一道
//! 各自的语义（per-IP 挡**换邮箱**的洪水，per-email 挡**换 IP**的重放）。
//!
//! # 邮箱规范化（`canonicalBusinessEmail`）是**安全**步骤，不是清洁步骤
//!
//! 上游逐字：「checking the raw string allows `Ada <ada@gmail.com>` to slip past the
//! free-email block list because the parsed RFC 5322 address would have domain `gmail.com`
//! while the raw "@" suffix would be `gmail.com>`」⇒ **必须**先抽出 `local@domain`
//! 再查域名表。本仓不引入 `mail` 解析器（Rust 侧无等价标准库件），改为**规范化到
//! 最小的 `local@domain` 形状 + 查表**，并显式拒掉带显示名 / 尖括号 / 注释的形式
//! （那正是上游那条注释点名的绕过手法）。
//!
//! # 判负阶梯（逐字对齐上游的顺序）
//!
//! `400 invalid request body` → `400 <field> is required` / `<field> is too long`
//! → `400 business_email is invalid` → `400 business_email is too long`
//! → `400 please use a business email address` → `400 company_size is invalid`
//! → `400 country_region is required` / `is too long` → `400 use_case is invalid`
//! → `400 goals is too long` → `429`（per-email 3/h）→ `201`。

use std::net::SocketAddr;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use mc_autopilot::webhook::ratelimit::{
    retry_after_secs, SlidingWindowLimiter, SlidingWindowRateLimit,
};
use mc_repos::contact_sales::{ContactSalesRepo, NewInquiry};
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ApiResult};
use crate::routes::agents::bad_request;
use crate::state::AppState;

/// 上游 `contactSalesMaxFirstName` / `MaxLastName` = 80（逐字）。
pub const MAX_FIRST_NAME: usize = 80;
/// 同上。
pub const MAX_LAST_NAME: usize = 80;
/// 上游 `contactSalesMaxEmail` = 254（逐字）。
pub const MAX_EMAIL: usize = 254;
/// 上游 `contactSalesMaxCompanyName` = 200（逐字）。
pub const MAX_COMPANY_NAME: usize = 200;
/// 上游 `contactSalesMaxGoals` = 2000（逐字）。
pub const MAX_GOALS: usize = 2000;
/// 上游 `country_region` 的宽松上限 = 80（逐字，`len(countryRegion) > 80`）。
pub const MAX_COUNTRY_REGION: usize = 80;
/// 上游 `contactSalesBodyLimit = 16 * 1024`（逐字）。
pub const CONTACT_SALES_BODY_LIMIT: usize = 16 * 1024;

/// 上游 `contactSalesHourlyEmailCap = 3`（逐字）。
pub const CONTACT_SALES_HOURLY_EMAIL_CAP: i64 = 3;

/// `RATE_LIMIT_CONTACT_SALES` 的**缺省值**（上游 `router.go:1479` 的 5 次/小时）。
pub const RATE_LIMIT_CONTACT_SALES_DEFAULT: u32 = 5;

/// `user_agent` 截断上限（上游 `truncateString(r.UserAgent(), 512)`）。
const USER_AGENT_MAX: usize = 512;

/// per-IP 滑动窗口闸（**进程级全局**；`state.rs` 是共享锚点、不在本片写集内）。
static CONTACT_SALES_LIMITER: LazyLock<SlidingWindowLimiter> = LazyLock::new(|| {
    SlidingWindowLimiter::new(SlidingWindowRateLimit::new(
        contact_sales_rate_limit(),
        Duration::from_secs(3600),
    ))
});

/// `RATE_LIMIT_CONTACT_SALES`（**`envPositiveInt` 语义**：非正 / 缺省 / 不可解析 ⇒ 5）。
fn contact_sales_rate_limit() -> u32 {
    std::env::var("RATE_LIMIT_CONTACT_SALES")
        .ok()
        .and_then(|raw| raw.trim().parse::<u32>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(RATE_LIMIT_CONTACT_SALES_DEFAULT)
}

/// 上游 `contactSalesAllowedCompanySize` 逐字（**闭合枚举**，前端下拉框那一组）。
pub const ALLOWED_COMPANY_SIZES: [&str; 6] = ["1-10", "11-50", "51-200", "201-500", "501-1000", "1000+"];

/// 上游 `contactSalesAllowedUseCase` 逐字（**闭合枚举**）。
pub const ALLOWED_USE_CASES: [&str; 6] = [
    "evaluate",
    "adopt_team",
    "self_host",
    "integrate",
    "partner",
    "other",
];

/// 上游 `freeEmailDomains` 逐字（**28 个**个人邮箱域名）。
pub const FREE_EMAIL_DOMAINS: [&str; 28] = [
    "gmail.com",
    "googlemail.com",
    "outlook.com",
    "hotmail.com",
    "live.com",
    "msn.com",
    "yahoo.com",
    "yahoo.co.uk",
    "yahoo.co.jp",
    "ymail.com",
    "icloud.com",
    "me.com",
    "mac.com",
    "aol.com",
    "protonmail.com",
    "proton.me",
    "pm.me",
    "gmx.com",
    "gmx.de",
    "mail.com",
    "zoho.com",
    "yandex.com",
    "yandex.ru",
    "qq.com",
    "163.com",
    "126.com",
    "sina.com",
    "foxmail.com",
];

/// 上游 `CreateContactSalesRequest`（逐字 11 个字段）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CreateContactSalesRequest {
    /// 名。
    #[serde(default)]
    pub first_name: String,
    /// 姓。
    #[serde(default)]
    pub last_name: String,
    /// 企业邮箱（**必须**规范化 + 过域名表）。
    #[serde(default)]
    pub business_email: String,
    /// 公司名。
    #[serde(default)]
    pub company_name: String,
    /// 公司规模（**闭合枚举**）。
    #[serde(default)]
    pub company_size: String,
    /// 国家/地区（自由串，≤ 80）。
    #[serde(default)]
    pub country_region: String,
    /// 用途（**闭合枚举**）。
    #[serde(default)]
    pub use_case: String,
    /// 目标描述（≤ 2000，可空）。
    #[serde(default)]
    pub goals: String,
    /// 来源标记（`page` / `onboarding` / `agents_page`；**不**校验，只进指标）。
    #[serde(default)]
    pub source: String,
    /// 同意接收外联。
    #[serde(default)]
    pub consent_outreach: bool,
    /// 同意接收产品更新。
    #[serde(default)]
    pub consent_updates: bool,
}

/// 上游 `ContactSalesResponse`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactSalesResponse {
    /// 新建的 `contact_sales_inquiry.id`。
    pub id: String,
    /// 提交时刻。
    pub created_at: String,
}

/// contact-sales 切片：1 条，**单形态**，且**不挂**鉴权提取器。
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/contact-sales", post(create_contact_sales))
}

/// 上游 `canonicalBusinessEmail` 的等价物：抽出**最小**的 `local@domain` 形状。
///
/// 显式拒掉上游那条注释点名的绕过形式（显示名 / 尖括号 / 注释），因为 Rust 侧没有
/// `net/mail` 那种 RFC 5322 解析器可依赖 ⇒ 这里**只**接受已经是最小形状的输入。
/// 返回小写化的 `local@domain`。
pub fn canonical_business_email(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    // 带显示名 / 尖括号 / 空白 ⇒ 上游会解析出 `addr.Address` 并**剥掉**它们。
    // 本仓不做 RFC 5322 解析（不引依赖）⇒ 宁可**拒**，也不让绕过形态溜过去。
    if trimmed.contains(['<', '>', '(', ')', ',', '"', ':', ';', '\\']) {
        return None;
    }
    if trimmed.chars().any(char::is_whitespace) {
        return None;
    }
    // 上游取 `LastIndex(email, "@")`（**最后一个** `@`）。
    let at = trimmed.rfind('@')?;
    // `at <= 0 || at == len-1` ⇒ `local` 或 `domain` 为空（上游逐字）。
    if at == 0 || at == trimmed.len() - 1 {
        return None;
    }
    let email = trimmed.to_lowercase();
    // 规范化后再确认一次 `local@domain` 仍然成立（`rfind` 在小写化之后位置不变，
    // 但显式重取一次让「哪一段是域名」与查表处**同源**）。
    let at = email.rfind('@')?;
    let domain = &email[at + 1..];
    // 域名至少要有一个点（`ada@localhost` 不是企业邮箱形态）—— 上游靠
    // `net/mail` 隐含地要求一个 TLD，这里显式化。
    if !domain.contains('.') {
        return None;
    }
    Some(email)
}

/// 上游 `isBusinessEmailDomain`：域名**不**在免费表里就是企业邮箱。
///
/// ⚠️ 上游逐字有 `domain := strings.ToLower(email[at+1:])` —— 比表**之前**先小写化，
/// 所以 `probe@GMAIL.COM` 同样被拒。
#[must_use]
pub fn is_business_email(email: &str) -> bool {
    let Some(at) = email.rfind('@') else {
        return false;
    };
    if at == email.len() - 1 {
        return false;
    }
    let domain = email[at + 1..].to_lowercase();
    !FREE_EMAIL_DOMAINS.contains(&domain.as_str())
}

/// 上游 `requireTrimmedField`：`TrimSpace` → 非空 → 长度上限，三条各自一条 400。
fn require_trimmed_field(raw: &str, field: &str, max: usize) -> Result<String, ApiError> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(ApiError(bad_request(format!("{field} is required"))));
    }
    if value.len() > max {
        return Err(ApiError(bad_request(format!("{field} is too long"))));
    }
    Ok(value.to_owned())
}

/// 上游 `truncateString`。
fn truncate(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

/// `POST /api/contact-sales`（上游 `CreateContactSales`）—— **公开面**。
pub async fn create_contact_sales(
    State(state): State<Arc<AppState>>,
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<(StatusCode, Json<ContactSalesResponse>)> {
    // 🔴 **没有** `AuthUser` 提取器 ⇒ 无会话可调用（`DoD` 第 1 条）。
    // ① 体上限 16 KiB ⇒ 超限与非法体**同一条 400**（上游 `MaxBytesReader` + `Decode`）。
    if body.len() > CONTACT_SALES_BODY_LIMIT {
        return Err(ApiError(bad_request("invalid request body")));
    }
    let request: CreateContactSalesRequest =
        serde_json::from_slice(&body).map_err(|_| ApiError(bad_request("invalid request body")))?;

    // ② 三个必填短字段（各带 required / too long 两档）。
    let first_name = require_trimmed_field(&request.first_name, "first_name", MAX_FIRST_NAME)?;
    let last_name = require_trimmed_field(&request.last_name, "last_name", MAX_LAST_NAME)?;
    let company_name = require_trimmed_field(&request.company_name, "company_name", MAX_COMPANY_NAME)?;

    // ③ 企业邮箱：规范化 → 长度 → **域名表**（`DoD` 第 2 条）。
    let email = canonical_business_email(&request.business_email)
        .ok_or_else(|| ApiError(bad_request("business_email is invalid")))?;
    if email.len() > MAX_EMAIL {
        return Err(ApiError(bad_request("business_email is too long")));
    }
    if !is_business_email(&email) {
        return Err(ApiError(bad_request(
            "please use a business email address",
        )));
    }

    // ④ `company_size` 闭合枚举（`DoD` 第 3 条）。
    let company_size = request.company_size.trim();
    if !ALLOWED_COMPANY_SIZES.contains(&company_size) {
        return Err(ApiError(bad_request("company_size is invalid")));
    }
    let company_size = company_size.to_owned();

    // ⑤ `country_region`：自由串但有界（`DoD` 第 4 条的 ≤80）。
    let country_region = request.country_region.trim();
    if country_region.is_empty() {
        return Err(ApiError(bad_request("country_region is required")));
    }
    if country_region.len() > MAX_COUNTRY_REGION {
        return Err(ApiError(bad_request("country_region is too long")));
    }
    let country_region = country_region.to_owned();

    // ⑥ `use_case` 闭合枚举。
    let use_case = request.use_case.trim();
    if !ALLOWED_USE_CASES.contains(&use_case) {
        return Err(ApiError(bad_request("use_case is invalid")));
    }
    let use_case = use_case.to_owned();

    // ⑦ `goals` 可空但有界。
    let goals = request.goals.trim();
    if goals.len() > MAX_GOALS {
        return Err(ApiError(bad_request("goals is too long")));
    }
    let goals = goals.to_owned();

    // ⑧ per-IP 5/h（`DoD` 第 4 条）。**消费**一次。
    let peer_ip = peer.map(|ConnectInfo(addr)| addr.ip().to_string());
    if let Some(ip) = peer_ip.as_deref() {
        if !CONTACT_SALES_LIMITER.allow(ip) {
            return Err(rate_limited(&CONTACT_SALES_LIMITER, ip));
        }
    } else {
        // 拿不到对端地址 ⇒ **跳过** per-IP 闸（与 `webhooks/autopilots.rs` 同款立场），
        // 但 per-email 3/h 那一道**照样**跑 ⇒ 公开面不是无防护的。
        tracing::warn!("contact-sales: no ConnectInfo; per-IP rate limit skipped");
    }

    let repo = ContactSalesRepo::new(state.db.clone());

    // ⑨ per-email 3/h（**独立**于 per-IP 那一道）。
    if repo
        .count_recent_by_email(&email)
        .await
        .map_err(|e| crate::routes::agents::repo_err(e, "contact sales inquiry"))?
        >= CONTACT_SALES_HOURLY_EMAIL_CAP
    {
        return Err(ApiError(mc_errors::Error::RateLimited {
            retry_after_secs: 3600,
        }));
    }

    let user_agent = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");

    let inquiry = NewInquiry {
        first_name,
        last_name,
        business_email: email,
        company_name,
        company_size,
        country_region,
        use_case,
        goals,
        consent_outreach: request.consent_outreach,
        consent_updates: request.consent_updates,
        submitter_ip: peer_ip,
        user_agent: truncate(user_agent, USER_AGENT_MAX),
    };

    let row = repo
        .create(&inquiry)
        .await
        .map_err(|e| crate::routes::agents::repo_err(e, "contact sales inquiry"))?;

    Ok((
        StatusCode::CREATED,
        Json(ContactSalesResponse {
            id: row.id.to_string(),
            created_at: row.created_at.to_rfc3339(),
        }),
    ))
}

/// 429（per-IP 那一道）—— `Retry-After` 向上取整、至少 1 秒（`webhook/ratelimit.rs` 逐字）。
fn rate_limited(limiter: &SlidingWindowLimiter, key: &str) -> ApiError {
    let retry = retry_after_secs(limiter.retry_after(key));
    ApiError(mc_errors::Error::RateLimited {
        retry_after_secs: u32::try_from(retry).unwrap_or(u32::MAX),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use crate::state::{AdapterRegistry, AppState, ConfigSnapshot, RuntimeHandles};

    fn state() -> Arc<AppState> {
        let db = mc_db::Db::connect_lazy("postgres://np:np@127.0.0.1:1/none", 1, 0)
            .expect("lazy");
        let realtime = mc_realtime::RealtimeHandle::start(8);
        let ws = Arc::new(mc_realtime::WsState::new(realtime.clone(), "lum-1820"));
        Arc::new(AppState::new(
            db,
            RuntimeHandles {
                actors: mc_core::actor::ActorRegistry::new(),
                adapters: Arc::new(AdapterRegistry::default()),
            },
            ConfigSnapshot::default(),
            realtime,
            ws,
        ))
    }

    fn probe() -> Router {
        let state = state();
        router().with_state(state)
    }

    async fn post(uri: &str, body: &str) -> (StatusCode, Vec<u8>) {
        use axum::body::Body as AxumBody;
        use http_body_util::BodyExt as _;
        use tower::ServiceExt as _;

        let response = probe()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(AxumBody::from(body.to_owned()))
                    .expect("request"),
            )
            .await
            .expect("router call");
        let status = response.status();
        let bytes = response
            .into_body()
            .collect()
            .await
            .expect("body")
            .to_bytes()
            .to_vec();
        (status, bytes)
    }

    /// 🔴 **公开面**的核心判据：**不带任何会话头**也能走到**校验层**（不是 401）。
    ///
    /// 库里没有用户、没有 workspace：请求会一路走到「per-email 计数」那一格才因为
    /// **库不可达**而 500。⇒ 判据是「**不是** 401 / **不是** 404」。
    #[tokio::test]
    async fn it_is_reachable_without_any_session() {
        let (status, body) = post("/api/contact-sales", "{}").await;
        assert_ne!(status, StatusCode::UNAUTHORIZED, "must be public: {body:?}");
        assert_ne!(status, StatusCode::NOT_FOUND, "must be registered: {body:?}");
        // 空体在**第一条**字段校验就被拒（`first_name is required`）—— 说明它进了 handler。
        let body: serde_json::Value = serde_json::from_slice(&body).expect("error body");
        assert_eq!(body["error"]["code"], "validation_error");
        assert!(body["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("first_name is required")));
    }

    /// 形态：单形态（补尾斜杠 = `EXTRA_ALIAS` 硬失败）。
    #[tokio::test]
    async fn registered_as_a_single_form_without_trailing_slash() {
        let (slash, _) = post("/api/contact-sales/", "{}").await;
        assert_eq!(slash, StatusCode::NOT_FOUND);
    }

    /// `DoD` 第 2 条：企业邮箱的**正例**与**反例**。
    #[test]
    fn business_email_normalization_and_the_free_domain_block_list() {
        // 正例：规范化（小写 + 剥空白）。
        assert_eq!(
            canonical_business_email("  Ada@Acme.IO ").as_deref(),
            Some("ada@acme.io")
        );
        assert!(is_business_email("ada@acme.io"));
        // 反例 ①：上游注释点名的绕过形态（显示名 / 尖括号）**必须**被拒。
        for smuggled in [
            "Ada <ada@gmail.com>",
            "<ada@gmail.com>",
            "ada@gmail.com (comment)",
            "\"Ada\" <ada@gmail.com>",
        ] {
            assert_eq!(
                canonical_business_email(smuggled),
                None,
                "{smuggled:?} must not normalize into a bypassable shape"
            );
        }
        // 反例 ②：形状不合法。
        for malformed in ["", "   ", "no-at-sign", "@acme.io", "ada@", "ada@localhost"] {
            assert_eq!(canonical_business_email(malformed), None, "{malformed:?}");
        }
        // 反例 ③：**28 个**免费域名逐条被拒（`DoD` 第 2 条的反例半边）。
        for domain in FREE_EMAIL_DOMAINS {
            let email = format!("probe@{domain}");
            assert!(
                !is_business_email(&email),
                "{email} must be rejected as a free provider"
            );
        }
        assert_eq!(FREE_EMAIL_DOMAINS.len(), 28);
        // 大小写：域名表比对前先小写化 ⇒ `GMAIL.COM` 同样被拒。
        assert!(!is_business_email("probe@GMAIL.COM"));
    }

    /// `DoD` 第 3 条：`company_size` 是**闭合** 6 值枚举。
    #[test]
    fn company_size_is_a_closed_six_value_enum() {
        assert_eq!(ALLOWED_COMPANY_SIZES.len(), 6);
        for size in ALLOWED_COMPANY_SIZES {
            assert!(ALLOWED_COMPANY_SIZES.contains(&size));
        }
        for invalid in ["", "0-10", "10", "1 - 10", "1000-2000", "unknown", "11-50 "] {
            assert!(
                !ALLOWED_COMPANY_SIZES.contains(&invalid),
                "{invalid:?} must be rejected"
            );
        }
        // `use_case` 同样闭合（6 值）。
        assert_eq!(ALLOWED_USE_CASES.len(), 6);
        for invalid in ["", "Evaluate", "adopt-team", "unknown"] {
            assert!(!ALLOWED_USE_CASES.contains(&invalid), "{invalid:?}");
        }
    }

    /// `DoD` 第 4 条：per-IP 限流**复用** `SlidingWindowLimiter`，缺省 5/h。
    ///
    /// `RATE_LIMIT_CONTACT_SALES` 未设 ⇒ 5。测试**不**改进程 env（那会与并发用例互相
    /// 污染）⇒ 断言的是缺省值与 `envPositiveInt` 语义。
    #[test]
    fn the_per_ip_limiter_defaults_to_five_per_hour_and_is_reused() {
        assert_eq!(RATE_LIMIT_CONTACT_SALES_DEFAULT, 5);
        // 闸的类型就是 M5-5 那个（**不是**第二份实现）—— 逐字按类型断言。
        let _: &SlidingWindowLimiter = &CONTACT_SALES_LIMITER;
        // 缺省 5/h：消费 5 次放行、第 6 次被拒；换一个 IP 照旧放行（**不是**全局闸）。
        let probe = SlidingWindowLimiter::new(SlidingWindowRateLimit::new(
            RATE_LIMIT_CONTACT_SALES_DEFAULT,
            Duration::from_secs(3600),
        ));
        for _ in 0..RATE_LIMIT_CONTACT_SALES_DEFAULT {
            assert!(probe.allow("198.51.100.7"));
        }
        assert!(!probe.allow("198.51.100.7"));
        assert!(probe.allow("198.51.100.8"));
    }

    /// 字段长度上限逐条钉住（`DoD` 第 4 条的 `country_region` ≤80 是点名的那一条）。
    #[test]
    fn the_length_caps_match_the_upstream_constants() {
        assert_eq!(MAX_FIRST_NAME, 80);
        assert_eq!(MAX_LAST_NAME, 80);
        assert_eq!(MAX_COMPANY_NAME, 200);
        assert_eq!(MAX_EMAIL, 254);
        assert_eq!(MAX_GOALS, 2000);
        assert_eq!(MAX_COUNTRY_REGION, 80);
        assert_eq!(CONTACT_SALES_BODY_LIMIT, 16 * 1024);
        assert_eq!(CONTACT_SALES_HOURLY_EMAIL_CAP, 3);
    }

    /// `require_trimmed_field` 的三档（空 / 超长 / 正常）。
    #[test]
    fn required_trimmed_field_has_three_outcomes() {
        assert_eq!(
            require_trimmed_field("   ", "first_name", MAX_FIRST_NAME)
                .unwrap_err()
                .0
                .to_string(),
            "validation error: first_name is required"
        );
        let long = "x".repeat(MAX_FIRST_NAME + 1);
        assert_eq!(
            require_trimmed_field(&long, "first_name", MAX_FIRST_NAME)
                .unwrap_err()
                .0
                .to_string(),
            "validation error: first_name is too long"
        );
        // 恰好等于上限**合法**。
        let at_limit = "x".repeat(MAX_FIRST_NAME);
        assert_eq!(
            require_trimmed_field(&at_limit, "first_name", MAX_FIRST_NAME)
                .map_err(|e| e.0.to_string())
                .expect("legal"),
            at_limit
        );
    }

    /// `truncate` 按**字符**截断（上游按字节切，本仓按字符 ⇒ 永不切坏 UTF-8）。
    #[test]
    fn truncate_counts_characters_and_never_splits_utf8() {
        assert_eq!(truncate("abcdef", 3), "abc");
        assert_eq!(truncate("abc", 10), "abc");
        let wide = "é".repeat(10);
        let cut = truncate(&wide, 3);
        assert_eq!(cut, "ééé");
        assert!(cut.chars().count() <= 3);
    }
}
