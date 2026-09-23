//! provider 适配面：请求头子集、信封归一化、去重键、事件作用域过滤。
//!
//! - **写者**：M5-5。
//! - **上游**：`normalizeWebhookPayload`62 / `inferEvent`26 / `stripBOM`10 /
//!   `extractDedupeKey`20 / `selectedHeadersJSON`22 / `splitWebhookEvent`22 /
//!   `isKnownProvider`8 / `webhookActionCandidates`30 / `webhookEventAllowedByTriggerScope`44。
//! - **与 M5-3 的边界**：`isAllowedWebhookProvider`(9) 属 M5-3（trigger **写**面），本文件只
//!   **消费**已落库的 `autopilot_trigger.provider`；`validateWebhookEventFilters` /
//!   `encodeWebhookEventFilters*` 同样已在 M5-3 落地（`mc_autopilot::trigger`），本文件只做
//!   **读**侧匹配，直接复用其 [`crate::dto::WebhookEventFilter`] 形状。
//!
//! # 为什么需要一个 `WebhookHeaders` 结构体
//!
//! `mc-autopilot` **没有 `http` / `axum` 依赖**（依赖表冻结，本波不得新增）⇒ 服务层不能收
//! `HeaderMap`。但服务层确实要用 8 个头（6 个进 `selected_headers`、`Content-Type` 进
//! `content_type` 列、`X-Hub-Signature-256` 进签名校验），所以本地用这个**纯数据**结构体做
//! 边界：`mc-http` 把 `HeaderMap` 折进来，服务层与 worker 侧则从
//! `webhook_delivery.selected_headers` 重建（[`WebhookHeaders::from_selected`]）。
//!
//! **只保留这 8 个键**（上游 `selectedHeadersJSON` 的同一份名单）—— 其余请求头一律丢弃，
//! 这样「被持久化的头」与「签名/去重用的头」是同一份名单，不会出现某条路径多读一个头。
//!
//! # 归一化的四条规则（上游 `normalizeWebhookPayload` 的注释逐条对应）
//!
//! 1. body 必须是 JSON 对象或数组：标量（`"hello"` / `123`）与非法 JSON 都报错（400，**不落库**）。
//! 2. 对象里带非空字符串 `event` ⇒ 保留它；再带 `eventPayload` 就整块取它，否则**整条 body** 当载荷。
//! 3. 否则 `event` 从头部/体字段推断（顺序见 [`infer_event`]），整条 body 当载荷。
//! 4. 推断不出来时兜底 `webhook.received`。
//!
//! # 事件过滤匹配的三处**刻意**对齐
//!
//! - **坏 `event_filters` 失败关闭**（fail-closed）：写侧校验本该拦住畸形形状，但若库里真有脏行，
//!   「把 only-allow-X 悄悄放宽成允许全部」比「先丢事件等运维发现」危险得多。
//! - **同名事件不短路**：UI 允许同名的多行 filter（如两行 `workflow_run` 覆盖不相交的 action），
//!   命中事件名后**继续扫**后面的行 —— 上游 PR #3231 修过一个「第一行静默遮蔽其余行」的 bug。
//! - **候选动作取自 body 字段**：[`webhook_action_candidates`] 除事件后缀外还读
//!   `action` / `state` / `conclusion` / `status`，因为不是所有 provider 都把动作编进事件名。

use std::collections::BTreeMap;

use chrono::{SecondsFormat, Utc};
use serde_json::{Map, Value};

use crate::dto::WebhookEventFilter;

use super::{WebhookEnvelope, WebhookRequest};

/// 进 `webhook_delivery.selected_headers` 的 6 个头（**小写**，上游同款）。
const SELECTED_HEADER_NAMES: [&str; 6] = [
    "user-agent",
    "x-github-event",
    "x-github-delivery",
    "x-gitlab-event",
    "x-event-type",
    "idempotency-key",
];

/// 签名头（**只记 present/absent，绝不记值** —— 否则 delivery dump 会泄漏 body 的 HMAC）。
const SIGNATURE_HEADER_NAME: &str = "x-hub-signature-256";
/// 签名头在 `selected_headers` 里的落库键。
const SIGNATURE_PRESENT_KEY: &str = "x-hub-signature-256-present";

/// 事件名兜底。
pub const DEFAULT_EVENT: &str = "webhook.received";

/// 入站关心的 8 个请求头（**路由无关**的纯数据）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebhookHeaders {
    /// `User-Agent`。
    pub user_agent: Option<String>,
    /// `X-GitHub-Event`。
    pub x_github_event: Option<String>,
    /// `X-GitHub-Delivery`（github 的去重键来源）。
    pub x_github_delivery: Option<String>,
    /// `X-Gitlab-Event`。
    pub x_gitlab_event: Option<String>,
    /// `X-Event-Type`。
    pub x_event_type: Option<String>,
    /// `Idempotency-Key`（generic 的去重键来源）。
    pub idempotency_key: Option<String>,
    /// `X-Hub-Signature-256`（**凭据**，只用于校验，绝不落库）。
    pub x_hub_signature_256: Option<String>,
    /// `Content-Type`（归一化后进 `webhook_delivery.content_type`）。
    pub content_type: Option<String>,
}

