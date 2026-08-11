//! R-INTEGRATION-2: Issue lifecycle hook that extracts mentions via pc-mentions.
//!
//! This module connects `pc-issues` (lifecycle hook system) with `pc-mentions`
//! (markdown mention parser). The hook:
//!
//! 1. On `on_created`: extracts mentions from the new issue's description
//! 2. On `on_commented`: extracts mentions from the comment body
//! 3. Records the extraction results in a thread-safe in-memory buffer
//!
//! Design notes:
//! - **No DB writes**: this hook is a *demonstration* of integration. Production
//!   mention persistence (writing into `issue_mentions` or similar) is a separate
//!   concern handled by the actual `IssueService` create paths, not by hooks.
//! - **Pure delegation**: the hook body is ~10 lines of pure delegation to
//!   `pc_mentions::extract_*` functions. Zero mention logic lives here.
//! - **Test-only side effects**: the recorded `ExtractedMentions` buffer is
//!   inspected by integration tests to verify the integration works.
//! - **`Send + Sync`**: required by the `IssueHook` trait, satisfied via
//!   `std::sync::Mutex`.
//!
//! ## Example
//!
//! ```no_run
//! use pc_issues::{IssueService, NoopIssueHook};
//! use pc_issues::mention_extraction_hook::MentionExtractionHook;
//! use std::sync::Arc;
//!
//! let extractor = Arc::new(MentionExtractionHook::default());
//! let service = IssueService::with_hooks(
//!     todo!(),
//!     vec![extractor.clone() as Arc<dyn IssueHook>],
//! );
//! // ... after service.create_comment(...), extractor.recorded() yields the buffer.
//! ```

use std::sync::Mutex;

use async_trait::async_trait;
use pc_core::Timestamp;
use pc_mentions::{
    extract_agent_mention_ids, extract_pipeline_mentions, extract_project_mention_ids,
    extract_routine_mention_ids, extract_skill_mention_ids, extract_user_mention_ids,
};
use pc_repos::issue::{IssueCommentRow, IssueRow};

use crate::{IssueHook, IssueServiceResult};

/// One extracted mention event — captures which source triggered the extraction
/// and which mention IDs were found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedMentions {
    pub source: MentionSource,
    pub issue_id: uuid::Uuid,
    pub comment_id: Option<uuid::Uuid>,
    pub project_ids: Vec<String>,
    pub agent_ids: Vec<String>,
    pub user_ids: Vec<String>,
    pub skill_ids: Vec<String>,
    pub routine_ids: Vec<String>,
    pub pipeline_mentions: Vec<String>,
}

/// Identifies which lifecycle event produced the extracted mentions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MentionSource {
    IssueCreated,
    IssueCommented,
}

impl MentionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IssueCreated => "issue_created",
            Self::IssueCommented => "issue_commented",
        }
    }
}

/// Extract every mention kind from a markdown string.
/// Pure delegation to pc-mentions; the hook body is intentionally tiny.
fn extract_all_from_markdown(
    markdown: &str,
) -> (
    Vec<String>,
    Vec<String>,
    Vec<String>,
    Vec<String>,
    Vec<String>,
    Vec<String>,
) {
    (
        extract_project_mention_ids(markdown),
        extract_agent_mention_ids(markdown),
        extract_user_mention_ids(markdown),
        extract_skill_mention_ids(markdown),
        extract_routine_mention_ids(markdown),
        extract_pipeline_mentions(markdown)
            .into_iter()
            .map(|p| p.pipeline_id)
            .collect(),
    )
}

/// Hook that records mention extractions into an in-memory buffer.
///
/// Use `recorded()` (or `take_recorded()`) to inspect what was extracted.
/// Thread-safe; safe to register as a single `Arc<dyn IssueHook>`.
#[derive(Default)]
pub struct MentionExtractionHook {
    buffer: Mutex<Vec<ExtractedMentions>>,
}

impl MentionExtractionHook {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read-only snapshot of all recorded extractions.
    pub fn recorded(&self) -> Vec<ExtractedMentions> {
        self.buffer.lock().expect("mention hook poisoned").clone()
    }

    /// Take ownership of recorded extractions, leaving the buffer empty.
    pub fn take_recorded(&self) -> Vec<ExtractedMentions> {
        std::mem::take(&mut *self.buffer.lock().expect("mention hook poisoned"))
    }

    /// Count of recorded extractions (cheap O(1)).
    pub fn len(&self) -> usize {
        self.buffer.lock().expect("mention hook poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn record(&self, mentions: ExtractedMentions) {
        self.buffer
            .lock()
            .expect("mention hook poisoned")
            .push(mentions);
    }
}

#[async_trait]
impl IssueHook for MentionExtractionHook {
    async fn on_created(&self, row: &IssueRow) -> IssueServiceResult<()> {
        let description = row.description.as_deref().unwrap_or("");
        let (projects, agents, users, skills, routines, pipelines) =
            extract_all_from_markdown(description);
        self.record(ExtractedMentions {
            source: MentionSource::IssueCreated,
            issue_id: row.id,
            comment_id: None,
            project_ids: projects,
            agent_ids: agents,
            user_ids: users,
            skill_ids: skills,
            routine_ids: routines,
            pipeline_mentions: pipelines,
        });
        Ok(())
    }

