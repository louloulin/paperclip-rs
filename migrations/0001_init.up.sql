-- 0001_init.up.sql
--
-- Multica-rs initial schema (对应 multica/server/migrations/001_init.up.sql 子集)。
-- 与 upstream multica schema 1:1 对齐。
--
-- 本文件作为 M0 占位：仅包含核心实体表结构；后续 milestone 持续追加
-- 034_projects、042_autopilot、084_squad、285_plugin_lifecycle_v1、
-- 509_issue_wakeup、124_channel_generalization、216_vcs_integration、
-- 314_workspace_mcp_config 等迁移。

-- Enable extensions
CREATE EXTENSION IF NOT EXISTS "pgcrypto";
CREATE EXTENSION IF NOT EXISTS "pg_trgm";

-- Users
CREATE TABLE "user" (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL,
    email TEXT UNIQUE NOT NULL,
    avatar_url TEXT,
    email_verified_at TIMESTAMPTZ,
    language TEXT,
    timezone TEXT,
    profile_description TEXT,
    onboarded_at TIMESTAMPTZ,
    onboarding_state JSONB,
    starter_content_state JSONB,
    cloud_waitlist_at TIMESTAMPTZ,
    onboarding_runtime_choice TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Workspaces
CREATE TABLE workspace (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL,
    slug TEXT UNIQUE NOT NULL,
    description TEXT,
    avatar_url TEXT,
    settings JSONB NOT NULL DEFAULT '{}',
    attribution_fail_closed BOOLEAN NOT NULL DEFAULT false,
    private_plugin_identity JSONB,
    archived_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Members (user <-> workspace)
CREATE TABLE member (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('owner', 'admin', 'member', 'guest')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(workspace_id, user_id)
);

-- Workspace invitation
CREATE TABLE workspace_invitation (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
    email TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('admin', 'member', 'guest')),
    invited_by_user_id UUID NOT NULL REFERENCES "user"(id),
    token TEXT UNIQUE NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    accepted_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Agents
CREATE TABLE agent (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    description TEXT,
    avatar_url TEXT,
    visibility TEXT NOT NULL DEFAULT 'workspace' CHECK (visibility IN ('workspace', 'private')),
    status TEXT NOT NULL DEFAULT 'offline' CHECK (status IN ('idle', 'working', 'blocked', 'error', 'offline')),
    max_concurrent_tasks INT NOT NULL DEFAULT 1,
    owner_id UUID REFERENCES "user"(id),
    archived_at TIMESTAMPTZ,
    service_tier TEXT,
    model TEXT,
    thinking_level TEXT,
    disabled_runtime_skills JSONB NOT NULL DEFAULT '[]',
    starter_prompts JSONB NOT NULL DEFAULT '[]',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(workspace_id, name)
);

-- Agent runtimes (host machines / cloud workspaces)
CREATE TABLE agent_runtime (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    runtime_mode TEXT NOT NULL CHECK (runtime_mode IN ('local', 'cloud')),
    profile TEXT NOT NULL,                 -- 26 个 runtime profile 之一
    status TEXT NOT NULL DEFAULT 'offline' CHECK (status IN ('online', 'offline')),
    timezone TEXT,
    last_seen_at TIMESTAMPTZ,
    online_since TIMESTAMPTZ,
    daemon_uuid TEXT,
    owner_user_id UUID REFERENCES "user"(id),
    visibility TEXT NOT NULL DEFAULT 'workspace' CHECK (visibility IN ('workspace', 'private')),
    custom_name TEXT,
    runtime_type TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(workspace_id, name)
);
CREATE INDEX agent_runtime_workspace_id_idx ON agent_runtime(workspace_id);
CREATE INDEX agent_runtime_status_idx ON agent_runtime(status);

-- Runtime profile add-up registry (kept in code; DB table for persistence)
CREATE TABLE runtime_profile (
    name TEXT PRIMARY KEY,                       -- "claude-code" / "codex" / ...
    cli_command TEXT NOT NULL,                   -- claude|codex|cursor-agent|...
    enabled_by_default BOOLEAN NOT NULL DEFAULT true,
    description TEXT,
    added_in_migration TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Issues
CREATE TABLE issue (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
    number INT NOT NULL,
    identifier TEXT NOT NULL,                    -- "LUM-1234" form
    title TEXT NOT NULL,
    description TEXT,
    status TEXT NOT NULL DEFAULT 'backlog',
    status_name TEXT,
    priority TEXT NOT NULL DEFAULT 'none' CHECK (priority IN ('urgent', 'high', 'medium', 'low', 'none')),
    assignee_type TEXT CHECK (assignee_type IN ('user', 'agent', 'squad', 'autopilot')),
    assignee_id TEXT,
    creator_type TEXT NOT NULL CHECK (creator_type IN ('user', 'agent', 'system')),
    creator_id TEXT NOT NULL,
    parent_issue_id UUID REFERENCES issue(id) ON DELETE SET NULL,
    project_id UUID,
    position DOUBLE PRECISION NOT NULL DEFAULT 0,
    stage INT,
    start_date DATE,
    due_date DATE,
    last_activity_at TIMESTAMPTZ,
    revision BIGINT NOT NULL DEFAULT 1,
    metadata JSONB NOT NULL DEFAULT '{}',
    properties JSONB NOT NULL DEFAULT '{}',
    triage_state TEXT,
    origin TEXT,
    origin_task_id UUID,
    source_context_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(workspace_id, number)
);
CREATE INDEX issue_workspace_status_position_idx ON issue(workspace_id, status, position);
CREATE INDEX issue_workspace_assignee_idx ON issue(workspace_id, assignee_type, assignee_id);
CREATE INDEX issue_parent_idx ON issue(parent_issue_id);
CREATE INDEX issue_project_idx ON issue(project_id);

-- Issue status catalog
CREATE TABLE issue_status (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    key TEXT NOT NULL,
    category TEXT NOT NULL CHECK (category IN ('open', 'closed')),
    icon TEXT,
    position DOUBLE PRECISION NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(workspace_id, key)
);

-- Comments
CREATE TABLE comment (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
    issue_id UUID NOT NULL REFERENCES issue(id) ON DELETE CASCADE,
    parent_id UUID REFERENCES comment(id) ON DELETE CASCADE,
    author_type TEXT NOT NULL CHECK (author_type IN ('user', 'agent', 'system', 'plugin', 'squad', 'autopilot')),
    author_id TEXT NOT NULL,
    body TEXT NOT NULL,
    source_task_id UUID,
    routing_escalation TEXT,
    revision BIGINT NOT NULL DEFAULT 1,
    resolved_at TIMESTAMPTZ,
    deleted_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX comment_issue_idx ON comment(issue_id);
CREATE INDEX comment_parent_idx ON comment(parent_id);

-- Projects
CREATE TABLE project (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    slug TEXT NOT NULL,
    description TEXT,
    icon TEXT,
    priority TEXT NOT NULL DEFAULT 'none' CHECK (priority IN ('urgent', 'high', 'medium', 'low', 'none')),
    start_date DATE,
    target_date DATE,
    archived_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(workspace_id, slug)
);
CREATE INDEX project_workspace_idx ON project(workspace_id);

-- Project resources (links between project and other entities)
CREATE TABLE project_resource (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES project(id) ON DELETE CASCADE,
    resource_type TEXT NOT NULL,
    resource_id UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(project_id, resource_type, resource_id)
);

-- Agent task queue
CREATE TABLE agent_task_queue (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
    agent_id UUID NOT NULL REFERENCES agent(id) ON DELETE CASCADE,
    issue_id UUID NOT NULL REFERENCES issue(id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'queued' CHECK (status IN ('queued', 'running', 'terminal_completed', 'terminal_cancelled', 'terminal_failed', 'delegated_failure')),
    session_id TEXT,
    retired_session_id TEXT,
    dispatched_at TIMESTAMPTZ,
    lease_expires_at TIMESTAMPTZ,
    terminal_completed_at TIMESTAMPTZ,
    retry_count INT NOT NULL DEFAULT 0,
    source_task_id UUID,
    trigger_comment_id UUID,
    delegated_failure_evidence JSONB,
    initiator_user_id UUID,
    runtime_id UUID REFERENCES agent_runtime(id),
    runtime_mcp_overlay JSONB,
    runtime_connected_apps JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX agent_task_queue_workspace_idx ON agent_task_queue(workspace_id);
CREATE INDEX agent_task_queue_agent_id_keyset_idx ON agent_task_queue(agent_id, created_at DESC, id DESC);

-- Chat sessions
CREATE TABLE chat_session (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    agent_id UUID REFERENCES agent(id),
    runtime_id UUID REFERENCES agent_runtime(id),
    project_id UUID REFERENCES project(id),
    title TEXT,
    pinned BOOLEAN NOT NULL DEFAULT false,
    unread_since TIMESTAMPTZ,
    read_cursor TEXT,
    pinned_agent BOOLEAN NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Chat messages
CREATE TABLE chat_message (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id UUID NOT NULL REFERENCES chat_session(id) ON DELETE CASCADE,
    workspace_id UUID NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('user', 'assistant', 'system', 'tool')),
    kind TEXT NOT NULL DEFAULT 'text',
    content TEXT NOT NULL,
    task_id UUID,
    tool_call_id TEXT,
    input_owner TEXT,
    elapsed_ms INT,
    truncated BOOLEAN NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Inbox items
CREATE TABLE inbox_item (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    issue_id UUID REFERENCES issue(id) ON DELETE SET NULL,
    actor_type TEXT NOT NULL CHECK (actor_type IN ('user', 'agent', 'system', 'autopilot', 'squad', 'channel')),
    actor_id TEXT NOT NULL,
    category TEXT NOT NULL,
    title TEXT NOT NULL,
    body TEXT,
    read_at TIMESTAMPTZ,
    archived_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Skills
CREATE TABLE skill (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    slug TEXT NOT NULL,
    description TEXT,
    body TEXT NOT NULL,
    visibility TEXT NOT NULL DEFAULT 'workspace' CHECK (visibility IN ('workspace', 'private')),
    enabled BOOLEAN NOT NULL DEFAULT true,
    owner_id UUID REFERENCES "user"(id),
    plugin_key TEXT,
    plugin_installation_id UUID,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(workspace_id, slug)
);

-- Squads
CREATE TABLE squad (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    slug TEXT NOT NULL,
    description TEXT,
    avatar_url TEXT,
    instructions TEXT,
    leader_agent_id UUID REFERENCES agent(id),
    archived_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(workspace_id, slug)
);

-- Autopilot
CREATE TABLE autopilot (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    description TEXT,
    enabled BOOLEAN NOT NULL DEFAULT true,
    trigger TEXT NOT NULL CHECK (trigger IN ('manual', 'cron', 'webhook', 'event', 'issue_assigned', 'issue_closed')),
    cron_expression TEXT,
    webhook_url TEXT,
    trigger_event_filters JSONB NOT NULL DEFAULT '[]',
    squad_id UUID REFERENCES squad(id),
    project_id UUID REFERENCES project(id),
    rule_version BIGINT NOT NULL DEFAULT 1,
    rule JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Plugins
CREATE TABLE plugin (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID REFERENCES workspace(id) ON DELETE CASCADE,  -- NULL = global
    plugin_key TEXT NOT NULL,
    display_name TEXT NOT NULL,
    version TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'ready' CHECK (status IN ('ready', 'installing', 'uninstalling', 'failed')),
    install_order INT NOT NULL DEFAULT 0,
    manifest JSONB NOT NULL,
    package_path TEXT,
    config_revision BIGINT NOT NULL DEFAULT 1,
    secret_revision BIGINT NOT NULL DEFAULT 1,
    via_attribution TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Wakeups
CREATE TABLE wakeup (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
    issue_id UUID NOT NULL REFERENCES issue(id) ON DELETE CASCADE,
    source TEXT NOT NULL CHECK (source IN ('event', 'time', 'manual')),
    event_type TEXT,
    due_at TIMESTAMPTZ,
    actor_type TEXT,
    actor_id TEXT,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'active', 'settled', 'cancelled')),
    run_id UUID,
    receipt_id TEXT,
    coalesced_count INT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Channel installations
CREATE TABLE channel_installation (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workspace_id UUID NOT NULL REFERENCES workspace(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('slack', 'lark', 'dingtalk', 'wecom', 'telegram', 'custom')),
    external_id TEXT NOT NULL,
    display_name TEXT NOT NULL,
    config JSONB NOT NULL DEFAULT '{}',
    enabled BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE(workspace_id, kind, external_id)
);

-- Personal access tokens
CREATE TABLE personal_access_token (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES "user"(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    token_hash TEXT NOT NULL,
    token_last4 TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    last_used_at TIMESTAMPTZ,
    scopes JSONB NOT NULL DEFAULT '[]',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Verification codes
CREATE TABLE verification_code (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID REFERENCES "user"(id) ON DELETE CASCADE,
    email TEXT,
    purpose TEXT NOT NULL CHECK (purpose IN ('email_verification', 'password_reset', 'two_factor', 'workspace_invite')),
    code_hash TEXT NOT NULL,
    attempts INT NOT NULL DEFAULT 0,
    expires_at TIMESTAMPTZ NOT NULL,
    consumed_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);