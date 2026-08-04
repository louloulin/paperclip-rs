use axum::http::HeaderMap;
use uuid::Uuid;

use pc_repos::change_consent_gate::{
    AssertConsentInput, ChangeConsentError, ChangeConsentGateRepo,
};

use crate::{ApiError, ApiResult, AppState};

pub(crate) async fn assert_agent_change_consented(
    state: &AppState,
    headers: &HeaderMap,
    company_id: Uuid,
    target_keys: Vec<String>,
) -> ApiResult<()> {
    let actor_agent_id = headers
        .get("x-paperclip-agent-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| Uuid::parse_str(value).ok());
    if actor_agent_id.is_none() {
        return Ok(());
    }
    let actor_run_id = headers
        .get("x-paperclip-run-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| Uuid::parse_str(value).ok());
    ChangeConsentGateRepo::new(&state.db)
        .assert_consented(AssertConsentInput {
            company_id,
            actor_agent_id,
            actor_run_id,
            target_keys,
        })
        .await
        .map_err(|error| match error {
            ChangeConsentError::Db(error) => ApiError::Internal(error.to_string()),
            other => ApiError::Forbidden(other.to_string()),
        })?;
    Ok(())
}
