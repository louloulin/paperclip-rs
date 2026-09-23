//! autopilot 配额（`autopilot_quota_period` / `autopilot_quota_reservation`）。
//!
//! - **写者**：M5-1。类型来自 `mc_core::autopilot_quota`（M5-0 已按 `352` + `448` 的列写死）。
//! - **上游**：`service/autopilot_quota.go`414 + `autopilot_quota_notifications.go`198 +
//!   `AutopilotQuotaUsage`40（`GET /api/autopilots/usage` 的唯一契约来源）+ `QuotaEnabled()`6。
//! - **`QuotaEnabled()` 依赖 entitlement 平面**（R7）⇒ 本地没有「商业默认值」可抄：限额与周期边界
//!   由云侧在运行时下发（`352` 的迁移注释原话）。`QuotaUsage.limit` 因此是 `Option<i64>`。
//! - **保留（reservation）语义**：`uq_autopilot_quota_reservation_key` 是
//!   `(workspace_id, period_start, period_end, idempotency_key) WHERE state <> 'released'`
//!   ⇒ `released` **会释放幂等键**（这是重试语义，不是漏洞）；
//!   `idx_autopilot_quota_reservation_state` 是 `state = 'reserved'` 的部分索引 ⇒ 需要一个
//!   **扫陈旧保留**的入口，因为 `autopilot_run.quota_reservation_id` **没有外键**
//!   （「run 已终态但保留还挂着」是可达状态）。
//! - **不要发明 reason code 词表**：`reason_code` / `source` 在库里是自由文本、无 CHECK。
//!
//! # M5-1 落地了什么（`docs/46-M5-1-READ-FACE.md`）
//!
//! 上游这个文件 414 行里，**读面**只有两块：`quotaPolicy`(80)（拿策略）与
//! `AutopilotQuotaUsage`(302)（把策略 + 周期行折成响应）。M5-1 落的就是这两块 + 一个
//! **entitlement 平面的接缝**；写面（`createAutopilotRunWithQuota`(111) / `consume` / `release` /
//! `sweep` / 通知）归 M5-4 的 dispatch 片，但**阈值判定这条纯函数**（[`usage_from_period`]）在此
//! 定稿，M5-4 直接复用，不写第二份。
//!
//! ## entitlement 平面在本仓是「可安装的接缝」，不是硬编码常量
//!
//! R7 的原话是「本仓无 entitlement 平面 ⇒ quota 关闭」。若把这句话写成
//! `fn quota_enabled() -> bool { false }`，M9 接云侧时就只能改函数体，而**所有**调用点
//! （usage 路由、M5-4 的准入）都会跟着变 —— 且没有地方能替换成按工作区取策略的实现。
//! 因此落成 [`QuotaPolicyProvider`] trait + [`install_policy_provider`]（进程内装一次）：
//!
//! - 默认平面是 [`NoEntitlementPlane`] ⇒ [`is_enabled`] 恒 `false`、`usage` 路由返回
//!   `{"action":"off"}` + 其余全 `null`（上游那条「gate 关掉时不读配额表」的分支，逐字等价）；
//! - M9/云侧装自己的实现即可让**同一批调用点**变成按工作区下发策略，`usage` 与 M5-4 的准入
//!   不可能分叉成两套判定（它们读的是同一个 [`policy_for`]）。
//!
//! 这条接缝也是本片唯一能给 `usage` 的 `observe`/`enforce` 分支做**端到端**测试的手段
//! （见 `crates/mc-http/tests/autopilots.rs` 里装 stub 平面的那条用例）——把 `false` 写死
//! 就等于把响应的一半形状变成不可测。

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use mc_core::{Id, Timestamp};
use mc_repos::autopilot::quota::{get_period, QuotaPeriodRow};

use crate::dto::AutopilotQuotaUsageResponse;
use crate::error::AutopilotError;

/// 上游 entitlement 的 `off` 态：没有平面 / 平面不认识该工作区。
pub const ACTION_OFF: &str = "off";
/// 上游 entitlement 的 `observe` 态：正常计数、不拦截。
pub const ACTION_OBSERVE: &str = "observe";
/// 上游 entitlement 的 `enforce` 态：到限额就拒。
pub const ACTION_ENFORCE: &str = "enforce";

/// 上游 `entitlement.Action` 的两态（`off` 用 [`ACTION_OFF`] 表达「没有策略」而不是一个枚举值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaAction {
    /// 观察：照常计数，永不拒绝。
    Observe,
    /// 强制：`used + reserved >= limit` 时拒绝新 run。
    Enforce,
}

