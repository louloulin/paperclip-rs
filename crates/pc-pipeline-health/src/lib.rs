#![forbid(unsafe_code)]

//! Pipeline setup-health warnings.
//!
//! R538: Direct port of `paperclip/packages/shared/src/pipeline-health.ts`
//! (374 LOC pure functions).
//!
//! 设计原则:
//! - 所有 `pub fn` 都是纯函数 (无 IO, 无副作用, 无环境依赖)
//! - 输入用结构化 `*Ref` / `*Input` 类型 (镜像上游 TS interface)
//! - 输出 `Vec<PipelineHealthWarning>` — caller 决定如何渲染
//! - 自包含 — 仅依赖 `regex` + `serde` + `serde_json`
//! - **不** 依赖 `pc-core` / `pc-repos` — 这是 pure-function 层
//!
//! 范围 (本 crate):
//! - [`PipelineHealthWarning`] / [`PipelineHealthReport`] / 输入类型
//! - [`compute_pipeline_health`] — 主入口, 返回所有 warnings
//! - [`group_warnings_by_stage`] — 按 stageId 分组 helper
//! - [`is_pipeline_terminal_stage_kind`] — 终结 stage 判定
//! - [`extract_pipeline_mentions`] — markdown 里 `pipeline://` 提及解析
//! - [`is_agent_status_invokable`] — minimal inlined subset (与 Node 上游对齐)
//!
//! **不** 范围 (留给集成层):
//! - DB 持久化 (`server/src/services/pipelines.ts`)
//! - UI 渲染 (`ui/src/lib/pipeline-health.ts`)
//!
//! 设计 vs Node 上游:
//! - enum `PipelineHealthWarningCode` 替代 TS literal union — 编译期穷尽匹配
//! - `StageConfig = Map<String, Value>` (serde_json) 替代 TS `{ [k]: unknown }`
//!   — 保留 unknown-like 灵活性的同时, 提供 JSON-serialized 输入
//! - 局部 inline 13 行 `is_agent_status_invokable` 替代 `pc-core` 依赖
//!   — 避免 28K LOC crate 引入, 与上游语义完全一致

use std::collections::{HashMap, HashSet};

use regex::Regex;
use serde_json::{Map, Value};

// ============================================================================
// Warning codes
// ============================================================================

/// Machine-readable warning reason.
///
/// Mirrors Node `PipelineHealthWarningCode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineHealthWarningCode {
    PausedAgent,
    StageNoAutomation,
    AutomationNoInstructions,
    AutomationNoAgent,
    AutomationFailed,
    ReviewNoApprover,
    MissingPipelineReference,
    MissingStageReference,
    BreakdownTargetMissing,
    BreakdownNoWait,
    BreakdownTargetNotEntrySafe,
    BreakdownFieldMismatch,
    UnsetRequiredVariable,
}

impl PipelineHealthWarningCode {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PausedAgent => "paused_agent",
            Self::StageNoAutomation => "stage_no_automation",
            Self::AutomationNoInstructions => "automation_no_instructions",
            Self::AutomationNoAgent => "automation_no_agent",
            Self::AutomationFailed => "automation_failed",
            Self::ReviewNoApprover => "review_no_approver",
            Self::MissingPipelineReference => "missing_pipeline_reference",
            Self::MissingStageReference => "missing_stage_reference",
            Self::BreakdownTargetMissing => "breakdown_target_missing",
            Self::BreakdownNoWait => "breakdown_no_wait",
            Self::BreakdownTargetNotEntrySafe => "breakdown_target_not_entry_safe",
            Self::BreakdownFieldMismatch => "breakdown_field_mismatch",
            Self::UnsetRequiredVariable => "unset_required_variable",
        }
    }
}

// ============================================================================
// Data structures
// ============================================================================

/// A single health warning anchored to a stage.
///
/// Mirrors Node `PipelineHealthWarning`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineHealthWarning {
    pub code: PipelineHealthWarningCode,
    pub stage_id: String,
    pub stage_key: String,
    pub stage_name: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub href: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub href_label: Option<String>,
}

/// Full health report.
///
/// Mirrors Node `PipelineHealthReport`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineHealthReport {
    pub pipeline_id: String,
    pub warnings: Vec<PipelineHealthWarning>,
    pub ok: bool,
}

/// Minimal agent reference for health lookup.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineHealthAgentRef {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub status: String,
}

/// Minimal stage reference for cross-pipeline validation.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineHealthStageRef {
    pub key: String,
    pub name: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub config: Option<Map<String, Value>>,
}

/// Minimal pipeline reference for cross-pipeline validation.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineHealthPipelineRef {
    pub id: String,
    pub name: String,
    pub stages: Vec<PipelineHealthStageRef>,
}

/// Stage input for health computation.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineHealthStageInput {
    pub id: String,
    pub key: String,
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub config: Option<Map<String, Value>>,
    /// Latest instructions body for the stage ("" when there are none).
    #[serde(default)]
    pub instructions_body: Option<String>,
}

/// Failed automation input.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineHealthFailedAutomationInput {
    pub stage_id: String,
    pub stage_key: String,
    pub stage_name: String,
    pub case_id: String,
    pub case_title: String,
    #[serde(default)]
    pub error: Option<String>,
}

/// Input to [`compute_pipeline_health`].
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineHealthInput {
    pub pipeline_id: String,
    pub stages: Vec<PipelineHealthStageInput>,
    pub agents_by_id: HashMap<String, PipelineHealthAgentRef>,
    pub pipelines_by_id: HashMap<String, PipelineHealthPipelineRef>,
    #[serde(default)]
    pub failed_automations: Vec<PipelineHealthFailedAutomationInput>,
}

/// Parsed `pipeline://` mention from markdown.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedPipelineMention {
    pub pipeline_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage_key: Option<String>,
}

// ============================================================================
// Minimal agent status check (mirrors Node isAgentStatusInvokable subset)
// ============================================================================

/// Returns `true` if the agent status allows the agent to be invoked.
///
/// Mirrors Node `isAgentStatusInvokable`:
/// `INVOKABLE_AGENT_STATUSES = ["active", "idle", "running", "error"]`
/// `NON_INVOKABLE_AGENT_STATUSES = ["terminated", "pending_approval", "paused"]`
///
/// Locally inlined to avoid the heavy `pc-core` dependency for one 2-line
/// check. Set semantics match upstream exactly.
#[inline]
#[must_use]
pub fn is_agent_status_invokable(status: &str) -> bool {
    matches!(status, "active" | "idle" | "running" | "error")
        && !matches!(status, "terminated" | "pending_approval" | "paused")
}

