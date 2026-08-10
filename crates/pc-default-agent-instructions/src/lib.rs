#![forbid(unsafe_code)]
//! Default agent instructions bundle business service.
mod service;
pub use pc_repos::default_agent_instructions as repo;
pub use service::{
    AgentInstructionsRole, DefaultAgentInstructionsError, DefaultAgentInstructionsHook,
    DefaultAgentInstructionsHookEvent, DefaultAgentInstructionsService, NoopDefaultHook,
    RecordingDefaultHook,
};
