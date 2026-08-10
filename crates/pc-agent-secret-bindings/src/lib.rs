#![forbid(unsafe_code)]
//! `pc-agent-secret-bindings` —— Agent secret binding 同步层。
//!
//! 对应 Node `server/src/services/agent-secret-bindings.ts`（175 行）。
//!
//! 设计目标：
//!
//! - **纯函数 collector**：扫描 adapter config JSON，提取 `secret_ref` / `user_secret_ref` bindings。
//! - **依赖倒置 service**：通过 `SecretBindingSync` trait 注入实际 secrets 服务，便于单测。
//! - **零 DB**：本 crate 不持有 DB / Repo —— 完全在内存中完成。
//!
//! 公共 API：
//!
//! - [`collect_secret_refs`] / [`collect_user_secret_refs`] —— 提取器
//! - [`sync_agent_adapter_env_bindings`] —— 主流程
//! - [`sync_agent_adapter_env_bindings_fallback`] —— 兼容旧 secrets 服务的 env-binding 路径
//! - [`SecretRef`] / [`UserSecretRef`] / [`SecretVersionSelector`] / [`SecretProjectionClass`] —— DTO
//! - [`SecretBindingSync`] —— secrets 服务注入 trait
//!
//! 设计原则：
//!
//! - **高内聚**：binding 收集 + 同步逻辑集中在本 crate。
//! - **低耦合**：secrets 服务通过 trait 注入；上游调用方无需关心具体实现。
//! - **可测**：纯函数 collector + trait mock service，单测无需 DB。

mod collector;
mod service;
mod types;

pub use collector::{collect_secret_refs, collect_user_secret_refs};
pub use service::{sync_agent_adapter_env_bindings, sync_agent_adapter_env_bindings_fallback};
pub use types::{
    BindingTarget, BindingTargetType, SecretBindingError, SecretBindingResult, SecretBindingSync,
    SecretProjectionClass, SecretRef, SecretVersionSelector, UserSecretRef,
};
