#![forbid(unsafe_code)]

//! Issue thread interaction pure helpers.
//! R711: Direct port of issue-thread-interactions.ts pure functions.

use serde::{Deserialize, Serialize};

/// Actor kind for an interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InteractionActorKind {
    Agent,
    User,
    System,
}

impl InteractionActorKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::User => "user",
            Self::System => "system",
        }
    }
}

/// Interaction kind (subset relevant to deriveTargetType).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InteractionKind {
    RequestConfirmation,
    RequestCheckboxConfirmation,
    RequestItemVerdicts,
    AskUserQuestions,
    SuggestTasks,
    Other,
}

impl InteractionKind {
    pub fn from_str(s: &str) -> Self {
        match s {
            "request_confirmation" => Self::RequestConfirmation,
            "request_checkbox_confirmation" => Self::RequestCheckboxConfirmation,
            "request_item_verdicts" => Self::RequestItemVerdicts,
            "ask_user_questions" => Self::AskUserQuestions,
            "suggest_tasks" => Self::SuggestTasks,
            _ => Self::Other,
        }
    }
}

/// Issue terminal status check.
pub fn is_terminal_issue_status(status: &str) -> bool {
    status == "done" || status == "cancelled"
}

/// Coerce any number to a non-negative integer.
pub fn non_negative_integer(value: f64) -> u32 {
    if !value.is_finite() { return 0; }
    let truncated = value.trunc() as i64;
    if truncated < 0 { 0 } else { truncated as u32 }
}

/// Resolve actor kind from resolvedBy fields.
pub fn resolve_actor_kind(resolved_by_agent_id: Option<&str>, resolved_by_user_id: Option<&str>) -> InteractionActorKind {
    if resolved_by_agent_id.is_some() { return InteractionActorKind::Agent; }
    if resolved_by_user_id.is_some() { return InteractionActorKind::User; }
    InteractionActorKind::System
}

/// Resolve creator kind from createdBy fields. Returns None if neither.
pub fn resolve_creator_kind(created_by_agent_id: Option<&str>, created_by_user_id: Option<&str>) -> Option<InteractionActorKind> {
    if created_by_agent_id.is_some() { return Some(InteractionActorKind::Agent); }
    if created_by_user_id.is_some() { return Some(InteractionActorKind::User); }
    None
}

/// Derive target type from interaction kind + payload target.
pub fn derive_target_type(kind: InteractionKind, payload_target_type: Option<&str>) -> String {
    match kind {
        InteractionKind::RequestConfirmation
        | InteractionKind::RequestCheckboxConfirmation
        | InteractionKind::RequestItemVerdicts => {
            payload_target_type.unwrap_or("none").to_string()
        }
        _ => "none".to_string(),
    }
}

/// Should interaction be superseded on user comment?
pub fn should_supersede_interaction_on_user_comment(payload_supersede_on_user_comment: bool) -> bool {
    payload_supersede_on_user_comment
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    #[test]
    fn terminal_status_done() {
        assert!(is_terminal_issue_status("done"));
    }
    #[test]
    fn terminal_status_cancelled() {
        assert!(is_terminal_issue_status("cancelled"));
    }
    #[test]
    fn terminal_status_open_not_terminal() {
        assert!(!is_terminal_issue_status("open"));
        assert!(!is_terminal_issue_status("in_progress"));
        assert!(!is_terminal_issue_status("in_review"));
        assert!(!is_terminal_issue_status(""));
    }

    #[test]
    fn non_negative_integer_basic() {
        assert_eq!(non_negative_integer(5.0), 5);
        assert_eq!(non_negative_integer(0.0), 0);
        assert_eq!(non_negative_integer(5.7), 5);
        assert_eq!(non_negative_integer(-3.0), 0);
    }
    #[test]
    fn non_negative_integer_handles_nan_and_inf() {
        assert_eq!(non_negative_integer(f64::NAN), 0);
        assert_eq!(non_negative_integer(f64::INFINITY), 0);
        assert_eq!(non_negative_integer(f64::NEG_INFINITY), 0);
    }

    #[test]
    fn resolve_actor_kind_agent() {
        assert_eq!(resolve_actor_kind(Some("a-1"), None), InteractionActorKind::Agent);
    }
    #[test]
    fn resolve_actor_kind_user() {
        assert_eq!(resolve_actor_kind(None, Some("u-1")), InteractionActorKind::User);
    }
    #[test]
    fn resolve_actor_kind_agent_priority() {
        assert_eq!(resolve_actor_kind(Some("a"), Some("u")), InteractionActorKind::Agent);
    }
    #[test]
    fn resolve_actor_kind_system_fallback() {
        assert_eq!(resolve_actor_kind(None, None), InteractionActorKind::System);
    }

    #[test]
    fn resolve_creator_kind_returns_none_for_system() {
        assert_eq!(resolve_creator_kind(None, None), None);
    }
    #[test]
    fn resolve_creator_kind_agent() {
        assert_eq!(resolve_creator_kind(Some("a"), None), Some(InteractionActorKind::Agent));
    }
    #[test]
    fn resolve_creator_kind_user() {
        assert_eq!(resolve_creator_kind(None, Some("u")), Some(InteractionActorKind::User));
    }

    #[test]
    fn derive_target_type_request_confirmation_with_target() {
        assert_eq!(derive_target_type(InteractionKind::RequestConfirmation, Some("document")), "document");
    }
    #[test]
    fn derive_target_type_request_confirmation_no_target() {
        assert_eq!(derive_target_type(InteractionKind::RequestConfirmation, None), "none");
    }
    #[test]
    fn derive_target_type_other_kinds_return_none() {
        assert_eq!(derive_target_type(InteractionKind::AskUserQuestions, Some("document")), "none");
        assert_eq!(derive_target_type(InteractionKind::SuggestTasks, None), "none");
        assert_eq!(derive_target_type(InteractionKind::Other, Some("document")), "none");
    }

    #[test]
    fn should_supersede_basic() {
        assert!(should_supersede_interaction_on_user_comment(true));
        assert!(!should_supersede_interaction_on_user_comment(false));
    }

    #[test]
    fn interaction_kind_parsing() {
        assert_eq!(InteractionKind::from_str("request_confirmation"), InteractionKind::RequestConfirmation);
        assert_eq!(InteractionKind::from_str("suggest_tasks"), InteractionKind::SuggestTasks);
        assert_eq!(InteractionKind::from_str("unknown"), InteractionKind::Other);
    }

    #[test]
    fn actor_kind_serde() {
        let j = serde_json::to_string(&InteractionActorKind::Agent).unwrap();
        assert_eq!(j, "\"agent\"");
    }
}
