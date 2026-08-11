use async_trait::async_trait;
use pc_errors::{internal, Error as PcError, Result as PcResult};
use pc_repos::{
    issue_approvals::{
        ApprovalForIssueItem, IssueApprovalLinkRow, IssueApprovalRepo, IssueForApprovalItem,
        LinkActor,
    },
    Db,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone, Default)]
pub struct IssueApprovalLinkActor {
    pub agent_id: Option<Uuid>,
    pub user_id: Option<String>,
}
impl From<IssueApprovalLinkActor> for LinkActor {
    fn from(a: IssueApprovalLinkActor) -> Self {
        Self {
            agent_id: a.agent_id,
            user_id: a.user_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum IssueApprovalHookEvent {
    Linked {
        company_id: Uuid,
        issue_id: Uuid,
        approval_id: Uuid,
    },
    Unlinked {
        company_id: Uuid,
        issue_id: Uuid,
        approval_id: Uuid,
    },
    BulkLinked {
        company_id: Uuid,
        approval_id: Uuid,
        count: usize,
    },
}

#[async_trait]
pub trait IssueApprovalHook: Send + Sync {
    async fn on_issue_approval_event(&self, _event: IssueApprovalHookEvent) -> PcResult<()> {
        Ok(())
    }
}

pub struct NoopIssueApprovalHook;
#[async_trait]
impl IssueApprovalHook for NoopIssueApprovalHook {}

#[derive(Default)]
pub struct RecordingIssueApprovalHook {
    pub events: std::sync::Mutex<Vec<IssueApprovalHookEvent>>,
}
impl RecordingIssueApprovalHook {
    pub fn events_snapshot(&self) -> Vec<IssueApprovalHookEvent> {
        self.events.lock().expect("mutex").clone()
    }
    pub fn clear(&self) {
        self.events.lock().expect("mutex").clear()
    }
    pub fn len(&self) -> usize {
        self.events.lock().expect("mutex").len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
#[async_trait]
impl IssueApprovalHook for RecordingIssueApprovalHook {
    async fn on_issue_approval_event(&self, e: IssueApprovalHookEvent) -> PcResult<()> {
        self.events.lock().expect("mutex").push(e);
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum IssueApprovalError {
    #[error("validation: {0}")]
    Validation(String),
    #[error("issue and approval belong to different companies")]
    CrossCompany,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Pc(#[from] PcError),
}
impl From<pc_repos::RepoError> for IssueApprovalError {
    fn from(e: pc_repos::RepoError) -> Self {
        Self::Pc(internal(e.to_string()))
    }
}

impl From<pc_repos::issue_approvals::IssueApprovalError> for IssueApprovalError {
    fn from(e: pc_repos::issue_approvals::IssueApprovalError) -> Self {
        use pc_repos::issue_approvals::IssueApprovalError as RepoErr;
        match e {
            RepoErr::IssueNotFound => Self::Validation("issue not found".into()),
            RepoErr::ApprovalNotFound => Self::Validation("approval not found".into()),
            RepoErr::IssuesNotFound => Self::Validation("one or more issues not found".into()),
            RepoErr::CrossCompany => Self::CrossCompany,
            RepoErr::Db(err) => Self::Db(err),
        }
    }
}
pub type IssueApprovalResult<T> = std::result::Result<T, IssueApprovalError>;

fn require_non_nil(id: Uuid, field: &str) -> IssueApprovalResult<()> {
    if id.is_nil() {
        Err(IssueApprovalError::Validation(format!(
            "{field} is required"
        )))
    } else {
        Ok(())
    }
}

#[derive(Clone)]
pub struct IssueApprovalService {
    db: Db,
    hooks: Vec<Arc<dyn IssueApprovalHook>>,
}

impl IssueApprovalService {
    pub fn new(db: Db) -> Self {
        Self { db, hooks: vec![] }
    }
    pub fn with_hooks(db: Db, hooks: Vec<Arc<dyn IssueApprovalHook>>) -> Self {
        Self { db, hooks }
    }
    pub fn add_hook(mut self, h: Arc<dyn IssueApprovalHook>) -> Self {
        self.hooks.push(h);
        self
    }
    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }
    fn repo(&self) -> IssueApprovalRepo {
        IssueApprovalRepo::new(&self.db)
    }
    async fn dispatch(&self, e: IssueApprovalHookEvent) {
        for h in &self.hooks {
            if let Err(err) = h.on_issue_approval_event(e.clone()).await {
                tracing::warn!(?err, "issue approval hook failed");
            }
        }
    }

    pub async fn list_approvals_for_issue(
        &self,
        issue_id: Uuid,
    ) -> IssueApprovalResult<Vec<ApprovalForIssueItem>> {
        require_non_nil(issue_id, "issueId")?;
        self.repo()
            .list_approvals_for_issue(issue_id)
            .await
            .map_err(|e| IssueApprovalError::Pc(internal(e.to_string())))
    }
    pub async fn list_issues_for_approval(
        &self,
        approval_id: Uuid,
    ) -> IssueApprovalResult<Vec<IssueForApprovalItem>> {
        require_non_nil(approval_id, "approvalId")?;
        self.repo()
            .list_issues_for_approval(approval_id)
            .await
            .map_err(|e| IssueApprovalError::Pc(internal(e.to_string())))
    }
    pub async fn link(
        &self,
        issue_id: Uuid,
        approval_id: Uuid,
        actor: Option<IssueApprovalLinkActor>,
    ) -> IssueApprovalResult<()> {
        require_non_nil(issue_id, "issueId")?;
        require_non_nil(approval_id, "approvalId")?;
        let link_actor = actor.unwrap_or_default();
        let link_res = self
            .repo()
            .link(
                issue_id,
                approval_id,
                Some(LinkActor {
                    agent_id: link_actor.agent_id,
                    user_id: link_actor.user_id,
                }),
            )
            .await;
        let row: IssueApprovalLinkRow = link_res
            .map_err(IssueApprovalError::from)?
            .ok_or(IssueApprovalError::CrossCompany)?;
        self.dispatch(IssueApprovalHookEvent::Linked {
            company_id: row.company_id,
            issue_id: row.issue_id,
            approval_id: row.approval_id,
        })
        .await;
        Ok(())
    }
    pub async fn unlink(&self, issue_id: Uuid, approval_id: Uuid) -> IssueApprovalResult<()> {
        require_non_nil(issue_id, "issueId")?;
        require_non_nil(approval_id, "approvalId")?;
        self.repo()
            .unlink(issue_id, approval_id)
            .await
            .map_err(|e| IssueApprovalError::Pc(internal(e.to_string())))?;
        // we don't know company here without re-reading; fire generic event
        self.dispatch(IssueApprovalHookEvent::Unlinked {
            company_id: Uuid::nil(),
            issue_id,
            approval_id,
        })
        .await;
        Ok(())
    }
    pub async fn link_many(
        &self,
        approval_id: Uuid,
        issue_ids: Vec<Uuid>,
    ) -> IssueApprovalResult<()> {
        require_non_nil(approval_id, "approvalId")?;
        if issue_ids.is_empty() {
            return Err(IssueApprovalError::Validation(
                "issue_ids must not be empty".into(),
            ));
        }
        for id in &issue_ids {
            require_non_nil(*id, "issueId")?;
        }
        let count = issue_ids.len();
        self.repo()
            .link_many_for_approval(approval_id, &issue_ids, None)
            .await?;
        self.dispatch(IssueApprovalHookEvent::BulkLinked {
            company_id: Uuid::nil(),
            approval_id,
            count,
        })
        .await;
        Ok(())
    }
}
