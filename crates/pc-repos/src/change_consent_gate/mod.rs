//! Reflection Coach 受保护变更的单次 consent gate。

mod keys;
mod repository;
mod rules;

pub use keys::{
    agent_instructions_change_target_key, agent_profile_change_target_key,
    skill_change_target_key, skill_import_change_target_key, skill_slug_change_target_key,
    skills_scan_projects_change_target_key, touches_agent_profile_change_consent_fields,
};
pub use repository::ChangeConsentGateRepo;

use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct AssertConsentInput {
    pub company_id: Uuid,
    pub actor_agent_id: Option<Uuid>,
    pub actor_run_id: Option<Uuid>,
    pub target_keys: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ChangeConsentError {
    #[error("Reflection Coach mutations require a run id")]
    RunIdRequired,
    #[error("Reflection Coach mutation target is not gateable")]
    TargetRequired,
    #[error("Reflection Coach mutations require an accepted request_confirmation with a displayed diff for this target, created in a previous run and not already consumed")]
    GateRequired,
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

pub type ChangeConsentResult<T> = Result<T, ChangeConsentError>;

#[cfg(test)]
mod tests;
