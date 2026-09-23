//! autopilot trigger 的**领域类型**：时区、kind/provider 闭集、事件过滤（校验 + 编码）。
//!
//! - **写者**：M5-3。
//! - **上游**：`CreateAutopilotTrigger`202 / `createWebhookTriggerWithMintedToken`56 /
//!   `isAllowedWebhookProvider`9 / `UpdateAutopilotTrigger`172 / `DeleteAutopilotTrigger`74 /
//!   `RotateAutopilotTriggerWebhookToken`69 / `SetAutopilotTriggerSigningSecret`65 +
//!   `computeNextRun`75 + `webhookPathForToken`4 + `service/cron.go`138（`NextOccurrenceAfterUTC` /
//!   `NextOccurrencesAfterUTC`）。
//! - **cron 解析不在本文件**：唯一落点是 [`crate::cron`]（M5-1 已落地，`cron-preview`(#2) 与
//!   M5-7/M5-8 的 `plan_time` 共用同一个解析器）。M5-0 的骨架注释曾写「cron 落 `src/trigger.rs`」，
//!   已被 M5-1 更正（见 `src/lib.rs` 的偏离条目）—— **别在这里写第二份解析器**，本文件只调用
//!   [`crate::cron::compute_next_run`] 算 `next_run_at`。
//! - **`Timezone`**：本文件持有 `Timezone` newtype + IANA 校验（内部就是
//!   [`crate::cron::resolve_timezone`]，即 `chrono_tz::Tz::from_str`），对应上游
//!   `ValidateTimezone`（`service/cron.go:112`）与 `resolveAutopilotTriggerTimezone`(1708)。
//!   之所以不在 `mc-core`、也不另写 newtype 的第二份实现：见 `src/lib.rs` 的偏离说明。
//! - **`autopilot_trigger.timezone` 是 `TEXT NULL DEFAULT 'UTC'`**（**可空**），
//!   而 `issue_wakeup.timezone` 是 `TEXT NOT NULL DEFAULT 'UTC'` ⇒ 两边的 NULL 语义不同，
//!   别共用同一个解包（本文件的 [`Timezone::from_column`] 只服务前者）。
//! - **`provider` 只有 `{generic,github}`**（`093` 的 CHECK + 上游 `isAllowedWebhookProvider`）；
//!   `created_by_type` / `published_by_*` **没有 CHECK**（约定 `member|agent`）⇒ 解码时按开放字符串处理。
//! - **事件过滤**：校验与编码各一份实现（`validateWebhookEventFilters` / `encodeWebhookEventFilters`
//!   / `encodeWebhookEventFiltersAlways`，`autopilot_webhook.go:615–700`）。M5-5 的匹配器**复用**
//!   这里的 [`webhook_event_filter_matches`]，不要复制——它是「写入期校验」与「入口期匹配」
//!   共用的那一份真值。

use chrono::{DateTime, Utc};
use chrono_tz::Tz;

use crate::cron::{compute_next_run, resolve_timezone, CronError};
use crate::dto::WebhookEventFilter;

/// `autopilot_trigger.kind = schedule`。
pub const TRIGGER_KIND_SCHEDULE: &str = "schedule";
/// `autopilot_trigger.kind = webhook`。
pub const TRIGGER_KIND_WEBHOOK: &str = "webhook";
/// `autopilot_trigger.kind = api`：**已废弃**（无调度、无入口，唯一触发方式是手写 `POST /trigger`）。
///
/// 保留常量只为了让「读面能解码存量行 + 写面能 400 明确拒绝」写得出人话。
pub const TRIGGER_KIND_API: &str = "api";

/// 上游 `DefaultAutopilotTriggerTimezone`（`service/autopilot.go:49`）。
pub const DEFAULT_TIMEZONE: &str = "UTC";

/// `provider` 闭集里的默认值（`093` 的列默认 + 上游 `isAllowedWebhookProvider`）。
pub const WEBHOOK_PROVIDER_GENERIC: &str = "generic";
/// `provider` 闭集里的 GitHub 适配值。
pub const WEBHOOK_PROVIDER_GITHUB: &str = "github";