impl QuotaAction {
    /// wire 取值（与上游 `string(action)` 逐字相同）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Observe => ACTION_OBSERVE,
            Self::Enforce => ACTION_ENFORCE,
        }
    }

    /// 解析 wire 取值；`off` / 未知值都返回 `None`（= 没有策略）。
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            ACTION_OBSERVE => Some(Self::Observe),
            ACTION_ENFORCE => Some(Self::Enforce),
            _ => None,
        }
    }
}

/// 上游 `autopilotQuotaPolicy`：一个工作区在一个周期内的额度事实。
///
/// `period_start` / `period_end` 是**周期行的主键**（`get_period` 按它俩定位），
/// `reset_at` 是给客户端看的重置时刻（上游取自同一策略，本地不假设它等于 `period_end`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotaPolicy {
    /// `observe` / `enforce`。
    pub action: QuotaAction,
    /// 周期内的最大 run 数。
    pub limit: i64,
    /// 周期开始（也是周期行的键）。
    pub period_start: Timestamp,
    /// 周期结束（也是周期行的键）。
    pub period_end: Timestamp,
    /// 额度重置时刻。
    pub reset_at: Timestamp,
    /// 策略修订号（写进预留行，供对账）。
    pub policy_revision: i64,
    /// 订阅版本号（同上）。
    pub subscription_version: i64,
}

impl QuotaPolicy {
    /// 判定用的构造器（测试与 M9 的实现都用它，免得各自拼 7 个字段）。
    #[must_use]
    pub fn new(
        action: QuotaAction,
        limit: i64,
        period_start: Timestamp,
        period_end: Timestamp,
    ) -> Self {
        Self {
            action,
            limit,
            reset_at: period_end,
            period_start,
            period_end,
            policy_revision: 1,
            subscription_version: 1,
        }
    }
}

/// entitlement 平面：给定工作区，回答「本周期有没有额度策略」。
///
/// 实现者必须**同步、无 IO** —— 它在每个准入与每次 `usage` 读里都被调用（上游
/// `quotaPolicy` 也是纯内存判定，策略由云侧推送后缓存）。要读库的实现请自己在内部缓存。
pub trait QuotaPolicyProvider: Send + Sync {
    /// `None` = 该工作区没有策略（= 上游 `QuotaEnabled()` 为假）。
    fn policy(&self, workspace_id: Id) -> Option<QuotaPolicy>;
}

/// 本仓默认平面：没有 entitlement 面 ⇒ 永远答「没有策略」。
///
/// 这是 R7 的落点，**不是**「quota 被禁用」的商业判断 —— 上游 gate 关掉时同样走这条分支
/// （`AutopilotQuotaUsage` 里的 `if !enabled { return AutopilotQuotaUsage{Enabled:false} }`）。
#[derive(Debug, Clone, Copy, Default)]
pub struct NoEntitlementPlane;

impl QuotaPolicyProvider for NoEntitlementPlane {
    fn policy(&self, _workspace_id: Id) -> Option<QuotaPolicy> {
        None
    }
}

/// 进程内安装的平面（装一次；后装者不覆盖，见 [`install_policy_provider`] 的返回值）。
static PROVIDER: OnceLock<Arc<dyn QuotaPolicyProvider>> = OnceLock::new();

/// 默认平面是零尺寸常量，`&'static` 借用不需要任何初始化。
static DEFAULT_PLANE: NoEntitlementPlane = NoEntitlementPlane;

/// 安装 entitlement 平面（M9 / Cloud 侧唯一的接线点）。
///
/// 返回 `false` = 已经装过，**本次调用被忽略**（不覆盖先装的实现）。做成「一次即终态」是为了
/// 让 quota 判定在整个进程生命周期内单调：策略提供者中途换人会让同一批调用的行为在两次
/// 请求之间翻转，而 `usage` 与准入读的是同一个 [`policy_for`]，两者必须始终看到同一份事实。
pub fn install_policy_provider(provider: Arc<dyn QuotaPolicyProvider>) -> bool {
    PROVIDER.set(provider).is_ok()
}

/// 当前平面（默认 [`NoEntitlementPlane`]，因此这里**不会** panic）。
#[must_use]
pub fn policy_provider() -> &'static dyn QuotaPolicyProvider {
    match PROVIDER.get() {
        Some(installed) => installed.as_ref(),
        None => &DEFAULT_PLANE,
    }
}

/// 该工作区本周期的策略（`None` = quota 关闭）。
#[must_use]
pub fn policy_for(workspace_id: Id) -> Option<QuotaPolicy> {
    policy_provider().policy(workspace_id)
}

/// 上游 `QuotaEnabled()`：`s.Entitlements != nil`。
///
/// 本仓等价物是「当前平面能为该工作区给出策略」—— 注意它是**按工作区**的，不是进程级的：
/// 上游读了租户订阅之后 gate 也可能对单个工作区为假。
#[must_use]
pub fn is_enabled(workspace_id: Id) -> bool {
    policy_for(workspace_id).is_some()
}

