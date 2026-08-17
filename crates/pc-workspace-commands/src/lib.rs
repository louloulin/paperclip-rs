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

#[cfg(test)]
mod internal_tests {
    use super::*;
    use serde_json::json;

    fn def(name: &str, kind: WorkspaceCommandKind, service_index: Option<usize>, command: Option<&str>, cwd: Option<&str>) -> WorkspaceCommandDefinition {
        let (source_kind, source_index) = match kind {
            WorkspaceCommandKind::Service => (WorkspaceCommandSourceKey::Services, service_index.unwrap_or(0)),
            WorkspaceCommandKind::Job => (WorkspaceCommandSourceKey::Jobs, 0),
        };
        WorkspaceCommandDefinition {
            id: name.to_string(),
            name: name.to_string(),
            kind,
            command: command.map(str::to_string),
            cwd: cwd.map(str::to_string),
            lifecycle: None,
            service_index,
            disabled_reason: None,
            raw_config: Value::Null,
            source: WorkspaceCommandSource { kind: source_kind, index: source_index },
        }
    }

    fn svc_input(id: &str, name: Option<&str>, command: Option<&str>, cwd: Option<&str>, config_index: Option<usize>) -> WorkspaceRuntimeServiceMatchInput {
        WorkspaceRuntimeServiceMatchInput {
            config_index,
            service_name: name.map(str::to_string),
            command: command.map(str::to_string),
            cwd: cwd.map(str::to_string),
            id: id.to_string(),
        }
    }

    #[test]
    fn r784_kind_as_str() {
        assert_eq!(WorkspaceCommandKind::Service.as_str(), "service");
        assert_eq!(WorkspaceCommandKind::Job.as_str(), "job");
    }

    #[test]
    fn r784_lifecycle_as_str() {
        assert_eq!(WorkspaceCommandLifecycle::Shared.as_str(), "shared");
        assert_eq!(WorkspaceCommandLifecycle::Ephemeral.as_str(), "ephemeral");
    }

    #[test]
    fn r784_source_key_as_str() {
        assert_eq!(WorkspaceCommandSourceKey::Commands.as_str(), "commands");
        assert_eq!(WorkspaceCommandSourceKey::Services.as_str(), "services");
        assert_eq!(WorkspaceCommandSourceKey::Jobs.as_str(), "jobs");
    }

    #[test]
    fn r784_list_definitions_empty_when_workspace_runtime_none() {
        assert_eq!(list_workspace_command_definitions(None).len(), 0);
    }

    #[test]
    fn r784_list_definitions_empty_when_workspace_runtime_empty() {
        let v = json!({});
        assert_eq!(list_workspace_command_definitions(Some(&v)).len(), 0);
    }

