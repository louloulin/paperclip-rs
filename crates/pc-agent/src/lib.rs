#![forbid(unsafe_code)]

//! Agent 领域服务与基于 kameo 的串行化命令入口。

mod actor;
mod built_in_agent_metadata;
mod instructions;
mod permissions;
mod service;
mod snapshot;

pub use actor::{
    spawn_agent_supervisor, AgentSupervisor, ApproveAgentCommand, ClearAgentErrorCommand,
    CreateAgentCommand, CreateAgentKeyCommand, HireAgentCommand, PauseAgentCommand,
    ResetRuntimeSessionCommand, ResumeAgentCommand, RevokeAgentKeyCommand,
    RollbackConfigRevisionCommand, TerminateAgentCommand, UpdateAgentCommand,
    UpdateAgentPermissionsCommand,
};
pub use built_in_agent_metadata::{
    built_in_agent_markers_equal, read_built_in_agent_marker, with_built_in_agent_marker,
    BuiltInAgentMarker, BUILT_IN_AGENT_METADATA_KEY,
};
pub use instructions::{
    AgentInstructionsBundle, AgentInstructionsFileDetail, AgentInstructionsFileSummary,
    AgentInstructionsService, DeleteInstructionsFileResult, InstructionAgent,
    InstructionsBundleUpdate, InstructionsUpdateResult, WriteInstructionsFileResult,
};
pub use pc_repos::agent::AgentTaskSessionRow as AgentTaskSession;
pub use permissions::{
    default_permissions_for_role, normalize_agent_permissions, AgentPermissions,
};
pub use service::{
    AgentApiKey, AgentConfigRevision, AgentHire, AgentKeyCreated, AgentPatch,
    AgentPermissionUpdate, AgentRuntimeState, AgentService, CreateAgent, CreateAgentKey,
    PauseReason, ResetRuntimeSession, ResetRuntimeState, RevisionContext,
};
pub use snapshot::{
    contains_redacted_marker, sanitize_snapshot_value, AgentConfigSnapshot, REDACTED_VALUE,
};
