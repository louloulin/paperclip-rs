//! 归属解析（run → `agent_task_queue` 的 `originator_*` / `accountable_*` 三件套）。
//!
//! - **写者**：M5-4。
//! - **上游**：`internal/attribution/attribution.go`（`DirectHumanRun`348 / `TriggerOwner`432 /
//!   `RuleOwner`390 / `Unattributed`368 / `OwnerFallback`551 / `EvidenceAutopilotRun`111）+
//!   `internal/service/task.go`（`triggerOwnerAttribution`630 / `applyAttributionFallback`774 /
//!   `attributionCreateParams`802 / `ruleOwnerAttribution`~590）。
//!
//! # 判定顺序（逐条对应上游）
//!
//! ```text
//! actor_user_id 有效          → direct_human      （originator = accountable = 点击人）
//! 否则 trigger 能解出主体       → trigger_owner     （originator = accountable = trigger.created_by）
//! 否则存在活跃规则版本快照       → rule_owner        （originator = NULL，accountable = 发布者，审计用）
//! 否则                        → unattributed     （两者皆 NULL，只有证据）
//! 然后（仅 unattributed）       → applyAttributionFallback：
//!                                  工作区 fail_closed=真 / 读不到策略 → 拒绝（跳派）
//!                                  否则 accountable = agent.owner_id，标 owner_fallback
//!                                  没有 agent owner → 拒绝（不伪造人）
//! ```
//!
//! # 两处**刻意**的不对称（照抄上游，不要「修」）
//!
//! 1. **`rule_owner` 不是授权身份**：`originator` 留 `NULL`（MUL-6951 的 Elon review 结论 ——
//!    规则发布者从没「授权」过任何一次运行，把他升成 originator 等于白送调用权）。
//! 2. **`owner_fallback` 同样只做审计**：`originator` 不动，只有 `accountable` 被填。
//!
//! `autopilot_run` 的**来源标签**只有 `direct_human` / `trigger_owner` / `rule_owner` /
//! `owner_fallback` / `unattributed` 五种；上游源码里 `delegation` / `comment_source` /
//! `backfill` 走的是别的入口（评论触发、回填），本文件不产生。

use mc_repos::autopilot::run as run_sql;
use mc_repos::autopilot::AutopilotRow;
use mc_repos::RepoError;
use sqlx::PgPool;
use uuid::Uuid;

/// `EvidenceAutopilotRun`（`attribution.go:111`）：两条派发线的证据都指向 run 本身。
pub(crate) const EVIDENCE_AUTOPILOT_RUN: &str = "autopilot_run";

/// `SourceDirectHuman`。
pub(crate) const SOURCE_DIRECT_HUMAN: &str = "direct_human";
/// `SourceTriggerOwner`。
pub(crate) const SOURCE_TRIGGER_OWNER: &str = "trigger_owner";
/// `SourceRuleOwner`。
pub(crate) const SOURCE_RULE_OWNER: &str = "rule_owner";
/// `SourceOwnerFallback`。
pub(crate) const SOURCE_OWNER_FALLBACK: &str = "owner_fallback";
/// `SourceUnattributed`。
pub(crate) const SOURCE_UNATTRIBUTED: &str = "unattributed";

/// 解析出来的归属（`attribution.Result` 的本地形态）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RunAttribution {
    /// `originator_user_id` —— **授权**那个值（`rule_owner` / `owner_fallback` / `unattributed`
    /// 三条路径都为 `None`）。
    pub user_id: Option<Uuid>,
    /// `accountable_user_id` —— 审计那个值。
    pub accountable_user_id: Option<Uuid>,
    /// `originator_source`（**永远有值**，绝不留 NULL）。
    pub source: &'static str,
    /// `delegated_from_task_id`：派发线永不产生（那是委托链的事）。
    pub delegated_from_task_id: Option<Uuid>,
    /// `trigger_evidence_kind`。
    pub evidence_kind: Option<&'static str>,
    /// `trigger_evidence_ref_id`（= run id）。
    pub evidence_ref_id: Option<Uuid>,
    /// `rule_version_id`：只有 `rule_owner` 路径会有。
    pub rule_version_id: Option<Uuid>,
}

impl RunAttribution {
    /// `finalizeAttribution`（`attribution.go:188`）：有 originator 就把它同步成 accountable
    /// （DB 不变式：`originator IS NULL OR accountable = originator`）。
    fn finalize(mut self) -> Self {
        if let Some(user_id) = self.user_id {
            self.accountable_user_id = Some(user_id);
        }
        self
    }

