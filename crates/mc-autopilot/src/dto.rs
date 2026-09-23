//! 跨切片共享的响应 DTO（`mc-autopilot` 侧）。
//!
//! - **写者**：M5-1。各切片私有的 DTO 放各自文件，只有跨切片共享的进这里。
//! - **上游**：`autopilotToResponse`37 / `triggerToResponse`50 / `runToResponse`32 /
//!   `runToResponseSlim`88（`handler/autopilot.go`）。
//! - **最容易被抄错的契约**（`docs/44` §4.2 M5-1 原话）：`assignee_type` / `pause_reason` /
//!   `execution_mode` / `can_write` / `can_manage_access`，以及列表专属的 `trigger_kinds` /
//!   `next_run_at` / `last_run_status`。
//! - **`can_write` 是 `Option<bool>`，不是 `bool`**：上游文档注释写明「不带 caller 时省略该字段，
//!   客户端按 unknown 处理」⇒ 本地必须区分「省略」与 `false`。
//! - **时间戳一律 `mc_core::Timestamp`**，序列化形态由 `mc-core` 定（不要各切片自己 `to_rfc3339`）。
//!
//! # 为什么 wire 形状在本 crate 而不是 `mc-http`
//!
//! `docs/44` §3.2 把 `mc-http/…/autopilots/dto.rs` 判给 M5-1、`docs/44` §4.2 要求
//! `AutopilotQuotaUsage` 是 `usage` 路由的**唯一**契约来源；而 quota / dispatch / webhook 切片
//! （M5-4/M5-5）也要发出同一批形状（`triggerToResponse` 的广播副本、`runToResponse` 的 run 列表）。
//! 因此本文件持有**形状**（纯 serde，无 `axum`），`mc-http` 侧 `autopilots/dto.rs`
//! **re-export** 它们并补上「仓储行 → DTO」的构造器与 HTTP 专属件（错误体、`MULTICA_PUBLIC_URL`）。
//! 一份形状、一份实现，避免两处各写一遍 30 个字段。
//!
//! ⚠️ 上游 `triggerToResponse` 里也有「行 → DTO」的映射，本仓把它放在 `mc-http` 侧（矩阵把
//! `triggerToResponse` 判给 `mc-http/…/dto.rs`）；下游切片要用它就走 `mc_http::routes::autopilots::dto`。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::OnceLock;

use mc_core::Timestamp;

/// 上游 `autopilotToResponse` 的 `assignee_type` 兜底值（老行可能是 `""`）。
pub const DEFAULT_ASSIGNEE_TYPE: &str = "agent";

/// 上游 `triggerToResponse` 的 provider 兜底值。
pub const DEFAULT_WEBHOOK_PROVIDER: &str = "generic";

/// webhook 公开基址的环境变量名（上游 `h.cfg.PublicURL` 的本地来源）。
///
/// 上游读取点：`AutopilotTriggerResponse.WebhookURL` 的注释 ——
/// 「absolute URL composed from the server's `MULTICA_PUBLIC_URL` setting」。
pub const PUBLIC_URL_ENV: &str = "MULTICA_PUBLIC_URL";

/// 上游 `WebhookEventFilter`：`{event, actions?}`。
///
/// `actions` 是 `omitempty` ⇒ 本地 `Option` + `skip_serializing_if`（空数组与 `None` 都省略，
/// 与 Go 的 `omitempty` 语义一致）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebhookEventFilter {
    /// 事件名（自由文本，写侧校验）。
    pub event: String,
    /// 该事件下允许的动作子集；`None` = 不限制。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actions: Option<Vec<String>>,
}

/// 上游 `AutopilotSubscriberEntry`。
///
/// `user_type` 在库层只允许 `member`（`120`），字段保留在 wire 上是为了将来扩展
/// agent/squad 时是**加性**变更。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AutopilotSubscriberEntry {
    /// 主体类型（眼下恒为 `member`）。
    pub user_type: String,
    /// 主体 id。
    pub user_id: uuid::Uuid,
    /// 订阅时间。
    pub created_at: Timestamp,
}

/// 上游 `AutopilotCollaboratorEntry`（详情页 + 协作者端点的元素形状）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AutopilotCollaboratorEntry {
    /// 主体类型（眼下恒为 `member`）。
    pub user_type: String,
    /// 被授权者。
    pub user_id: uuid::Uuid,
    /// 授权人。
    pub granted_by: uuid::Uuid,
    /// 授权时间。
    pub created_at: Timestamp,
}

