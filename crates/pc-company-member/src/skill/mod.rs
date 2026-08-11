//! Company-skill business service（原 `pc-company-skill` 已下沉）。
mod service;
pub use pc_repos::skill::{
    CompanySkillRow, NewCompanySkill, SkillSharingScope, SkillSourceType, SkillTrustLevel,
};
pub use service::{
    CompanySkillError, CompanySkillHook, CompanySkillHookEvent, CompanySkillService,
    NoopCompanySkillHook, RecordingCompanySkillHook,
};
