#![forbid(unsafe_code)]
//! Company-skill business service.
mod service;
pub use pc_repos::skill::{
    CompanySkillRow, NewCompanySkill, SkillSharingScope, SkillSourceType, SkillTrustLevel,
};
pub use service::{
    CompanySkillError, CompanySkillHook, CompanySkillHookEvent, CompanySkillService,
    NoopCompanySkillHook, RecordingCompanySkillHook,
};