/// 上游 `AutopilotTriggerResponse`（19 个 JSON 键，字段顺序与上游一致）。
///
/// `webhook_token` / `webhook_path` / `webhook_url` 三者**同生同死**：读面上非写者拿到的是
/// [`redact_webhook_secrets`] 之后的副本（上游 `redactWebhookSecrets`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AutopilotTriggerResponse {
    /// trigger id。
    pub id: uuid::Uuid,
    /// 所属 autopilot。
    pub autopilot_id: uuid::Uuid,
    /// `schedule` / `webhook` / `api`。
    pub kind: String,
    /// 是否启用。
    pub enabled: bool,
    /// cron 表达式（仅 `schedule`）。
    pub cron_expression: Option<String>,
    /// 时区名（仅 `schedule`）。
    pub timezone: Option<String>,
    /// 下次触发（仅 `schedule`，展示列）。
    pub next_run_at: Option<Timestamp>,
    /// webhook bearer token（**凭据**，只有写者能看到）。
    pub webhook_token: Option<String>,
    /// token 拼出的 ingress 路径（**凭据**，只有写者能看到）。
    pub webhook_path: Option<String>,
    /// 由 `MULTICA_PUBLIC_URL` 拼出的绝对 URL（**凭据**；未配置时省略）。
    pub webhook_url: Option<String>,
    /// 签名/去重约定（`generic` / `github`）。
    pub provider: Option<String>,
    /// 是否配了签名密钥（密钥本体永不返回）。
    pub has_signing_secret: bool,
    /// 签名密钥末 4 位（区分「配过 / 轮换过」用）。
    pub signing_secret_hint: Option<String>,
    /// 展示名。
    pub label: Option<String>,
    /// 上次触发时间。
    pub last_fired_at: Option<Timestamp>,
    /// 创建时间。
    pub created_at: Timestamp,
    /// 更新时间。
    pub updated_at: Timestamp,
    /// 事件过滤范围；省略 = 接受全部事件。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub event_filters: Vec<WebhookEventFilter>,
}

/// 上游 `AutopilotResponse`（16 个基础键 + 3 个列表专属 + subscribers + 两个权限位）。
///
/// 列表专属三件（`trigger_kinds` / `next_run_at` / `last_run_status`）在详情/创建/更新响应上
/// **不出现**（`omitempty`），本地用 `skip_serializing_if` 表达同一语义。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AutopilotResponse {
    /// autopilot id。
    pub id: uuid::Uuid,
    /// 所属工作区。
    pub workspace_id: uuid::Uuid,
    /// 标题。
    pub title: String,
    /// 描述（**无 omitempty** ⇒ 未设置时显式 `null`）。
    pub description: Option<String>,
    /// 项目 id。
    pub project_id: Option<uuid::Uuid>,
    /// 指派目标类型（`agent` / `squad`）。
    pub assignee_type: String,
    /// 指派目标 id。
    pub assignee_id: uuid::Uuid,
    /// `active` / `paused` / `archived`…
    pub status: String,
    /// 暂停原因。
    pub pause_reason: Option<String>,
    /// 执行模式。
    pub execution_mode: String,
    /// issue 标题模板。
    pub issue_title_template: Option<String>,
    /// 创建主体类型。
    pub created_by_type: String,
    /// 创建主体 id。
    pub created_by_id: uuid::Uuid,
    /// 上次运行时间。
    pub last_run_at: Option<Timestamp>,
    /// 创建时间。
    pub created_at: Timestamp,
    /// 更新时间。
    pub updated_at: Timestamp,
    /// **列表专属**：enabled 触发器的 kind 去重升序集合（无 enabled 触发器时省略）。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub trigger_kinds: Vec<String>,
    /// **列表专属**：enabled `schedule` 触发器里最早的下次触发。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_run_at: Option<Timestamp>,
    /// **列表专属**：最近一次 run 的状态（从没跑过时省略）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_status: Option<String>,
    /// 订阅者（**恒非空数组**：没有订阅者时是 `[]`，上游把该字段当权威值）。
    pub subscribers: Vec<AutopilotSubscriberEntry>,
    /// 调用者能否写/执行（`None` = 无 caller 上下文，客户端按 unknown 处理）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub can_write: Option<bool>,
    /// 调用者能否管理协作者列表（比写权限更窄，`None` 同上）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub can_manage_access: Option<bool>,
}

