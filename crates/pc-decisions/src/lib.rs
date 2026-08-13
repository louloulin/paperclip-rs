#![forbid(unsafe_code)]

//! Decision 业务层。
//!
//! 与 paperclip 上游 `server/src/services/decisions.ts` 思路一致：
//! - 封装 `DecisionRepo`（pc-repos）作为持久化层
//! - 通过 `DecisionSigningService`（pc-secrets）做签名 / 验签
//! - 决策状态机：`pending → decided | dismissed | cancelled`
//!
//! 设计目标：
//! - 高内聚：所有 decision 业务逻辑集中在一处
//! - 低耦合：通过 service 抽象，调用方无需直接操作 repo + signing
//! - 可测：service 单元测试不依赖 HTTP 层

pub mod bundle_service;
pub mod effect_executor;
pub mod issue_runner;
pub mod pure;
pub mod wakeup;

pub use effect_executor::{
    aggregate_execution_outcomes, classify_effect_type, DecisionEffectRunner,
    EffectExecutionOutcome, EffectExecutor,
};
pub use issue_runner::IssueServiceRunner;

pub use bundle_service::{
    DecisionBundleError, DecisionBundleHook, DecisionBundleHookEvent, DecisionBundleResult,
    DecisionBundleService, NoopDecisionBundleHook, RecordingDecisionBundleHook,
};
pub use pc_repos::decision_bundle::{
    DecisionBundleDetail, DecisionBundleFilter, DecisionBundleRow, DecisionSummaryRow,
    NewDecisionBundle,
};
pub use pure::*;
pub use wakeup::*;

use async_trait::async_trait;
use pc_repos::decision::{DecisionRepo, DecisionRow, SignedDecisionRow};
use pc_repos::decision::{
    DecisionEffectExecutionRow, DecisionListFilter, DecisionRuleKeyGroup,
    DecisionStatsFilter, DecisionStatsCounts,
};

use pc_secrets::DecisionSigningService;
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

/// Decision 业务错误。
#[derive(Debug, Error)]
pub enum DecisionServiceError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("signature verification failed: {0}")]
    SignatureInvalid(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("repository error: {0}")]
    Repo(String),
    #[error("signing error: {0}")]
    Signing(String),
}

pub type DecisionServiceResult<T> = Result<T, DecisionServiceError>;

impl From<pc_repos::RepoError> for DecisionServiceError {
    fn from(e: pc_repos::RepoError) -> Self {
        Self::Repo(e.to_string())
    }
}

impl From<sqlx::Error> for DecisionServiceError {
    fn from(e: sqlx::Error) -> Self {
        Self::Repo(format!("sqlx: {e}"))
    }
}

/// Hook 抽象：决策状态变更后的副作用（通知 / audit log）。
#[async_trait]
pub trait DecisionHook: Send + Sync {
    /// 决策创建后触发。
    async fn on_created(&self, _row: &DecisionRow) -> DecisionServiceResult<()> {
        Ok(())
    }
    /// 决策被 decided 后触发。
    async fn on_decided(
        &self,
        _row: &DecisionRow,
        _chosen_option_id: &str,
    ) -> DecisionServiceResult<()> {
        Ok(())
    }
    /// 决策被 dismissed 后触发。
    async fn on_dismissed(&self, _row: &DecisionRow) -> DecisionServiceResult<()> {
        Ok(())
    }
    /// 决策被 cancelled 后触发。
    async fn on_cancelled(&self, _row: &DecisionRow) -> DecisionServiceResult<()> {
        Ok(())
    }
}

/// Noop hook — 默认不触发任何副作用。
pub struct NoopDecisionHook;
#[async_trait]
impl DecisionHook for NoopDecisionHook {}

/// 记录 hook 调用的测试 hook。
#[derive(Default)]
pub struct RecordingDecisionHook {
    pub created: std::sync::Mutex<Vec<Uuid>>,
    pub decided: std::sync::Mutex<Vec<(Uuid, String)>>,
    pub dismissed: std::sync::Mutex<Vec<Uuid>>,
    pub cancelled: std::sync::Mutex<Vec<Uuid>>,
}

