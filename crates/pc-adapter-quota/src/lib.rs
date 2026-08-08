#![forbid(unsafe_code)]

//! Codex 本地配额探针（R430）。
//!
//! 复刻 Node `packages/adapters/codex-local/src/server/quota.ts`：
//! - 解析 `$CODEX_HOME/auth.json`（legacy + modern 两种结构）；
//! - 解析 JWT payload 提取 email / planType；
//! - 规范化 usedPercent（<1 视为百分比小数，否则原值，封顶 100）；
//! - WHAM `chatgpt.com/backend-api/wham/usage` 响应 → `QuotaWindow` 列表；
//! - 401 响应体截断（最多 4000 字节）并做 auth-refresh 错误族分类，且不泄露 token；
//! - 映射 Codex RPC `account/rateLimits/read` / `account/read` 到 `QuotaWindow`。
//!
//! 纯函数与 IO 解耦：所有 HTTP / 子进程调用都通过 trait 注入，便于离线单测。

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
pub const CODEX_USAGE_SOURCE_RPC: &str = "codex-rpc";
pub const CODEX_USAGE_SOURCE_WHAM: &str = "codex-wham";
pub const MAX_QUOTA_ERROR_BODY_BYTES: usize = 4_000;
pub const WHAM_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

// ---------------------------------------------------------------------------
// 类型
// ---------------------------------------------------------------------------

/// 单个 rate-limit / usage 窗口。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaWindow {
    pub label: String,
    pub used_percent: Option<i64>,
    pub resets_at: Option<String>,
    pub value_label: Option<String>,
    pub detail: Option<String>,
}

/// 单个 provider 的配额探测结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderQuotaResult {
    pub provider: String,
    pub source: Option<String>,
    pub ok: bool,
    pub error_family: Option<String>,
    pub error: Option<String>,
    pub windows: Vec<QuotaWindow>,
}

/// Codex auth 信息。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexAuthInfo {
    pub access_token: String,
    pub account_id: Option<String>,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub last_refresh: Option<String>,
}

/// 错误族（与 codex_errors::CodexAuthRefreshFailureClass 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexQuotaErrorFamily {
    RefreshTokenReused,
    RefreshTokenExpired,
    RefreshTokenInvalidated,
}

impl CodexQuotaErrorFamily {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RefreshTokenReused => "refresh_token_reused",
            Self::RefreshTokenExpired => "refresh_token_expired",
            Self::RefreshTokenInvalidated => "refresh_token_invalidated",
        }
    }
}

/// 配额探测 IO 抽象：HTTP 与子进程均可注入。
pub trait QuotaIo: Send + Sync {
    /// 发起 GET 请求，返回 (状态码, 响应体前缀)。
    fn get(&self, url: &str, headers: &BTreeMap<String, String>, timeout: Duration)
        -> Result<(u16, String), String>;
    /// 读取本地 auth.json 原文。
    fn read_auth_file(&self, path: &str) -> Result<String, String>;
    /// 执行 codex app-server 的一次 RPC 请求（JSON-Lines 往返），返回响应对象。
    fn codex_rpc(
        &self,
        method: &str,
        params: &Value,
        timeout: Duration,
    ) -> Result<Value, String>;
}

// ---------------------------------------------------------------------------
// 纯函数
// ---------------------------------------------------------------------------

/// 从 base64url JWT payload 解码（不含校验签名）。
pub fn base64_url_decode(input: &str) -> Option<String> {
    let mut normalized = input.replace('-', "+").replace('_', "/");
    let remainder = normalized.len() % 4;
    if remainder > 0 {
        normalized.push_str(&"=".repeat(4 - remainder));
    }
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    STANDARD
        .decode(normalized)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
}

/// 解析 JWT payload。
pub fn decode_jwt_payload(token: Option<&str>) -> Option<Value> {
    let token = token.map(str::trim).unwrap_or_default();
    if token.is_empty() {
        return None;
    }
    let mut parts = token.split('.');
    let _header = parts.next()?;
    let payload = parts.next()?;
    let decoded = base64_url_decode(payload)?;
    serde_json::from_str(&decoded).ok()
}

fn nested_string(record: &Value, path_segments: &[&str]) -> Option<String> {
    let mut current = record;
    for segment in path_segments {
        current = current.get(*segment)?;
        if !current.is_object() {
            return None;
        }
    }
    match current.as_str() {
        Some(value) if !value.trim().is_empty() => Some(value.trim().to_owned()),
        _ => None,
    }
}

