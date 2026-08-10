#![forbid(unsafe_code)]
//! Feedback vote domain service layer.
//!
//! See `lib.rs` for module-level docs.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub use pc_repos::feedback_vote::{FeedbackVoteRow, NewFeedbackVote};
use pc_repos::feedback_vote::FeedbackVoteRepo;
use pc_repos::Db;

use pc_errors::{internal, validation, Error as PcError, Result};

// =============================================================================
// R612: lifecycle events surfaced to hooks
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum FeedbackVoteHookEvent {
    Cast {
        company_id: Uuid,
        issue_id: Uuid,
        vote_id: Uuid,
        vote: String,
        author_user_id: String,
    },
}

// =============================================================================
// R612: hook trait
// =============================================================================

#[async_trait]
pub trait FeedbackVoteHook: Send + Sync {
    async fn on_feedback_vote_event(&self, _event: FeedbackVoteHookEvent) -> Result<()> {
        Ok(())
    }
}

pub struct NoopFeedbackVoteHook;
#[async_trait]
impl FeedbackVoteHook for NoopFeedbackVoteHook {}

#[derive(Default)]
pub struct RecordingFeedbackVoteHook {
    pub events: std::sync::Mutex<Vec<FeedbackVoteHookEvent>>,
}

#[async_trait]
impl FeedbackVoteHook for RecordingFeedbackVoteHook {
    async fn on_feedback_vote_event(&self, event: FeedbackVoteHookEvent) -> Result<()> {
        self.events.lock().expect("lock").push(event);
        Ok(())
    }
}

impl RecordingFeedbackVoteHook {
    #[must_use]
    pub fn events_snapshot(&self) -> Vec<FeedbackVoteHookEvent> {
        self.events.lock().expect("lock").clone()
    }

    pub fn clear(&self) {
        self.events.lock().expect("lock").clear();
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.events.lock().expect("lock").len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.events.lock().expect("lock").is_empty()
    }
}

// =============================================================================
// R612: error type
// =============================================================================

#[derive(Debug, thiserror::Error)]
pub enum FeedbackVoteError {
    #[error("validation: {0}")]
    Validation(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Pc(#[from] PcError),
}

impl From<pc_repos::RepoError> for FeedbackVoteError {
    fn from(e: pc_repos::RepoError) -> Self {
        Self::Pc(internal(e.to_string()))
    }
}

pub type FeedbackVoteResult<T> = std::result::Result<T, FeedbackVoteError>;

/// Allowed vote values. The DB stores arbitrary strings but only "up" /
/// "down" are accepted at the service layer.
pub const ALLOWED_VOTES: &[&str] = &["up", "down"];

// =============================================================================
// R612: input validation helpers
// =============================================================================

fn normalize_new(input: &NewFeedbackVote) -> Result<()> {
    if input.company_id.is_nil() {
        return Err(validation("companyId is required"));
    }
    if input.issue_id.is_nil() {
        return Err(validation("issueId is required"));
    }
    if input.target_type.trim().is_empty() {
        return Err(validation("targetType must not be empty"));
    }
    if input.target_id.trim().is_empty() {
        return Err(validation("targetId must not be empty"));
    }
    if input.author_user_id.trim().is_empty() {
        return Err(validation("authorUserId must not be empty"));
    }
    if !ALLOWED_VOTES.contains(&input.vote.as_str()) {
        return Err(validation(format!(
            "vote must be one of {ALLOWED_VOTES:?}, got {:?}",
            input.vote
        )));
    }
    Ok(())
}

// =============================================================================
// R612: FeedbackVoteService
// =============================================================================

#[derive(Clone)]
pub struct FeedbackVoteService {
    db: Db,
    hooks: Vec<Arc<dyn FeedbackVoteHook>>,
}

impl FeedbackVoteService {
    pub fn new(db: Db) -> Self {
        Self { db, hooks: Vec::new() }
    }

    pub fn with_hooks(db: Db, hooks: Vec<Arc<dyn FeedbackVoteHook>>) -> Self {
        Self { db, hooks }
    }

    pub fn add_hook(mut self, h: Arc<dyn FeedbackVoteHook>) -> Self {
        self.hooks.push(h);
        self
    }

    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }

    async fn dispatch(&self, event: FeedbackVoteHookEvent) {
        for h in &self.hooks {
            if let Err(e) = h.on_feedback_vote_event(event.clone()).await {
                tracing::warn!(?e, "feedback vote hook failed");
            }
        }
    }

    fn repo(&self) -> FeedbackVoteRepo<'_> {
        FeedbackVoteRepo::new(&self.db)
    }

    // -------------------------------------------------------------------------
    // Read paths (direct repo passthrough)
    // -------------------------------------------------------------------------