    /// `DirectHumanRun`348：手动点击的人既是 originator 也是 accountable。
    #[must_use]
    pub(crate) fn direct_human(user_id: Uuid, run_id: Uuid) -> Self {
        Self {
            user_id: Some(user_id),
            accountable_user_id: None,
            source: SOURCE_DIRECT_HUMAN,
            delegated_from_task_id: None,
            evidence_kind: Some(EVIDENCE_AUTOPILOT_RUN),
            evidence_ref_id: Some(run_id),
            rule_version_id: None,
        }
        .finalize()
    }

    /// `TriggerOwner`432：trigger 主体的 `created_by` 既是 originator 也是 accountable。
    #[must_use]
    pub(crate) fn trigger_owner(user_id: Uuid, run_id: Uuid) -> Self {
        Self {
            user_id: Some(user_id),
            accountable_user_id: None,
            source: SOURCE_TRIGGER_OWNER,
            delegated_from_task_id: None,
            evidence_kind: Some(EVIDENCE_AUTOPILOT_RUN),
            evidence_ref_id: Some(run_id),
            rule_version_id: None,
        }
        .finalize()
    }

    /// `RuleOwner`390：发布者只填 `accountable`；没有发布者（`system` 发布 / 没有版本行）
    /// 退化成 [`Self::unattributed`]。
    #[must_use]
    pub(crate) fn rule_owner(publisher: Option<Uuid>, rule_version_id: Uuid, run_id: Uuid) -> Self {
        match publisher {
            Some(publisher) => Self {
                user_id: None,
                accountable_user_id: Some(publisher),
                source: SOURCE_RULE_OWNER,
                delegated_from_task_id: None,
                evidence_kind: Some(EVIDENCE_AUTOPILOT_RUN),
                evidence_ref_id: Some(run_id),
                rule_version_id: Some(rule_version_id),
            },
            None => Self::unattributed(run_id),
        }
    }

    /// `Unattributed`368：明确「没有解出人」，但**不是** NULL 来源旁路（`source` 有标签）。
    #[must_use]
    pub(crate) fn unattributed(run_id: Uuid) -> Self {
        Self {
            user_id: None,
            accountable_user_id: None,
            source: SOURCE_UNATTRIBUTED,
            delegated_from_task_id: None,
            evidence_kind: Some(EVIDENCE_AUTOPILOT_RUN),
            evidence_ref_id: Some(run_id),
            rule_version_id: None,
        }
    }

    /// `OwnerFallback`551：把 `unattributed` 降级成 `owner_fallback`（accountable = agent owner，
    /// originator 不动）。非 `unattributed` 或没有 owner ⇒ 原样返回（**不伪造人**）。
    #[must_use]
    pub(crate) fn owner_fallback(mut self, owner_user_id: Option<Uuid>) -> Self {
        if self.source != SOURCE_UNATTRIBUTED {
            return self;
        }
        if let Some(owner) = owner_user_id {
            self.source = SOURCE_OWNER_FALLBACK;
            self.accountable_user_id = Some(owner);
        }
        self
    }

    /// `attributionCreateParams`802：`(originator_source, evidence_kind, evidence_ref)`；
    /// `delegated_from` 本文件不用（恒 `None`）。
    #[must_use]
    pub(crate) fn task_params(&self) -> (String, Option<String>, Option<Uuid>) {
        (
            self.source.to_string(),
            self.evidence_kind.map(ToString::to_string),
            self.evidence_ref_id,
        )
    }
}

/// 归属判定失败（`ErrAttributionFailClosed` 的本地形态）—— 调用方必须**跳派**
/// （`ReasonCode::AttributionBlocked`），不是失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttributionBlocked {
    /// `fail_closed` 为真（或读不到策略）：不许兜底。
    FailClosed,
    /// 兜底也没有人能问责（agent 没有 `owner_id`）。
    NoAccountableHuman,
}

impl AttributionBlocked {
    /// 跳过原因（上游 `formatAdmissionReason(ap, "workspace fail-closed: no accountable human
    /// for autopilot run")` 只在 `applyAttributionFallback` 出错那一支）。
    pub(crate) const REASON: &'static str =
        "workspace fail-closed: no accountable human for autopilot run";
}

