//! autopilot 面**响应契约**共享模块（**非路由**，不含 `router()`）。
//!
//! - **写者**：M5-1（`docs/44` §3.2）。M5-2/3/4/5 只**读**（各自 handler 复用这里的投影）。
//! - **上游**：`autopilotToResponse`37 / `triggerToResponse`50 / `runToResponse`32 /
//!   `runToResponseSlim`88（`handler/autopilot.go`）。
//! - **最容易被抄错的字段**（`docs/44` §4.2 原文列举）：`assignee_type` / `pause_reason` /
//!   `execution_mode` / `can_write` / `can_manage_access`。
//! - **`can_write` 是 `Option<bool>`**：上游注释写明「不带 caller 时省略该字段，客户端按 unknown
//!   处理」⇒ 本地必须区分「省略」与 `false`（`Option` + `skip_serializing_if`，不要 `bool`）。
//! - **列表/slim 形态不同**：`runToResponseSlim`88 是列表用的裁剪版，别拿全量 DTO 顶列表。
//! - **时间戳用 `mc_core::Timestamp`**，不要在本文件各自 `to_rfc3339`。
//!
//! # M5-1 落地了什么（`docs/46-M5-1-READ-FACE.md`）
//!
//! 本文件是**行结构 → wire 形状**的唯一投影层：`mc-repos` 的行类型不带任何 serde 注解
//! （`AutopilotRow` 的 `Uuid` / `DateTime<Utc>` 上了 wire 就是另一套形状），所以每个响应字段都
//! 要在这里显式过一遍 `Timestamp::from` / `Id` 转换。四张表四条映射：
//!
//! | 行类型 | 映射 | 上游 |
//! | --- | --- | --- |
//! | [`AutopilotRow`] | [`autopilot_to_response`] | `autopilotToResponse` |
//! | [`AutopilotListRow`] | [`autopilot_list_to_response`] | 同上 + handler 里的三条派生列 |
//! | [`AutopilotTriggerRow`] | [`trigger_to_response`] | `triggerToResponse` |
//! | [`AutopilotSubscriberRow`] / [`AutopilotCollaboratorRow`] | [`subscriber_entry`] / [`collaborator_entry`] | 字面量 |
//!
//! ## 三条**易错语义**（都在下面的映射里落成显式分支）
//!
//! 1. **`assignee_type` 的空串兜底**：`autopilotToResponse` 在 `assignee_type == ""` 时回填
//!    `"agent"`（历史行 + 旧 schema 视图），理由是「API 契约里这个字段非空」。
//! 2. **列表的三条派生列只属于列表**：`trigger_kinds` / `next_run_at` / `last_run_status` 在
//!    详情/创建/更新响应上**必须缺席**（上游 `omitempty`），因此它们不放进基映射，
//!    只在 [`autopilot_list_to_response`] 里填 —— 详情路由调用基映射，天然拿不到这三列。
//! 3. **`subscribers` 恒为数组**：上游为 MUL-6680 专门写成「空数组而不是 `null`」
//!    （列表页曾经传 nil 导致每行都显示「无订阅者」，比缺字段更糟）。本地
//!    `Vec` + `skip_serializing_if` **不能**加空判断 —— `[]` 是权威值。
//!
//! ## 本文件唯一的**扁平**错误体
//!
//! [`CronPreviewErrorBody`] 是 `{"error": "msg", "code": "…"}`（上游 `writeCronPreviewError`）。
//! 本仓其余端点一律是嵌套的 `{"error":{"code","message"}}`（`mc-errors::ErrorResponse`），
//! 而 `cron-preview` 的上游契约就是扁平的（`error` 是**字符串**不是对象），排程编辑器按它
//! 分支 ⇒ 这条形状必须逐字保留，不能「统一格式」。

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

use mc_core::Timestamp;
use mc_repos::autopilot::{
    AutopilotCollaboratorRow, AutopilotListRow, AutopilotRow, AutopilotSubscriberRow,
    AutopilotTriggerRow,
};

pub use mc_autopilot::dto::{
    redact_webhook_secrets, signing_secret_hint, webhook_path_for_token,
    AutopilotCollaboratorEntry, AutopilotQuotaUsageResponse, AutopilotResponse,
    AutopilotRunResponse, AutopilotSubscriberEntry, AutopilotTriggerResponse, WebhookEventFilter,
    DEFAULT_ASSIGNEE_TYPE, DEFAULT_WEBHOOK_PROVIDER, PUBLIC_URL_ENV,
};

/// `assignee_type` 的空串兜底（见模块文档第 1 条）。
#[must_use]
pub fn assignee_type_or_default(raw: &str) -> String {
    if raw.is_empty() {
        DEFAULT_ASSIGNEE_TYPE.to_string()
    } else {
        raw.to_string()
    }
}

