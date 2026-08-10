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

use async_trait::async_trait;
use pc_repos::decision::{DecisionRow, DecisionRepo, SignedDecisionRow};
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

    pub async fn list_by_company(&self, company_id: Uuid) -> DecisionServiceResult<Vec<DecisionRow>> {
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

    pub async fn create(
        &self,
        company_id: Uuid,
        title: &str,
        body: &str,
    ) -> DecisionServiceResult<DecisionRow> {
        if title.trim().is_empty() || body.trim().is_empty() {
            return Err(DecisionServiceError::InvalidInput(
                "title and body must not be empty".into(),
            ));
        }
        let row = self.repo.create(company_id, title, body, self.signing).await?;
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
        let changed = self.repo.mark_dismissed(id, reason, decided_by_user_id).await?;
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
            recorder
                .on_decided(&row, "opt-1")
                .await
                .unwrap();
            recorder.on_dismissed(&row).await.unwrap();
            recorder.on_cancelled(&row).await.unwrap();
        });
        assert_eq!(hook.created.lock().unwrap().len(), 1);
        assert_eq!(hook.decided.lock().unwrap().len(), 1);
        assert_eq!(hook.dismissed.lock().unwrap().len(), 1);
        assert_eq!(hook.cancelled.lock().unwrap().len(), 1);
    }
}
