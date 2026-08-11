#![forbid(unsafe_code)]

//! Agent 领域服务与基于 kameo 的串行化命令入口。

pub mod action_audit;
mod actor;
mod agent_assignability;
mod built_in_agent_metadata;
mod default_agent_instructions;
mod instructions;
pub mod invokability;
mod permissions;
pub mod secret_bindings;
mod service;
mod skill_selection;
mod snapshot;
pub mod start_lock;

pub use actor::{
    spawn_agent_supervisor, AgentSupervisor, ApproveAgentCommand, ClearAgentErrorCommand,
    CreateAgentCommand, CreateAgentKeyCommand, HireAgentCommand, PauseAgentCommand,
    ResetRuntimeSessionCommand, ResumeAgentCommand, RevokeAgentKeyCommand,
    RollbackConfigRevisionCommand, TerminateAgentCommand, UpdateAgentCommand,
    UpdateAgentPermissionsCommand,
};
pub use agent_assignability::{
    assert_assignable_agent, AgentAssignmentConflictDetail, AgentAssignmentConflictReason,
    AgentAssignmentError, AgentAssignmentKind,
};
pub use built_in_agent_metadata::{
    built_in_agent_markers_equal, read_built_in_agent_marker, with_built_in_agent_marker,
    BuiltInAgentMarker, BUILT_IN_AGENT_METADATA_KEY,
};
pub use default_agent_instructions::{
    load_default_agent_instructions_bundle, resolve_default_agent_instructions_bundle_role,
    AgentInstructionsRole as DefaultAgentInstructionsRole, DefaultAgentInstructionsError,
    DefaultAgentInstructionsHook, DefaultAgentInstructionsHookEvent,
    DefaultAgentInstructionsResult, DefaultAgentInstructionsService, NoopDefaultHook,
    RecordingDefaultHook,
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
    is_uuid_like, normalize_agent_url_key, AgentApiKey, AgentConfigRevision, AgentHire, AgentHook,
    AgentKeyCreated, AgentLifecycleEvent, AgentPatch, AgentPermissionUpdate, AgentRuntimeState,
    AgentService, ChainOfCommandNode, CreateAgent, CreateAgentKey, NoopAgentHook, OrgChartNode,
    PauseReason, RecordingAgentHook, ResetRuntimeSession, ResetRuntimeState, ResolveByRefResult,
    RevisionContext,
};
pub use skill_selection::{
    skill_version_selection_map, SkillSelectionEntry, SkillVersionSelectionOptions,
};
pub use snapshot::{
    contains_redacted_marker, sanitize_snapshot_value, AgentConfigSnapshot, REDACTED_VALUE,
};