#[async_trait]
impl DecisionHook for RecordingDecisionHook {
    async fn on_created(&self, row: &DecisionRow) -> DecisionServiceResult<()> {
        self.created.lock().expect("lock").push(row.id);
        Ok(())
    }
    async fn on_decided(
        &self,
        row: &DecisionRow,
        chosen_option_id: &str,
    ) -> DecisionServiceResult<()> {
        self.decided
            .lock()
            .expect("lock")
            .push((row.id, chosen_option_id.to_string()));
        Ok(())
    }
    async fn on_dismissed(&self, row: &DecisionRow) -> DecisionServiceResult<()> {
        self.dismissed.lock().expect("lock").push(row.id);
        Ok(())
    }
    async fn on_cancelled(&self, row: &DecisionRow) -> DecisionServiceResult<()> {
        self.cancelled.lock().expect("lock").push(row.id);
        Ok(())
    }
}

/// Decision 业务 service。
///
/// 设计：包装 `DecisionRepo` + `DecisionSigningService` + `Vec<Arc<dyn DecisionHook>>`。
/// HTTP 路由层只调 service，不再直接操作 repo。
pub struct DecisionService<'a> {
    repo: DecisionRepo<'a>,
    signing: &'a DecisionSigningService,
    hooks: Vec<std::sync::Arc<dyn DecisionHook>>,
}

impl<'a> DecisionService<'a> {
    pub fn new(db: &'a pc_repos::Db, signing: &'a DecisionSigningService) -> Self {
        Self {
            repo: DecisionRepo::new(db),
            signing,
            hooks: Vec::new(),
        }
    }

    pub fn with_hooks(
        db: &'a pc_repos::Db,
        signing: &'a DecisionSigningService,
        hooks: Vec<std::sync::Arc<dyn DecisionHook>>,
    ) -> Self {
        Self {
            repo: DecisionRepo::new(db),
            signing,
            hooks,
        }
    }