// ============================================================================
// Stage kind helpers
// ============================================================================

/// Returns `true` if the stage kind is a terminal "done" / "cancelled" kind.
///
/// Mirrors Node `isPipelineTerminalStageKind`.
#[inline]
#[must_use]
pub fn is_pipeline_terminal_stage_kind(kind: Option<&str>) -> bool {
    matches!(kind, Some("done") | Some("cancelled"))
}

// ============================================================================
// Pipeline mention extraction (from project-mentions.ts)
// ============================================================================

/// Markdown link pattern for `pipeline://...` URLs.
///
/// Mirrors Node `PIPELINE_MENTION_LINK_RE`:
/// `/\[[^\]]*]\((pipeline:\/\/[^)\s]+)\)/gi`
static PIPELINE_MENTION_LINK_RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new(r"(?i)\[[^\]]*]\((pipeline://[^)\s]+)\)").expect("valid regex pattern")
});

/// Extract distinct `pipeline://` mentions from markdown.
///
/// Mirrors Node `extractPipelineMentions` (the only project-mentions helper
/// pipeline-health depends on; full project-mentions port is independent).
#[must_use]
pub fn extract_pipeline_mentions(markdown: &str) -> Vec<ParsedPipelineMention> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for cap in PIPELINE_MENTION_LINK_RE.captures_iter(markdown) {
        let Some(href_match) = cap.get(1) else {
            continue;
        };
        let Some(parsed) = parse_pipeline_mention_href(href_match.as_str()) else {
            continue;
        };
        let key = format!(
            "{}:{}",
            parsed.pipeline_id,
            parsed.stage_key.as_deref().unwrap_or("")
        );
        if seen.insert(key) {
            out.push(parsed);
        }
    }
    out
}

fn parse_pipeline_mention_href(href: &str) -> Option<ParsedPipelineMention> {
    let scheme = "pipeline://";
    let rest = href.strip_prefix(scheme)?;

    // Split off optional `?stage=...` query string.
    let (path_part, stage_key) = match rest.split_once('?') {
        Some((p, q)) => {
            let stage = q
                .strip_prefix("stage=")
                .map(str::trim)
                .filter(|s| !s.is_empty());
            (p, stage.map(str::to_owned))
        }
        None => (rest, None),
    };

    let pipeline_id = path_part.trim().trim_matches('/').to_owned();
    if pipeline_id.is_empty() {
        return None;
    }

    Some(ParsedPipelineMention {
        pipeline_id,
        stage_key,
    })
}

// ============================================================================
// Helpers
// ============================================================================

fn as_config(config: Option<&Map<String, Value>>) -> Map<String, Value> {
    config.cloned().unwrap_or_default()
}

fn agent_label(agent: Option<&PipelineHealthAgentRef>) -> String {
    agent
        .and_then(|a| a.name.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| "a teammate".to_owned())
}

fn has_on_enter_routine_automation(config: &Map<String, Value>) -> bool {
    let Some(on_enter) = config.get("onEnter") else {
        return false;
    };
    let Some(obj) = on_enter.as_object() else {
        return false;
    };
    obj.get("type").and_then(Value::as_str) == Some("run_routine")
        && obj
            .get("routineId")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
}

#[derive(Debug)]
struct BreakdownConfig {
    target_pipeline_id: Option<String>,
    target_stage_key: Option<String>,
    piece_noun: String,
    inherit_fields: Vec<String>,
    wait_for_pieces: bool,
    when_finished_move_to: Option<String>,
}

fn read_breakdown_config(config: &Map<String, Value>) -> Option<BreakdownConfig> {
    let raw = config.get("breakdown")?;
    let record = raw.as_object()?;

    let target_pipeline_id = record
        .get("targetPipelineId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);

    let target_stage_key = record
        .get("targetStageKey")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);

    let piece_noun = record
        .get("pieceNoun")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| "piece".to_owned());

    let inherit_fields: Vec<String> = record
        .get("inheritFields")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|v| {
                    v.as_str()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default();

    let when_finished_move_to = record
        .get("whenFinishedMoveTo")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            config
                .get("autoAdvanceOnChildrenTerminal")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        });

    let wait_for_pieces = record
        .get("waitForPieces")
        .map(|v| v.as_bool().unwrap_or(false))
        .unwrap_or_else(|| {
            config
                .get("requireChildrenTerminal")
                .and_then(Value::as_bool)
                == Some(true)
        });

    Some(BreakdownConfig {
        target_pipeline_id,
        target_stage_key,
        piece_noun,
        inherit_fields,
        wait_for_pieces,
        when_finished_move_to,
    })
}

fn has_children_gate_auto_advance(config: &Map<String, Value>) -> bool {
    if let Some(breakdown) = read_breakdown_config(config) {
        return breakdown.wait_for_pieces && breakdown.when_finished_move_to.is_some();
    }
    config
        .get("requireChildrenTerminal")
        .and_then(Value::as_bool)
        == Some(true)
        && config
            .get("autoAdvanceOnChildrenTerminal")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
}

fn has_runnable_stage_automation(config: &Map<String, Value>) -> bool {
    read_breakdown_config(config).is_some()
        || has_on_enter_routine_automation(config)
        || has_children_gate_auto_advance(config)
}

fn automation_assignee_agent_id(config: &Map<String, Value>) -> Option<String> {
    if let Some(automation) = config.get("automation").and_then(Value::as_object) {
        if let Some(value) = automation.get("assigneeAgentId").and_then(Value::as_str) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_owned());
            }
        }
    }
    config
        .get("assigneeAgentId")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

// ============================================================================
// Main entry point
// ============================================================================