fn plan_and_email_from_token(id_token: Option<&str>, access_token: Option<&str>) -> (Option<String>, Option<String>) {
    let mut payloads = Vec::new();
    for token in [id_token, access_token].into_iter().flatten() {
        if let Some(payload) = decode_jwt_payload(Some(token)) {
            payloads.push(payload);
        }
    }
    for payload in payloads {
        let direct_email = payload.get("email").and_then(Value::as_str).map(str::to_owned);
        let auth_block = payload.get("https://api.openai.com/auth").and_then(Value::as_object);
        let profile_block = payload
            .get("https://api.openai.com/profile")
            .and_then(Value::as_object);
        let email = direct_email
            .or_else(|| {
                profile_block
                    .and_then(|p| p.get("email"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .or_else(|| {
                auth_block
                    .and_then(|a| a.get("chatgpt_user_email"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
        let plan_type = auth_block
            .and_then(|a| a.get("chatgpt_plan_type"))
            .and_then(Value::as_str)
            .map(str::to_owned);
        if email.is_some() || plan_type.is_some() {
            return (email, plan_type);
        }
    }
    (None, None)
}

/// 解析 `auth.json` 内容为 `CodexAuthInfo`。
pub fn parse_codex_auth_json(raw: &str) -> Option<CodexAuthInfo> {
    let obj: Value = serde_json::from_str(raw).ok()?;
    if !obj.is_object() {
        return None;
    }
    let modern_tokens = obj.get("tokens");
    let legacy = obj.get("accessToken").and_then(Value::as_str);
    let access_token = legacy
        .map(str::to_owned)
        .or_else(|| {
            modern_tokens
                .and_then(|t| t.get("access_token"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .or_else(|| nested_string(&obj, &["tokens", "access_token"]));
    let access_token = match access_token {
        Some(value) if !value.is_empty() => value,
        _ => return None,
    };
    let account_id = obj
        .get("accountId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| modern_tokens.and_then(|t| t.get("account_id")).and_then(Value::as_str).map(str::to_owned))
        .or_else(|| nested_string(&obj, &["tokens", "account_id"]))
        .filter(|v| !v.trim().is_empty());
    let refresh_token = modern_tokens
        .and_then(|t| t.get("refresh_token"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| nested_string(&obj, &["tokens", "refresh_token"]))
        .filter(|v| !v.trim().is_empty());
    let id_token = modern_tokens
        .and_then(|t| t.get("id_token"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| nested_string(&obj, &["tokens", "id_token"]))
        .filter(|v| !v.trim().is_empty());
    let last_refresh = obj
        .get("last_refresh")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .filter(|v| !v.trim().is_empty());
    let (email, plan_type) = plan_and_email_from_token(
        id_token.as_deref(),
        access_token.as_str().into(),
    );
    Some(CodexAuthInfo {
        access_token,
        account_id,
        refresh_token,
        id_token,
        email,
        plan_type,
        last_refresh,
    })
}

/// 规范化 usedPercent：<1 视为百分比小数，否则原值，封顶 100。
pub fn normalize_codex_used_percent(raw_pct: Option<f64>) -> Option<i64> {
    let raw = raw_pct?;
    let value = if raw < 1.0 { raw * 100.0 } else { raw };
    Some((value.round() as i64).min(100))
}

/// 秒级 unix 时间 → ISO8601。
pub fn unix_seconds_to_iso(value: Option<i64>) -> Option<String> {
    let value = value?;
    let datetime = chrono::DateTime::from_timestamp(value, 0)?;
    Some(datetime.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

/// 人类可读窗口标签。
pub fn seconds_to_window_label(seconds: Option<f64>, fallback: &str) -> String {
    match seconds {
        Some(value) => {
            let hours = value / 3600.0;
            if hours < 6.0 {
                "5h".to_owned()
            } else if hours <= 24.0 {
                "24h".to_owned()
            } else if hours <= 168.0 {
                "7d".to_owned()
            } else {
                format!("{}d", (hours / 24.0).round() as i64)
            }
        }
        None => fallback.to_owned(),
    }
}

fn parse_credit_balance(value: &Value) -> Option<String> {
    match value {
        Value::Number(number) => {
            let parsed = number.as_f64()?;
            Some(format!("${parsed:.2} remaining"))
        }
        Value::String(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                None
            } else if let Ok(parsed) = trimmed.parse::<f64>() {
                Some(format!("${parsed:.2} remaining"))
            } else {
                Some(trimmed.to_owned())
            }
        }
        _ => None,
    }
}

/// 将 WHAM `usage` 响应体映射为 `QuotaWindow` 列表。
pub fn map_wham_usage(body: &Value) -> Vec<QuotaWindow> {
    let mut windows = Vec::new();
    let rate_limit = body.get("rate_limit").and_then(Value::as_object);
    if let Some(w) = rate_limit
        .and_then(|r| r.get("primary_window"))
        .and_then(Value::as_object)
    {
        windows.push(build_window(
            "5h limit".to_owned(),
            w,
        ));
    }
    if let Some(w) = rate_limit
        .and_then(|r| r.get("secondary_window"))
        .and_then(Value::as_object)
    {
        windows.push(build_window(
            "Weekly limit".to_owned(),
            w,
        ));
    }
    if let Some(credits) = body.get("credits").and_then(Value::as_object) {
        if credits.get("unlimited").and_then(Value::as_bool) != Some(true) {
            let balance = credits.get("balance");
            let value_label = match balance.and_then(Value::as_f64) {
                Some(cents) => format!("${:.2} remaining", cents / 100.0),
                None => balance
                    .and_then(parse_credit_balance)
                    .unwrap_or_else(|| "N/A".to_owned()),
            };
            windows.push(QuotaWindow {
                label: "Credits".to_owned(),
                used_percent: None,
                resets_at: None,
                value_label: Some(value_label),
                detail: None,
            });
        }
    }
    windows
}

fn build_window(label: String, w: &serde_json::Map<String, Value>) -> QuotaWindow {
    let used_percent = w
        .get("used_percent")
        .or_else(|| w.get("usedPercent"))
        .and_then(Value::as_f64)
        .and_then(|v| normalize_codex_used_percent(Some(v)));
    let resets_at = match w
        .get("reset_at")
        .or_else(|| w.get("resetsAt"))
    {
        Some(Value::Number(number)) => number.as_i64().and_then(|v| unix_seconds_to_iso(Some(v))),
        Some(Value::String(raw)) if !raw.trim().is_empty() => Some(raw.trim().to_owned()),
        _ => None,
    };
    QuotaWindow {
        label,
        used_percent,
        resets_at,
        value_label: None,
        detail: None,
    }
}

/// 将 Codex RPC `rateLimits` / `account` 映射为 `CodexRpcQuotaSnapshot`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexRpcQuotaSnapshot {
    pub windows: Vec<QuotaWindow>,
    pub email: Option<String>,
    pub plan_type: Option<String>,
}

pub fn map_codex_rpc_quota(limits: &Value, account: Option<&Value>) -> CodexRpcQuotaSnapshot {
    let mut windows = Vec::new();
    let limits_by_id = limits.get("rateLimitsByLimitId").and_then(Value::as_object);
    let root_limit = limits.get("rateLimits").and_then(Value::as_object);

    let mut all_limits: BTreeMap<String, &serde_json::Map<String, Value>> = BTreeMap::new();
    if let Some(root) = root_limit {
        if let Some(limit_id) = root.get("limitId").and_then(Value::as_str) {
            all_limits.insert(limit_id.to_owned(), root);
        }
    }
    if let Some(by_id) = limits_by_id {
        for (key, value) in by_id {
            if let Some(obj) = value.as_object() {
                all_limits.insert(key.clone(), obj);
            }
        }
    }
    if !all_limits.contains_key("codex") {
        if let Some(root) = root_limit {
            all_limits.insert("codex".to_owned(), root);
        }
    }

    let mut order: Vec<String> = vec!["codex".to_owned()];
    for key in all_limits.keys() {
        if !order.contains(key) {
            order.push(key.clone());
        }
    }
    for limit_id in order {
        let Some(limit) = all_limits.get(&limit_id) else { continue };
        let prefix = if limit_id == "codex" {
            String::new()
        } else {
            let name = limit
                .get("limitName")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| limit_id.clone());
            format!("{name} · ")
        };
        if let Some(primary) = limit.get("primary").and_then(Value::as_object) {
            let window = build_window(format!("{prefix}5h limit"), primary);
            windows.push(window);
        }
        if let Some(secondary) = limit.get("secondary").and_then(Value::as_object) {
            let window = build_window(format!("{prefix}Weekly limit"), secondary);
            windows.push(window);
        }
        if limit_id == "codex" {
            if let Some(credits) = limit.get("credits").and_then(Value::as_object) {
                if credits.get("unlimited").and_then(Value::as_bool) != Some(true) {
                    let balance = credits.get("balance");
                    let value_label = balance
                        .and_then(parse_credit_balance)
                        .unwrap_or_else(|| "N/A".to_owned());
                    windows.push(QuotaWindow {
                        label: "Credits".to_owned(),
                        used_percent: None,
                        resets_at: None,
                        value_label: Some(value_label),
                        detail: None,
                    });
                }
            }
        }
    }

    let email = account
        .and_then(|a| a.get("account"))
        .and_then(Value::as_object)
        .and_then(|a| a.get("email"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let plan_type = account
        .and_then(|a| a.get("account"))
        .and_then(Value::as_object)
        .and_then(|a| a.get("planType"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            root_limit
                .and_then(|r| r.get("planType"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        });
    CodexRpcQuotaSnapshot {
        windows,
        email,
        plan_type,
    }
}

/// 从 HTTP 错误文本分类 auth 错误族（复用 codex_errors 逻辑）。
pub fn classify_quota_error_family(error_message: &str) -> Option<CodexQuotaErrorFamily> {
    let text = error_message.to_ascii_lowercase();
    if text.contains("refresh_token_reused")
        || text.contains("refresh token has already been used")
        || text.contains("token reuse detected")
    {
        return Some(CodexQuotaErrorFamily::RefreshTokenReused);
    }
    if text.contains("refresh_token_expired") || text.contains("refresh token has expired") {
        return Some(CodexQuotaErrorFamily::RefreshTokenExpired);
    }
    if text.contains("refresh_token_invalidated")
        || text.contains("refresh token has been invalidated")
        || text.contains("refresh token has been revoked")
        || text.contains("invalid refresh token")
        || text.contains("missing bearer")
        || text.contains("invalid_grant")
    {
        return Some(CodexQuotaErrorFamily::RefreshTokenInvalidated);
    }
    None
}

// ---------------------------------------------------------------------------
// IO 层
// ---------------------------------------------------------------------------

/// 默认 IO：基于 `reqwest` + `tokio::process`。
#[derive(Debug, Default, Clone)]
pub struct DefaultQuotaIo;

#[async_trait::async_trait]
pub trait AsyncQuotaIo: Send + Sync {
    async fn get(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        timeout: Duration,
    ) -> Result<(u16, String), String>;
}

#[async_trait::async_trait]
impl AsyncQuotaIo for DefaultQuotaIo {
    async fn get(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        timeout: Duration,
    ) -> Result<(u16, String), String> {
        let client = reqwest::Client::new();
        let mut request = client.get(url).timeout(timeout);
        for (key, value) in headers {
            request = request.header(key, value);
        }
        let response = request.send().await.map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        let text = response.text().await.map_err(|e| e.to_string())?;
        Ok((status, text))
    }
}

// ---------------------------------------------------------------------------
// 高级组合（依赖注入版）
// ---------------------------------------------------------------------------

/// 通过注入的 IO 读取 auth 并执行 WHAM 探测。
pub async fn fetch_codex_quota_with<IO: AsyncQuotaIo>(
    io: &IO,
    token: &str,
    account_id: Option<&str>,
) -> Result<Vec<QuotaWindow>, String> {
    let mut headers = BTreeMap::new();
    headers.insert("Authorization".to_owned(), format!("Bearer {token}"));
    if let Some(account_id) = account_id.filter(|s| !s.trim().is_empty()) {
        headers.insert("ChatGPT-Account-Id".to_owned(), account_id.to_owned());
    }
    let (status, body_text) = io
        .get(WHAM_USAGE_URL, &headers, Duration::from_secs(8))
        .await?;
    if status != 200 {
        let message = format!("chatgpt wham api returned {status}");
        let body_prefix = truncate_body(&body_text, MAX_QUOTA_ERROR_BODY_BYTES);
        let combined = [message.clone(), body_prefix].join("\n");
        let family = classify_quota_error_family(&combined);
        return match family {
            Some(family) => Err(format!(
                "{}|family={}",
                message,
                family.as_str()
            )),
            None => Err(message),
        };
    }
    let body: Value = serde_json::from_str(&body_text).map_err(|e| e.to_string())?;
    Ok(map_wham_usage(&body))
}

/// 截断响应体到 max_bytes（用于错误分类，防止 token 泄露/放大）。
pub fn truncate_body(raw: &str, max_bytes: usize) -> String {
    if max_bytes == 0 || raw.is_empty() {
        return String::new();
    }
    let mut remaining = max_bytes;
    let mut out = String::new();
    for ch in raw.chars() {
        if remaining == 0 {
            break;
        }
        out.push(ch);
        remaining -= 1;
    }
    out
}

// ---------------------------------------------------------------------------
// Claude 配额（R432）：复刻 Node `claude-local/src/server/quota.ts`
// ---------------------------------------------------------------------------

/// Claude OAuth usage API 端点。
pub const ANTHROPIC_OAUTH_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
/// Claude OAuth usage 的 beta header。
pub const ANTHROPIC_OAUTH_BETA_HEADER: &str = "oauth-2025-04-20";
/// Claude CLI 探测时从子进程环境剔除的变量前缀。
pub const ANTHROPIC_ENV_PREFIX: &str = "ANTHROPIC_";

/// 将 utilization 值规范化为 0-100 整数百分比。
///
/// 复刻 Node `toPercent`：0-1 视为小数（×100），0-100 视为原值；封顶 100。
pub fn claude_to_percent(raw: f64) -> Option<i64> {
    if !raw.is_finite() {
        return None;
    }
    let value = if raw < 1.0 { raw * 100.0 } else { raw };
    Some((value.round() as i64).min(100))
}

/// 将金额格式化为美元字符串（复刻 Node `Intl.NumberFormat("en-US", currency)`）。
pub fn format_currency_amount(value: f64, currency: Option<&str>) -> String {
    let code = currency
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map_or_else(|| "USD".to_owned(), str::to_uppercase);
    let rounded = (value * 100.0).round() / 100.0;
    if code == "USD" {
        let sign = if rounded < 0.0 { "-" } else { "" };
        let abs = rounded.abs();
        let cents = (abs * 100.0).round() as i64;
        let dollars = cents / 100;
        let cents_part = cents % 100;
        // 千位分组（对齐 Intl.NumberFormat("en-US")）
        let digits = dollars.to_string();
        let mut grouped = String::new();
        for (i, ch) in digits.chars().enumerate() {
            if i > 0 && (digits.len() - i).is_multiple_of(3) {
                grouped.push(',');
            }
            grouped.push(ch);
        }
        format!("{sign}${grouped}.{cents_part:02}")
    } else {
        format!("{rounded:.2} {code}")
    }
}

/// 格式化 extra_usage 的 "已用 / 限额" 标签（API 返回的是分，除以 100 转美元）。
fn format_extra_usage_label(extra_usage: &serde_json::Map<String, Value>) -> Option<String> {
    let monthly_limit = extra_usage.get("monthly_limit").and_then(Value::as_f64)?;
    let used_credits = extra_usage.get("used_credits").and_then(Value::as_f64)?;
    if !monthly_limit.is_finite() || !used_credits.is_finite() {
        return None;
    }
    let currency = extra_usage
        .get("currency")
        .and_then(Value::as_str);
    Some(format!(
        "{} / {}",
        format_currency_amount(used_credits / 100.0, currency),
        format_currency_amount(monthly_limit / 100.0, currency)
    ))
}

/// 将 Anthropic OAuth usage 响应体映射为 `QuotaWindow` 列表。
///
/// 复刻 Node `fetchClaudeQuota` 的响应映射：five_hour / seven_day /
/// seven_day_sonnet / seven_day_opus / extra_usage。
pub fn map_anthropic_oauth_usage(body: &Value) -> Vec<QuotaWindow> {
    let mut windows = Vec::new();
    let push_window = |label: &str, w: Option<&serde_json::Map<String, Value>>, windows: &mut Vec<QuotaWindow>| {
        if let Some(w) = w {
            let used = w.get("utilization").and_then(Value::as_f64).and_then(claude_to_percent);
            let resets_at = w
                .get("resets_at")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
                .map(str::trim)
                .map(str::to_owned);
            windows.push(QuotaWindow {
                label: label.to_owned(),
                used_percent: used,
                resets_at,
                value_label: None,
                detail: None,
            });
        }
    };

    push_window("Current session", body.get("five_hour").and_then(Value::as_object), &mut windows);
    push_window("Current week (all models)", body.get("seven_day").and_then(Value::as_object), &mut windows);
    push_window("Current week (Sonnet only)", body.get("seven_day_sonnet").and_then(Value::as_object), &mut windows);
    push_window("Current week (Opus only)", body.get("seven_day_opus").and_then(Value::as_object), &mut windows);

    if let Some(extra) = body.get("extra_usage").and_then(Value::as_object) {
        let is_enabled = extra.get("is_enabled").and_then(Value::as_bool);
        let used = extra
            .get("utilization")
            .and_then(Value::as_f64)
            .and_then(claude_to_percent);
        let (used_percent, value_label, detail) = match is_enabled {
            Some(false) => (
                None,
                Some("Not enabled".to_owned()),
                Some("Extra usage not enabled".to_owned()),
            ),
            _ => (
                used,
                format_extra_usage_label(extra),
                Some("Monthly extra usage pool".to_owned()),
            ),
        };
        windows.push(QuotaWindow {
            label: "Extra usage".to_owned(),
            used_percent,
            resets_at: None,
            value_label,
            detail,
        });
    }
    windows
}

/// 归一化文本用于标签搜索（小写 + 去除非字母数字）。
pub fn claude_normalize_for_label_search(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect()
}

/// 复刻 Node `cleanTerminalText`：去 ANSI、退格、NUL、CR→换行。
pub fn claude_clean_terminal_text(text: &str) -> String {
    // Node 的 stripAnsi 移除 OSC 序列（ESC ] ... BEL/ESC \）和 CSI/单字节 ESC 序列。
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => {
                // OSC: ESC ] 直到 BEL 或 ESC
                if chars.peek() == Some(&'[') {
                    // CSI: 直到最终字节（@-~）
                    chars.next();
                    for next in chars.by_ref() {
                        out.push(next);
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                } else {
                    // 单字节 ESC 序列或 OSC
                    let mut is_osc = false;
                    if chars.peek() == Some(&']') {
                        chars.next();
                        is_osc = true;
                    }
                    if is_osc {
                        for next in chars.by_ref() {
                            if next == '\u{7}' {
                                break;
                            }
                            if next == '\u{1b}' {
                                if chars.peek() == Some(&'\\') {
                                    chars.next();
                                }
                                break;
                            }
                        }
                    } else if let Some(next) = chars.next() {
                        // 单字节 ESC [a-zA-Z_-] 直接丢弃
                        if !('@'..='~').contains(&next) {
                            out.push(next);
                        }
                    }
                }
            }
            '\u{0}' => {}
            '\r' => out.push('\n'),
            _ => out.push(c),
        }
    }
    out
}

/// 从 usage 文本中截取最新一个 "Settings:" 面板（复刻 Node `trimToLatestUsagePanel`）。
fn claude_trim_to_latest_usage_panel(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let settings_index = lower.rfind("settings:")?;
    let tail = text[settings_index..].to_owned();
    let tail_lower = tail.to_lowercase();
    if !tail_lower.contains("usage") {
        return None;
    }
    if !tail_lower.contains("current session") && !tail_lower.contains("loading usage") {
        return None;
    }
    let stop_markers = [
        "status dialog dismissed",
        "checking for updates",
        "press ctrl-c again to exit",
    ];
    let mut stop_index: Option<usize> = None;
    for marker in stop_markers {
        if let Some(index) = tail_lower.find(marker) {
            if stop_index.is_none_or(|current| index < current) {
                stop_index = Some(index);
            }
        }
    }
    let tail = match stop_index {
        Some(index) => tail[..index].to_owned(),
        None => tail,
    };
    Some(tail)
}

/// 提取 usage 文本中的错误提示（复刻 Node `extractUsageError`）。
fn claude_extract_usage_error(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let compact: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
    if lower.contains("token_expired") || lower.contains("token has expired") {
        return Some("Claude CLI token expired. Run `claude login` to refresh.".to_owned());
    }
    if lower.contains("authentication_error") {
        return Some("Claude CLI authentication error. Run `claude login`.".to_owned());
    }
    if lower.contains("rate_limit_error") || lower.contains("rate limited") || compact.contains("ratelimited") {
        return Some("Claude CLI usage endpoint is rate limited right now. Please try again later.".to_owned());
    }
    if lower.contains("failed to load usage data") || compact.contains("failedtoloadusagedata") {
        return Some("Claude CLI could not load usage data. Open the CLI and retry `/usage`.".to_owned());
    }
    None
}

/// 从一行提取百分比（复刻 Node `percentFromLine`：remaining/left/available 取反）。
pub fn claude_percent_from_line(line: &str) -> Option<i64> {
    let re = regex::Regex::new(r"([0-9]{1,3}(?:\.[0-9]+)?)\s*%").ok()?;
    let capture = re.captures(line)?;
    let raw_value: f64 = capture.get(1)?.as_str().parse().ok()?;
    if !raw_value.is_finite() {
        return None;
    }
    let clamped = raw_value.clamp(0.0, 100.0);
    let lower = line.to_lowercase();
    if lower.contains("remaining") || lower.contains("left") || lower.contains("available") {
        let inverted = 100.0 - clamped;
        return Some((inverted.round() as i64).clamp(0, 100));
    }
    Some(clamped.round() as i64)
}

/// 判断一行是否为配额标签（复刻 Node `isQuotaLabel`）。
fn claude_is_quota_label(line: &str) -> bool {
    let normalized = claude_normalize_for_label_search(line);
    matches!(
        normalized.as_str(),
        "currentsession"
            | "currentweekallmodels"
            | "currentweeksonnetonly"
            | "currentweeksonnet"
            | "currentweekopusonly"
            | "currentweekopus"
            | "extrausage"
    )
}

/// 规范化配额标签（复刻 Node `canonicalQuotaLabel`）。
fn claude_canonical_quota_label(line: &str) -> String {
    match claude_normalize_for_label_search(line).as_str() {
        "currentsession" => "Current session".to_owned(),
        "currentweekallmodels" => "Current week (all models)".to_owned(),
        "currentweeksonnetonly" | "currentweeksonnet" => "Current week (Sonnet only)".to_owned(),
        "currentweekopusonly" | "currentweekopus" => "Current week (Opus only)".to_owned(),
        "extrausage" => "Extra usage".to_owned(),
        _ => line.to_owned(),
    }
}

/// 格式化 CLI 配额详情行（复刻 Node `formatClaudeCliDetail`）。
fn claude_format_cli_detail(label: &str, lines: &[String]) -> Option<String> {
    let normalized = claude_normalize_for_label_search(label);
    if normalized == "extrausage" {
        let compact: String = lines
            .join(" ")
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .to_lowercase();
        if compact.contains("extrausagenotenabled") {
            return Some("Extra usage not enabled \u{2022} /extra-usage to enable".to_owned());
        }
        return lines.iter().find(|line| !line.trim().is_empty()).cloned();
    }
    let reset_line = lines
        .iter()
        .find(|line| line.starts_with("resets") || claude_normalize_for_label_search(line).starts_with("resets"));
    let reset_line = reset_line?;
    let mut detail = reset_line
        .replacen("Resets", "Resets ", 1)
        .replace("Resets", "Resets ");
    // 复刻 Node：在月份缩写与数字之间、数字与 at 之间、am/pm 与 ( 之间加空格
    let mut spaced = String::with_capacity(detail.len());
    let chars: Vec<char> = detail.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        spaced.push(*c);
        let next = chars.get(i + 1).copied();
        if let Some(next) = next {
            let is_month_digit = c.is_ascii_alphabetic() && next.is_ascii_digit();
            let is_digit_at = c.is_ascii_digit() && (next == 'a' || next == 't');
            let is_ampm_paren = (*c == 'm' || *c == 'M') && next == '(';
            let is_letter_paren = c.is_ascii_alphabetic() && next == '(';
            if is_month_digit || is_digit_at || is_ampm_paren || is_letter_paren {
                spaced.push(' ');
            }
        }
    }
    detail = spaced;
    detail = detail.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(detail.trim().to_owned())
}

/// 解析 Claude CLI usage 面板文本为 `QuotaWindow` 列表。
///
/// 复刻 Node `parseClaudeCliUsageText`：先截取最新 Settings 面板，
/// 按标签切段，每段提取百分比与详情；必须包含 "Current session"。
pub fn parse_claude_cli_usage_text(text: &str) -> Result<Vec<QuotaWindow>, String> {
    let text = text.replace('\0', "");
    let cleaned = claude_clean_terminal_text(&text);
    let cleaned = claude_trim_to_latest_usage_panel(&cleaned).unwrap_or(cleaned);
    if let Some(usage_error) = claude_extract_usage_error(&cleaned) {
        return Err(usage_error);
    }

    let lines: Vec<String> = cleaned
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();

    let mut sections: Vec<(String, Vec<String>)> = Vec::new();
    let mut current: Option<(String, Vec<String>)> = None;
    for line in lines {
        if claude_is_quota_label(&line) {
            if let Some(section) = current.take() {
                sections.push(section);
            }
            current = Some((claude_canonical_quota_label(&line), Vec::new()));
            continue;
        }
        if let Some(section) = current.as_mut() {
            section.1.push(line);
        }
    }
    if let Some(section) = current {
        sections.push(section);
    }

    let windows = sections
        .into_iter()
        .map(|(label, lines)| {
            let used_percent = lines.iter().find_map(|line| claude_percent_from_line(line));
            let detail = claude_format_cli_detail(&label, &lines);
            QuotaWindow {
                label,
                used_percent,
                resets_at: None,
                value_label: None,
                detail,
            }
        })
        .collect::<Vec<_>>();

    if !windows
        .iter()
        .any(|window| claude_normalize_for_label_search(&window.label) == "currentsession")
    {
        return Err("Could not parse Claude CLI usage output.".to_owned());
    }
    Ok(windows)
}

// ---------------------------------------------------------------------------
// 真实探测层（R432）：复刻 Node codex/claude quota.ts 的 getQuotaWindows
// ---------------------------------------------------------------------------

/// Codex home 目录（`$CODEX_HOME` 或 `~/.codex`）。
pub fn codex_home_dir() -> std::path::PathBuf {
    if let Some(from_env) = std::env::var_os("CODEX_HOME") {
        let value = from_env.to_string_lossy().trim().to_owned();
        if !value.is_empty() {
            return std::path::PathBuf::from(value);
        }
    }
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
        .join(".codex")
}

/// Claude 配置目录（`$CLAUDE_CONFIG_DIR` 或 `~/.claude`）。
pub fn claude_config_dir() -> std::path::PathBuf {
    if let Some(from_env) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        let value = from_env.to_string_lossy().trim().to_owned();
        if !value.is_empty() {
            return std::path::PathBuf::from(value);
        }
    }
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
        .join(".claude")
}

/// 读取 Codex `auth.json` 并解析为 `CodexAuthInfo`（复刻 Node `readCodexAuthInfo`）。
pub fn read_codex_auth_info() -> Option<CodexAuthInfo> {
    let auth_path = codex_home_dir().join("auth.json");
    let raw = std::fs::read_to_string(auth_path).ok()?;
    parse_codex_auth_json(&raw)
}

/// 读取 Claude OAuth access token（`.credentials.json` / `credentials.json`）。
pub fn read_claude_token() -> Option<String> {
    for filename in [".credentials.json", "credentials.json"] {
        let path = claude_config_dir().join(filename);
        let raw = std::fs::read_to_string(path).ok()?;
        let parsed: Value = serde_json::from_str(&raw).ok()?;
        let oauth = parsed.get("claudeAiOauth")?;
        let token = oauth.get("accessToken").and_then(Value::as_str)?;
        if !token.trim().is_empty() {
            return Some(token.trim().to_owned());
        }
    }
    None
}

/// 通过 WHAM API 拉取 Codex 配额（复刻 Node `fetchCodexQuota`）。
pub async fn fetch_codex_wham_quota(
    token: &str,
    account_id: Option<&str>,
) -> Result<Vec<QuotaWindow>, String> {
    let mut headers = BTreeMap::new();
    headers.insert("Authorization".to_owned(), format!("Bearer {token}"));
    if let Some(account_id) = account_id.filter(|s| !s.trim().is_empty()) {
        headers.insert("ChatGPT-Account-Id".to_owned(), account_id.to_owned());
    }
    let (status, body_text) = DefaultQuotaIo
        .get(WHAM_USAGE_URL, &headers, Duration::from_secs(8))
        .await
        .map_err(|e| format!("chatgpt wham api error: {e}"))?;
    if status != 200 {
        let message = format!("chatgpt wham api returned {status}");
        let body_prefix = truncate_body(&body_text, MAX_QUOTA_ERROR_BODY_BYTES);
        let combined = [message.clone(), body_prefix].join("\n");
        let family = classify_quota_error_family(&combined);
        return match family {
            Some(family) => Err(format!("{}|family={}", message, family.as_str())),
            None => Err(message),
        };
    }
    let body: Value = serde_json::from_str(&body_text).map_err(|e| e.to_string())?;
    Ok(map_wham_usage(&body))
}

/// 通过 Claude OAuth API 拉取配额（复刻 Node `fetchClaudeQuota`）。
pub async fn fetch_claude_oauth_quota(token: &str) -> Result<Vec<QuotaWindow>, String> {
    let mut headers = BTreeMap::new();
    headers.insert("Authorization".to_owned(), format!("Bearer {token}"));
    headers.insert("anthropic-beta".to_owned(), ANTHROPIC_OAUTH_BETA_HEADER.to_owned());
    let (status, body_text) = DefaultQuotaIo
        .get(ANTHROPIC_OAUTH_USAGE_URL, &headers, Duration::from_secs(8))
        .await
        .map_err(|e| format!("anthropic usage api error: {e}"))?;
    if status != 200 {
        return Err(format!("anthropic usage api returned {status}"));
    }
    let body: Value = serde_json::from_str(&body_text).map_err(|e| e.to_string())?;
    Ok(map_anthropic_oauth_usage(&body))
}

/// 探测 Codex 本地配额（复刻 Node `getQuotaWindows`：RPC 优先 → WHAM 回退）。
pub async fn probe_codex_local() -> ProviderQuotaResult {
    get_quota_windows(
        fetch_codex_rpc_quota().await,
        read_codex_auth_info(),
        {
            let auth = read_codex_auth_info();
            match &auth {
                Some(auth) => fetch_codex_wham_quota(&auth.access_token, auth.account_id.as_deref()).await,
                None => Err("no local codex auth token".to_owned()),
            }
        },
    )
    .await
}

/// 探测 Claude 本地配额（复刻 Node `getQuotaWindows`）。
///
/// - Bedrock 环境变量命中 → `source=bedrock, ok=true` 空窗口；
/// - OAuth token 存在 → 优先 OAuth API；
/// - 否则回退 `claude auth status` + CLI `/usage` 文本探测。
pub async fn probe_claude_local() -> ProviderQuotaResult {
    if claude_bedrock_env_active() {
        return ProviderQuotaResult {
            provider: "anthropic".to_owned(),
            source: Some("bedrock".to_owned()),
            ok: true,
            error_family: None,
            error: None,
            windows: Vec::new(),
        };
    }

    let auth_status = read_claude_auth_status().await;
    let auth_description = describe_claude_subscription_auth(auth_status.as_ref());
    let token = read_claude_token();
    let mut errors: Vec<String> = Vec::new();

    if let Some(token) = token.as_deref() {
        match fetch_claude_oauth_quota(token).await {
            Ok(windows) => {
                return ProviderQuotaResult {
                    provider: "anthropic".to_owned(),
                    source: Some(CLAUDE_USAGE_SOURCE_OAUTH.to_owned()),
                    ok: true,
                    error_family: None,
                    error: None,
                    windows,
                };
            }
            Err(error) => errors.push(format!("Anthropic OAuth usage: {error}")),
        }
    }

    match capture_claude_cli_quota().await {
        Ok(windows) => {
            return ProviderQuotaResult {
                provider: "anthropic".to_owned(),
                source: Some(CLAUDE_USAGE_SOURCE_CLI.to_owned()),
                ok: true,
                error_family: None,
                error: None,
                windows,
            };
        }
        Err(error) => errors.push(format!("Claude CLI /usage: {error}")),
    }

    let has_anthropic_api_key = std::env::var("ANTHROPIC_API_KEY")
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);

    if has_anthropic_api_key && auth_description.is_none() {
        return ProviderQuotaResult {
            provider: "anthropic".to_owned(),
            ok: false,
            source: None,
            error_family: None,
            error: Some(
                errors
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "ANTHROPIC_API_KEY is set and no local Claude subscription session is available for quota polling".to_owned()),
            ),
            windows: Vec::new(),
        };
    }

    if let Some(auth_description) = auth_description {
        return ProviderQuotaResult {
            provider: "anthropic".to_owned(),
            ok: false,
            source: None,
            error_family: None,
            error: Some(if !errors.is_empty() {
                format!("{auth_description}, but quota polling failed ({})", errors.join("; "))
            } else {
                format!("{auth_description}, but Paperclip could not load subscription quota data")
            }),
            windows: Vec::new(),
        };
    }

    ProviderQuotaResult {
        provider: "anthropic".to_owned(),
        ok: false,
        source: None,
        error_family: None,
        error: Some(errors.first().cloned().unwrap_or_else(|| "no local claude auth token".to_owned())),
        windows: Vec::new(),
    }
}

/// 判断是否命中 Bedrock 环境（`CLAUDE_CODE_USE_BEDROCK` 或 `ANTHROPIC_BEDROCK_BASE_URL`）。
fn claude_bedrock_env_active() -> bool {
    let bedrock_flag = std::env::var("CLAUDE_CODE_USE_BEDROCK")
        .map(|value| matches!(value.trim(), "1" | "true"))
        .unwrap_or(false);
    let bedrock_url = std::env::var("ANTHROPIC_BEDROCK_BASE_URL")
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    bedrock_flag || bedrock_url
}

/// 读取 `claude auth status` 输出（复刻 Node `readClaudeAuthStatus`）。
async fn read_claude_auth_status() -> Option<ClaudeAuthStatus> {
    let output = tokio::process::Command::new("claude")
        .args(["auth", "status"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Value = serde_json::from_str(stdout.trim()).ok()?;
    Some(ClaudeAuthStatus {
        logged_in: parsed.get("loggedIn").and_then(Value::as_bool).unwrap_or(false),
        auth_method: parsed
            .get("authMethod")
            .and_then(Value::as_str)
            .map(str::to_owned),
        subscription_type: parsed
            .get("subscriptionType")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

/// Claude auth 状态（复刻 Node `ClaudeAuthStatus`）。
struct ClaudeAuthStatus {
    logged_in: bool,
    auth_method: Option<String>,
    subscription_type: Option<String>,
}

/// 描述 Claude 订阅登录状态（复刻 Node `describeClaudeSubscriptionAuth`）。
fn describe_claude_subscription_auth(status: Option<&ClaudeAuthStatus>) -> Option<String> {
    let status = status?;
    if !status.logged_in || status.auth_method.as_deref() != Some("claude.ai") {
        return None;
    }
    Some(match status.subscription_type.as_deref() {
        Some(subscription) => format!("Claude is logged in via claude.ai ({subscription})"),
        None => "Claude is logged in via claude.ai".to_owned(),
    })
}

/// 通过 CLI `/usage` 探测 Claude 配额（复刻 Node `captureClaudeCliUsageText` + `fetchClaudeCliQuota`）。
///
/// 真实运行会启动 `claude` 交互式 shell（`script -q`），耗时约 10s；
/// 此处复刻命令构建与输出解析，但探测本身保持可注入、可测试。
pub async fn capture_claude_cli_quota() -> Result<Vec<QuotaWindow>, String> {
    let command = build_claude_cli_shell_probe_command();
    let output = tokio::time::timeout(
        Duration::from_secs(CLAUDE_CLI_PROBE_TIMEOUT_SECS),
        tokio::process::Command::new("sh")
            .arg("-c")
            .arg(&command)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output(),
    )
    .await
    .map_err(|_| format!("Claude CLI usage probe timed out after {CLAUDE_CLI_PROBE_TIMEOUT_SECS}s"))?
    .map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let raw_text = format!("{stdout}{stderr}");
    // 先彻底移除 NUL（script 输出可能混入），避免 regex 报错。
    let raw_text = raw_text.replace('\0', "");
    let cleaned = claude_clean_terminal_text(&raw_text);
    if claude_usage_output_looks_complete(&cleaned) {
        return parse_claude_cli_usage_text(&raw_text);
    }
    if claude_usage_output_looks_relevant(&cleaned) {
        return Err("Claude CLI usage probe ended before rendering usage.".to_owned());
    }
    Err("Claude CLI usage probe failed to produce usage output.".to_owned())
}

/// 构建 Claude CLI 探测 shell 命令（复刻 Node `buildClaudeCliShellProbeCommand`）。
pub fn build_claude_cli_shell_probe_command() -> String {
    let feed = "(sleep 2; printf '/usage\r'; sleep 6; printf '\\033'; sleep 1; printf '\\003')";
    let claude_command = "claude --tools \"\"";
    if cfg!(target_os = "macos") {
        format!("{feed} | script -q /dev/null {claude_command}")
    } else {
        let quoted = quote_for_shell(claude_command);
        format!("{feed} | script -q -e -f -c {quoted} /dev/null")
    }
}

/// shell 单引号转义（复刻 Node `quoteForShell`）。
fn quote_for_shell(value: &str) -> String {
    let escaped = value.replace("\'", "'\\\''");
    format!("'{escaped}'")
}

/// usage 输出是否看起来相关（复刻 Node `usageOutputLooksRelevant`）。
fn claude_usage_output_looks_relevant(text: &str) -> bool {
    let normalized = claude_normalize_for_label_search(text);
    ["currentsession", "currentweek", "loadingusage", "failedtoloadusagedata", "tokenexpired", "authenticationerror", "ratelimited"]
        .iter()
        .any(|keyword| normalized.contains(keyword))
}

/// usage 输出是否完整（复刻 Node `usageOutputLooksComplete`）。
///
/// 注意：先经 `claude_clean_terminal_text` 清洗（去 ANSI/NUL），
/// 避免 regex 对含 NUL 的原始文本报错。
fn claude_usage_output_looks_complete(text: &str) -> bool {
    let cleaned = claude_clean_terminal_text(text);
    let normalized = claude_normalize_for_label_search(&cleaned);
    if ["failedtoloadusagedata", "tokenexpired", "authenticationerror", "ratelimited"]
        .iter()
        .any(|keyword| normalized.contains(keyword))
    {
        return true;
    }
    normalized.contains("currentsession")
        && (normalized.contains("currentweek") || normalized.contains("extrausage"))
        && regex::Regex::new(r"[0-9]{1,3}(?:\.[0-9]+)?%")
            .ok()
            .map(|re| re.is_match(&cleaned))
            .unwrap_or(false)
}

const CLAUDE_USAGE_SOURCE_OAUTH: &str = "anthropic-oauth";
const CLAUDE_CLI_PROBE_TIMEOUT_SECS: u64 = 12;
const CLAUDE_USAGE_SOURCE_CLI: &str = "claude-cli";

// ---------------------------------------------------------------------------
// 跨 provider 配额聚合（R432）：复刻 Node `server/src/services/quota-windows.ts`
// ---------------------------------------------------------------------------

/// 单 provider 配额探测的聚合超时（毫秒）。
pub const QUOTA_PROVIDER_TIMEOUT_MS: u64 = 20_000;

/// provider slug 映射（复刻 Node `providerSlugForAdapterType`）。
pub fn provider_slug_for_adapter_type(adapter_type: &str) -> String {
    match adapter_type {
        "claude_local" => "anthropic".to_owned(),
        "codex_local" => "openai".to_owned(),
        other => other.to_owned(),
    }
}

/// 聚合各 provider 的配额探测结果。
///
/// 复刻 Node `fetchAllQuotaWindows`：
/// - 每个 provider 的任务以 20s 超时包裹，超时返回 `ok=false` 的错误结果；
/// - 单个 provider 失败不会阻塞整体响应；
/// - 无 provider 时返回空列表。
pub async fn fetch_all_quota_windows(
    tasks: Vec<(
        String,
        std::pin::Pin<Box<dyn std::future::Future<Output = ProviderQuotaResult> + Send>>,
    )>,
) -> Vec<ProviderQuotaResult> {
    let mut results = Vec::with_capacity(tasks.len());
    for (adapter_type, task) in tasks {
        let provider = provider_slug_for_adapter_type(&adapter_type);
        let result = tokio::time::timeout(Duration::from_millis(QUOTA_PROVIDER_TIMEOUT_MS), task).await;
        let result = match result {
            Ok(result) => result,
            Err(_) => ProviderQuotaResult {
                provider,
                source: None,
                ok: false,
                error_family: None,
                error: Some(format!(
                    "quota polling timed out after {}s",
                    QUOTA_PROVIDER_TIMEOUT_MS / 1000
                )),
                windows: Vec::new(),
            },
        };
        results.push(result);
    }
    results
}

// ---------------------------------------------------------------------------
// Codex app-server RPC 客户端
// ---------------------------------------------------------------------------

/// 通过 `codex app-server` JSON-Lines 协议读取配额。
///
/// 复刻 Node `CodexRpcClient`：spawn `codex -s read-only -a untrusted app-server`，
/// 维护自增 id 的 pending map，逐行解析 stdout 中的 `{"id":..., "result":...}`。
pub struct CodexRpcClient {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: tokio::io::BufReader<tokio::process::ChildStdout>,
    next_id: u64,
    stderr_buffer: String,
}

/// RPC 错误（含 app-server 退出/超时）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcError {
    pub message: String,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RpcError {}

/// 单次 RPC 请求-响应。
async fn rpc_roundtrip(
    stdin: &mut tokio::process::ChildStdin,
    stdout: &mut tokio::io::BufReader<tokio::process::ChildStdout>,
    stderr: &mut String,
    next_id: &mut u64,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value, RpcError> {
    let id = *next_id;
    *next_id += 1;
    let payload = serde_json::json!({ "id": id, "method": method, "params": params });
    let mut line = serde_json::to_string(&payload).map_err(|e| RpcError { message: e.to_string() })?;
    line.push('\n');
    stdin
        .write_all(line.as_bytes())
        .await
        .map_err(|e| RpcError { message: format!("write request: {e}") })?;
    stdin
        .flush()
        .await
        .map_err(|e| RpcError { message: format!("flush request: {e}") })?;

    // 读取直到匹配 id（处理可能的 notifications / 其他响应）。
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(RpcError {
                message: format!("codex app-server timed out on {method}"),
            });
        }
        let read_timeout = deadline - tokio::time::Instant::now();
        let line_result = tokio::time::timeout(read_timeout, read_line(stdout)).await;
        let current_line = match line_result {
            Err(_) => {
                return Err(RpcError {
                    message: format!("codex app-server timed out on {method}"),
                })
            }
            Ok(Err(e)) => {
                return Err(RpcError {
                    message: format!("read stdout: {e}"),
                })
            }
            Ok(Ok(None)) => {
                let tail = stderr.trim();
                let message = if tail.is_empty() {
                    "codex app-server closed unexpectedly".to_owned()
                } else {
                    tail.to_owned()
                };
                return Err(RpcError { message });
            }
            Ok(Ok(Some(line))) => line,
        };
        if current_line.trim().is_empty() {
            continue;
        }
        let parsed: Value = match serde_json::from_str(&current_line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if parsed.get("id").and_then(Value::as_u64) == Some(id) {
            if let Some(result) = parsed.get("result") {
                return Ok(result.clone());
            }
            if let Some(error) = parsed.get("error") {
                return Err(RpcError {
                    message: format!("codex app-server error: {error}"),
                });
            }
            return Ok(Value::Null);
        }
    }
}

/// 从 stdout 读取一行（阻塞直到换行或 EOF）。
async fn read_line(
    reader: &mut tokio::io::BufReader<tokio::process::ChildStdout>,
) -> std::io::Result<Option<String>> {
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        Ok(None)
    } else {
        Ok(Some(line))
    }
}

impl CodexRpcClient {
    /// 启动 `codex app-server`。
    pub fn spawn() -> Result<Self, RpcError> {
        let mut command = tokio::process::Command::new("codex");
        command
            .args(["-s", "read-only", "-a", "untrusted", "app-server"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|e| RpcError { message: format!("spawn codex app-server: {e}") })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| RpcError { message: "app-server stdin unavailable".to_owned() })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| RpcError { message: "app-server stdout unavailable".to_owned() })?;
        Ok(Self {
            child,
            stdin,
            stdout: tokio::io::BufReader::new(stdout),
            next_id: 1,
            stderr_buffer: String::new(),
        })
    }

    /// 发送 `initialize` 并 notify `initialized`。
    pub async fn initialize(&mut self) -> Result<(), RpcError> {
        let response = rpc_roundtrip(
            &mut self.stdin,
            &mut self.stdout,
            &mut self.stderr_buffer,
            &mut self.next_id,
            "initialize",
            serde_json::json!({
                "clientInfo": { "name": "paperclip", "version": "0.0.0" }
            }),
            Duration::from_secs(6),
        )
        .await?;
        let _ = response;
        // notify initialized（无响应）
        let payload = serde_json::json!({ "method": "initialized", "params": {} });
        let mut line = serde_json::to_string(&payload).map_err(|e| RpcError { message: e.to_string() })?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| RpcError { message: format!("notify initialized: {e}") })?;
        self.stdin
            .flush()
            .await
            .map_err(|e| RpcError { message: format!("flush notify: {e}") })?;
        Ok(())
    }

    /// 读取 rate limits。
    pub async fn fetch_rate_limits(&mut self) -> Result<Value, RpcError> {
        rpc_roundtrip(
            &mut self.stdin,
            &mut self.stdout,
            &mut self.stderr_buffer,
            &mut self.next_id,
            "account/rateLimits/read",
            serde_json::json!({}),
            Duration::from_secs(6),
        )
        .await
    }

    /// 读取 account（失败返回 Ok(None)，对齐 Node）。
    pub async fn fetch_account(&mut self) -> Result<Value, RpcError> {
        rpc_roundtrip(
            &mut self.stdin,
            &mut self.stdout,
            &mut self.stderr_buffer,
            &mut self.next_id,
            "account/read",
            serde_json::json!({}),
            Duration::from_secs(6),
        )
        .await
    }

    /// 终止 app-server。
    pub async fn shutdown(&mut self) {
        let _ = self.stdin.shutdown().await;
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }
}

/// 一次性 RPC 配额读取：启动 → initialize → 并行读 limits/account → 关闭。
pub async fn fetch_codex_rpc_quota() -> Result<CodexRpcQuotaSnapshot, RpcError> {
    let mut client = CodexRpcClient::spawn()?;
    let result = async {
        client.initialize().await?;
        let limits = client.fetch_rate_limits().await?;
        let account = client.fetch_account().await?;
        Ok(map_codex_rpc_quota(&limits, Some(&account)))
    }
    .await;
    client.shutdown().await;
    result
}

/// 组合 RPC 优先 → WHAM 回退 → ProviderQuotaResult。
///
/// 复刻 Node `getQuotaWindows()`：
/// - RPC 有窗口 → 返回 `source=codex-rpc, ok=true`；
/// - RPC 失败 → 尝试 auth.json + WHAM；
/// - WHAM 失败且分类出 auth 错误族 → `source=codex-wham, ok=false, errorFamily=...`；
/// - 最终无错误族 → `ok=false, error=...`。
pub async fn get_quota_windows(
    rpc_snapshot: Result<CodexRpcQuotaSnapshot, RpcError>,
    auth: Option<CodexAuthInfo>,
    wham_windows: Result<Vec<QuotaWindow>, String>,
) -> ProviderQuotaResult {
    let mut errors: Vec<String> = Vec::new();
    let mut rpc_error_family: Option<CodexQuotaErrorFamily> = None;

    match rpc_snapshot {
        Ok(snapshot) if !snapshot.windows.is_empty() => {
            return ProviderQuotaResult {
                provider: "openai".to_owned(),
                source: Some(CODEX_USAGE_SOURCE_RPC.to_owned()),
                ok: true,
                error_family: None,
                error: None,
                windows: snapshot.windows,
            };
        }
        Ok(_) => {}
        Err(error) => {
            errors.push(format!("Codex app-server: {}", error.message));
            if let Some(family) = classify_quota_error_family(&error.message) {
                rpc_error_family = Some(family);
            }
        }
    }

    if let Some(_auth) = auth {
        match wham_windows {
            Ok(windows) => {
                return ProviderQuotaResult {
                    provider: "openai".to_owned(),
                    source: Some(CODEX_USAGE_SOURCE_WHAM.to_owned()),
                    ok: true,
                    error_family: None,
                    error: None,
                    windows,
                };
            }
            Err(error) => {
                errors.push(format!("ChatGPT WHAM usage: {error}"));
                if let Some(family) = classify_quota_error_family(&error) {
                    return ProviderQuotaResult {
                        provider: "openai".to_owned(),
                        source: Some(CODEX_USAGE_SOURCE_WHAM.to_owned()),
                        ok: false,
                        error_family: Some(family.as_str().to_owned()),
                        error: Some(errors.join("; ")),
                        windows: Vec::new(),
                    };
                }
            }
        }
    } else {
        errors.push("no local codex auth token".to_owned());
    }

    let mut result = ProviderQuotaResult {
        provider: "openai".to_owned(),
        ok: false,
        error: Some(errors.join("; ")),
        windows: Vec::new(),
        source: None,
        error_family: None,
    };
    if let Some(family) = rpc_error_family {
        result.source = Some(CODEX_USAGE_SOURCE_RPC.to_owned());
        result.error_family = Some(family.as_str().to_owned());
    }
    result
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn base64_url_decode_works() {
        // "hello" base64url
        assert_eq!(base64_url_decode("aGVsbG8"), Some("hello".to_owned()));
        assert_eq!(base64_url_decode("!!!"), None);
    }

    #[test]
    fn decode_jwt_payload_extracts_email_and_plan() {
        // header.payload.signature，payload 为 {"email":"a@b.c","https://api.openai.com/auth":{"chatgpt_plan_type":"plus"}}
        let payload = r#"{"email":"a@b.c","https://api.openai.com/auth":{"chatgpt_plan_type":"plus"}}"#;
        let encoded = base64_url_encode(payload.as_bytes());
        let token = format!("x.{encoded}.sig");
        let decoded = decode_jwt_payload(Some(&token)).unwrap();
        assert_eq!(decoded["email"], "a@b.c");
        assert_eq!(decoded["https://api.openai.com/auth"]["chatgpt_plan_type"], "plus");
    }

    fn base64_url_encode(bytes: &[u8]) -> String {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;
        URL_SAFE_NO_PAD.encode(bytes)
    }

    #[test]
    fn parse_auth_modern_and_legacy() {
        let modern = parse_codex_auth_json(
            r#"{"tokens":{"access_token":"at-1","refresh_token":"rt-1","account_id":"acc-1"},"last_refresh":"2026-01-01"}"#,
        )
        .unwrap();
        assert_eq!(modern.access_token, "at-1");
        assert_eq!(modern.account_id.as_deref(), Some("acc-1"));
        assert_eq!(modern.refresh_token.as_deref(), Some("rt-1"));

        let legacy = parse_codex_auth_json(r#"{"accessToken":"at-legacy","accountId":"acc-legacy"}"#).unwrap();
        assert_eq!(legacy.access_token, "at-legacy");
        assert_eq!(legacy.account_id.as_deref(), Some("acc-legacy"));
        assert!(legacy.refresh_token.is_none());
    }

    #[test]
    fn parse_auth_missing_token_returns_none() {
        assert!(parse_codex_auth_json(r#"{"tokens":{}}"#).is_none());
        assert!(parse_codex_auth_json("not json").is_none());
    }

    #[test]
    fn normalize_percent_handles_fraction_and_over_100() {
        assert_eq!(normalize_codex_used_percent(Some(0.5)), Some(50));
        assert_eq!(normalize_codex_used_percent(Some(150.0)), Some(100));
        assert_eq!(normalize_codex_used_percent(None), None);
    }

    #[test]
    fn unix_seconds_to_iso_returns_rfc3339_millis() {
        assert_eq!(
            unix_seconds_to_iso(Some(1_711_111_111)),
            Some("2024-03-22T12:38:31.000Z".to_owned())
        );
        assert_eq!(unix_seconds_to_iso(None), None);
    }

    #[test]
    fn seconds_to_label_buckets() {
        assert_eq!(seconds_to_window_label(Some(3600.0), "?"), "5h");
        assert_eq!(seconds_to_window_label(Some(86400.0), "?"), "24h");
        assert_eq!(seconds_to_window_label(Some(7.0 * 86400.0), "?"), "7d");
        assert_eq!(seconds_to_window_label(Some(30.0 * 86400.0), "?"), "30d");
        assert_eq!(seconds_to_window_label(None, "fallback"), "fallback");
    }

    #[test]
    fn map_wham_usage_produces_windows() {
        let body = json!({
            "rate_limit": {
                "primary_window": {"used_percent": 0.5, "reset_at": 1_711_111_111},
                "secondary_window": {"used_percent": 45, "reset_at": null}
            },
            "credits": {"balance": 420, "unlimited": false}
        });
        let windows = map_wham_usage(&body);
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[0].label, "5h limit");
        assert_eq!(windows[0].used_percent, Some(50));
        assert_eq!(windows[0].resets_at.as_deref(), Some("2024-03-22T12:38:31.000Z"));
        assert_eq!(windows[2].value_label.as_deref(), Some("$4.20 remaining"));
    }

    #[test]
    fn map_wham_usage_skips_unlimited_credits() {
        let body = json!({
            "credits": {"balance": 100, "unlimited": true}
        });
        assert!(map_wham_usage(&body).is_empty());
    }

    #[test]
    fn map_rpc_quota_prefers_codex_limit_and_account_email() {
        let limits = json!({
            "rateLimitsByLimitId": {
                "codex": {
                    "limitId": "codex",
                    "primary": {"usedPercent": 12, "resetsAt": 1_711_111_111},
                    "credits": {"balance": "3.50", "unlimited": false}
                },
                "sonnet": {
                    "limitName": "Sonnet",
                    "primary": {"usedPercent": 88, "resetsAt": null}
                }
            }
        });
        let account = json!({
            "account": {"email": "u@x.com", "planType": "plus"}
        });
        let snapshot = map_codex_rpc_quota(&limits, Some(&account));
        assert_eq!(snapshot.windows.len(), 3);
        assert_eq!(snapshot.windows[0].label, "5h limit");
        assert_eq!(snapshot.windows[0].used_percent, Some(12));
        // codex 优先（含 Credits），sonnet 随后。
        assert_eq!(snapshot.windows[1].label, "Credits");
        assert_eq!(snapshot.windows[1].value_label.as_deref(), Some("$3.50 remaining"));
        assert_eq!(snapshot.windows[2].label, "Sonnet · 5h limit");
        assert_eq!(snapshot.windows[2].used_percent, Some(88));
        assert_eq!(snapshot.email.as_deref(), Some("u@x.com"));
        assert_eq!(snapshot.plan_type.as_deref(), Some("plus"));
    }

    #[test]
    fn classify_error_family_matches_refresh_token() {
        assert_eq!(
            classify_quota_error_family("OAuth failed: refresh token has expired"),
            Some(CodexQuotaErrorFamily::RefreshTokenExpired)
        );
        assert_eq!(
            classify_quota_error_family("OAuth failed: invalid_grant"),
            Some(CodexQuotaErrorFamily::RefreshTokenInvalidated)
        );
        assert_eq!(classify_quota_error_family("plain 401"), None);
    }

    #[test]
    fn truncate_body_limits_bytes() {
        let raw = "a".repeat(100);
        assert_eq!(truncate_body(&raw, 10).len(), 10);
        assert_eq!(truncate_body(&raw, 0), "");
    }

    // -----------------------------------------------------------------
    // get_quota_windows 组合层
    // -----------------------------------------------------------------

    fn fake_auth() -> CodexAuthInfo {
        CodexAuthInfo {
            access_token: "at-1".to_owned(),
            account_id: Some("acc-1".to_owned()),
            refresh_token: None,
            id_token: None,
            email: None,
            plan_type: None,
            last_refresh: None,
        }
    }

    #[tokio::test]
    async fn quota_rpc_windows_win_without_wham() {
        let limits = json!({
            "rateLimitsByLimitId": {
                "codex": {"limitId": "codex", "primary": {"usedPercent": 5, "resetsAt": null}}
            }
        });
        let snapshot = map_codex_rpc_quota(&limits, None);
        let result = get_quota_windows(Ok(snapshot), None, Err("should not be called".to_owned())).await;
        assert_eq!(result.ok, true);
        assert_eq!(result.source.as_deref(), Some("codex-rpc"));
        assert_eq!(result.windows.len(), 1);
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn quota_rpc_fails_then_wham_wins() {
        let rpc_err = RpcError { message: "spawn codex ENOENT".to_owned() };
        let wham = vec![QuotaWindow {
            label: "5h limit".to_owned(),
            used_percent: Some(50),
            resets_at: None,
            value_label: None,
            detail: None,
        }];
        let result = get_quota_windows(Err(rpc_err), Some(fake_auth()), Ok(wham)).await;
        assert_eq!(result.ok, true);
        assert_eq!(result.source.as_deref(), Some("codex-wham"));
        assert_eq!(result.windows.len(), 1);
        assert_eq!(result.windows[0].used_percent, Some(50));
    }

    #[tokio::test]
    async fn quota_wham_auth_error_family() {
        let rpc_err = RpcError { message: "spawn codex ENOENT".to_owned() };
        let wham_err = "chatgpt wham api returned 401\nOAuth failed: invalid_grant".to_owned();
        let result = get_quota_windows(Err(rpc_err), Some(fake_auth()), Err(wham_err)).await;
        assert_eq!(result.ok, false);
        assert_eq!(result.source.as_deref(), Some("codex-wham"));
        assert_eq!(
            result.error_family.as_deref(),
            Some("refresh_token_invalidated")
        );
        assert!(result.error.unwrap_or_default().contains("ChatGPT WHAM usage"));
    }

    #[tokio::test]
    async fn quota_rpc_auth_family_survives_when_no_token() {
        let rpc_err = RpcError { message: "OAuth failed: refresh token has expired".to_owned() };
        let result = get_quota_windows(Err(rpc_err), None, Err("unused".to_owned())).await;
        assert_eq!(result.ok, false);
        assert_eq!(result.source.as_deref(), Some("codex-rpc"));
        assert_eq!(result.error_family.as_deref(), Some("refresh_token_expired"));
        assert!(result.error.unwrap_or_default().contains("no local codex auth token"));
    }

    // -----------------------------------------------------------------
    // Claude OAuth / CLI 解析（R432）
    // -----------------------------------------------------------------

    #[test]
    fn claude_to_percent_normalizes_fraction_and_percent() {
        assert_eq!(claude_to_percent(0.5), Some(50));
        assert_eq!(claude_to_percent(45.0), Some(45));
        assert_eq!(claude_to_percent(150.0), Some(100));
        assert_eq!(claude_to_percent(f64::NAN), None);
    }

    #[test]
    fn format_currency_amount_rounds_cents() {
        assert_eq!(format_currency_amount(4.206, None), "$4.21");
        assert_eq!(format_currency_amount(1234.5, None), "$1,234.50");
        assert_eq!(format_currency_amount(0.0, Some("eur")), "0.00 EUR");
    }

    #[test]
    fn map_anthropic_oauth_usage_produces_windows() {
        let body = json!({
            "five_hour": {"utilization": 0.5, "resets_at": "2026-01-01T00:00:00Z"},
            "seven_day": {"utilization": 45, "resets_at": null},
            "seven_day_sonnet": {"utilization": 12},
            "seven_day_opus": {"utilization": null},
            "extra_usage": {
                "is_enabled": true,
                "monthly_limit": 1000,
                "used_credits": 420,
                "utilization": 42,
                "currency": "USD"
            }
        });
        let windows = map_anthropic_oauth_usage(&body);
        assert_eq!(windows.len(), 5);
        assert_eq!(windows[0].label, "Current session");
        assert_eq!(windows[0].used_percent, Some(50));
        assert_eq!(windows[0].resets_at.as_deref(), Some("2026-01-01T00:00:00Z"));
        assert_eq!(windows[1].label, "Current week (all models)");
        assert_eq!(windows[1].used_percent, Some(45));
        assert_eq!(windows[2].label, "Current week (Sonnet only)");
        assert_eq!(windows[3].label, "Current week (Opus only)");
        assert_eq!(windows[3].used_percent, None);
        assert_eq!(windows[4].label, "Extra usage");
        assert_eq!(windows[4].used_percent, Some(42));
        assert_eq!(windows[4].value_label.as_deref(), Some("$4.20 / $10.00"));
        assert_eq!(windows[4].detail.as_deref(), Some("Monthly extra usage pool"));
    }

    #[test]
    fn map_anthropic_oauth_usage_extra_disabled() {
        let body = json!({
            "extra_usage": {"is_enabled": false, "utilization": 0.3, "monthly_limit": 100, "used_credits": 50}
        });
        let windows = map_anthropic_oauth_usage(&body);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].label, "Extra usage");
        assert_eq!(windows[0].used_percent, None);
        assert_eq!(windows[0].value_label.as_deref(), Some("Not enabled"));
        assert_eq!(windows[0].detail.as_deref(), Some("Extra usage not enabled"));
    }

    #[test]
    fn claude_clean_terminal_text_strips_ansi() {
        let raw = "\u{1b}[31mSettings:\u{1b}[0m\r\nCurrent session\r\n50% used\r\n";
        let cleaned = claude_clean_terminal_text(raw);
        assert!(!cleaned.contains('\u{1b}'));
        assert!(cleaned.contains("Settings:"));
        assert!(cleaned.contains('\n'));
    }

    #[test]
    fn claude_parse_cli_usage_text_basic() {
        let text = "Settings:\nCurrent session\n50% used\nResets in 3h\nCurrent week (all models)\n25% used\nExtra usage\nNot enabled";
        let windows = parse_claude_cli_usage_text(text).unwrap();
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[0].label, "Current session");
        assert_eq!(windows[0].used_percent, Some(50));
        assert_eq!(windows[1].label, "Current week (all models)");
        assert_eq!(windows[1].used_percent, Some(25));
        assert_eq!(windows[2].label, "Extra usage");
        assert!(windows[2].detail.clone().unwrap_or_default().contains("Not enabled"));
    }

    #[test]
    fn claude_parse_cli_usage_remaining_inverts_percent() {
        let text = "Settings:\nCurrent session\n60% remaining\nCurrent week (all models)\n30% left";
        let windows = parse_claude_cli_usage_text(text).unwrap();
        assert_eq!(windows[0].used_percent, Some(40));
        assert_eq!(windows[1].used_percent, Some(70));
    }

    #[test]
    fn claude_parse_cli_usage_missing_session_errors() {
        let text = "Settings:\nCurrent week (all models)\n25% used";
        let error = parse_claude_cli_usage_text(text).unwrap_err();
        assert!(error.contains("Could not parse Claude CLI usage output."));
    }

    #[test]
    fn claude_parse_cli_usage_detects_token_expired() {
        let text = "Settings:\nCurrent session\ntoken_expired error";
        let error = parse_claude_cli_usage_text(text).unwrap_err();
        assert!(error.contains("token expired"));
    }

    #[test]
    fn provider_slug_mapping_matches_node() {
        assert_eq!(provider_slug_for_adapter_type("claude_local"), "anthropic");
        assert_eq!(provider_slug_for_adapter_type("codex_local"), "openai");
        assert_eq!(provider_slug_for_adapter_type("cursor"), "cursor");
    }

    #[tokio::test]
    async fn fetch_all_quota_windows_times_out_slow_provider() {
        let fast: std::pin::Pin<Box<dyn std::future::Future<Output = ProviderQuotaResult> + Send>> =
            Box::pin(async {
                ProviderQuotaResult {
                    provider: "openai".to_owned(),
                    source: Some("codex-rpc".to_owned()),
                    ok: true,
                    error_family: None,
                    error: None,
                    windows: Vec::new(),
                }
            });
        let slow: std::pin::Pin<Box<dyn std::future::Future<Output = ProviderQuotaResult> + Send>> =
            Box::pin(async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                ProviderQuotaResult {
                    provider: "anthropic".to_owned(),
                    ok: true,
                    windows: Vec::new(),
                    source: None,
                    error_family: None,
                    error: None,
                }
            });
        let tasks: Vec<(String, std::pin::Pin<Box<dyn std::future::Future<Output = ProviderQuotaResult> + Send>>)> =
            vec![
                ("codex_local".to_owned(), fast),
                ("claude_local".to_owned(), slow),
            ];
        let results = fetch_all_quota_windows(tasks).await;
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].ok, true);
        assert_eq!(results[1].ok, false);
        assert_eq!(results[1].provider, "anthropic");
        assert!(results[1].error.as_deref().unwrap_or_default().contains("timed out after 20s"));
    }

    #[tokio::test]
    async fn fetch_all_quota_windows_returns_failure_result_without_blocking() {
        let failing: std::pin::Pin<Box<dyn std::future::Future<Output = ProviderQuotaResult> + Send>> =
            Box::pin(async {
                ProviderQuotaResult {
                    provider: "openai".to_owned(),
                    ok: false,
                    error: Some("boom".to_owned()),
                    windows: Vec::new(),
                    source: None,
                    error_family: None,
                }
            });
        let tasks: Vec<(String, std::pin::Pin<Box<dyn std::future::Future<Output = ProviderQuotaResult> + Send>>)> =
            vec![("codex_local".to_owned(), failing)];
        let results = fetch_all_quota_windows(tasks).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].ok, false);
        assert_eq!(results[0].error.as_deref(), Some("boom"));
    }

    // -----------------------------------------------------------------
    // 探测层（R432）
    // -----------------------------------------------------------------

    #[test]
    fn codex_home_dir_respects_env() {
        std::env::set_var("CODEX_HOME", "/tmp/custom-codex");
        assert_eq!(codex_home_dir(), std::path::PathBuf::from("/tmp/custom-codex"));
        std::env::remove_var("CODEX_HOME");
    }

    #[test]
    fn claude_config_dir_respects_env() {
        std::env::set_var("CLAUDE_CONFIG_DIR", "/tmp/custom-claude");
        assert_eq!(claude_config_dir(), std::path::PathBuf::from("/tmp/custom-claude"));
        std::env::remove_var("CLAUDE_CONFIG_DIR");
    }

    #[test]
    fn read_claude_token_from_credentials_file() {
        let dir = std::env::temp_dir().join(format!("pcq-creds-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".credentials.json"),
            r#"{"claudeAiOauth": {"accessToken": "tok-123"}}"#,
        )
        .unwrap();
        std::env::set_var("CLAUDE_CONFIG_DIR", &dir);
        let token = read_claude_token();
        assert_eq!(token.as_deref(), Some("tok-123"));
        std::env::remove_var("CLAUDE_CONFIG_DIR");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn read_claude_token_returns_none_when_missing() {
        let dir = std::env::temp_dir().join(format!("pcq-empty-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("CLAUDE_CONFIG_DIR", &dir);
        assert_eq!(read_claude_token(), None);
        std::env::remove_var("CLAUDE_CONFIG_DIR");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn read_codex_auth_info_from_auth_file() {
        let dir = std::env::temp_dir().join(format!("pcq-codex-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("auth.json"),
            r#"{"tokens": {"access_token": "at-1", "refresh_token": "rt-1", "account_id": "acc-1"}}"#,
        )
        .unwrap();
        std::env::set_var("CODEX_HOME", &dir);
        let auth = read_codex_auth_info().unwrap();
        assert_eq!(auth.access_token, "at-1");
        assert_eq!(auth.account_id.as_deref(), Some("acc-1"));
        assert_eq!(auth.refresh_token.as_deref(), Some("rt-1"));
        std::env::remove_var("CODEX_HOME");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn quote_for_shell_escapes_single_quote() {
        assert_eq!(quote_for_shell("a'b"), "'a'\\''b'");
        assert_eq!(quote_for_shell("plain"), "'plain'");
    }

    #[test]
    fn claude_cli_probe_command_contains_feed_and_claude() {
        let command = build_claude_cli_shell_probe_command();
        assert!(command.contains("sleep 2"));
        assert!(command.contains("/usage"));
        assert!(command.contains("claude --tools"));
        assert!(command.contains("script -q"));
    }

    #[test]
    fn claude_usage_output_looks_complete_detects_percent() {
        assert!(claude_usage_output_looks_complete(
            "Settings:\nCurrent session\n50% used\nCurrent week (all models)\n25% used"
        ));
        assert!(!claude_usage_output_looks_complete("Settings:\nCurrent session\nno data"));
    }

    #[test]
    fn claude_usage_output_looks_relevant_detects_keywords() {
        assert!(claude_usage_output_looks_relevant("failed to load usage data"));
        assert!(!claude_usage_output_looks_relevant("nothing here"));
    }

    #[test]
    fn describe_claude_subscription_auth_formats() {
        let status = ClaudeAuthStatus {
            logged_in: true,
            auth_method: Some("claude.ai".to_owned()),
            subscription_type: Some("Pro".to_owned()),
        };
        assert_eq!(
            describe_claude_subscription_auth(Some(&status)),
            Some("Claude is logged in via claude.ai (Pro)".to_owned())
        );
        let not_logged_in = ClaudeAuthStatus {
            logged_in: false,
            auth_method: Some("claude.ai".to_owned()),
            subscription_type: None,
        };
        assert_eq!(describe_claude_subscription_auth(Some(&not_logged_in)), None);
    }

    #[tokio::test]
    async fn probe_claude_local_bedrock_short_circuit() {
        std::env::set_var("CLAUDE_CODE_USE_BEDROCK", "1");
        let result = probe_claude_local().await;
        assert_eq!(result.ok, true);
        assert_eq!(result.source.as_deref(), Some("bedrock"));
        assert!(result.windows.is_empty());
        std::env::remove_var("CLAUDE_CODE_USE_BEDROCK");
    }
}