/// `autopilot_trigger.provider` 的空串兜底（上游只在 `webhook` 分支里做这件事）。
#[must_use]
pub fn provider_or_default(raw: &str) -> String {
    if raw.is_empty() {
        DEFAULT_WEBHOOK_PROVIDER.to_string()
    } else {
        raw.to_string()
    }
}

/// 上游 `autopilotToResponse`：`autopilot` 行 → wire 形状（**不含**列表派生列与权限位）。
///
/// `subscribers` 由调用方传入（列表页批量取、详情页单机取），本函数不做 IO。
#[must_use]
pub fn autopilot_to_response(
    row: &AutopilotRow,
    subscribers: Vec<AutopilotSubscriberEntry>,
) -> AutopilotResponse {
    AutopilotResponse {
        id: row.id,
        workspace_id: row.workspace_id,
        title: row.title.clone(),
        // 无 `omitempty` ⇒ 未设置时显式 `null`。
        description: row.description.clone(),
        project_id: row.project_id,
        assignee_type: assignee_type_or_default(&row.assignee_type),
        assignee_id: row.assignee_id,
        status: row.status.clone(),
        pause_reason: row.pause_reason.clone(),
        execution_mode: row.execution_mode.clone(),
        issue_title_template: row.issue_title_template.clone(),
        created_by_type: row.created_by_type.clone(),
        created_by_id: row.created_by_id,
        last_run_at: row.last_run_at.map(Timestamp::from),
        created_at: Timestamp::from(row.created_at),
        updated_at: Timestamp::from(row.updated_at),
        // 三条列表专属列在基映射里恒空 ⇒ 详情/写面响应不会带上它们。
        trigger_kinds: Vec::new(),
        next_run_at: None,
        last_run_status: None,
        subscribers,
        // 权限位由调用方按「本请求的 caller」填（`list.rs` 的读面恒有 caller）。
        can_write: None,
        can_manage_access: None,
    }
}

/// 上游 `ListAutopilots`：基映射 + 三条派生列（`trigger_kinds` / `next_run_at` / `last_run_status`）。
///
/// `last_run_status` 的空串折叠在 `AutopilotListRow::last_run_status_or_none` 里
/// （`COALESCE` 出来的 `""` = 从没跑过 = 省略字段，不是「状态是空串」）。
#[must_use]
pub fn autopilot_list_to_response(
    row: &AutopilotListRow,
    subscribers: Vec<AutopilotSubscriberEntry>,
) -> AutopilotResponse {
    let mut resp = autopilot_to_response(&row.base(), subscribers);
    // `None`（没有 enabled 触发器）= `omitempty` 省略；`Some` 里也不会有空数组
    // （`array_agg` 只在有行时非空），但 `unwrap_or_default()` 让两个分支都落到「空 = 省略」。
    resp.trigger_kinds = row.trigger_kinds.clone().unwrap_or_default();
    resp.next_run_at = row.next_run_at.map(Timestamp::from);
    resp.last_run_status = row.last_run_status_or_none().map(str::to_string);
    resp
}

/// 上游 `autopilotToResponse` 的订阅者元素映射。
#[must_use]
pub fn subscriber_entry(row: &AutopilotSubscriberRow) -> AutopilotSubscriberEntry {
    AutopilotSubscriberEntry {
        user_type: row.user_type.clone(),
        user_id: row.user_id,
        created_at: Timestamp::from(row.created_at),
    }
}

/// 上游 `collaboratorToEntry`。
#[must_use]
pub fn collaborator_entry(row: &AutopilotCollaboratorRow) -> AutopilotCollaboratorEntry {
    AutopilotCollaboratorEntry {
        user_type: row.user_type.clone(),
        user_id: row.user_id,
        granted_by: row.granted_by,
        created_at: Timestamp::from(row.created_at),
    }
}

