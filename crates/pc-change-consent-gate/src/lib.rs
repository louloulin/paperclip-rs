#![forbid(unsafe_code)]
//! `pc-change-consent-gate` —— Change-consent gate 业务服务。
//!
//! 对应 Node `server/src/services/change-consent-gate.ts`（232 行）。
//!
//! ## 设计目标
//!
//! - **Reflection Coach 写操作的硬门槛**：任何"modify agent instructions / profile / skill"
//!   类型的 mutation 必须先有一份"已 accepted、显示过 diff、未被消费"的 `request_confirmation`，
//!   否则抛 `Forbidden`。
//! - **防并发消费**：消费动作通过 `result->>'consumedByRunId' IS NULL` 的 UPDATE 锁，确保
//!   同一份 confirmation 不会被并发两次 mutation 抢用。
//! - **Legacy 兼容**：保留旧 `reflection-coach:*` target key 形状以兼容历史数据。
//!
//! ## 公共 API
//!
//! - [`ChangeConsentGateService::assert_consented`] —— 主入口
//! - [`AssertConsentedInput`] —— 输入 DTO
//! - [`helpers::payload_has_displayed_diff`] / [`helpers::request_confirmation_result_consumed`] —— 纯函数
//! - [`helpers::agent_*_change_target_key`] / [`helpers::skill_*_change_target_key`] —— target key 构造
//!
//! ## 设计原则
//!
//! - **高内聚**：gate 逻辑、legacy 展开、payload 判定集中在本 crate。
//! - **低耦合**：上游 HTTP 层只需构造 DTO + 调用 service。
//! - **真实测试**：e2e 测试打到真实 Postgres。

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
    AGENT_PROFILE_CHANGE_CONSENT_FIELDS,
    codes, mark_result_consumed, AssertConsentedInput, ChangeConsentError, ChangeConsentResult,
};
