//! 模板渲染与时区解析（上游 `autopilotTriggerLocation`1757 / `formatAutopilotRunTimestamp`1732 /
//! `formatAutopilotRunDate`1743 / `resolveAutopilotTriggerTimezone`1709 / `buildIssueDescription`1776 /
//! `interpolateTemplate`1837 / `prettifyJSON`1826）。
//!
//! 时区口径：`resolve_trigger_timezone` 只回「合法 IANA 名」或 `UTC`；渲染前再解析一次
//! （上游也是两段：服务层校验 + 渲染层 `LoadLocation`）。
//!
//! **为何这里重复了 M5-3 的时区解析**：M5-3 的 `src/trigger.rs::Timezone` 与它并发开发
//! （同一波 C），跨片引用会在合并时互相锁死。本片自带一份最小解析（`chrono_tz` + UTC 兜底），
//! 已作为重复项登记（`docs/44` §8）。`mc-autopilot` 早已依赖 `chrono-tz` ⇒ 不新增第三方依赖。

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use mc_repos::autopilot::run as run_sql;
use mc_repos::autopilot::run::AutopilotRunRow;
use mc_repos::autopilot::AutopilotRow;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

/// `DefaultAutopilotTriggerTimezone`（`autopilot.go:49`）。
pub(crate) const DEFAULT_TRIGGER_TIMEZONE: &str = "UTC";

/// 唯一支持的模板变量（`SupportedIssueTitleTemplateVariables`）。
const SUPPORTED_TEMPLATE_VARIABLES: [&str; 1] = ["date"];

/// 上游 `autopilotTriggerLocation`：`(解析出来的时区, 展示标签)`；解析不了落 UTC。
fn trigger_location(timezone: Option<&str>) -> (Tz, String) {
    let label = timezone.unwrap_or_default().trim();
    let label = if label.is_empty() {
        DEFAULT_TRIGGER_TIMEZONE
    } else {
        label
    };
    match label.parse::<Tz>() {
        Ok(tz) => (tz, label.to_string()),
        Err(_) => (Tz::UTC, DEFAULT_TRIGGER_TIMEZONE.to_string()),
    }
}

/// 时区名是否合法（`time.LoadLocation` 的等价判定）。
#[must_use]
pub(crate) fn is_valid_timezone(timezone: &str) -> bool {
    timezone.trim().parse::<Tz>().is_ok()
}

/// 上游 `resolveAutopilotTriggerTimezone`：读 trigger 的 `timezone`，非法/缺失都落 `UTC`。
pub(crate) async fn resolve_trigger_timezone(pool: &PgPool, trigger_id: Option<Uuid>) -> String {
    let Some(trigger_id) = trigger_id else {
        return DEFAULT_TRIGGER_TIMEZONE.to_string();
    };
    match run_sql::load_trigger_timezone(pool, trigger_id).await {
        Ok(Some(raw)) => {
            let timezone = raw.trim();
            if timezone.is_empty() {
                return DEFAULT_TRIGGER_TIMEZONE.to_string();
            }
            if is_valid_timezone(timezone) {
                timezone.to_string()
            } else {
                tracing::warn!(
                    %trigger_id,
                    timezone,
                    "invalid autopilot trigger timezone; falling back to UTC"
                );
                DEFAULT_TRIGGER_TIMEZONE.to_string()
            }
        }
        Ok(None) => DEFAULT_TRIGGER_TIMEZONE.to_string(),
        Err(err) => {
            tracing::warn!(
                %trigger_id,
                error = %err,
                "failed to load autopilot trigger timezone; falling back to UTC"
            );
            DEFAULT_TRIGGER_TIMEZONE.to_string()
        }
    }
}

/// `autopilotRunTriggeredAt`：`triggered_at`（本地非空）优先，否则 `created_at`。
fn run_triggered_at(run: &AutopilotRunRow) -> DateTime<Utc> {
    run.triggered_at
}

/// 上游 `formatAutopilotRunTimestamp`：`2006-01-02 15:04` + 空格 + 时区标签。
#[must_use]
pub(crate) fn format_run_timestamp(run: &AutopilotRunRow, timezone: &str) -> String {
    let (tz, label) = trigger_location(Some(timezone));
    let local = run_triggered_at(run).with_timezone(&tz);
    format!("{} {label}", local.format("%Y-%m-%d %H:%M"))
}

/// 上游 `formatAutopilotRunDate`：`2006-01-02`（`{{date}}` 用）。
#[must_use]
pub(crate) fn format_run_date(run: &AutopilotRunRow, timezone: &str) -> String {
    let (tz, _) = trigger_location(Some(timezone));
    run_triggered_at(run)
        .with_timezone(&tz)
        .format("%Y-%m-%d")
        .to_string()
}