/// 解析这次派发的归属（`dispatchRunOnly`981 的归属段 + `applyAttributionFallback`774）。
///
/// `actor_user_id` = 手动触发的人（`direct_human`）；否则按 trigger 主体 / 规则版本逐级降级。
/// `leader_owner_id` = 执行 agent 的 `owner_id`（兜底用）。
///
/// # Errors
///
/// 库错。**归属不可问责**不是错误：见 [`AttributionBlocked`]。
pub(crate) async fn resolve_run_attribution(
    pool: &PgPool,
    autopilot: &AutopilotRow,
    run_id: Uuid,
    trigger_id: Option<Uuid>,
    actor_user_id: Option<Uuid>,
    leader_owner_id: Option<Uuid>,
) -> Result<Result<RunAttribution, AttributionBlocked>, RepoError> {
    if let Some(actor) = actor_user_id {
        // 手动线到此为止：精确归属**不读工作区策略**（上游注释逐字：precise 路径不读 policy）。
        return Ok(Ok(RunAttribution::direct_human(actor, run_id)));
    }
    let precise = match trigger_id {
        Some(trigger_id) => {
            match run_sql::load_trigger_principal(
                pool,
                trigger_id,
                autopilot.id,
                autopilot.workspace_id,
            )
            .await?
            {
                Some(principal) => RunAttribution::trigger_owner(principal, run_id),
                None => rule_owner_for(pool, autopilot, run_id).await?,
            }
        }
        None => rule_owner_for(pool, autopilot, run_id).await?,
    };
    if precise.source != SOURCE_UNATTRIBUTED {
        return Ok(Ok(precise));
    }
    // 到这里才读策略（只有罕见的 unattributed 路径付出这次读）。
    match run_sql::workspace_attribution_fail_closed(pool, autopilot.workspace_id).await {
        Ok(Some(false)) => {}
        // 读不到策略 / fail-closed 工作区：**拒绝**，不静默跑一条无人问责的任务。
        Ok(Some(true)) | Ok(None) | Err(_) => return Ok(Err(AttributionBlocked::FailClosed)),
    }
    let fallback = precise.owner_fallback(leader_owner_id);
    if fallback.source == SOURCE_UNATTRIBUTED {
        return Ok(Err(AttributionBlocked::NoAccountableHuman));
    }
    Ok(Ok(fallback))
}

/// `ruleOwnerAttribution`：取活跃规则版本快照的发布者（没有版本行 ⇒ `unattributed`）。
async fn rule_owner_for(
    pool: &PgPool,
    autopilot: &AutopilotRow,
    run_id: Uuid,
) -> Result<RunAttribution, RepoError> {
    match run_sql::load_active_rule_version(pool, autopilot.workspace_id, autopilot.id).await? {
        Some((version_id, publisher)) => {
            Ok(RunAttribution::rule_owner(publisher, version_id, run_id))
        }
        None => Ok(RunAttribution::unattributed(run_id)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid(n: u8) -> Uuid {
        Uuid::from_bytes([n; 16])
    }

    #[test]
    fn finalize_syncs_accountable_from_originator() {
        let attr = RunAttribution::direct_human(uuid(1), uuid(9));
        assert_eq!(attr.user_id, attr.accountable_user_id);
        assert_eq!(attr.source, SOURCE_DIRECT_HUMAN);
    }

    #[test]
    fn rule_owner_never_authorizes() {
        let attr = RunAttribution::rule_owner(Some(uuid(2)), uuid(3), uuid(9));
        assert_eq!(attr.user_id, None);
        assert_eq!(attr.accountable_user_id, Some(uuid(2)));
        assert_eq!(attr.rule_version_id, Some(uuid(3)));
        // system 发布（没有 member 发布者）→ unattributed，而不是伪造成 nobody 可问责
        let degraded = RunAttribution::rule_owner(None, uuid(3), uuid(9));
        assert_eq!(degraded.source, SOURCE_UNATTRIBUTED);
        assert_eq!(degraded.accountable_user_id, None);
    }

    #[test]
    fn owner_fallback_is_audit_only() {
        let fallback = RunAttribution::unattributed(uuid(9)).owner_fallback(Some(uuid(4)));
        assert_eq!(fallback.source, SOURCE_OWNER_FALLBACK);
        assert_eq!(fallback.user_id, None);
        assert_eq!(fallback.accountable_user_id, Some(uuid(4)));

        // 没有 agent owner ⇒ 原样退化成 unattributed（调用方拒派）
        let refused = RunAttribution::unattributed(uuid(9)).owner_fallback(None);
        assert_eq!(refused.source, SOURCE_UNATTRIBUTED);

        // 精确归属不被改写
        let precise = RunAttribution::direct_human(uuid(5), uuid(9)).owner_fallback(Some(uuid(4)));
        assert_eq!(precise.source, SOURCE_DIRECT_HUMAN);
        assert_eq!(precise.accountable_user_id, Some(uuid(5)));
    }

    #[test]
    fn task_params_always_stamp_source() {
        let (source, kind, reference) = RunAttribution::unattributed(uuid(9)).task_params();
        assert_eq!(source, SOURCE_UNATTRIBUTED);
        assert_eq!(kind.as_deref(), Some(EVIDENCE_AUTOPILOT_RUN));
        assert_eq!(reference, Some(uuid(9)));
    }
}