/// 上游 `AutopilotQuotaUsage{Enabled:false}` 的响应形态：`action=off` + 其余全 `null`。
///
/// **不是** `skip_serializing_if` 的省略 —— `AutopilotQuotaUsageResponse` 的字段全带 `Option`
/// 而**不带** `omitempty`（上游 struct 也没有），所以关掉时客户端看到的是显式 `null`
/// （`blocked_counts` 也是 `null`，不是 `{}`）。这条差异有专门的用例锁
/// （`mc-http/tests/autopilots.rs::usage_off_by_default`）。
#[must_use]
pub fn off_usage() -> AutopilotQuotaUsageResponse {
    AutopilotQuotaUsageResponse {
        action: ACTION_OFF.to_string(),
        used: None,
        reserved: None,
        total: None,
        limit: None,
        reached: None,
        period_start: None,
        period_end: None,
        reset_at: None,
        blocked_counts: None,
    }
}

/// 上游 `AutopilotQuotaUsage` 的纯映射：策略 ×（可选的）周期行 → 响应。
///
/// 只有这一处做「限额判定」，因此 `usage` 路由与 M5-4 的准入不可能对「是否已达上限」给出
/// 两个答案。语义逐条对应上游：
///
/// - **没有周期行**（`pgx.ErrNoRows`）⇒ `used`/`reserved` 记 `0`，这是正常路径而不是错误
///   （本周期还没用过额度）；
/// - `total = used + reserved`，`limit` 来自策略（**`observe` 下也照发**，前端只是不拦）；
/// - `reached` **只在 `enforce` 下有值**：上游 `observe` 下 `reached` 是 `nil`（永不作废一个 run）；
/// - `blocked_counts` 打开时**至少是 `{}`**（上游 `make(map[string]int64)` 后再按需填）；
///   行里的 JSON 解不开时返回 500（上游 `json.Unmarshal` 的 err 分支），**不**降级成空表 ——
///   静默把「拒绝过 3 次」显示成「没拒绝过」比报错更糟。
pub fn usage_from_period(
    plan: &QuotaPolicy,
    period: Option<&QuotaPeriodRow>,
) -> Result<AutopilotQuotaUsageResponse, AutopilotError> {
    let (used, reserved, blocked_counts) = match period {
        Some(row) => (
            row.used_count,
            row.reserved_count,
            decode_blocked_counts(row)?,
        ),
        None => (0, 0, BTreeMap::new()),
    };
    let total = used + reserved;
    let reached = match plan.action {
        QuotaAction::Enforce => Some(total >= plan.limit),
        QuotaAction::Observe => None,
    };
    Ok(AutopilotQuotaUsageResponse {
        action: plan.action.as_str().to_string(),
        used: Some(used),
        reserved: Some(reserved),
        total: Some(total),
        limit: Some(plan.limit),
        reached,
        period_start: Some(plan.period_start),
        period_end: Some(plan.period_end),
        reset_at: Some(plan.reset_at),
        blocked_counts: Some(blocked_counts),
    })
}

/// `blocked_counts` 列（jsonb）→ 计数表。
///
/// 容忍 `null`（返回空表）与已经是对象的值；**解不开就报错**（见 [`usage_from_period`] 的说明）。
fn decode_blocked_counts(row: &QuotaPeriodRow) -> Result<BTreeMap<String, i64>, AutopilotError> {
    if row.blocked_counts.is_null() {
        return Ok(BTreeMap::new());
    }
    let counts: BTreeMap<String, i64> = serde_json::from_value(row.blocked_counts.clone())
        .map_err(|err| {
            AutopilotError::internal(format!("decode autopilot quota blocked counts: {err}"))
        })?;
    Ok(counts)
}