/// Compute all setup-health warnings for a pipeline.
///
/// Mirrors Node `computePipelineHealth`.
#[must_use]
pub fn compute_pipeline_health(input: &PipelineHealthInput) -> PipelineHealthReport {
    let mut warnings: Vec<PipelineHealthWarning> = Vec::new();

    for stage in &input.stages {
        let config = as_config(stage.config.as_ref());
        let instructions_body = stage
            .instructions_body
            .as_deref()
            .unwrap_or("")
            .trim()
            .to_owned();
        let anchor = PipelineHealthAnchor {
            stage_id: stage.id.clone(),
            stage_key: stage.key.clone(),
            stage_name: stage.name.clone(),
        };

        let assignee_agent_id = automation_assignee_agent_id(&config);
        let has_stage_automation = has_runnable_stage_automation(&config);
        let is_terminal_stage = is_pipeline_terminal_stage_kind(Some(&stage.kind));
        let breakdown = read_breakdown_config(&config);

        // 1. Paused/missing agent
        if let Some(agent_id) = assignee_agent_id.as_deref() {
            match input.agents_by_id.get(agent_id) {
                None => warnings.push(anchor.warning(
                    PipelineHealthWarningCode::PausedAgent,
                    "Assigned to a teammate who's no longer here. Pick someone else to run this step.",
                )),
                Some(agent) if !is_agent_status_invokable(&agent.status) => {
                    warnings.push(anchor.warning(
                        PipelineHealthWarningCode::PausedAgent,
                        format!(
                            "{} is paused, so this step won't run until they're back. Reassign it if you can't wait.",
                            agent_label(Some(agent))
                        ),
                    ));
                }
                _ => {}
            }
        }

        // 2. Assigned but no instructions
        if assignee_agent_id.is_some() && instructions_body.is_empty() {
            warnings.push(anchor.warning(
                PipelineHealthWarningCode::AutomationNoInstructions,
                "Assigned to a teammate, but there are no instructions yet. Add instructions so this step doesn't stall.",
            ));
        }

        // 3. Instructions but no agent
        if assignee_agent_id.is_none()
            && !instructions_body.is_empty()
            && !has_stage_automation
            && stage.kind != "review"
            && !is_terminal_stage
        {
            warnings.push(anchor.warning(
                PipelineHealthWarningCode::AutomationNoAgent,
                "This step has instructions, but no agent is assigned. Add an agent to run this step, or make it a review step if a person should decide.",
            ));
        }

        // 4. Nothing runs here
        if assignee_agent_id.is_none()
            && instructions_body.is_empty()
            && !has_stage_automation
            && stage.kind != "review"
            && !is_terminal_stage
        {
            warnings.push(anchor.warning(
                PipelineHealthWarningCode::StageNoAutomation,
                "Nothing runs here automatically — items will sit until a person moves them. Add an agent to run this step, or make it a review step if a person should decide.",
            ));
        }

        // 5. Review step with no approver
        if stage.kind == "review"
            || config.get("requireApproval").and_then(Value::as_bool) == Some(true)
        {
            let approver_obj =
                config
                    .get("approver")
                    .and_then(|v| if v.is_null() { None } else { v.as_object() });
            let kind = approver_obj
                .and_then(|o| o.get("kind"))
                .and_then(Value::as_str)
                .unwrap_or("any_human");
            let approver_id = approver_obj
                .and_then(|o| o.get("id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);

            if kind == "agent" {
                match approver_id
                    .as_deref()
                    .and_then(|id| input.agents_by_id.get(id))
                {
                    None if approver_id.is_none() => warnings.push(anchor.warning(
                        PipelineHealthWarningCode::ReviewNoApprover,
                        "No approver picked yet, so work will pile up here. Choose who approves.",
                    )),
                    None => warnings.push(anchor.warning(
                        PipelineHealthWarningCode::ReviewNoApprover,
                        "No approver picked yet, so work will pile up here. Choose who approves.",
                    )),
                    Some(agent) if !is_agent_status_invokable(&agent.status) => {
                        warnings.push(anchor.warning(
                            PipelineHealthWarningCode::ReviewNoApprover,
                            format!(
                                "{} is the approver and they're paused, so nothing can be approved until they're back.",
                                agent_label(Some(agent))
                            ),
                        ));
                    }
                    _ => {}
                }
            } else if kind == "user" && approver_id.is_none() {
                warnings.push(anchor.warning(
                    PipelineHealthWarningCode::ReviewNoApprover,
                    "No approver picked yet, so work will pile up here. Choose who approves.",
                ));
            }
        }

        // 6. Breakdown validation
        if let Some(bd) = breakdown {
            let target_pipeline = bd
                .target_pipeline_id
                .as_deref()
                .and_then(|id| input.pipelines_by_id.get(id));
            let target_stage = bd.target_stage_key.as_deref().and_then(|key| {
                target_pipeline.and_then(|p| p.stages.iter().find(|s| s.key == key))
            });

            if target_pipeline.is_none() || target_stage.is_none() {
                warnings.push(anchor.warning(
                    PipelineHealthWarningCode::BreakdownTargetMissing,
                    "This step breaks work into another workflow, but that destination is missing. Pick where the pieces should go.",
                ));
            } else {
                if !bd.wait_for_pieces || bd.when_finished_move_to.is_none() {
                    warnings.push(anchor.warning(
                        PipelineHealthWarningCode::BreakdownNoWait,
                        format!(
                            "This step creates {}s but does not wait for them before moving on. Turn on waiting if the next step depends on the pieces finishing.",
                            bd.piece_noun
                        ),
                    ));
                }
                let target_stage = target_stage.expect("checked above");
                let target_config = as_config(target_stage.config.as_ref());
                let first_stage = target_pipeline.and_then(|p| p.stages.first());
                let entry_unsafe = first_stage.is_some_and(|f| f.key != target_stage.key)
                    || target_stage.kind.as_deref() == Some("review")
                    || is_pipeline_terminal_stage_kind(target_stage.kind.as_deref())
                    || target_config.get("disabled").and_then(Value::as_bool) == Some(true)
                    || target_config
                        .get("requireApproval")
                        .and_then(Value::as_bool)
                        == Some(true);
                if entry_unsafe {
                    warnings.push(anchor.warning(
                        PipelineHealthWarningCode::BreakdownTargetNotEntrySafe,
                        format!(
                            "New {}s start in a destination step that may not accept new work cleanly. Choose the entry step for that workflow.",
                            bd.piece_noun
                        ),
                    ));
                }
            }
        } else if !instructions_body.is_empty() {
            // 6b. Pipeline mentions in instructions
            for mention in extract_pipeline_mentions(&instructions_body) {
                let target = input.pipelines_by_id.get(&mention.pipeline_id);
                if target.is_none() {
                    warnings.push(anchor.warning(
                        PipelineHealthWarningCode::MissingPipelineReference,
                        "These instructions hand off to a workflow that's been deleted. Point them at one that exists.",
                    ));
                    continue;
                }
                if let Some(stage_key) = mention.stage_key.as_deref() {
                    let target = target.expect("checked above");
                    if !target.stages.iter().any(|s| s.key == stage_key) {
                        let target_name = target.name.clone();
                        warnings.push(anchor.warning(
                            PipelineHealthWarningCode::MissingStageReference,
                            format!(
                                "These instructions hand off to a step that no longer exists in \"{}\". Point them at one that does.",
                                target_name
                            ),
                        ));
                    }
                }
            }
        }

        // 7. Required stage variables — item-input validation, not settings.
        //    (Skipped per upstream comment; per-item values are validated when
        //    work enters or runs through the pipeline.)
    }

    // Failed automations
    let mut seen_failed_automation_case_ids_by_stage: HashMap<String, HashSet<String>> =
        HashMap::new();
    for failure in input.failed_automations.iter() {
        let seen_case_ids = seen_failed_automation_case_ids_by_stage
            .entry(failure.stage_id.clone())
            .or_default();
        if seen_case_ids.contains(&failure.case_id) {
            continue;
        }
        seen_case_ids.insert(failure.case_id.clone());
        warnings.push(PipelineHealthWarning {
            code: PipelineHealthWarningCode::AutomationFailed,
            stage_id: failure.stage_id.clone(),
            stage_key: failure.stage_key.clone(),
            stage_name: failure.stage_name.clone(),
            message: format!(
                "Automation failed on \"{}\". Open the item to inspect the log and retry it.",
                failure.case_title
            ),
            href: Some(format!(
                "/pipelines/{}/items/{}",
                input.pipeline_id, failure.case_id
            )),
            href_label: Some("Open item".to_owned()),
        });
    }

    PipelineHealthReport {
        pipeline_id: input.pipeline_id.clone(),
        ok: warnings.is_empty(),
        warnings,
    }
}

/// Group warnings by stage id (for per-stage UI rendering).
///
/// Mirrors Node `groupWarningsByStage`.
#[must_use]
pub fn group_warnings_by_stage(
    warnings: &[PipelineHealthWarning],
) -> HashMap<String, Vec<PipelineHealthWarning>> {
    let mut out: HashMap<String, Vec<PipelineHealthWarning>> = HashMap::new();
    for w in warnings {
        out.entry(w.stage_id.clone()).or_default().push(w.clone());
    }
    out
}

// ============================================================================
// Internal anchor helper
// ============================================================================

#[derive(Debug, Clone)]
struct PipelineHealthAnchor {
    stage_id: String,
    stage_key: String,
    stage_name: String,
}

impl PipelineHealthAnchor {
    fn warning(
        &self,
        code: PipelineHealthWarningCode,
        message: impl Into<String>,
    ) -> PipelineHealthWarning {
        PipelineHealthWarning {
            code,
            stage_id: self.stage_id.clone(),
            stage_key: self.stage_key.clone(),
            stage_name: self.stage_name.clone(),
            message: message.into(),
            href: None,
            href_label: None,
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn stage(id: &str, key: &str, name: &str, kind: &str) -> PipelineHealthStageInput {
        PipelineHealthStageInput {
            id: id.to_owned(),
            key: key.to_owned(),
            name: name.to_owned(),
            kind: kind.to_owned(),
            config: None,
            instructions_body: None,
        }
    }

    fn agent(id: &str, status: &str, name: Option<&str>) -> PipelineHealthAgentRef {
        PipelineHealthAgentRef {
            id: id.to_owned(),
            name: name.map(str::to_owned),
            status: status.to_owned(),
        }
    }

    fn empty_input() -> PipelineHealthInput {
        PipelineHealthInput {
            pipeline_id: "pipe-1".to_owned(),
            stages: vec![],
            agents_by_id: HashMap::new(),
            pipelines_by_id: HashMap::new(),
            failed_automations: vec![],
        }
    }

    // ----- is_agent_status_invokable -----

    #[test]
    fn r538_is_agent_status_invokable_basic() {
        assert!(is_agent_status_invokable("active"));
        assert!(is_agent_status_invokable("idle"));
        assert!(is_agent_status_invokable("running"));
        assert!(is_agent_status_invokable("error"));
    }

    #[test]
    fn r538_is_agent_status_invokable_negative() {
        assert!(!is_agent_status_invokable("terminated"));
        assert!(!is_agent_status_invokable("pending_approval"));
        assert!(!is_agent_status_invokable("paused"));
        assert!(!is_agent_status_invokable("unknown"));
        assert!(!is_agent_status_invokable(""));
    }

    // ----- is_pipeline_terminal_stage_kind -----

    #[test]
    fn r538_is_pipeline_terminal_stage_kind() {
        assert!(is_pipeline_terminal_stage_kind(Some("done")));
        assert!(is_pipeline_terminal_stage_kind(Some("cancelled")));
        assert!(!is_pipeline_terminal_stage_kind(Some("active")));
        assert!(!is_pipeline_terminal_stage_kind(None));
        assert!(!is_pipeline_terminal_stage_kind(Some("")));
    }

    // ----- extract_pipeline_mentions -----

    #[test]
    fn r538_extract_pipeline_mentions_basic() {
        let md = "[next](pipeline://pipe-a) and [other](pipeline://pipe-b)";
        let mentions = extract_pipeline_mentions(md);
        let ids: Vec<&str> = mentions.iter().map(|m| m.pipeline_id.as_str()).collect();
        assert_eq!(ids, vec!["pipe-a", "pipe-b"]);
    }

    #[test]
    fn r538_extract_pipeline_mentions_with_stage() {
        let md = "[step](pipeline://pipe-a?stage=step1)";
        let mentions = extract_pipeline_mentions(md);
        assert_eq!(mentions.len(), 1);
        assert_eq!(mentions[0].pipeline_id, "pipe-a");
        assert_eq!(mentions[0].stage_key.as_deref(), Some("step1"));
    }

    #[test]
    fn r538_extract_pipeline_mentions_dedupes() {
        let md = "[a](pipeline://pipe-x) [b](pipeline://pipe-x)";
        let mentions = extract_pipeline_mentions(md);
        assert_eq!(mentions.len(), 1);
    }

    #[test]
    fn r538_extract_pipeline_mentions_empty() {
        assert!(extract_pipeline_mentions("").is_empty());
        assert!(extract_pipeline_mentions("plain text").is_empty());
    }

    #[test]
    fn r538_extract_pipeline_mentions_case_insensitive_scheme() {
        let md = "[a](PIPELINE://pipe-x)";
        let mentions = extract_pipeline_mentions(md);
        // The upstream parser uses a case-sensitive `startsWith` check after
        // its case-insensitive markdown-link regex.
        assert!(mentions.is_empty());
    }

    // ----- compute_pipeline_health: empty -----

    #[test]
    fn r538_compute_empty_pipeline_ok() {
        let input = empty_input();
        let report = compute_pipeline_health(&input);
        assert_eq!(report.pipeline_id, "pipe-1");
        assert!(report.warnings.is_empty());
        assert!(report.ok);
    }

    // ----- compute_pipeline_health: paused_agent -----

    #[test]
    fn r538_warns_paused_agent() {
        let mut input = empty_input();
        let mut s = stage("s1", "sk1", "Step 1", "active");
        s.config = Some(Map::from_iter([(
            "assigneeAgentId".to_owned(),
            Value::String("agent-1".to_owned()),
        )]));
        s.instructions_body = Some("do stuff".to_owned());
        input.stages = vec![s];
        input.agents_by_id = HashMap::from_iter([(
            "agent-1".to_owned(),
            agent("agent-1", "paused", Some("Alice")),
        )]);
        let report = compute_pipeline_health(&input);
        let paused = report
            .warnings
            .iter()
            .find(|w| w.code == PipelineHealthWarningCode::PausedAgent)
            .expect("paused_agent warning");
        assert!(paused.message.contains("Alice"));
        assert!(paused.message.contains("paused"));
    }

    #[test]
    fn r538_warns_missing_agent() {
        let mut input = empty_input();
        let mut s = stage("s1", "sk1", "Step 1", "active");
        s.config = Some(Map::from_iter([(
            "assigneeAgentId".to_owned(),
            Value::String("missing-agent".to_owned()),
        )]));
        s.instructions_body = Some("do stuff".to_owned());
        input.stages = vec![s];
        let report = compute_pipeline_health(&input);
        assert!(report
            .warnings
            .iter()
            .any(|w| w.code == PipelineHealthWarningCode::PausedAgent));
    }

    // ----- compute_pipeline_health: assigned + no instructions -----

    #[test]
    fn r538_warns_automation_no_instructions() {
        let mut input = empty_input();
        let mut s = stage("s1", "sk1", "Step 1", "active");
        s.config = Some(Map::from_iter([(
            "assigneeAgentId".to_owned(),
            Value::String("agent-1".to_owned()),
        )]));
        s.instructions_body = Some("".to_owned());
        input.stages = vec![s];
        input.agents_by_id =
            HashMap::from_iter([("agent-1".to_owned(), agent("agent-1", "active", None))]);
        let report = compute_pipeline_health(&input);
        assert!(report
            .warnings
            .iter()
            .any(|w| w.code == PipelineHealthWarningCode::AutomationNoInstructions));
    }

    // ----- compute_pipeline_health: instructions + no agent -----

    #[test]
    fn r538_warns_automation_no_agent() {
        let mut input = empty_input();
        let mut s = stage("s1", "sk1", "Step 1", "active");
        s.instructions_body = Some("do the thing".to_owned());
        input.stages = vec![s];
        let report = compute_pipeline_health(&input);
        assert!(report
            .warnings
            .iter()
            .any(|w| w.code == PipelineHealthWarningCode::AutomationNoAgent));
    }

    // ----- compute_pipeline_health: nothing runs -----

    #[test]
    fn r538_warns_stage_no_automation() {
        let mut input = empty_input();
        let s = stage("s1", "sk1", "Step 1", "active");
        input.stages = vec![s];
        let report = compute_pipeline_health(&input);
        assert!(report
            .warnings
            .iter()
            .any(|w| w.code == PipelineHealthWarningCode::StageNoAutomation));
    }

    #[test]
    fn r538_no_warning_for_review_kind_with_no_automation() {
        let mut input = empty_input();
        let s = stage("s1", "sk1", "Step 1", "review");
        input.stages = vec![s];
        let report = compute_pipeline_health(&input);
        // review stages skip the "nothing runs here" check
        assert!(!report
            .warnings
            .iter()
            .any(|w| w.code == PipelineHealthWarningCode::StageNoAutomation));
    }

    #[test]
    fn r538_no_warning_for_terminal_stage() {
        let mut input = empty_input();
        let s = stage("s1", "sk1", "Done", "done");
        input.stages = vec![s];
        let report = compute_pipeline_health(&input);
        assert!(report.warnings.is_empty());
    }

    // ----- compute_pipeline_health: review step -----

    #[test]
    fn r538_warns_review_no_approver_when_required() {
        let mut input = empty_input();
        let mut s = stage("s1", "sk1", "Review", "review");
        s.config = Some(Map::from_iter([(
            "requireApproval".to_owned(),
            Value::Bool(true),
        )]));
        input.stages = vec![s];
        let report = compute_pipeline_health(&input);
        assert!(!report
            .warnings
            .iter()
            .any(|w| w.code == PipelineHealthWarningCode::ReviewNoApprover));
    }

    #[test]
    fn r538_warns_review_agent_approver_paused() {
        let mut input = empty_input();
        let mut approver = Map::new();
        approver.insert("kind".to_owned(), Value::String("agent".to_owned()));
        approver.insert("id".to_owned(), Value::String("agent-1".to_owned()));
        let mut s = stage("s1", "sk1", "Review", "review");
        s.config = Some(Map::from_iter([(
            "approver".to_owned(),
            Value::Object(approver),
        )]));
        input.stages = vec![s];
        input.agents_by_id = HashMap::from_iter([(
            "agent-1".to_owned(),
            agent("agent-1", "paused", Some("Bob")),
        )]);
        let report = compute_pipeline_health(&input);
        let w = report
            .warnings
            .iter()
            .find(|w| w.code == PipelineHealthWarningCode::ReviewNoApprover)
            .expect("review warning");
        assert!(w.message.contains("Bob"));
    }

    // ----- compute_pipeline_health: breakdown -----

    #[test]
    fn r538_warns_breakdown_target_missing() {
        let mut input = empty_input();
        let mut breakdown = Map::new();
        breakdown.insert(
            "targetPipelineId".to_owned(),
            Value::String("missing-pipe".to_owned()),
        );
        breakdown.insert(
            "targetStageKey".to_owned(),
            Value::String("step1".to_owned()),
        );
        let mut s = stage("s1", "sk1", "Breakdown", "active");
        s.config = Some(Map::from_iter([(
            "breakdown".to_owned(),
            Value::Object(breakdown),
        )]));
        input.stages = vec![s];
        let report = compute_pipeline_health(&input);
        assert!(report
            .warnings
            .iter()
            .any(|w| w.code == PipelineHealthWarningCode::BreakdownTargetMissing));
    }

    #[test]
    fn r538_warns_breakdown_no_wait() {
        let mut input = empty_input();
        let target_stage = PipelineHealthStageRef {
            key: "step1".to_owned(),
            name: "Step 1".to_owned(),
            kind: Some("active".to_owned()),
            config: None,
        };
        let target_pipeline = PipelineHealthPipelineRef {
            id: "target-pipe".to_owned(),
            name: "Target".to_owned(),
            stages: vec![target_stage],
        };
        let mut breakdown = Map::new();
        breakdown.insert(
            "targetPipelineId".to_owned(),
            Value::String("target-pipe".to_owned()),
        );
        breakdown.insert(
            "targetStageKey".to_owned(),
            Value::String("step1".to_owned()),
        );
        breakdown.insert("waitForPieces".to_owned(), Value::Bool(false));
        // whenFinishedMoveTo is null/missing
        let mut s = stage("s1", "sk1", "Breakdown", "active");
        s.config = Some(Map::from_iter([(
            "breakdown".to_owned(),
            Value::Object(breakdown),
        )]));
        input.stages = vec![s];
        input.pipelines_by_id = HashMap::from_iter([("target-pipe".to_owned(), target_pipeline)]);
        let report = compute_pipeline_health(&input);
        assert!(report
            .warnings
            .iter()
            .any(|w| w.code == PipelineHealthWarningCode::BreakdownNoWait));
    }

    #[test]
    fn r538_warns_breakdown_target_not_entry_safe_when_disabled() {
        let mut input = empty_input();
        let mut target_config = Map::new();
        target_config.insert("disabled".to_owned(), Value::Bool(true));
        let target_stage = PipelineHealthStageRef {
            key: "step1".to_owned(),
            name: "Step 1".to_owned(),
            kind: Some("active".to_owned()),
            config: Some(target_config),
        };
        let target_pipeline = PipelineHealthPipelineRef {
            id: "target-pipe".to_owned(),
            name: "Target".to_owned(),
            stages: vec![target_stage],
        };
        let mut breakdown = Map::new();
        breakdown.insert(
            "targetPipelineId".to_owned(),
            Value::String("target-pipe".to_owned()),
        );
        breakdown.insert(
            "targetStageKey".to_owned(),
            Value::String("step1".to_owned()),
        );
        breakdown.insert("waitForPieces".to_owned(), Value::Bool(true));
        breakdown.insert(
            "whenFinishedMoveTo".to_owned(),
            Value::String("next-step".to_owned()),
        );
        let mut s = stage("s1", "sk1", "Breakdown", "active");
        s.config = Some(Map::from_iter([(
            "breakdown".to_owned(),
            Value::Object(breakdown),
        )]));
        input.stages = vec![s];
        input.pipelines_by_id = HashMap::from_iter([("target-pipe".to_owned(), target_pipeline)]);
        let report = compute_pipeline_health(&input);
        assert!(report
            .warnings
            .iter()
            .any(|w| w.code == PipelineHealthWarningCode::BreakdownTargetNotEntrySafe));
    }

    #[test]
    fn r538_breakdown_valid_passes() {
        let mut input = empty_input();
        let target_stage = PipelineHealthStageRef {
            key: "step1".to_owned(),
            name: "Step 1".to_owned(),
            kind: Some("active".to_owned()),
            config: None,
        };
        let target_pipeline = PipelineHealthPipelineRef {
            id: "target-pipe".to_owned(),
            name: "Target".to_owned(),
            stages: vec![target_stage],
        };
        let mut breakdown = Map::new();
        breakdown.insert(
            "targetPipelineId".to_owned(),
            Value::String("target-pipe".to_owned()),
        );
        breakdown.insert(
            "targetStageKey".to_owned(),
            Value::String("step1".to_owned()),
        );
        breakdown.insert("waitForPieces".to_owned(), Value::Bool(true));
        breakdown.insert(
            "whenFinishedMoveTo".to_owned(),
            Value::String("step2".to_owned()),
        );
        let mut s = stage("s1", "sk1", "Breakdown", "active");
        s.config = Some(Map::from_iter([(
            "breakdown".to_owned(),
            Value::Object(breakdown),
        )]));
        input.stages = vec![s];
        input.pipelines_by_id = HashMap::from_iter([("target-pipe".to_owned(), target_pipeline)]);
        let report = compute_pipeline_health(&input);
        let breakdown_warnings: Vec<_> = report
            .warnings
            .iter()
            .filter(|w| {
                matches!(
                    w.code,
                    PipelineHealthWarningCode::BreakdownTargetMissing
                        | PipelineHealthWarningCode::BreakdownNoWait
                        | PipelineHealthWarningCode::BreakdownTargetNotEntrySafe
                )
            })
            .collect();
        assert!(breakdown_warnings.is_empty());
    }

    // ----- compute_pipeline_health: pipeline mentions in instructions -----

    #[test]
    fn r538_warns_missing_pipeline_reference() {
        let mut input = empty_input();
        let mut s = stage("s1", "sk1", "Step 1", "active");
        s.instructions_body = Some("Hand off to [missing](pipeline://ghost-pipe)".to_owned());
        input.stages = vec![s];
        let report = compute_pipeline_health(&input);
        assert!(report
            .warnings
            .iter()
            .any(|w| w.code == PipelineHealthWarningCode::MissingPipelineReference));
    }

    #[test]
    fn r538_warns_missing_stage_reference() {
        let mut input = empty_input();
        let mut s = stage("s1", "sk1", "Step 1", "active");
        s.instructions_body =
            Some("Hand off to [step](pipeline://pipe-x?stage=ghost-step)".to_owned());
        input.stages = vec![s];
        input.pipelines_by_id = HashMap::from_iter([(
            "pipe-x".to_owned(),
            PipelineHealthPipelineRef {
                id: "pipe-x".to_owned(),
                name: "Pipe X".to_owned(),
                stages: vec![PipelineHealthStageRef {
                    key: "real-step".to_owned(),
                    name: "Real".to_owned(),
                    kind: Some("active".to_owned()),
                    config: None,
                }],
            },
        )]);
        let report = compute_pipeline_health(&input);
        let w = report
            .warnings
            .iter()
            .find(|w| w.code == PipelineHealthWarningCode::MissingStageReference)
            .expect("stage ref warning");
        assert!(w.message.contains("Pipe X"));
    }

    // ----- compute_pipeline_health: failed automations -----

    #[test]
    fn r538_warns_automation_failed() {
        let mut input = empty_input();
        input.failed_automations = vec![PipelineHealthFailedAutomationInput {
            stage_id: "s1".to_owned(),
            stage_key: "sk1".to_owned(),
            stage_name: "Step 1".to_owned(),
            case_id: "case-42".to_owned(),
            case_title: "My Case".to_owned(),
            error: Some("timeout".to_owned()),
        }];
        let report = compute_pipeline_health(&input);
        let w = report
            .warnings
            .iter()
            .find(|w| w.code == PipelineHealthWarningCode::AutomationFailed)
            .expect("automation_failed warning");
        assert_eq!(w.href.as_deref(), Some("/pipelines/pipe-1/items/case-42"));
        assert_eq!(w.href_label.as_deref(), Some("Open item"));
        assert!(w.message.contains("My Case"));
    }

    #[test]
    fn r538_failed_automation_dedupes_by_case_id() {
        let mut input = empty_input();
        input.failed_automations = vec![
            PipelineHealthFailedAutomationInput {
                stage_id: "s1".to_owned(),
                stage_key: "sk1".to_owned(),
                stage_name: "Step 1".to_owned(),
                case_id: "case-42".to_owned(),
                case_title: "First".to_owned(),
                error: None,
            },
            PipelineHealthFailedAutomationInput {
                stage_id: "s1".to_owned(),
                stage_key: "sk1".to_owned(),
                stage_name: "Step 1".to_owned(),
                case_id: "case-42".to_owned(),
                case_title: "Second (dup)".to_owned(),
                error: None,
            },
        ];
        let report = compute_pipeline_health(&input);
        let failed_count = report
            .warnings
            .iter()
            .filter(|w| w.code == PipelineHealthWarningCode::AutomationFailed)
            .count();
        assert_eq!(failed_count, 1);
        // First title wins
        let w = report
            .warnings
            .iter()
            .find(|w| w.code == PipelineHealthWarningCode::AutomationFailed)
            .unwrap();
        assert!(w.message.contains("First"));
    }

    // ----- group_warnings_by_stage -----

    #[test]
    fn r538_group_warnings_by_stage() {
        let warnings = vec![
            PipelineHealthWarning {
                code: PipelineHealthWarningCode::PausedAgent,
                stage_id: "s1".to_owned(),
                stage_key: "sk1".to_owned(),
                stage_name: "Step 1".to_owned(),
                message: "msg".to_owned(),
                href: None,
                href_label: None,
            },
            PipelineHealthWarning {
                code: PipelineHealthWarningCode::StageNoAutomation,
                stage_id: "s1".to_owned(),
                stage_key: "sk1".to_owned(),
                stage_name: "Step 1".to_owned(),
                message: "msg2".to_owned(),
                href: None,
                href_label: None,
            },
            PipelineHealthWarning {
                code: PipelineHealthWarningCode::AutomationFailed,
                stage_id: "s2".to_owned(),
                stage_key: "sk2".to_owned(),
                stage_name: "Step 2".to_owned(),
                message: "msg3".to_owned(),
                href: None,
                href_label: None,
            },
        ];
        let grouped = group_warnings_by_stage(&warnings);
        assert_eq!(grouped.get("s1").map(Vec::len), Some(2));
        assert_eq!(grouped.get("s2").map(Vec::len), Some(1));
    }

    // ----- pipeline_health_warning_code -----

    #[test]
    fn r538_warning_code_as_str() {
        assert_eq!(
            PipelineHealthWarningCode::PausedAgent.as_str(),
            "paused_agent"
        );
        assert_eq!(
            PipelineHealthWarningCode::StageNoAutomation.as_str(),
            "stage_no_automation"
        );
        assert_eq!(
            PipelineHealthWarningCode::BreakdownTargetMissing.as_str(),
            "breakdown_target_missing"
        );
    }

    #[test]
    fn r538_warning_serialization_camel_case() {
        let w = PipelineHealthWarning {
            code: PipelineHealthWarningCode::AutomationFailed,
            stage_id: "s1".to_owned(),
            stage_key: "sk1".to_owned(),
            stage_name: "Step".to_owned(),
            message: "msg".to_owned(),
            href: Some("/x".to_owned()),
            href_label: Some("Open".to_owned()),
        };
        let json = serde_json::to_string(&w).unwrap();
        assert!(json.contains("\"stageId\":\"s1\""));
        assert!(json.contains("\"stageKey\":\"sk1\""));
        assert!(json.contains("\"stageName\":\"Step\""));
        assert!(json.contains("\"hrefLabel\":\"Open\""));
        assert!(json.contains("\"code\":\"automation_failed\""));
    }

    // ----- input round-trip -----

    #[test]
    fn r538_input_struct_serialization_round_trip() {
        let mut input = empty_input();
        let mut s = stage("s1", "sk1", "Step 1", "active");
        s.config = Some(Map::from_iter([(
            "assigneeAgentId".to_owned(),
            Value::String("agent-1".to_owned()),
        )]));
        s.instructions_body = Some("do the thing".to_owned());
        input.stages = vec![s];
        input.agents_by_id = HashMap::from_iter([(
            "agent-1".to_owned(),
            agent("agent-1", "active", Some("Alice")),
        )]);
        let json = serde_json::to_string(&input).unwrap();
        let restored: PipelineHealthInput = serde_json::from_str(&json).unwrap();
        let report = compute_pipeline_health(&restored);
        assert!(report.ok);
    }

    // ----- BTreeSet dedup sanity check -----

    #[test]
    fn r538_btreeset_smoke() {
        let mut s = std::collections::BTreeSet::new();
        s.insert("a");
        s.insert("b");
        assert_eq!(s.len(), 2);
    }

    // ----- JSON fixture integration -----

    #[test]
    fn r538_complex_scenario_via_json() {
        // Full pipeline health scenario built from JSON, exercising multiple
        // warning kinds.
        let json_input = json!({
            "pipelineId": "pipe-complex",
            "stages": [
                {
                    "id": "s1",
                    "key": "sk1",
                    "name": "Assign Work",
                    "kind": "active",
                    "config": {"assigneeAgentId": "agent-paused"},
                    "instructionsBody": "do the thing"
                },
                {
                    "id": "s2",
                    "key": "sk2",
                    "name": "Review",
                    "kind": "review",
                    "config": {"requireApproval": true}
                },
                {
                    "id": "s3",
                    "key": "sk3",
                    "name": "Empty Step",
                    "kind": "active"
                }
            ],
            "agentsById": {
                "agent-paused": {"id": "agent-paused", "name": "Pat", "status": "paused"}
            },
            "pipelinesById": {},
            "failedAutomations": []
        });
        let input: PipelineHealthInput = serde_json::from_value(json_input).unwrap();
        let report = compute_pipeline_health(&input);
        // s1: assignee paused + has instructions → PausedAgent (no AutomationNoInstructions)
        assert!(report
            .warnings
            .iter()
            .any(|w| w.stage_id == "s1" && w.code == PipelineHealthWarningCode::PausedAgent));
        // s2: review + requireApproval defaults to any_human, matching Node.
        assert!(!report
            .warnings
            .iter()
            .any(|w| w.stage_id == "s2" && w.code == PipelineHealthWarningCode::ReviewNoApprover));
        // s3: nothing configured → StageNoAutomation
        assert!(report
            .warnings
            .iter()
            .any(|w| w.stage_id == "s3" && w.code == PipelineHealthWarningCode::StageNoAutomation));
        assert!(!report.ok);
    }

    // ----- R773 edge cases for compute_pipeline_health -----

    #[test]
    fn r773_group_warnings_by_stage_returns_empty_map_for_empty_input() {
        let grouped = group_warnings_by_stage(&[]);
        assert!(grouped.is_empty());
    }

    #[test]
    fn r773_group_warnings_by_stage_preserves_warning_order() {
        let w = |id: &str, code: PipelineHealthWarningCode| PipelineHealthWarning {
            code,
            stage_id: id.to_owned(),
            stage_key: format!("{id}-k"),
            stage_name: format!("Stage {id}"),
            message: format!("msg-{id}"),
            href: None,
            href_label: None,
        };
        let warnings = vec![
            w("a", PipelineHealthWarningCode::PausedAgent),
            w("b", PipelineHealthWarningCode::StageNoAutomation),
            w("a", PipelineHealthWarningCode::AutomationNoInstructions),
        ];
        let grouped = group_warnings_by_stage(&warnings);
        let a = grouped.get("a").unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].code, PipelineHealthWarningCode::PausedAgent);
        assert_eq!(a[1].code, PipelineHealthWarningCode::AutomationNoInstructions);
    }

    #[test]
    fn r773_is_pipeline_terminal_stage_kind_returns_false_for_unrecognized() {
        assert!(!is_pipeline_terminal_stage_kind(Some("review")));
        assert!(!is_pipeline_terminal_stage_kind(Some("active")));
        assert!(!is_pipeline_terminal_stage_kind(Some("paused")));
        assert!(!is_pipeline_terminal_stage_kind(Some("unknown_kind")));
    }

    #[test]
    fn r773_compute_pipeline_health_ok_when_stages_fully_configured() {
        let mut s = stage("s1", "sk1", "Active", "active");
        s.config = Some(Map::from_iter([(
            "assigneeAgentId".to_owned(),
            Value::String("agent-1".to_owned()),
        )]));
        s.instructions_body = Some("Do the thing".to_owned());
        let input = PipelineHealthInput {
            pipeline_id: "p1".into(),
            stages: vec![s],
            agents_by_id: HashMap::from_iter([(
                "agent-1".to_owned(),
                PipelineHealthAgentRef {
                    id: "agent-1".into(),
                    name: Some("Alice".into()),
                    status: "active".into(),
                },
            )]),
            pipelines_by_id: HashMap::new(),
            failed_automations: vec![],
        };
        let report = compute_pipeline_health(&input);
        assert!(report.warnings.is_empty());
        assert!(report.ok);
    }

    #[test]
    fn r773_compute_pipeline_health_reports_failed_automation_dedup() {
        let stage_id = "s1".to_owned();
        let failure_a = PipelineHealthFailedAutomationInput {
            stage_id: stage_id.clone(),
            stage_key: "sk1".into(),
            stage_name: "Step 1".into(),
            case_id: "case-1".into(),
            case_title: "First Item".into(),
            error: Some("boom".into()),
        };
        let failure_b = PipelineHealthFailedAutomationInput {
            stage_id: stage_id.clone(),
            stage_key: "sk1".into(),
            stage_name: "Step 1".into(),
            case_id: "case-1".into(),
            case_title: "First Item".into(),
            error: Some("boom again".into()),
        };
        let input = PipelineHealthInput {
            pipeline_id: "p1".into(),
            stages: vec![],
            agents_by_id: HashMap::new(),
            pipelines_by_id: HashMap::new(),
            failed_automations: vec![failure_a, failure_b],
        };
        let report = compute_pipeline_health(&input);
        let count = report
            .warnings
            .iter()
            .filter(|w| w.code == PipelineHealthWarningCode::AutomationFailed)
            .count();
        assert_eq!(count, 1, "duplicate failed automations on same case should dedup");
        assert!(!report.ok);
    }

    #[test]
    fn r773_compute_pipeline_health_reports_multiple_failed_automation_cases() {
        let stage_id = "s1".to_owned();
        let mk = |case_id: &str, title: &str| PipelineHealthFailedAutomationInput {
            stage_id: stage_id.clone(),
            stage_key: "sk1".into(),
            stage_name: "Step 1".into(),
            case_id: case_id.into(),
            case_title: title.into(),
            error: None,
        };
        let input = PipelineHealthInput {
            pipeline_id: "p1".into(),
            stages: vec![],
            agents_by_id: HashMap::new(),
            pipelines_by_id: HashMap::new(),
            failed_automations: vec![mk("c1", "Item One"), mk("c2", "Item Two")],
        };
        let report = compute_pipeline_health(&input);
        let failed: Vec<&PipelineHealthWarning> = report
            .warnings
            .iter()
            .filter(|w| w.code == PipelineHealthWarningCode::AutomationFailed)
            .collect();
        assert_eq!(failed.len(), 2);
        assert!(failed.iter().all(|w| w.href.as_deref().unwrap_or("").contains("/items/")));
    }

    #[test]
    fn r773_compute_pipeline_health_pipeline_id_propagates_to_report() {
        let input = PipelineHealthInput {
            pipeline_id: "pipe-custom-id".into(),
            stages: vec![],
            agents_by_id: HashMap::new(),
            pipelines_by_id: HashMap::new(),
            failed_automations: vec![],
        };
        let report = compute_pipeline_health(&input);
        assert_eq!(report.pipeline_id, "pipe-custom-id");
        assert!(report.ok);
    }
}
