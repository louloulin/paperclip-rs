#![forbid(unsafe_code)]
#![allow(clippy::doc_markdown)]

//! Workspace runtime command definitions and runtime-service matching.
//!
//! R548: Direct port of `paperclip/packages/shared/src/workspace-commands.ts` (208 LOC).
//! All helpers operate on `&serde_json::Value` (Node's `Record<string, unknown>` equivalent)
//! for the input runtime config, but produce typed `WorkspaceCommandDefinition` outputs.

use serde_json::Value;

/// Distinguishes service-style commands (long-running) from job-style commands (one-shot).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkspaceCommandKind {
    Service,
    Job,
}

impl WorkspaceCommandKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Service => "service",
            Self::Job => "job",
        }
    }
}

/// Lifecycle of a service command: shared (re-used) or ephemeral (one-shot per invocation).
/// Always `None` for job-kind commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceCommandLifecycle {
    Shared,
    Ephemeral,
}

impl WorkspaceCommandLifecycle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Ephemeral => "ephemeral",
        }
    }
}

/// Source array within the workspace runtime that produced this command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkspaceCommandSourceKey {
    Commands,
    Services,
    Jobs,
}

impl WorkspaceCommandSourceKey {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Commands => "commands",
            Self::Services => "services",
            Self::Jobs => "jobs",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceCommandSource {
    pub kind: WorkspaceCommandSourceKey,
    pub index: usize,
}

#[derive(Debug, Clone)]
pub struct WorkspaceCommandDefinition {
    pub id: String,
    pub name: String,
    pub kind: WorkspaceCommandKind,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub lifecycle: Option<WorkspaceCommandLifecycle>,
    pub service_index: Option<usize>,
    pub disabled_reason: Option<String>,
    pub raw_config: Value,
    pub source: WorkspaceCommandSource,
}

#[derive(Debug, Clone)]
pub struct WorkspaceRuntimeServiceMatchInput {
    pub config_index: Option<usize>,
    pub service_name: Option<String>,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub id: String,
}

// ---------- helpers ----------

fn read_non_empty_string(value: Option<&Value>) -> Option<String> {
    let raw = value?.as_str()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn slugify(value: Option<&str>) -> Option<String> {
    let raw = value?.trim().to_ascii_lowercase();
    if raw.is_empty() {
        return None;
    }
    let mut normalized = String::with_capacity(raw.len());
    let mut last_was_dash = true; // suppress leading dash
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            normalized.push(ch);
            last_was_dash = false;
        } else if !last_was_dash {
            normalized.push('-');
            last_was_dash = true;
        }
    }
    while normalized.ends_with('-') {
        normalized.pop();
    }
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn derive_workspace_command_id(
    kind: WorkspaceCommandKind,
    explicit_id: Option<&str>,
    name: &str,
    index: usize,
) -> String {
    if let Some(id) = slugify(explicit_id) {
        return id;
    }
    if let Some(name_slug) = slugify(Some(name)) {
        return format!("{}:{}", kind.as_str(), name_slug);
    }
    format!("{}:{}", kind.as_str(), index + 1)
}

fn read_name_from_entry(entry: &Value, fallback: &str) -> String {
    read_non_empty_string(entry.get("name"))
        .or_else(|| read_non_empty_string(entry.get("label")))
        .or_else(|| read_non_empty_string(entry.get("title")))
        .unwrap_or_else(|| fallback.to_string())
}

fn build_definition(
    entry: &Value,
    kind: WorkspaceCommandKind,
    source_key: WorkspaceCommandSourceKey,
    source_index: usize,
    service_index: Option<usize>,
    fallback_name: &str,
) -> WorkspaceCommandDefinition {
    let name = read_name_from_entry(entry, fallback_name);
    let id = derive_workspace_command_id(
        kind,
        read_non_empty_string(entry.get("id")).as_deref(),
        &name,
        source_index,
    );
    let lifecycle = if matches!(kind, WorkspaceCommandKind::Service) {
        match entry.get("lifecycle").and_then(|v| v.as_str()) {
            Some("ephemeral") => Some(WorkspaceCommandLifecycle::Ephemeral),
            _ => Some(WorkspaceCommandLifecycle::Shared),
        }
    } else {
        None
    };
    WorkspaceCommandDefinition {
        id,
        name,
        kind,
        command: read_non_empty_string(entry.get("command")),
        cwd: read_non_empty_string(entry.get("cwd")),
        lifecycle,
        service_index,
        disabled_reason: read_non_empty_string(entry.get("disabledReason")),
        raw_config: entry.clone(),
        source: WorkspaceCommandSource {
            kind: source_key,
            index: source_index,
        },
    }
}

fn unique_workspace_command_id(
    seen: &mut std::collections::HashSet<String>,
    command_id: String,
    source_key: WorkspaceCommandSourceKey,
    source_index: usize,
) -> String {
    if seen.insert(command_id.clone()) {
        return command_id;
    }
    let fallback = format!(
        "{}-{}-{}",
        command_id,
        source_key.as_str(),
        source_index + 1
    );
    seen.insert(fallback.clone());
    fallback
}

fn read_command_entries<'a>(workspace_runtime: Option<&'a Value>, key: &str) -> Vec<&'a Value> {
    let Some(raw) = workspace_runtime.and_then(|v| v.get(key)) else {
        return Vec::new();
    };
    let Some(arr) = raw.as_array() else {
        return Vec::new();
    };
    arr.iter().filter(|entry| entry.is_object()).collect()
}