    pub async fn list_by_issue(
        &self,
        issue_id: Uuid,
        limit: i64,
    ) -> FeedbackVoteResult<Vec<FeedbackVoteRow>> {
        if issue_id.is_nil() {
            return Err(FeedbackVoteError::Validation("issueId is required".into()));
        }
        Ok(self.repo().list_by_issue(issue_id, limit).await?)
    }

    pub async fn get_by_id(&self, id: Uuid) -> FeedbackVoteResult<Option<FeedbackVoteRow>> {
        Ok(self.repo().get_by_id(id).await?)
    }

    pub async fn count_by_issue(&self, issue_id: Uuid) -> FeedbackVoteResult<i64> {
        if issue_id.is_nil() {
            return Err(FeedbackVoteError::Validation("issueId is required".into()));
        }
        Ok(self.repo().count_by_issue(issue_id).await?)
    }

    // -------------------------------------------------------------------------
    // Write paths
    // -------------------------------------------------------------------------

    /// Cast a feedback vote. The caller must supply the company_id (matching
    /// the issue's company). Mirrors Node `feedbackVoteRepo.create`.
    pub async fn cast(&self, input: NewFeedbackVote) -> FeedbackVoteResult<Uuid> {
        normalize_new(&input)?;
        let id = self.repo().create(&input).await?;
        self.dispatch(FeedbackVoteHookEvent::Cast {
            company_id: input.company_id,
            issue_id: input.issue_id,
            vote_id: id,
            vote: input.vote,
            author_user_id: input.author_user_id,
        })
        .await;
        Ok(id)
    }

    /// Cast a feedback vote given only the issue_id; resolves company_id from
    /// the issue row. Returns NotFound if the issue does not exist.
    /// Mirrors Node `feedbackVoteRepo.createForIssue`.
    pub async fn cast_for_issue(
        &self,
        issue_id: Uuid,
        target_type: &str,
        target_id: &str,
        author_user_id: &str,
        vote: &str,
        reason: Option<&str>,
    ) -> FeedbackVoteResult<Uuid> {
        if issue_id.is_nil() {
            return Err(FeedbackVoteError::Validation("issueId is required".into()));
        }
        if target_type.trim().is_empty() {
            return Err(FeedbackVoteError::Validation("targetType must not be empty".into()));
        }
        if target_id.trim().is_empty() {
            return Err(FeedbackVoteError::Validation("targetId must not be empty".into()));
        }
        if author_user_id.trim().is_empty() {
            return Err(FeedbackVoteError::Validation("authorUserId must not be empty".into()));
        }
        if !ALLOWED_VOTES.contains(&vote) {
            return Err(FeedbackVoteError::Validation(format!(
                "vote must be one of {ALLOWED_VOTES:?}, got {vote:?}"
            )));
        }

        let id = self
            .repo()
            .create_for_issue(issue_id, target_type, target_id, author_user_id, vote, reason)
            .await
            .map_err(|e| match e {
                sqlx::Error::RowNotFound => {
                    FeedbackVoteError::NotFound("Issue not found".into())
                }
                other => FeedbackVoteError::Db(other),
            })?;

        // Resolve company_id for the hook event by re-querying.
        let company_id = self
            .repo()
            .issue_company_id(issue_id)
            .await?
            .unwrap_or(Uuid::nil());

        self.dispatch(FeedbackVoteHookEvent::Cast {
            company_id,
            issue_id,
            vote_id: id,
            vote: vote.to_string(),
            author_user_id: author_user_id.to_string(),
        })
        .await;

        Ok(id)
    }
}

// =============================================================================
// Unit tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn make_valid_input() -> NewFeedbackVote {
        NewFeedbackVote {
            company_id: Uuid::new_v4(),
            issue_id: Uuid::new_v4(),
            target_type: "agent".into(),
            target_id: "agent-1".into(),
            author_user_id: "u1".into(),
            vote: "up".into(),
            reason: None,
        }
    }

    #[test]
    fn normalize_new_accepts_valid_up() {
        assert!(normalize_new(&make_valid_input()).is_ok());
    }

    #[test]
    fn normalize_new_accepts_valid_down() {
        let mut input = make_valid_input();
        input.vote = "down".into();
        assert!(normalize_new(&input).is_ok());
    }

    #[test]
    fn normalize_new_rejects_unknown_vote() {
        let mut input = make_valid_input();
        input.vote = "sideways".into();
        assert!(normalize_new(&input).is_err());
    }

    #[test]
    fn normalize_new_rejects_nil_company() {
        let mut input = make_valid_input();
        input.company_id = Uuid::nil();
        assert!(normalize_new(&input).is_err());
    }

    #[test]
    fn normalize_new_rejects_empty_author() {
        let mut input = make_valid_input();
        input.author_user_id = "".into();
        assert!(normalize_new(&input).is_err());
    }
}