impl WebhookHeaders {
    /// 入站关心的**全部** 8 个头（6 个落库名单 + 签名头 + `Content-Type`）。
    ///
    /// `mc-http` 拿它把 `HeaderMap` 折成 [`WebhookHeaders`]（按名取**第一个**值 = 上游
    /// `headers.Get`）。名单与 [`Self::set`] 的 `match` 必须同步 —— 有单测钉住。
    pub const INBOUND_HEADER_NAMES: [&str; 8] = [
        "user-agent",
        "x-github-event",
        "x-github-delivery",
        "x-gitlab-event",
        "x-event-type",
        "idempotency-key",
        SIGNATURE_HEADER_NAME,
        "content-type",
    ];

    /// 空集合。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 按（大小写不敏感）头名写入；**不认识的头名被静默忽略**（名单见模块头）。
    pub fn set(&mut self, name: &str, value: &str) {
        let slot = match name.to_ascii_lowercase().as_str() {
            "user-agent" => &mut self.user_agent,
            "x-github-event" => &mut self.x_github_event,
            "x-github-delivery" => &mut self.x_github_delivery,
            "x-gitlab-event" => &mut self.x_gitlab_event,
            "x-event-type" => &mut self.x_event_type,
            "idempotency-key" => &mut self.idempotency_key,
            SIGNATURE_HEADER_NAME => &mut self.x_hub_signature_256,
            "content-type" => &mut self.content_type,
            _ => return,
        };
        *slot = Some(value.to_owned());
    }

    /// 从 `webhook_delivery.selected_headers` + `content_type` 列重建（worker 侧）。
    ///
    /// 签名头**必然丢失**（只存了 present 标记）：这是刻意的 —— worker 不需要重验签名，
    /// 入站已经判过并落进 `signature_status`。所以重建出来的 `x_hub_signature_256` 恒为 `None`。
    #[must_use]
    pub fn from_selected(selected: &Value, content_type: Option<&str>) -> Self {
        let mut headers = Self::default();
        if let Some(map) = selected.as_object() {
            for name in SELECTED_HEADER_NAMES {
                if let Some(value) = map.get(name).and_then(Value::as_str) {
                    headers.set(name, value);
                }
            }
        }
        headers.content_type = content_type
            .map(str::to_owned)
            .filter(|value| !value.is_empty());
        headers
    }

    /// 上游 `selectedHeadersJSON`：只带非空值的 6 个头 + 签名头的 present 标记。
    #[must_use]
    pub fn to_selected_json(&self) -> Value {
        let mut out = Map::new();
        let pairs: [(&str, Option<&String>); 6] = [
            ("user-agent", self.user_agent.as_ref()),
            ("x-github-event", self.x_github_event.as_ref()),
            ("x-github-delivery", self.x_github_delivery.as_ref()),
            ("x-gitlab-event", self.x_gitlab_event.as_ref()),
            ("x-event-type", self.x_event_type.as_ref()),
            ("idempotency-key", self.idempotency_key.as_ref()),
        ];
        for (name, value) in pairs {
            if let Some(value) = value.filter(|value| !value.is_empty()) {
                out.insert(name.to_owned(), Value::String(value.clone()));
            }
        }
        if self
            .x_hub_signature_256
            .as_ref()
            .is_some_and(|value| !value.is_empty())
        {
            out.insert(SIGNATURE_PRESENT_KEY.to_owned(), Value::Bool(true));
        }
        Value::Object(out)
    }
}