/// `GET /api/autopilots/usage` 的服务层入口（上游 `GetAutopilotQuotaUsage` → `AutopilotQuotaUsage`）。
///
/// 接 `PgExecutor` 而不是连接池，理由与 `mc-repos` 的配额自由函数相同：读面用池、
/// 将来的准入事务用 `&mut *tx`，同一份判定两处复用。
///
/// gate 关掉时在**读配额表之前**返回（上游注释原话：「When the gate is off or malformed, the
/// service returns before any quota-table read」）⇒ 没有策略的工作区不会因为配额表缺行而报错。
pub async fn quota_usage<'c, E>(
    executor: E,
    workspace_id: Id,
) -> Result<AutopilotQuotaUsageResponse, AutopilotError>
where
    E: sqlx::PgExecutor<'c> + Send + 'c,
{
    let Some(plan) = policy_for(workspace_id) else {
        return Ok(off_usage());
    };
    let period = get_period(
        executor,
        workspace_id.0,
        plan.period_start.into(),
        plan.period_end.into(),
    )
    .await
    .map_err(AutopilotError::from)?;
    usage_from_period(&plan, period.as_ref())
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};
    use serde_json::json;

    use super::*;

    fn period_row(used: i64, reserved: i64, blocked: serde_json::Value) -> QuotaPeriodRow {
        let t = Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap();
        QuotaPeriodRow {
            workspace_id: uuid::Uuid::nil(),
            period_start: t,
            period_end: t + chrono::Duration::days(30),
            used_count: used,
            reserved_count: reserved,
            blocked_counts: blocked,
            created_at: t,
            updated_at: t,
            rejection_notified_at: None,
        }
    }

    fn plan(action: QuotaAction, limit: i64) -> QuotaPolicy {
        let t = Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap();
        QuotaPolicy::new(
            action,
            limit,
            t.into(),
            (t + chrono::Duration::days(30)).into(),
        )
    }

    /// 关掉时：`off` + 全 `None`（含 `blocked_counts`，它是 `null` 而不是 `{}`）。
    #[test]
    fn off_shape_has_explicit_nulls() {
        let off = off_usage();
        assert_eq!(off.action, "off");
        assert_eq!(off.used, None);
        assert_eq!(off.reserved, None);
        assert_eq!(off.total, None);
        assert_eq!(off.limit, None);
        assert_eq!(off.reached, None);
        assert_eq!(off.period_start, None);
        assert_eq!(off.blocked_counts, None);
        let wire = serde_json::to_value(&off).unwrap();
        assert_eq!(
            wire,
            json!({
                "action": "off", "used": null, "reserved": null, "total": null, "limit": null,
                "reached": null, "period_start": null, "period_end": null, "reset_at": null,
                "blocked_counts": null,
            })
        );
    }

    /// 没有周期行 = 本周期还没用过额度：`0/0/0`，不是错误。
    #[test]
    fn enabled_without_period_row_reports_zeroes() {
        let resp = usage_from_period(&plan(QuotaAction::Enforce, 10), None).unwrap();
        assert_eq!(resp.action, "enforce");
        assert_eq!(
            (resp.used, resp.reserved, resp.total),
            (Some(0), Some(0), Some(0))
        );
        assert_eq!(resp.limit, Some(10));
        assert_eq!(resp.reached, Some(false));
        assert_eq!(resp.blocked_counts, Some(BTreeMap::new()));
        // `{}` 而不是 `null`：打开时该字段至多是空对象。
        let wire = serde_json::to_value(&resp).unwrap();
        assert_eq!(wire["blocked_counts"], json!({}));
    }

    /// `total = used + reserved`，`reached` 到限额就为真。
    #[test]
    fn enforce_reached_is_used_plus_reserved_at_limit() {
        let row = period_row(7, 3, json!({"quota_exhausted": 2}));
        let resp = usage_from_period(&plan(QuotaAction::Enforce, 10), Some(&row)).unwrap();
        assert_eq!(
            (resp.used, resp.reserved, resp.total),
            (Some(7), Some(3), Some(10))
        );
        assert_eq!(resp.reached, Some(true));
        assert_eq!(
            resp.blocked_counts.as_ref().unwrap().get("quota_exhausted"),
            Some(&2)
        );
    }

    /// `observe` **永不说 reached**（上游 `reached` 是 nil）—— 观察态作废 run 就是 bug。
    #[test]
    fn observe_never_reports_reached() {
        let row = period_row(99, 99, json!({}));
        let resp = usage_from_period(&plan(QuotaAction::Observe, 10), Some(&row)).unwrap();
        assert_eq!(resp.action, "observe");
        assert_eq!(resp.reached, None);
        // limit 照发：前端要展示「99/10」而不拦截。
        assert_eq!(resp.limit, Some(10));
    }

    /// 行里的 jsonb 坏掉 ⇒ 500，**不**降级成空表（静默丢掉拒绝计数更糟）。
    #[test]
    fn corrupt_blocked_counts_is_an_error() {
        let row = period_row(1, 0, json!("not-an-object"));
        let err = usage_from_period(&plan(QuotaAction::Enforce, 10), Some(&row)).unwrap_err();
        assert_eq!(err.http_status(), 500);
    }

    /// 平面是「一次即终态」：后装的实现被忽略，且默认平面答 `None`。
    #[test]
    fn policy_provider_defaults_to_no_entitlement_plane() {
        let ws = Id::new();
        assert!(policy_for(ws).is_none());
        assert!(!is_enabled(ws));
        // 默认平面是常量借用、不需要初始化：借出的 trait object 恒指向同一个地址。
        assert!(std::ptr::eq(policy_provider(), policy_provider()));
        assert!(policy_provider().policy(ws).is_none());
    }
}