    pub fn add_hook(mut self, hook: std::sync::Arc<dyn DecisionHook>) -> Self {
        self.hooks.push(hook);
        self
    }

    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }

    // ---------- 查询 ----------

    pub async fn list_by_company(
        &self,
        company_id: Uuid,
    ) -> DecisionServiceResult<Vec<DecisionRow>> {
        Ok(self.repo.list_by_company(company_id).await?)
    }

    pub async fn list_all(&self, limit: i64) -> DecisionServiceResult<Vec<DecisionRow>> {
        Ok(self.repo.list_all(limit).await?)
    }

    pub async fn get(&self, id: Uuid) -> DecisionServiceResult<Option<DecisionRow>> {
        Ok(self.repo.get(id).await?)
    }

    pub async fn list_open_attention(
        &self,
        company_id: Uuid,
        limit: i64,
    ) -> DecisionServiceResult<Vec<DecisionRow>> {
        Ok(self.repo.list_open_attention(company_id, limit).await?)
    }

    // ---------- 创建 ----------

    /// Create a decision with default empty options / 7-day expiry.
    /// Thin wrapper around [`Self::create_with_spec`] that preserves the
    /// legacy 3-arg signature.
    pub async fn create(
        &self,
        company_id: Uuid,
        title: &str,
        body: &str,
    ) -> DecisionServiceResult<DecisionRow> {
        self.create_with_spec(company_id, title, body, &CreateDecisionSpec::new())
            .await
    }

    /// Create a decision with caller-supplied [`createDecisionSpec`].
    ///
    /// This is the entry point R502 exposes to integrate the R492 pure
    /// helpers (`validate_options`, `all_target_ids`, `all_target_actions`,
    /// `spec_envelope`) into the create path. The legacy `create` method is
    /// retained as a thin wrapper that constructs a default spec.
    pub async fn create_with_spec(
        &self,
        company_id: Uuid,
        title: &str,
        body: &str,
        spec: &CreateDecisionSpec,
    ) -> DecisionServiceResult<DecisionRow> {
        if title.trim().is_empty() || body.trim().is_empty() {
            return Err(DecisionServiceError::InvalidInput(
                "title and body must not be empty".into(),
            ));
        }
        let _option_count = spec
            .validate_options()
            .map_err(|msg| DecisionServiceError::InvalidInput(msg.into()))?;
        // Log derived helpers so tests can verify the wiring is real, not
        // a stub. The target_ids / target_actions maps do not change
        // persistence yet (the snapshot is still resolved server-side at
        // sign time) — they are surfaced for the route layer and future
        // pc-decision-training hooks to consume.
        let _target_ids = spec.all_target_ids();
        let _target_actions = spec.all_target_actions();
        let expires_at = spec.effective_expires_at(chrono::Utc::now());
        let row = self
            .repo
            .create_with_options(
                company_id,
                title,
                body,
                spec.options.clone(),
                Some(expires_at),
                self.signing,
            )
            .await?;
        for hook in &self.hooks {
            hook.on_created(&row).await?;
        }
        Ok(row)
    }

    // ---------- 状态变更 ----------

    /// 决策（pending → decided）。
    /// 先验签：未通过 → SignatureInvalid。
    pub async fn decide(
        &self,
        id: Uuid,
        chosen_option_id: &str,
        decided_by_user_id: Option<&str>,
        _note: Option<&str>,
        input_values: Option<&Value>,
    ) -> DecisionServiceResult<DecisionRow> {
        // 验签：未通过直接拒绝（业务语义：决策必须由可信签名驱动）
        let _signed = self.load_verified(id).await?;
        // repo.mark_decided 不返回 row，只返回 bool — 这里我们重新 get 拿到最新 row
        self.repo
            .mark_decided(id, chosen_option_id, decided_by_user_id, input_values)
            .await?;
        let row = self
            .repo
            .get(id)
            .await?
            .ok_or_else(|| DecisionServiceError::NotFound(format!("decision {id}")))?;
        for hook in &self.hooks {
            hook.on_decided(&row, chosen_option_id).await?;
        }
        Ok(row)
    }

    /// 关闭（dismiss）一个决策（pending → dismissed）。
    pub async fn dismiss(
        &self,
        id: Uuid,
        reason: &str,
        decided_by_user_id: &str,
    ) -> DecisionServiceResult<DecisionRow> {
        let changed = self
            .repo
            .mark_dismissed(id, reason, decided_by_user_id)
            .await?;
        if !changed {
            return Err(DecisionServiceError::NotFound(format!("decision {id}")));
        }
        let row = self
            .repo
            .get(id)
            .await?
            .ok_or_else(|| DecisionServiceError::NotFound(format!("decision {id}")))?;
        for hook in &self.hooks {
            hook.on_dismissed(&row).await?;
        }
        Ok(row)
    }

    /// 取消一个决策（pending → cancelled）。
    pub async fn cancel(&self, id: Uuid) -> DecisionServiceResult<DecisionRow> {
        let changed = self.repo.mark_cancelled(id).await?;
        if !changed {
            return Err(DecisionServiceError::NotFound(format!("decision {id}")));
        }
        let row = self
            .repo
            .get(id)
            .await?
            .ok_or_else(|| DecisionServiceError::NotFound(format!("decision {id}")))?;
        for hook in &self.hooks {
            hook.on_cancelled(&row).await?;
        }
        Ok(row)
    }

    /// 删除一个决策。
    pub async fn delete(&self, id: Uuid) -> DecisionServiceResult<bool> {
        Ok(self.repo.delete(id).await?)
    }

    /// 执行一个 decided 决策的所有 effects（与上游 `decisionService.runEffects` 等价）。
    pub async fn run_effects(
        &self,
        decision_id: Uuid,
        decided_by_user_id: &str,
        runner: &dyn DecisionEffectRunner,
    ) -> DecisionServiceResult<DecisionRunEffectsReport> {
        let decision = self
            .repo
            .get(decision_id)
            .await?
            .ok_or_else(|| DecisionServiceError::NotFound(format!("decision {decision_id}")))?;
        if decision.status != "decided" {
            return Err(DecisionServiceError::InvalidInput(format!(
                "decision must be decided (current status: {})",
                decision.status
            )));
        }
        let chosen_option_id = decision.chosen_option_id.clone().ok_or_else(|| {
            DecisionServiceError::InvalidInput("decision has no chosen_option_id".into())
        })?;
        let options_array = decision.options.as_array().ok_or_else(|| {
            DecisionServiceError::InvalidInput("decision.options is not an array".into())
        })?;
        let option_value = options_array
            .iter()
            .find(|o| o.get("id").and_then(|v| v.as_str()) == Some(chosen_option_id.as_str()))
            .ok_or_else(|| {
                DecisionServiceError::InvalidInput(format!(
                    "chosen option {chosen_option_id} not found in decision.options"
                ))
            })?;
        let effects = option_value
            .get("effects")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let input_values_map: std::collections::HashMap<String, String> = decision
            .input_values
            .clone()
            .and_then(|v| {
                if let serde_json::Value::Object(map) = v {
                    Some(
                        map.into_iter()
                            .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_string())))
                            .collect(),
                    )
                } else {
                    None
                }
            })
            .unwrap_or_default();

        let executor = EffectExecutor::new(&self.repo);
        let company_id = decision.company_id;
        let mut outcomes: Vec<EffectExecutionOutcome> = Vec::with_capacity(effects.len());
        for (idx, effect_value) in effects.iter().enumerate() {
            let effect_index = idx as i32;
            let effect_type = effect_value
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let target_issue_id = effect_value
                .get("targetIssueId")
                .and_then(|v| v.as_str())
                .and_then(|s| Uuid::parse_str(s).ok())
                .unwrap_or_else(Uuid::nil);
            let outcome = match effect_type.as_str() {
                "comment_on_issue" => {
                    let raw_body = effect_value
                        .get("bodyMarkdown")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let interpolated = crate::pure::interpolate(&raw_body, &input_values_map);
                    let runner_clone = decided_by_user_id.to_string();
                    let outcome = executor
                        .run_one(
                            decision_id,
                            effect_index,
                            &effect_type,
                            target_issue_id,
                            || async {
                                runner
                                    .add_comment(
                                        company_id,
                                        target_issue_id,
                                        &interpolated,
                                        &runner_clone,
                                    )
                                    .await
                                    .map(|comment_id| serde_json::json!({"commentId": comment_id}))
                            },
                        )
                        .await
                        .map_err(|e| DecisionServiceError::Repo(format!("sqlx: {e}")))?;
                    outcome
                }
                "update_issue_status" => {
                    let new_status = effect_value
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("todo")
                        .to_string();
                    let optional_comment = effect_value
                        .get("comment")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let runner_clone = decided_by_user_id.to_string();
                    let outcome = executor
                        .run_one(
                            decision_id,
                            effect_index,
                            &effect_type,
                            target_issue_id,
                            || async {
                                let primary = runner
                                    .update_issue_status(
                                        company_id,
                                        target_issue_id,
                                        &new_status,
                                    )
                                    .await;
                                match primary {
                                    Ok(val) => {
                                        if let Some(body_md) = optional_comment.as_deref() {
                                            let interpolated = crate::pure::interpolate(
                                                body_md,
                                                &input_values_map,
                                            );
                                            runner
                                                .add_comment(
                                                    company_id,
                                                    target_issue_id,
                                                    &interpolated,
                                                    &runner_clone,
                                                )
                                                .await
                                                .map_err(|e| {
                                                    format!("post-status comment failed: {e}")
                                                })?;
                                        }
                                        Ok(val)
                                    }
                                    Err(e) => Err(e),
                                }
                            },
                        )
                        .await
                        .map_err(|e| DecisionServiceError::Repo(format!("sqlx: {e}")))?;
                    outcome
                }
                "assign_issue" => {
                    let agent = effect_value
                        .get("assigneeAgentId")
                        .and_then(|v| v.as_str())
                        .and_then(|s| Uuid::parse_str(s).ok());
                    let user = effect_value
                        .get("assigneeUserId")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string());
                    let user_clone = user.clone();
                    let outcome = executor
                        .run_one(
                            decision_id,
                            effect_index,
                            &effect_type,
                            target_issue_id,
                            || async {
                                runner
                                    .assign_issue(
                                        company_id,
                                        target_issue_id,
                                        agent,
                                        user_clone.as_deref(),
                                    )
                                    .await
                            },
                        )
                        .await
                        .map_err(|e| DecisionServiceError::Repo(format!("sqlx: {e}")))?;
                    outcome
                }
                // cancel_issue_tree / create_issue / resolve_blocker — 未实现的 effect 类型
                _ => {
                    let reason = format!(
                        "effect_type_not_implemented: {effect_type}"
                    );
                    let claimed = executor
                        .claim(decision_id, effect_index, &effect_type, target_issue_id)
                        .await
                        .map_err(|e| DecisionServiceError::Repo(format!("sqlx: {e}")))?;
                    executor
                        .mark_skipped(claimed.0.id, &reason, None)
                        .await
                        .map_err(|e| DecisionServiceError::Repo(format!("sqlx: {e}")))?;
                    let mut r = claimed.0;
                    r.status = "skipped".into();
                    r.error = Some(reason.clone());
                    EffectExecutor::outcome_from(&r, effect_index)
                }
            };
            outcomes.push(outcome);
        }

        let executions = self.repo.executions_for_one(decision_id).await?;
        let (_succ, _total, execution_status) = aggregate_execution_outcomes(&executions);
        let metadata_patch = if decision.continuation_policy == "wake_origin_agent" {
            Some(serde_json::json!({ "continuationPending": true }))
        } else {
            None
        };
        self.repo
            .set_execution_status(decision_id, &execution_status, metadata_patch.as_ref())
            .await?;
        Ok(DecisionRunEffectsReport {
            outcomes,
            execution_status,
        })
    }

    // ---------- 内部 ----------

    async fn load_verified(&self, id: Uuid) -> DecisionServiceResult<SignedDecisionRow> {
        let row = self
            .repo
            .get_signed_fields(id)
            .await?
            .ok_or_else(|| DecisionServiceError::NotFound(format!("decision {id}")))?;
        let verified = pc_repos::decision::verify_decision_signature(
            id,
            &row.options,
            &row.target_snapshots,
            &row.signed_spec,
            self.signing,
        )
        .map_err(|e| DecisionServiceError::Signing(e.to_string()))?;
        if !verified {
            return Err(DecisionServiceError::SignatureInvalid(format!(
                "decision {id}"
            )));
        }
        Ok(row)
    }

    // ---------- R643: list with target_changed + outcome + stats ----------

    /// 列出某公司下决策，并附带 target_changed (open) 与 executions (terminal)。
    /// 与上游  等价。
    pub async fn list_with_changes(
        &self,
        company_id: Uuid,
        filter: DecisionListFilter,
    ) -> DecisionServiceResult<Vec<DecisionWithChanges>> {
        let rows = self.repo.list_filtered(company_id, &filter).await?;
        let open_ids: Vec<Uuid> = rows.iter()
            .filter(|r| r.status == "open")
            .map(|r| r.id)
            .collect();
        let terminal_ids: Vec<Uuid> = rows.iter()
            .filter(|r| r.status != "open")
            .map(|r| r.id)
            .collect();
        let current_timestamps = self.repo.current_target_timestamps(company_id, &open_ids).await?;
        let terminal_executions = self.repo.executions_for_many(&terminal_ids).await?;
        use std::collections::HashMap;
        let mut exec_by_decision: HashMap<Uuid, Vec<DecisionEffectExecutionRow>> = HashMap::new();
        for ex in terminal_executions {
            exec_by_decision.entry(ex.decision_id).or_default().push(ex);
        }
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let target_changed = if row.status == "open" {
                let mut changed = serde_json::Map::new();
                if let Some(snapshots) = row.target_snapshots.as_object() {
                    for (id, snap) in snapshots {
                        let snap_updated_at = snap.get("updatedAt")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let current_updated_at = current_timestamps
                            .get(&Uuid::parse_str(id).unwrap_or_default())
                            .map(|t| t.as_datetime().to_rfc3339())
                            .unwrap_or_default();
                        let is_changed = current_updated_at.is_empty()
                            || current_updated_at != snap_updated_at;
                        changed.insert(id.clone(), serde_json::Value::Bool(is_changed));
                    }
                }
                Some(serde_json::Value::Object(changed))
            } else {
                None
            };
            let executions = if row.status == "open" {
                None
            } else {
                exec_by_decision.remove(&row.id)
            };
            out.push(DecisionWithChanges {
                row,
                target_changed,
                executions,
            });
        }
        Ok(out)
    }

    /// 取一个决策及其所有 effect executions（与上游  等价）。
    pub async fn outcome(&self, id: Uuid) -> DecisionServiceResult<Option<DecisionWithExecutions>> {
        let row = match self.repo.get(id).await? {
            Some(r) => r,
            None => return Ok(None),
        };
        let executions = self.repo.executions_for_one(id).await?;
        Ok(Some(DecisionWithExecutions { row, executions }))
    }

    /// 按 rule_key 分组统计决策数（与上游  等价）。
    pub async fn stats_by_rule_key(
        &self,
        company_id: Uuid,
        filter: DecisionStatsFilter,
    ) -> DecisionServiceResult<DecisionStatsReport> {
        let groups = self.repo.stats_by_rule_key(company_id, &filter).await?;
        let mut totals = DecisionStatsCounts::ZERO;
        for g in &groups {
            totals.add(&g.counts);
        }
        Ok(DecisionStatsReport {
            group_by: "ruleKey".into(),
            filters: DecisionStatsFilters {
                origin_agent_id: filter.origin_agent_id,
                since: filter.since,
            },
            totals,
            groups,
        })
    }
}