/// 上游 `triggerToResponse`：`autopilot_trigger` 行 → wire 形状。
///
/// `webhook_token` / `webhook_path` / `webhook_url` 三者**同生同死**：只有
/// `kind == "webhook"` **且** token 非空时才出现，且调用方必须对非写者调
/// [`redact_webhook_secrets`]（上游读面的规则：token 是「绕过权限系统就能触发」的凭据）。
///
/// `event_filters` 解不开时**丢掉该字段**而不是 500 —— 上游注释写明「写入期已经严格校验过，
/// 这条分支不该可达；真漏进来时匹配器会 fail closed」，此时把原始 JSONB 字节吐给客户端
/// 比省略更糟。
#[must_use]
pub fn trigger_to_response(row: &AutopilotTriggerRow) -> AutopilotTriggerResponse {
    let mut resp = AutopilotTriggerResponse {
        id: row.id,
        autopilot_id: row.autopilot_id,
        kind: row.kind.clone(),
        enabled: row.enabled,
        cron_expression: row.cron_expression.clone(),
        timezone: row.timezone.clone(),
        next_run_at: row.next_run_at.map(Timestamp::from),
        webhook_token: row.webhook_token.clone(),
        // 下面四个字段只在 webhook 且 token 非空的分支里才有值。
        webhook_path: None,
        webhook_url: None,
        provider: None,
        has_signing_secret: false,
        signing_secret_hint: None,
        label: row.label.clone(),
        last_fired_at: row.last_fired_at.map(Timestamp::from),
        created_at: Timestamp::from(row.created_at),
        updated_at: Timestamp::from(row.updated_at),
        event_filters: Vec::new(),
    };
    if row.is_webhook() {
        if let Some(token) = row.public_webhook_token() {
            let path = webhook_path_for_token(token);
            // `webhook_url` 需要 `MULTICA_PUBLIC_URL`；未配置时**省略**该字段，
            // 客户端自己用 `webhook_path` + 当前 origin 拼（上游注释原文）。
            if let Some(base) = mc_autopilot::dto::public_url() {
                resp.webhook_url = Some(format!("{base}{path}"));
            }
            resp.webhook_path = Some(path);
            resp.provider = Some(provider_or_default(&row.provider));
            if let Some(secret) = row.signing_secret.as_deref().filter(|s| !s.is_empty()) {
                resp.has_signing_secret = true;
                resp.signing_secret_hint = Some(signing_secret_hint(secret));
            }
            if let Some(raw) = row.event_filters.as_ref() {
                if let Ok(filters) = serde_json::from_value::<Vec<WebhookEventFilter>>(raw.clone())
                {
                    resp.event_filters = filters;
                }
            }
        }
    }
    resp
}

/// 上游 `ListAutopilots` 的外层信封。
///
/// 上游是在 handler 里内联构 `map[string]any{"autopilots": …, "total": len(resp)}`。
/// 本地用具名结构体而不是 `serde_json::json!`：信封是**契约**（`total` 是「本次返回条数」不是
/// 「库里的总数」—— 这个端点不分页，两者恰好相等，但别把 `total` 当成 `COUNT(*)` 去优化）。
#[derive(Debug, Serialize)]
pub struct AutopilotListEnvelope {
    /// 本次返回的自动机。
    pub autopilots: Vec<AutopilotResponse>,
    /// `autopilots.len()`（上游 `len(resp)`）。
    pub total: usize,
}

/// 上游 `GetAutopilot` 的外层信封（主对象 + 触发器 + 协作者，一次往返取齐）。
#[derive(Debug, Serialize)]
pub struct AutopilotDetailEnvelope {
    /// 主对象。
    pub autopilot: AutopilotResponse,
    /// 触发器；**非写者**的 webhook 凭据已被 [`redact_webhook_secrets`] 抹掉。
    pub triggers: Vec<AutopilotTriggerResponse>,
    /// 显式协作者授权（管理访问列表的 UI 靠它免一次往返）。
    pub collaborators: Vec<AutopilotCollaboratorEntry>,
}

/// `cron-preview` 的**扁平** 400 体：`{"error": "msg", "code": "invalid_cron" | "invalid_timezone"}`。
///
/// 见模块文档「本文件唯一的扁平错误体」。构造走 [`CronPreviewErrorBody::bad_request`]，
/// 免得调用方忘记状态码是 400（上游 `writeCronPreviewError` 把两者绑在同一处）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CronPreviewErrorBody {
    /// 人类可读消息（**字符串**，不是 `{"code","message"}` 对象）。
    pub error: String,
    /// 机器可读拒绝码（`invalid_cron` / `invalid_timezone`）。
    pub code: String,
}

impl CronPreviewErrorBody {
    /// 构造 400 响应（上游 `writeCronPreviewError`）。
    #[must_use]
    pub fn bad_request(code: &str, message: impl Into<String>) -> Response {
        (
            StatusCode::BAD_REQUEST,
            Json(Self {
                error: message.into(),
                code: code.to_string(),
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assignee_type_defaults_to_agent_only_when_empty() {
        assert_eq!(assignee_type_or_default(""), "agent");
        assert_eq!(assignee_type_or_default("squad"), "squad");
        assert_eq!(provider_or_default(""), "generic");
        assert_eq!(provider_or_default("github"), "github");
    }

    #[test]
    fn cron_preview_error_body_is_flat() {
        let response = CronPreviewErrorBody::bad_request("invalid_cron", "expr is required");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