/// 上游 `isAllowedWebhookProvider`：白名单是**闭集**（`093` 的 CHECK 同源）。
///
/// 上游注释写明为什么要在写入期拒绝未知值：「免得 create 时拼错 provider，悄悄退化成 generic，
/// 绕过 provider 专属的去重 / 签名行为」。
#[must_use]
pub fn is_allowed_webhook_provider(provider: &str) -> bool {
    matches!(provider, WEBHOOK_PROVIDER_GENERIC | WEBHOOK_PROVIDER_GITHUB)
}

/// 已校验的 IANA 时区名。
///
/// 上游 `ValidateTimezone` 是 `time.LoadLocation`：**`""` 与 `"UTC"` 都成功**（`LoadLocation("")`
/// 返回 UTC）。本地逐字保留这个语义 —— 空串是合法输入，解析成 UTC（见 [`Timezone::parse`]），
/// 因为「空值时按 UTC 解包」正是 `autopilot_trigger.timezone` 可空列的读法。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timezone {
    /// 解析后的时区；`Timezone::default()` 即 UTC。
    tz: Tz,
}

impl Default for Timezone {
    fn default() -> Self {
        Self { tz: Tz::UTC }
    }
}

impl Timezone {
    /// 上游 `ValidateTimezone` + `resolveAutopilotTriggerTimezone` 的合并形态。
    ///
    /// - `""` ⇒ UTC（`time.LoadLocation("")` 的语义），**不是**错误；
    /// - 其余按 IANA 名解析（`TZ=` 前缀形态由 `cron` 模块的解析器负责，不属于这里）。
    ///
    /// # Errors
    ///
    /// 不认识的 IANA 名 → [`CronError::InvalidTimezone`]（上游 `invalid timezone %q: %w`）。
    pub fn parse(name: &str) -> Result<Self, CronError> {
        if name.is_empty() {
            return Ok(Self::default());
        }
        Ok(Self {
            tz: resolve_timezone(name)?,
        })
    }

    /// 由**可空列**解包：`None` / `Some("")` ⇒ UTC（上游 handler 的 `tz := "UTC"` 兜底）。
    ///
    /// # Errors
    ///
    /// 列里存了不认识的名字（存量脏数据）→ [`CronError::InvalidTimezone`]。
    pub fn from_column(raw: Option<&str>) -> Result<Self, CronError> {
        Self::parse(raw.unwrap_or(DEFAULT_TIMEZONE))
    }

    /// 解析后的 `chrono-tz` 时区（交给 [`compute_next_run`] 等调用方）。
    #[must_use]
    pub fn as_tz(self) -> Tz {
        self.tz
    }

    /// 规范名（`chrono-tz` 的 `Display`，与 IANA 名一致：`UTC` / `Asia/Shanghai` / `America/New_York`）。
    #[must_use]
    pub fn name(self) -> String {
        self.tz.name().to_string()
    }
}

impl std::fmt::Display for Timezone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.tz.name())
    }
}

/// 上游 `computeNextRun(expr, tz)`：以**本地当前时刻**为锚算 `autopilot_trigger.next_run_at`。
///
/// ⚠️ 这个列是**纯展示**（列表里的「下次触发」）。调度判定**不得**用它 —— 派发必须走
/// [`crate::cron::next_occurrence_after_utc`] 并锚在 DB 时间上（`cron.rs` 的契约注释）。
/// `timezone` 传空串 ⇒ UTC（上游 `tz := "UTC"` 的兜底就在这里体现）。
///
/// # Errors
///
/// 表达式非法或时区不认识（上游 400，`err.Error()` 直出响应体）。
pub fn next_run_at_for(
    expression: &str,
    timezone: &Timezone,
) -> Result<Option<DateTime<Utc>>, CronError> {
    compute_next_run(expression, timezone.name().as_str())
}

// ---------------------------------------------------------------------------
// 事件过滤（webhook ingress 的收窄条件）
// ---------------------------------------------------------------------------

/// 上游 `validateWebhookEventFilters` 的判负类型。
///
/// 文案逐字保留（含下标），因为上游把它直接当 400 的 `message` 吐给客户端。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EventFilterError {
    /// 第 `index` 个过滤器的 `event` 为空。
    #[error("event_filters[{index}].event must not be empty")]
    EmptyEvent {
        /// 过滤器下标（0-based）。
        index: usize,
    },
    /// 第 `index` 个过滤器的第 `action` 个动作串为空。
    #[error("event_filters[{index}].actions[{action}] must not be empty")]
    EmptyAction {
        /// 过滤器下标（0-based）。
        index: usize,
        /// 动作下标（0-based）。
        action: usize,
    },
}