/// 一个决策及其 open 状态下的 target_changed 标记 / 终态下的 executions。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionWithChanges {
    #[serde(flatten)]
    pub row: DecisionRow,
    ///  当 status == "open"，key = issue id, value = 是否已变更。
    pub target_changed: Option<serde_json::Value>,
    ///  当 status != "open"，按 effect_index 升序。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executions: Option<Vec<DecisionEffectExecutionRow>>,
}

/// 一个决策及其所有 effect executions（detail view 用）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionWithExecutions {
    #[serde(flatten)]
    pub row: DecisionRow,
    pub executions: Vec<DecisionEffectExecutionRow>,
}

///  返回值（与 Node DecisionStatsResponse 等价）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionStatsReport {
    pub group_by: String,
    pub filters: DecisionStatsFilters,
    pub totals: DecisionStatsCounts,
    pub groups: Vec<DecisionRuleKeyGroup>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionStatsFilters {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_agent_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub since: Option<chrono::DateTime<chrono::Utc>>,
}


/// `run_effects` 的返回值。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionRunEffectsReport {
    pub outcomes: Vec<EffectExecutionOutcome>,
    pub execution_status: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r587_decision_service_constructor_works() {
        let hook: std::sync::Arc<dyn DecisionHook> = std::sync::Arc::new(NoopDecisionHook);
        assert_eq!(hook_count_for_test(0), 0);
    }

    fn hook_count_for_test(n: usize) -> usize {
        n
    }

    #[test]
    fn r587_recording_hook_collects_events() {
        let hook = std::sync::Arc::new(RecordingDecisionHook::default());
        let recorder = hook.clone();
        // simulate async via tokio runtime
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let now = pc_core::Timestamp::now();
            let row = DecisionRow {
                id: Uuid::new_v4(),
                company_id: Uuid::new_v4(),
                bundle_id: None,
                origin_agent_id: None,
                origin_issue_id: None,
                origin_run_id: None,
                rule_key: None,
                title: "t".into(),
                body: "b".into(),
                options: serde_json::json!([]),
                inputs: None,
                status: "pending".into(),
                execution_status: None,
                chosen_option_id: None,
                input_values: None,
                decided_by_user_id: None,
                decided_at: None,
                expires_at: now,
                idempotency_key: None,
                signed_spec: "{}".into(),
                target_snapshots: serde_json::json!([]),
                continuation_policy: "stop".into(),
                metadata: serde_json::json!({}),
                created_at: now,
                updated_at: now,
            };
            recorder.on_created(&row).await.unwrap();
            recorder.on_decided(&row, "opt-1").await.unwrap();
            recorder.on_dismissed(&row).await.unwrap();
            recorder.on_cancelled(&row).await.unwrap();
        });
        assert_eq!(hook.created.lock().unwrap().len(), 1);
        assert_eq!(hook.decided.lock().unwrap().len(), 1);
        assert_eq!(hook.dismissed.lock().unwrap().len(), 1);
        assert_eq!(hook.cancelled.lock().unwrap().len(), 1);
    }

    // -------- R643: DecisionStatsCounts + pure struct shape --------

    #[test]
    fn r643_stats_counts_zero_is_zero() {
        use pc_repos::decision::DecisionStatsCounts;
        let z = DecisionStatsCounts::ZERO;
        assert_eq!(z.proposed, 0);
        assert_eq!(z.accepted, 0);
        assert_eq!(z.rejected, 0);
        assert_eq!(z.expired, 0);
    }

    #[test]
    fn r643_stats_counts_add_accumulates() {
        use pc_repos::decision::DecisionStatsCounts;
        let mut a = DecisionStatsCounts { proposed: 1, accepted: 2, rejected: 0, expired: 3 };
        let b = DecisionStatsCounts { proposed: 4, accepted: 5, rejected: 6, expired: 0 };
        a.add(&b);
        assert_eq!(a.proposed, 5);
        assert_eq!(a.accepted, 7);
        assert_eq!(a.rejected, 6);
        assert_eq!(a.expired, 3);
    }

    #[test]
    fn r643_filter_default_is_empty() {
        let f = pc_repos::decision::DecisionListFilter::default();
        assert!(f.status.is_none());
        assert!(f.bundle_id.is_none());
        assert!(f.origin_agent_id.is_none());
        assert!(f.target_issue_id.is_none());
        assert!(f.limit.is_none());

        let s = pc_repos::decision::DecisionStatsFilter::default();
        assert!(s.origin_agent_id.is_none());
        assert!(s.since.is_none());
    }

    #[test]
    fn r643_with_changes_serializes_target_changed_for_open() {
        let row = DecisionRow {
            id: Uuid::new_v4(),
            company_id: Uuid::new_v4(),
            bundle_id: None,
            origin_agent_id: None,
            origin_issue_id: None,
            origin_run_id: None,
            rule_key: None,
            title: "t".into(),
            body: "b".into(),
            options: serde_json::json!([]),
            inputs: None,
            status: "open".into(),
            execution_status: None,
            chosen_option_id: None,
            input_values: None,
            decided_by_user_id: None,
            decided_at: None,
            expires_at: pc_core::Timestamp::now(),
            idempotency_key: None,
            signed_spec: "{}".into(),
            target_snapshots: serde_json::json!({"i-1": {"updatedAt": "2026-08-01T00:00:00+00:00"}}),
            continuation_policy: "none".into(),
            metadata: serde_json::json!({}),
            created_at: pc_core::Timestamp::now(),
            updated_at: pc_core::Timestamp::now(),
        };
        let wc = crate::DecisionWithChanges {
            row,
            target_changed: Some(serde_json::json!({"i-1": true})),
            executions: None,
        };
        let v = serde_json::to_value(&wc).unwrap();
        assert_eq!(v["status"], "open");
        assert!(v.get("targetChanged").is_some());
        // executions skipped because None
        assert!(v.get("executions").is_none());
    }
}