/// 上游 `webhookTokenPrefix` 的形态断言：`awt_` + 43 字符 `base64url`（32 字节 `RawURLEncoding`）。
///
/// **只用于测试与排障**，生产路径不做长度校验（token 只做等值查找，长度不对自然查不到 ⇒ 404）。
#[must_use]
pub fn looks_like_webhook_token(token: &str) -> bool {
    token.len() == super::WEBHOOK_TOKEN_PREFIX.len() + 43
        && token.starts_with(super::WEBHOOK_TOKEN_PREFIX)
        && token[super::WEBHOOK_TOKEN_PREFIX.len()..]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// 上游 `provider == "" ⇒ "generic"`。
#[must_use]
pub fn provider_or_default(provider: &str) -> &str {
    if provider.is_empty() {
        "generic"
    } else {
        provider
    }
}

/// 上游 `stripBOM`：去掉 PowerShell 一类客户端爱加的 UTF-8 BOM。
#[must_use]
pub fn strip_bom(body: &[u8]) -> &[u8] {
    match body {
        [0xEF, 0xBB, 0xBF, rest @ ..] => rest,
        other => other,
    }
}

/// 上游 `normalizeWebhookPayload`。错误文案原样透出（`mc-http` 直接当 400 body）。
pub fn normalize_webhook_payload(
    body: &[u8],
    headers: &WebhookHeaders,
) -> Result<WebhookEnvelope, String> {
    let body = strip_bom(body);
    if body.is_empty() {
        return Err("empty body".to_owned());
    }
    let parsed: Value =
        serde_json::from_slice(body).map_err(|err| format!("invalid json: {err}"))?;
    if !parsed.is_object() && !parsed.is_array() {
        return Err("body must be a JSON object or array".to_owned());
    }

    let request = WebhookRequest {
        received_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        content_type: base_content_type(headers.content_type.as_deref()),
    };

    // ① 调用方自带的 envelope（`event` + 可选 `eventPayload`）。
    let provided_event = parsed
        .as_object()
        .and_then(|object| object.get("event"))
        .and_then(Value::as_str)
        .filter(|event| !event.is_empty())
        .map(str::to_owned);
    if let Some(event) = provided_event {
        // 有 `eventPayload` ⇒ 用它；没有 ⇒ **整条 body** 当载荷（上游 "fall through to use whole
        // body as payload"）。两处都用**解析后的** `Value`：上游那边是 `json.RawMessage(body)`
        // 的本地等价物，把字节直接塞进 `Value` 只会得到一串数字数组。
        let event_payload = parsed
            .as_object()
            .and_then(|object| object.get("eventPayload").cloned())
            .unwrap_or_else(|| parsed.clone());
        return Ok(WebhookEnvelope {
            event,
            event_payload,
            request,
        });
    }

    // ② 推断事件；整条 body 当载荷。
    Ok(WebhookEnvelope {
        event: infer_event(headers, &parsed),
        event_payload: parsed,
        request,
    })
}

/// `Content-Type`：分号前的部分**并 trim**（上游只在 `strings.Index(contentType, ";") >= 0`
/// 的分支里 `TrimSpace`）；空 ⇒ `None`（Go 的 `omitempty`）。
///
/// **偏差登记（`docs/54` D20）**：没有分号时上游**不 trim** ⇒ `"  "` 会被原样存下来（非空、
/// 会出现在信封里）。这里逐字照抄，不做「顺手 trim」。
#[must_use]
pub fn base_content_type(raw: Option<&str>) -> Option<String> {
    let raw = raw?;
    let base = match raw.split_once(';') {
        Some((base, _)) => base.trim(),
        None => raw,
    };
    if base.is_empty() {
        None
    } else {
        Some(base.to_owned())
    }
}

/// 上游 `inferEvent`：`X-GitHub-Event`（对象带 `action` 时拼后缀）→ `X-Gitlab-Event` →
/// `X-Event-Type` → body.`event` → body.`type` → body.`action` → `webhook.received`。
#[must_use]
pub fn infer_event(headers: &WebhookHeaders, body: &Value) -> String {
    let object = body.as_object();
    let body_str = |key: &str| {
        object
            .and_then(|object| object.get(key))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
    };

    if let Some(github) = headers
        .x_github_event
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        if let Some(action) = body_str("action") {
            return format!("github.{github}.{action}");
        }
        return format!("github.{github}");
    }
    if let Some(gitlab) = headers
        .x_gitlab_event
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        return format!("gitlab.{gitlab}");
    }
    if let Some(event_type) = headers
        .x_event_type
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        return event_type.to_owned();
    }
    for key in ["event", "type", "action"] {
        if let Some(value) = body_str(key) {
            return value.to_owned();
        }
    }
    DEFAULT_EVENT.to_owned()
}

/// 上游 `extractDedupeKey`：`(键, 来源标签)`；没有可用头 ⇒ `(None, None)`。
///
/// 顺序：`github` provider 的 `X-GitHub-Delivery` → `Idempotency-Key` → 任意 provider 的
/// `X-GitHub-Delivery`。第三条让 github 的 header 在 generic trigger 上也能用（Postman 手工重放）。
#[must_use]
pub fn extract_dedupe_key(
    provider: &str,
    headers: &WebhookHeaders,
) -> (Option<String>, Option<String>) {
    let github_delivery = headers
        .x_github_delivery
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if provider == "github" {
        if let Some(value) = github_delivery {
            return (Some(value.to_owned()), Some("x-github-delivery".to_owned()));
        }
    }
    if let Some(value) = headers
        .idempotency_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return (Some(value.to_owned()), Some("idempotency-key".to_owned()));
    }
    if let Some(value) = github_delivery {
        return (Some(value.to_owned()), Some("x-github-delivery".to_owned()));
    }
    (None, None)
}

