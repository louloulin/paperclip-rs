use kameo::{
    actor::{Actor, ActorRef, Spawn},
    error::Infallible,
    message::{Context, Message},
};
use pc_errors::Result;
use pc_repos::{agent::AgentRow, Db};
use uuid::Uuid;

use crate::{
    AgentApiKey, AgentHire, AgentKeyCreated, AgentPatch, AgentPermissionUpdate, AgentService,
    CreateAgent, CreateAgentKey, PauseReason, ResetRuntimeSession, ResetRuntimeState,
    RevisionContext,
};

pub struct AgentSupervisor {
    service: AgentService,
}

impl Actor for AgentSupervisor {
    type Args = AgentService;
    type Error = Infallible;

    async fn on_start(
        service: Self::Args,
        _actor_ref: ActorRef<Self>,
    ) -> std::result::Result<Self, Self::Error> {
        Ok(Self { service })
    }
}

pub struct CreateAgentCommand(pub CreateAgent);

pub struct UpdateAgentCommand {
    pub id: Uuid,
    pub patch: AgentPatch,
    pub revision: RevisionContext,
}

pub struct RollbackConfigRevisionCommand {
    pub id: Uuid,
    pub revision_id: Uuid,
    pub actor: RevisionContext,
}

pub struct PauseAgentCommand {
    pub id: Uuid,
    pub reason: PauseReason,
}

pub struct ResumeAgentCommand(pub Uuid);

pub struct ClearAgentErrorCommand(pub Uuid);

pub struct TerminateAgentCommand(pub Uuid);

pub struct ResetRuntimeSessionCommand {
    pub id: Uuid,
    pub input: ResetRuntimeSession,
}

pub struct CreateAgentKeyCommand {
    pub id: Uuid,
    pub input: CreateAgentKey,
}

pub struct RevokeAgentKeyCommand {
    pub agent_id: Uuid,
    pub key_id: Uuid,
}

pub struct HireAgentCommand {
    pub input: CreateAgent,
    pub actor: RevisionContext,
}

pub struct ApproveAgentCommand(pub Uuid);

pub struct UpdateAgentPermissionsCommand {
    pub id: Uuid,
    pub input: AgentPermissionUpdate,
}

impl Message<CreateAgentCommand> for AgentSupervisor {
    type Reply = Result<AgentRow>;

    async fn handle(
        &mut self,
        message: CreateAgentCommand,
        _context: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.service.create(message.0).await
    }
}

impl Message<UpdateAgentCommand> for AgentSupervisor {
    type Reply = Result<Option<AgentRow>>;

    async fn handle(
        &mut self,
        message: UpdateAgentCommand,
        _context: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.service
            .update(message.id, message.patch, message.revision)
            .await
    }
}

impl Message<RollbackConfigRevisionCommand> for AgentSupervisor {
    type Reply = Result<Option<AgentRow>>;

    async fn handle(
        &mut self,
        message: RollbackConfigRevisionCommand,
        _context: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.service
            .rollback_config_revision(message.id, message.revision_id, message.actor)
            .await
    }
}

impl Message<PauseAgentCommand> for AgentSupervisor {
    type Reply = Result<Option<AgentRow>>;

    async fn handle(
        &mut self,
        message: PauseAgentCommand,
        _context: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.service.pause(message.id, message.reason).await
    }
}

impl Message<ResumeAgentCommand> for AgentSupervisor {
    type Reply = Result<Option<AgentRow>>;

    async fn handle(
        &mut self,
        message: ResumeAgentCommand,
        _context: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.service.resume(message.0).await
    }
}

impl Message<ClearAgentErrorCommand> for AgentSupervisor {
    type Reply = Result<Option<AgentRow>>;

    async fn handle(
        &mut self,
        message: ClearAgentErrorCommand,
        _context: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.service.clear_error(message.0).await
    }
}

impl Message<TerminateAgentCommand> for AgentSupervisor {
    type Reply = Result<Option<AgentRow>>;

    async fn handle(
        &mut self,
        message: TerminateAgentCommand,
        _context: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.service.terminate(message.0).await
    }
}

impl Message<ResetRuntimeSessionCommand> for AgentSupervisor {
    type Reply = Result<Option<ResetRuntimeState>>;

    async fn handle(
        &mut self,
        message: ResetRuntimeSessionCommand,
        _context: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.service
            .reset_runtime_session(message.id, message.input)
            .await
    }
}

impl Message<CreateAgentKeyCommand> for AgentSupervisor {
    type Reply = Result<AgentKeyCreated>;

    async fn handle(
        &mut self,
        message: CreateAgentKeyCommand,
        _context: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.service.create_api_key(message.id, message.input).await
    }
}

impl Message<RevokeAgentKeyCommand> for AgentSupervisor {
    type Reply = Result<Option<AgentApiKey>>;

    async fn handle(
        &mut self,
        message: RevokeAgentKeyCommand,
        _context: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.service
            .revoke_api_key(message.agent_id, message.key_id)
            .await
    }
}

impl Message<HireAgentCommand> for AgentSupervisor {
    type Reply = Result<AgentHire>;

    async fn handle(
        &mut self,
        message: HireAgentCommand,
        _context: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.service.hire(message.input, message.actor).await
    }
}

impl Message<ApproveAgentCommand> for AgentSupervisor {
    type Reply = Result<Option<AgentRow>>;

    async fn handle(
        &mut self,
        message: ApproveAgentCommand,
        _context: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.service.approve(message.0).await
    }
}

impl Message<UpdateAgentPermissionsCommand> for AgentSupervisor {
    type Reply = Result<Option<AgentRow>>;

    async fn handle(
        &mut self,
        message: UpdateAgentPermissionsCommand,
        _context: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.service
            .update_permissions(message.id, message.input)
            .await
    }
}

#[must_use]
pub fn spawn_agent_supervisor(db: Db) -> ActorRef<AgentSupervisor> {
    AgentSupervisor::spawn(AgentService::new(db))
}