/// 上游 `interpolateTemplate`：把 `{{date}}` 换成触发日；未知变量原样保留。
///
/// 手写扫描器（**不引 `regex`** —— 本波不得新增第三方依赖）：找 `{{`、找紧随的 `}}`，
/// 内层含花括号就整段不认（等价于正则的 `[^{}]*`）。支持 `{{date}}` 与 `{{ date }}` 两种写法。
fn interpolate(tmpl: &str, trigger_date: &str) -> String {
    let chars: Vec<char> = tmpl.chars().collect();
    let mut out = String::with_capacity(tmpl.len());
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '{' && chars.get(index + 1) == Some(&'{') {
            if let Some(close) = find_close(&chars, index + 2) {
                let inner: String = chars[index + 2..close].iter().collect();
                if !inner.contains('{') && !inner.contains('}') {
                    let name = inner.trim();
                    if SUPPORTED_TEMPLATE_VARIABLES.contains(&name) {
                        out.push_str(trigger_date);
                    } else {
                        out.push_str("{{");
                        out.push_str(&inner);
                        out.push_str("}}");
                    }
                    index = close + 2;
                    continue;
                }
            }
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

/// 找 `}}`；遇到 `{` 立即放弃（正则不允许 inner 含花括号）。
fn find_close(chars: &[char], from: usize) -> Option<usize> {
    let mut index = from;
    while index + 1 < chars.len() {
        if chars[index] == '}' && chars[index + 1] == '}' {
            return Some(index);
        }
        if chars[index] == '{' {
            return None;
        }
        index += 1;
    }
    None
}

/// 上游 `interpolateTemplate`：模板取 `issue_title_template`，为空回落到 autopilot 标题。
#[must_use]
pub(crate) fn interpolate_template(
    autopilot: &AutopilotRow,
    run: &AutopilotRunRow,
    timezone: &str,
) -> String {
    let tmpl = match autopilot.issue_title_template.as_deref() {
        Some(value) if !value.is_empty() => value,
        _ => autopilot.title.as_str(),
    };
    interpolate(tmpl, &format_run_date(run, timezone))
}

/// 上游 `prettifyJSON`：能解析就 2 空格缩进，解析不了原样返回。
fn prettify(payload: &Value) -> String {
    serde_json::to_string_pretty(payload).unwrap_or_else(|_| payload.to_string())
}

/// 上游 `buildIssueDescription`：用户描述 + 系统提示；webhook 来源再贴事件与载荷。
#[must_use]
pub(crate) fn build_issue_description(
    autopilot: &AutopilotRow,
    run: &AutopilotRunRow,
    timezone: &str,
) -> String {
    let triggered_at = format_run_timestamp(run, timezone);
    let mut out = String::new();
    out.push_str(autopilot.description.as_deref().unwrap_or_default());
    out.push_str("\n\n---\n*Autopilot run triggered at ");
    out.push_str(&triggered_at);
    out.push_str(
        ". After starting work, rename this issue to accurately reflect what you are doing.*",
    );

    if run.source == "webhook" {
        if let Some(payload) = &run.trigger_payload {
            let event = payload
                .get("event")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .unwrap_or("webhook.received");
            // 「有 `eventPayload` 就美化它，否则美化整个载荷」：用 `match` 而不是
            // `map(..).unwrap_or_else(..)`（后者会被 `clippy::map_unwrap_or` 拦下）。
            let pretty = match payload.get("eventPayload") {
                Some(event_payload) => prettify(event_payload),
                None => prettify(payload),
            };
            out.push_str("\n\nWebhook event: ");
            out.push_str(event);
            out.push_str("\n\nWebhook payload:\n```json\n");
            out.push_str(&pretty);
            out.push_str("\n```");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolate_handles_unknown_tokens_and_whitespace() {
        assert_eq!(interpolate("run {{date}}", "2026-09-23"), "run 2026-09-23");
        assert_eq!(
            interpolate("run {{ date }}", "2026-09-23"),
            "run 2026-09-23"
        );
        assert_eq!(interpolate("{{bogus}}", "2026-09-23"), "{{bogus}}");
        assert_eq!(interpolate("{{a{b}}", "2026-09-23"), "{{a{b}}");
        assert_eq!(interpolate("no tokens", "2026-09-23"), "no tokens");
        assert_eq!(interpolate("{{date}}{{date}}", "x"), "xx");
    }

    #[test]
    fn timezone_falls_back_to_utc() {
        assert_eq!(trigger_location(Some("Asia/Shanghai")).1, "Asia/Shanghai");
        assert_eq!(trigger_location(Some("  ")).1, DEFAULT_TRIGGER_TIMEZONE);
        assert_eq!(trigger_location(Some("Not/AZone")).1, "UTC");
        assert!(is_valid_timezone("UTC"));
        assert!(!is_valid_timezone("nope"));
    }
}
