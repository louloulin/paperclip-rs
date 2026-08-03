#![forbid(unsafe_code)]

//! Agent 领域服务与基于 kameo 的串行化命令入口。

mod actor;
mod instructions;
mod service;
mod snapshot;

pub use actor::{
    spawn_agent_supervisor, AgentSupervisor, ApproveAgentCommand, ClearAgentErrorCommand,
    CreateAgentCommand, CreateAgentKeyCommand, HireAgentCommand, PauseAgentCommand,
    ResetRuntimeSessionCommand, ResumeAgentCommand, RevokeAgentKeyCommand,
    RollbackConfigRevisionCommand, TerminateAgentCommand, UpdateAgentCommand,
    UpdateAgentPermissionsCommand,
};
pub use instructions::{
    AgentInstructionsBundle, AgentInstructionsFileDetail, AgentInstructionsFileSummary,
    AgentInstructionsService, DeleteInstructionsFileResult, InstructionAgent,
    InstructionsBundleUpdate, InstructionsUpdateResult, WriteInstructionsFileResult,
};
pub use pc_repos::agent::AgentTaskSessionRow as AgentTaskSession;
pub use service::{
    AgentApiKey, AgentConfigRevision, AgentHire, AgentKeyCreated, AgentPatch,
    AgentPermissionUpdate, AgentRuntimeState, AgentService, CreateAgent, CreateAgentKey,
    PauseReason, ResetRuntimeSession, ResetRuntimeState, RevisionContext,
};
pub use snapshot::{
    contains_redacted_marker, sanitize_snapshot_value, AgentConfigSnapshot, REDACTED_VALUE,
};