/// 上游 `validateWebhookEventFilters`：逐项检查 `event` 与 `actions[*]` 非空。
///
/// **不做**大小写折叠、不查白名单（`event` 是自由文本，由 provider 适配层解释）；
/// 空列表是合法的（= 接受全部事件）。
///
/// # Errors
///
/// 首个空字段 → [`EventFilterError`]（下标在文案里）。
pub fn validate_webhook_event_filters(
    filters: &[WebhookEventFilter],
) -> Result<(), EventFilterError> {
    for (index, filter) in filters.iter().enumerate() {
        if filter.event.trim().is_empty() {
            return Err(EventFilterError::EmptyEvent { index });
        }
        for (action, value) in filter.actions.iter().flatten().enumerate() {
            if value.trim().is_empty() {
                return Err(EventFilterError::EmptyAction { index, action });
            }
        }
    }
    Ok(())
}

/// 上游 `encodeWebhookEventFilters`（**create 路径**）：`nil` / 空 → `None`（落 SQL NULL）。
///
/// Go 的 `nil` 与空切片都编码成 `nil` 字节 —— 也就是**不写** `event_filters` 列。
///
/// 归一化：`actions: Some([])` ⇒ `None`。Go 的 `omitempty` 在 marshal 时跳过空切片，
/// 所以 `"actions":[]` 与字段缺失**存下来的字节不同**、行为相同；本地统一成缺失，
/// 让「同一语义只有一种字节形态」，也让 `update` 的实质变更比对不会被 `[]` / 缺失的差异误报。
#[must_use]
pub fn encode_webhook_event_filters(filters: &[WebhookEventFilter]) -> Option<serde_json::Value> {
    if filters.is_empty() {
        return None;
    }
    to_jsonb(filters)
}

/// 上游 `encodeWebhookEventFiltersAlways`（**update 清除路径**）：空列表编码成 `[]` 而不是 NULL。
///
/// 差异的原因在 SQL：`event_filters = COALESCE(narg, event_filters)` —— 传 NULL 是「保留原值」，
/// 所以要清空必须传一个 **非 NULL** 的 `[]`。create 路径没有这层 COALESCE，用不上本函数。
#[must_use]
pub fn encode_webhook_event_filters_always(filters: &[WebhookEventFilter]) -> serde_json::Value {
    to_jsonb(filters).unwrap_or_else(|| serde_json::Value::Array(Vec::new()))
}

/// `Vec<WebhookEventFilter>` → JSONB 值（`actions: Some([])` 归一成缺失，见
/// [`encode_webhook_event_filters`]）。
///
/// 失败**不可达**（结构里没有能失败的字段），返回 `None` 只为了让调用方不必 `unwrap`：
/// 上游这里会 500 `failed to encode event_filters`，本地把不可达分支折成 NULL 更安全。
fn to_jsonb(filters: &[WebhookEventFilter]) -> Option<serde_json::Value> {
    let normalized: Vec<WebhookEventFilter> = filters
        .iter()
        .map(|filter| WebhookEventFilter {
            event: filter.event.clone(),
            actions: filter.actions.clone().filter(|actions| !actions.is_empty()),
        })
        .collect();
    serde_json::to_value(&normalized).ok()
}

/// 上游 `eventFiltersMatch`（`autopilot_webhook.go`）的**单条**判定：`event` 相等且
/// `actions` 为空或不含该动作。
///
/// 放在本文件是因为它是 [`validate_webhook_event_filters`] 的对偶面：写入期校验过的形状，
/// 入口期按同一份结构匹配。M5-5 的 ingress 直接调用它。
#[must_use]
pub fn webhook_event_filter_matches(
    filter: &WebhookEventFilter,
    event: &str,
    action: &str,
) -> bool {
    if filter.event != event {
        return false;
    }
    match filter.actions.as_deref() {
        None | Some([]) => true,
        Some(actions) => actions.iter().any(|allowed| allowed == action),
    }
}

/// 上游 `eventFiltersMatch` 的整表判定：空表 = 接受全部事件。
#[must_use]
pub fn webhook_event_filters_match(
    filters: &[WebhookEventFilter],
    event: &str,
    action: &str,
) -> bool {
    filters.is_empty()
        || filters
            .iter()
            .any(|filter| webhook_event_filter_matches(filter, event, action))
}

#[cfg(test)]
mod tests;