/// 上游 `isKnownProvider`：事件名里带**已知 provider 前缀**时按 `provider.name.action` 切分。
#[must_use]
pub fn is_known_provider(prefix: &str) -> bool {
    matches!(prefix, "github" | "gitlab" | "bitbucket" | "gitea")
}

/// 上游 `splitWebhookEvent` → `(provider, name, action)`。
///
/// `"github.workflow_run.completed"` → `("github","workflow_run","completed")`；
/// `"issues"` → `("","issues","")`；`"issues.opened"` → `("","issues","opened")`。
/// 动作段用 `join(".")` 保回多点尾巴（`"github.push.refs"` → action `"refs"`… 逐段拼回）。
#[must_use]
pub fn split_webhook_event(event: &str) -> (String, String, String) {
    let parts: Vec<&str> = event.split('.').collect();
    if is_known_provider(parts[0]) {
        if parts.len() >= 3 {
            return (
                parts[0].to_owned(),
                parts[1].to_owned(),
                parts[2..].join("."),
            );
        }
        if parts.len() == 2 {
            return (parts[0].to_owned(), parts[1].to_owned(), String::new());
        }
        return (parts[0].to_owned(), String::new(), String::new());
    }
    if parts.len() >= 2 {
        return (String::new(), parts[0].to_owned(), parts[1..].join("."));
    }
    (String::new(), event.to_owned(), String::new())
}

/// 上游 `webhookActionCandidates`：事件后缀 + body 里的 `action`/`state`/`conclusion`/`status`。
///
/// 上游用 `map` 去重（**遍历顺序随机**）；本地用 `Vec` 保插入序 —— 判定是「任一命中」，
/// 顺序不影响结果，但**可复现**比随机好（同一份输入永远得到同一个 `Vec`）。
#[must_use]
pub fn webhook_action_candidates(event_action: &str, payload: &Value) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    push_unique(&mut out, event_action);
    if let Some(object) = payload.as_object() {
        for key in ["action", "state", "conclusion", "status"] {
            if let Some(value) = object.get(key).and_then(Value::as_str) {
                push_unique(&mut out, value);
            }
        }
    }
    out
}

/// trim 后非空且未出现过才入列（上游 `add` 闭包）。
fn push_unique(out: &mut Vec<String>, value: &str) {
    let value = value.trim();
    if value.is_empty() || out.iter().any(|existing| existing == value) {
        return;
    }
    out.push(value.to_owned());
}

/// 上游 `webhookEventAllowedByTriggerScope`：`None`/`null`/空表 ⇒ 接受全部。
///
/// 畸形 JSON ⇒ **拒绝**（fail-closed）；同名事件**不短路**（见模块头三条对齐）。
#[must_use]
pub fn event_allowed_by_trigger_scope(
    event_filters: Option<&Value>,
    envelope: &WebhookEnvelope,
) -> bool {
    let Some(event_filters) = event_filters.filter(|value| !value.is_null()) else {
        return true;
    };
    let Ok(filters) = serde_json::from_value::<Vec<WebhookEventFilter>>(event_filters.clone())
    else {
        tracing::warn!("webhook: malformed event_filters, denying");
        return false;
    };
    if filters.is_empty() {
        return true;
    }
    let (_, event_name, event_action) = split_webhook_event(&envelope.event);
    let candidates = webhook_action_candidates(&event_action, &envelope.event_payload);
    for filter in &filters {
        if filter.event != event_name {
            continue;
        }
        let Some(actions) = filter.actions.as_deref().filter(|list| !list.is_empty()) else {
            return true;
        };
        if candidates
            .iter()
            .any(|candidate| actions.iter().any(|allowed| allowed == candidate))
        {
            return true;
        }
        // 刻意**不**在这里返回 false：同名的后续 filter 还要机会（上游 PR #3231）。
    }
    false
}

/// 事件名在 `event_filters` 里出现过的同名行数（仅排障/测试用）。
#[must_use]
pub fn matching_filter_rows(event_filters: Option<&Value>, event: &str) -> usize {
    let Some(Value::Array(rows)) = event_filters else {
        return 0;
    };
    let (_, event_name, _) = split_webhook_event(event);
    rows.iter()
        .filter(|row| {
            row.get("event")
                .and_then(Value::as_str)
                .is_some_and(|name| name == event_name)
        })
        .count()
}

/// 排障用：把 `selected_headers` 折成有序 `BTreeMap`（键序稳定，便于日志/断言比对）。
#[must_use]
pub fn selected_headers_sorted(selected: &Value) -> BTreeMap<String, String> {
    selected
        .as_object()
        .map(|object| {
            object
                .iter()
                .map(|(key, value)| {
                    let rendered = value
                        .as_str()
                        .map_or_else(|| value.to_string(), str::to_owned);
                    (key.clone(), rendered)
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