    #[test]
    fn r784_list_definitions_extracts_commands() {
        let v = json!({
            "commands": [
                {"name": "build", "command": "npm run build", "cwd": "/app"},
                {"name": "test", "command": "npm test"},
            ]
        });
        let defs = list_workspace_command_definitions(Some(&v));
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0].name, "build");
        assert_eq!(defs[0].command.as_deref(), Some("npm run build"));
        assert_eq!(defs[0].cwd.as_deref(), Some("/app"));
    }

    #[test]
    fn r784_list_definitions_assigns_service_indices() {
        let v = json!({
            "services": [
                {"name": "db", "command": "postgres", "lifecycle": "shared"},
                {"name": "cache", "command": "redis", "lifecycle": "ephemeral"},
            ]
        });
        let defs = list_workspace_command_definitions(Some(&v));
        assert_eq!(defs.len(), 2);
        for (i, d) in defs.iter().enumerate() {
            assert_eq!(d.kind, WorkspaceCommandKind::Service);
            assert_eq!(d.service_index, Some(i));
        }
        assert_eq!(defs[0].lifecycle, Some(WorkspaceCommandLifecycle::Shared));
        assert_eq!(defs[1].lifecycle, Some(WorkspaceCommandLifecycle::Ephemeral));
    }

    #[test]
    fn r784_list_definitions_jobs_no_lifecycle() {
        let v = json!({
            "jobs": [
                {"name": "migrate", "command": "db-migrate"},
            ]
        });
        let defs = list_workspace_command_definitions(Some(&v));
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].kind, WorkspaceCommandKind::Job);
        assert_eq!(defs[0].lifecycle, None);
        assert_eq!(defs[0].service_index, None);
    }

    #[test]
    fn r784_list_service_filter_excludes_jobs() {
        let v = json!({
            "services": [{"name": "db"}],
            "jobs": [{"name": "migrate"}],
        });
        let service_defs = list_workspace_service_command_definitions(Some(&v));
        assert_eq!(service_defs.len(), 1);
        assert_eq!(service_defs[0].name, "db");
    }

    #[test]
    fn r784_find_definition_by_name() {
        let v = json!({
            "commands": [
                {"name": "build"},
                {"name": "test"},
            ]
        });
        // IDs are generated as "{kind}:{slug}" - for "commands" entries kind is Service by default
        let found = find_workspace_command_definition(Some(&v), Some("service:test"));
        assert!(found.is_some());
        assert_eq!(found.unwrap().name, "test");
        let not_found = find_workspace_command_definition(Some(&v), Some("service:missing"));
        assert!(not_found.is_none());
        let none_input = find_workspace_command_definition(Some(&v), None);
        assert!(none_input.is_none());
    }

    #[test]
    fn r784_score_exact_service_index_returns_100() {
        let cmd = def("db", WorkspaceCommandKind::Service, Some(0), None, None);
        let svc = svc_input("svc1", None, None, None, Some(0));
        assert_eq!(score_workspace_runtime_service_match(&cmd, &svc), 100);
    }

    #[test]
    fn r784_score_mismatched_service_index_returns_minus_one() {
        let cmd = def("db", WorkspaceCommandKind::Service, Some(0), None, None);
        let svc = svc_input("svc1", None, None, None, Some(1));
        assert_eq!(score_workspace_runtime_service_match(&cmd, &svc), -1);
    }

    #[test]
    fn r784_score_both_name_and_command_match() {
        let cmd = def("db", WorkspaceCommandKind::Job, None, Some("postgres"), None);
        let svc = svc_input("svc1", Some("db"), Some("postgres"), None, None);
        // name +4, command +4 = 8
        assert_eq!(score_workspace_runtime_service_match(&cmd, &svc), 8);
    }

    #[test]
    fn r784_score_cwd_match_path_completion() {
        let cmd = def("db", WorkspaceCommandKind::Job, None, Some("postgres"), Some("app"));
        let svc = svc_input("svc1", None, Some("postgres"), Some("/home/user/app"), None);
        // command == command (+4) + cwd matches with /app suffix (+2) = 6
        assert_eq!(score_workspace_runtime_service_match(&cmd, &svc), 6);
    }

    #[test]
    fn r784_score_command_mismatch_returns_minus_one() {
        let cmd = def("db", WorkspaceCommandKind::Job, None, Some("postgres"), None);
        let svc = svc_input("svc1", None, Some("redis"), None, None);
        assert_eq!(score_workspace_runtime_service_match(&cmd, &svc), -1);
    }

    #[test]
    fn r784_match_runtime_service_picks_best() {
        let cmd = def("db", WorkspaceCommandKind::Job, None, Some("postgres"), None);
        let services = vec![
            svc_input("svc1", Some("redis"), Some("redis-server"), None, None),
            svc_input("svc2", Some("db"), Some("postgres"), None, None),
        ];
        let idx = match_workspace_runtime_service_to_command(&cmd, Some(&services));
        assert_eq!(idx, Some(1));
    }

    #[test]
    fn r784_match_runtime_service_returns_none_when_no_match() {
        let cmd = def("db", WorkspaceCommandKind::Job, None, Some("postgres"), None);
        let services = vec![
            svc_input("svc1", None, None, None, None),
        ];
        let idx = match_workspace_runtime_service_to_command(&cmd, Some(&services));
        assert_eq!(idx, None);
    }

    #[test]
    fn r784_match_runtime_service_no_services() {
        let cmd = def("db", WorkspaceCommandKind::Job, None, Some("postgres"), None);
        let idx = match_workspace_runtime_service_to_command(&cmd, None);
        assert_eq!(idx, None);
    }
}
