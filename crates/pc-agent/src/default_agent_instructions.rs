//! Default agent instructions bundle business service.
//!
//! 原 `pc-default-agent-instructions` crate 已下沉为本 crate 的 `default_agent_instructions` 子模块。

#![allow(unused_imports)]

use async_trait::async_trait;
use pc_errors::{Error as PcError, Result as PcResult};
use pc_repos::default_agent_instructions as repo;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;

pub use pc_repos::default_agent_instructions::{
    load_default_agent_instructions_bundle, resolve_default_agent_instructions_bundle_role,
    AgentInstructionsRole,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DefaultAgentInstructionsHookEvent {
    Resolved { role: String, file_count: usize },
}

#[async_trait]
pub trait DefaultAgentInstructionsHook: Send + Sync {
    async fn on_default_agent_instructions_event(
        &self,
        _event: DefaultAgentInstructionsHookEvent,
    ) -> PcResult<()> {
        Ok(())
    }
}

pub struct NoopDefaultHook;
#[async_trait]
impl DefaultAgentInstructionsHook for NoopDefaultHook {}

#[derive(Default)]
pub struct RecordingDefaultHook {
    pub events: std::sync::Mutex<Vec<DefaultAgentInstructionsHookEvent>>,
}
impl RecordingDefaultHook {
    pub fn events_snapshot(&self) -> Vec<DefaultAgentInstructionsHookEvent> {
        self.events.lock().expect("mutex").clone()
    }
    pub fn clear(&self) {
        self.events.lock().expect("mutex").clear()
    }
    pub fn len(&self) -> usize {
        self.events.lock().expect("mutex").len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
#[async_trait]
impl DefaultAgentInstructionsHook for RecordingDefaultHook {
    async fn on_default_agent_instructions_event(
        &self,
        e: DefaultAgentInstructionsHookEvent,
    ) -> PcResult<()> {
        self.events.lock().expect("mutex").push(e);
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DefaultAgentInstructionsError {
    #[error("role must not be empty")]
    EmptyRole,
    #[error(transparent)]
    Pc(#[from] PcError),
}
pub type DefaultAgentInstructionsResult<T> = std::result::Result<T, DefaultAgentInstructionsError>;

#[derive(Default, Clone)]
pub struct DefaultAgentInstructionsService {
    hooks: Vec<Arc<dyn DefaultAgentInstructionsHook>>,
}

impl DefaultAgentInstructionsService {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_hooks(hooks: Vec<Arc<dyn DefaultAgentInstructionsHook>>) -> Self {
        Self { hooks }
    }
    pub fn add_hook(mut self, h: Arc<dyn DefaultAgentInstructionsHook>) -> Self {
        self.hooks.push(h);
        self
    }
    pub fn hook_count(&self) -> usize {
        self.hooks.len()
    }
    async fn dispatch(&self, e: DefaultAgentInstructionsHookEvent) {
        for h in &self.hooks {
            if let Err(err) = h.on_default_agent_instructions_event(e.clone()).await {
                tracing::warn!(?err, "default agent instructions hook failed");
            }
        }
    }

    /// Resolve a role string to the canonical bundle role.
    /// Unknown roles fall back to `Default`, mirroring Node semantics.
    pub fn resolve_role(&self, role: &str) -> AgentInstructionsRole {
        resolve_default_agent_instructions_bundle_role(role)
    }

    /// Load the onboarding bundle for the given role string. Fires one hook per call.
    pub async fn load_bundle_for(
        &self,
        role: &str,
    ) -> DefaultAgentInstructionsResult<BTreeMap<&'static str, &'static str>> {
        if role.trim().is_empty() {
            return Err(DefaultAgentInstructionsError::EmptyRole);
        }
        let canonical = resolve_default_agent_instructions_bundle_role(role);
        let bundle = load_default_agent_instructions_bundle(canonical);
        self.dispatch(DefaultAgentInstructionsHookEvent::Resolved {
            role: role.to_string(),
            file_count: bundle.len(),
        })
        .await;
        Ok(bundle)
    }

    /// Synchronous variant for hot paths: loads the bundle for a canonical role (no role normalization, no hook).
    pub fn load_bundle_canonical(
        &self,
        role: AgentInstructionsRole,
    ) -> BTreeMap<&'static str, &'static str> {
        load_default_agent_instructions_bundle(role)
    }

    /// Returns the role as a stable snake_case string.
    pub fn role_str(&self, role: AgentInstructionsRole) -> &'static str {
        role.as_str()
    }
}

#[allow(dead_code)]
pub fn __repo_alias_for_doc() -> repo::AgentInstructionsRole {
    repo::AgentInstructionsRole::Default
}
