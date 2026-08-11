//! Change-consent gate 业务服务。
//!
//! 对应 Node `server/src/services/change-consent-gate.ts`（232 行）1:1 复刻。
//! （原 `pc-change-consent-gate` crate 已下沉到 `pc-approvals::change_consent_gate`）。

mod helpers;
mod service;
mod types;

pub use helpers::{
    agent_instructions_change_target_key, agent_profile_change_target_key,
    expand_target_keys_for_legacy_compatibility, payload_has_displayed_diff,
    request_confirmation_result_consumed, skill_change_target_key, skill_import_change_target_key,
    skill_slug_change_target_key, skills_scan_projects_change_target_key,
    touches_agent_profile_change_consent_fields,
};
pub use service::ChangeConsentGateService;
pub use types::{
    codes, mark_result_consumed, AssertConsentedInput, ChangeConsentError, ChangeConsentResult,
    AGENT_PROFILE_CHANGE_CONSENT_FIELDS,
};