/// 上游 `AutopilotRunResponse`（`runToResponse` / `runToResponseSlim` 共用一个形状）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AutopilotRunResponse {
    /// run id。
    pub id: uuid::Uuid,
    /// 所属 autopilot。
    pub autopilot_id: uuid::Uuid,
    /// 触发它的 trigger（手动/API 触发时为空）。
    pub trigger_id: Option<uuid::Uuid>,
    /// `schedule` / `manual` / `webhook` / `api`。
    pub source: String,
    /// run 状态。
    pub status: String,
    /// 建出的 issue。
    pub issue_id: Option<uuid::Uuid>,
    /// 派出的 task。
    pub task_id: Option<uuid::Uuid>,
    /// 触发时间。
    pub triggered_at: Timestamp,
    /// 终结时间。
    pub completed_at: Option<Timestamp>,
    /// 失败原因（英文原文）。
    pub failure_reason: Option<String>,
    /// 机器可读的终态分类（`omitempty`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason_code: Option<String>,
    /// 触发载荷原样回放（**slim 变体里恒为 `null`**）。
    pub trigger_payload: Option<serde_json::Value>,
    /// 派发结果。
    pub result: Option<serde_json::Value>,
    /// 创建时间。
    pub created_at: Timestamp,
}

/// 上游 `AutopilotQuotaUsageResponse`（`GET /api/autopilots/usage` 的唯一响应形状）。
///
/// **没有一个字段带 `omitempty`**：配额关掉时上游写的是 `{"action":"off"}` + 其余**显式 `null`**
/// ⇒ 本地不能用 `skip_serializing_if`，全字段照发（`Option` 序列化成 `null`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AutopilotQuotaUsageResponse {
    /// `off` / `observe` / `enforce`。
    pub action: String,
    /// 已消费计数。
    pub used: Option<i64>,
    /// 已占位计数。
    pub reserved: Option<i64>,
    /// `used + reserved`。
    pub total: Option<i64>,
    /// 额度上限（entitlement 面下发；`Some` 但 action=observe 时前端只展示不拦截）。
    pub limit: Option<i64>,
    /// 是否已达上限（**只在 enforce 下有意义**，其余 `None`）。
    pub reached: Option<bool>,
    /// 周期开始。
    pub period_start: Option<Timestamp>,
    /// 周期结束。
    pub period_end: Option<Timestamp>,
    /// 额度重置时刻（本地取周期结束）。
    pub reset_at: Option<Timestamp>,
    /// 拒绝计数明细（**关掉时 `null`**，打开时至少 `{}`）。
    pub blocked_counts: Option<BTreeMap<String, i64>>,
}

/// 上游 `webhookPathForToken`（保持自由函数，测试不必构造 handler）。
#[must_use]
pub fn webhook_path_for_token(token: &str) -> String {
    format!("/api/webhooks/autopilots/{token}")
}

/// 上游 `signingSecretHint`：密钥末 4 个字符，短于 4 个字符返回空串。
///
/// 上游按**字节**切片（`secret[len(secret)-4:]`）。本地用 `get(..)` 防止切在多字节字符边界上
/// 触发 panic —— 那时的回应是空 hint（与「短于 4 字节」同一分支），不是 500。
#[must_use]
pub fn signing_secret_hint(secret: &str) -> String {
    if secret.len() < 4 {
        return String::new();
    }
    secret
        .get(secret.len() - 4..)
        .unwrap_or_default()
        .to_string()
}

/// 上游 `redactWebhookSecrets`：把 webhook 凭据三件套从响应副本里抹掉。
///
/// 读面上非写者、以及**所有** WebSocket 广播副本都要过它（上游 `broadcastAutopilotTriggerResponse`）
/// —— 广播到工作区房间的事件不经过任何写门，未抹即等于把 token 交给全体成员。
pub fn redact_webhook_secrets(resp: &mut AutopilotTriggerResponse) {
    resp.webhook_token = None;
    resp.webhook_path = None;
    resp.webhook_url = None;
}

/// `MULTICA_PUBLIC_URL`（进程内读一次；未设置 ⇒ `None`，客户端自己用 `webhook_path` 拼）。
///
/// 上游是配置字段 `h.cfg.PublicURL`，本仓 `ConfigSnapshot` 没有这个字段（不在 M5-1 的写集里），
/// 所以退回到环境变量 —— 变量名与上游注释里写的**逐字相同**。
#[must_use]
pub fn public_url() -> Option<&'static str> {
    static URL: OnceLock<Option<String>> = OnceLock::new();
    URL.get_or_init(|| {
        std::env::var(PUBLIC_URL_ENV)
            .ok()
            .filter(|value| !value.is_empty())
    })
    .as_deref()
}