    async fn on_commented(
        &self,
        parent_issue: &IssueRow,
        comment: &IssueCommentRow,
    ) -> IssueServiceResult<()> {
        let (projects, agents, users, skills, routines, pipelines) =
            extract_all_from_markdown(&comment.body);
        self.record(ExtractedMentions {
            source: MentionSource::IssueCommented,
            issue_id: parent_issue.id,
            comment_id: Some(comment.id),
            project_ids: projects,
            agent_ids: agents,
            user_ids: users,
            skill_ids: skills,
            routine_ids: routines,
            pipeline_mentions: pipelines,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // (chrono::Utc not needed; pc_core::Timestamp::now() used instead)

    fn make_issue(description: Option<&str>) -> IssueRow {
        IssueRow {
            id: uuid::Uuid::new_v4(),
            company_id: uuid::Uuid::new_v4(),
            project_id: None,
            project_workspace_id: None,
            goal_id: None,
            parent_id: None,
            title: "t".into(),
            description: description.map(|s| s.to_string()),
            status: "todo".into(),
            work_mode: "standard".into(),
            harness_kind: None,
            priority: "normal".into(),
            assignee_agent_id: None,
            assignee_user_id: None,
            checkout_run_id: None,
            execution_run_id: None,
            execution_agent_name_key: None,
            execution_locked_at: None,
            created_by_agent_id: None,
            created_by_user_id: None,
            responsible_user_id: None,
            issue_number: None,
            identifier: None,
            origin_kind: "user".into(),
            origin_id: None,
            origin_run_id: None,
            origin_fingerprint: "x".into(),
            request_depth: 0,
            billing_code: None,
            assignee_adapter_overrides: None,
            execution_policy: None,
            execution_state: None,
            monitor_next_check_at: None,
            monitor_wake_requested_at: None,
            monitor_last_triggered_at: None,
            monitor_attempt_count: 0,
            monitor_notes: None,
            monitor_scheduled_by: None,
            execution_workspace_id: None,
            execution_workspace_preference: None,
            execution_workspace_settings: None,
            source_trust: None,
            unblock_descriptor: None,
            blocked_transition_at: None,
            blocked_owner_notified_at: None,
            started_at: None,
            completed_at: None,
            cancelled_at: None,
            hidden_at: None,
            created_at: pc_core::Timestamp::now(),
            updated_at: pc_core::Timestamp::now(),
        }
    }

    fn make_comment(issue_id: uuid::Uuid, body: &str) -> IssueCommentRow {
        IssueCommentRow {
            id: uuid::Uuid::new_v4(),
            company_id: uuid::Uuid::new_v4(),
            issue_id,
            author_agent_id: None,
            author_user_id: None,
            body: body.to_string(),
            presentation: None,
            metadata: None,
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
        }
    }

    #[tokio::test]
    async fn on_created_extracts_from_description() {
        let hook = MentionExtractionHook::new();
        let row = make_issue(Some(
            "Hello [p1](project://proj-uuid-1) and [u2](user://user-uuid-2)!",
        ));
        hook.on_created(&row).await.unwrap();
        assert_eq!(hook.len(), 1);
        let recorded = hook.take_recorded();
        let r = recorded.first().unwrap();
        assert_eq!(r.source, MentionSource::IssueCreated);
        assert_eq!(r.issue_id, row.id);
        assert!(!r.project_ids.is_empty(), "should find 1+ project mentions");
        assert!(!r.user_ids.is_empty(), "should find 1+ user mentions");
    }

    #[tokio::test]
    async fn on_created_with_no_description_records_empty() {
        let hook = MentionExtractionHook::new();
        let row = make_issue(None);
        hook.on_created(&row).await.unwrap();
        let r = hook.take_recorded().into_iter().next().unwrap();
        assert_eq!(r.source, MentionSource::IssueCreated);
        assert!(r.project_ids.is_empty());
        assert!(r.agent_ids.is_empty());
    }

    #[tokio::test]
    async fn on_commented_extracts_from_body() {
        let hook = MentionExtractionHook::new();
        let row = make_issue(None);
        let comment = make_comment(row.id, "cc [a1](agent://agent-uuid-1)");
        hook.on_commented(&row, &comment).await.unwrap();
        let r = hook.take_recorded().into_iter().next().unwrap();
        assert_eq!(r.source, MentionSource::IssueCommented);
        assert_eq!(r.comment_id, Some(comment.id));
        assert_eq!(r.issue_id, row.id);
        assert!(!r.agent_ids.is_empty());
    }

    #[test]
    fn mention_source_as_str_stable() {
        assert_eq!(MentionSource::IssueCreated.as_str(), "issue_created");
        assert_eq!(MentionSource::IssueCommented.as_str(), "issue_commented");
    }

    #[test]
    fn is_empty_default_true() {
        let hook = MentionExtractionHook::new();
        assert!(hook.is_empty());
        assert_eq!(hook.len(), 0);
    }

    #[test]
    fn extract_all_from_markdown_handles_empty() {
        let (p, a, u, s, r, pi) = extract_all_from_markdown("");
        assert!(p.is_empty());
        assert!(a.is_empty());
        assert!(u.is_empty());
        assert!(s.is_empty());
        assert!(r.is_empty());
        assert!(pi.is_empty());
    }
}