fn entry_kind(entry: &Value) -> WorkspaceCommandKind {
    match entry.get("kind").and_then(|v| v.as_str()) {
        Some("job") => WorkspaceCommandKind::Job,
        _ => WorkspaceCommandKind::Service,
    }
}

// ---------- public API ----------

pub fn list_workspace_command_definitions(
    workspace_runtime: Option<&Value>,
) -> Vec<WorkspaceCommandDefinition> {
    let Some(workspace_runtime) = workspace_runtime else {
        return Vec::new();
    };

    let command_entries = read_command_entries(Some(workspace_runtime), "commands");
    let mut seen_ids = std::collections::HashSet::new();
    let mut next_service_index = 0usize;

    if !command_entries.is_empty() {
        return command_entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let kind = entry_kind(entry);
                let service_index = if matches!(kind, WorkspaceCommandKind::Service) {
                    let v = next_service_index;
                    next_service_index += 1;
                    Some(v)
                } else {
                    None
                };
                let fallback = match kind {
                    WorkspaceCommandKind::Job => format!("Job {}", index + 1),
                    WorkspaceCommandKind::Service => format!("Service {}", index + 1),
                };
                let def = build_definition(
                    entry,
                    kind,
                    WorkspaceCommandSourceKey::Commands,
                    index,
                    service_index,
                    &fallback,
                );
                let id = unique_workspace_command_id(
                    &mut seen_ids,
                    def.id,
                    def.source.kind,
                    def.source.index,
                );
                WorkspaceCommandDefinition { id, ..def }
            })
            .collect();
    }

    let mut out = Vec::new();
    let services = read_command_entries(Some(workspace_runtime), "services");
    for (index, entry) in services.iter().enumerate() {
        let service_index = next_service_index;
        next_service_index += 1;
        let fallback = format!("Service {}", index + 1);
        let def = build_definition(
            entry,
            WorkspaceCommandKind::Service,
            WorkspaceCommandSourceKey::Services,
            index,
            Some(service_index),
            &fallback,
        );
        let id =
            unique_workspace_command_id(&mut seen_ids, def.id, def.source.kind, def.source.index);
        out.push(WorkspaceCommandDefinition { id, ..def });
    }
    let jobs = read_command_entries(Some(workspace_runtime), "jobs");
    for (index, entry) in jobs.iter().enumerate() {
        let fallback = format!("Job {}", index + 1);
        let def = build_definition(
            entry,
            WorkspaceCommandKind::Job,
            WorkspaceCommandSourceKey::Jobs,
            index,
            None,
            &fallback,
        );
        let id =
            unique_workspace_command_id(&mut seen_ids, def.id, def.source.kind, def.source.index);
        out.push(WorkspaceCommandDefinition { id, ..def });
    }
    out
}

pub fn list_workspace_service_command_definitions(
    workspace_runtime: Option<&Value>,
) -> Vec<WorkspaceCommandDefinition> {
    list_workspace_command_definitions(workspace_runtime)
        .into_iter()
        .filter(|command| command.kind == WorkspaceCommandKind::Service)
        .collect()
}

pub fn find_workspace_command_definition(
    workspace_runtime: Option<&Value>,
    workspace_command_id: Option<&str>,
) -> Option<WorkspaceCommandDefinition> {
    let normalized_id = workspace_command_id
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    list_workspace_command_definitions(workspace_runtime)
        .into_iter()
        .find(|command| command.id == normalized_id)
}

pub fn score_workspace_runtime_service_match(
    command: &WorkspaceCommandDefinition,
    runtime_service: &WorkspaceRuntimeServiceMatchInput,
) -> i32 {
    let command_command = command.command.as_deref();
    let runtime_command = runtime_service.command.as_deref();
    if command_command.is_some() && runtime_command.is_some() && runtime_command != command_command
    {
        return -1;
    }

    if let (Some(cmd_idx), Some(rt_idx)) = (command.service_index, runtime_service.config_index) {
        return if rt_idx == cmd_idx { 100 } else { -1 };
    }

    let mut score = 0i32;
    if runtime_service.service_name.as_deref() == Some(command.name.as_str()) {
        score += 4;
    }
    if runtime_service.command == command.command {
        score += 4;
    }
    if let (Some(cmd_cwd), Some(rt_cwd)) = (command.cwd.as_deref(), runtime_service.cwd.as_deref())
    {
        if rt_cwd == cmd_cwd || rt_cwd.ends_with(&format!("/{cmd_cwd}")) {
            score += 2;
        }
    }
    score
}

pub fn match_workspace_runtime_service_to_command(
    command: &WorkspaceCommandDefinition,
    runtime_services: Option<&[WorkspaceRuntimeServiceMatchInput]>,
) -> Option<usize> {
    let mut best_match: Option<usize> = None;
    let mut best_score = -1i32;
    if let Some(services) = runtime_services {
        for (idx, svc) in services.iter().enumerate() {
            let score = score_workspace_runtime_service_match(command, svc);
            if score > best_score {
                best_match = Some(idx);
                best_score = score;
            }
        }
    }
    if best_score > 0 {
        best_match
    } else {
        None
    }
}
