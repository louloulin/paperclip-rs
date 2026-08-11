//! Hand-written OpenAPI 3.1 component schemas for the 5 core DTOs.
//!
//! **Why hand-written, not derived?** The current Rust types
//! (`pc_repos::decision::DecisionRow`, `pc_repos::company::CompanyRow`, etc.)
//! use field-level `serde_json::Value` for some fields (e.g. `options`,
//! `permissions`, `execution_state`) which don't map cleanly to a single
//! OpenAPI `type`. Until we adopt `utoipa::ToSchema` derives across all
//! pc-repos types (a much larger refactor — see ARCHITECTURE.md §6 R504),
//! hand-rolling the schemas for the top-5 most-consumed DTOs gives us:
//!
//! - **High cohesion**: all DTO schemas live in one module
//! - **Low coupling**: pc-openapi does NOT depend on pc-repos
//! - **Real evolution path**: when `utoipa` lands, this file is the
//!   authoritative reference for what each schema should look like
//!
//! Adding more DTOs is a 20-line change per type — extend the
//! `register_core_dtos` function and add a `define_*_schema()` helper.

use crate::builder::OpenApiRegistry;
use crate::schema::SchemaRef;
use serde_json::{json, Value};

/// Register schemas for the 5 core DTOs into the registry.
///
/// Idempotent: registering the same name twice is allowed (the registry
/// overwrites). All schemas are added to `components.schemas` of the
/// resulting OpenAPI 3.1 spec.
pub fn register_core_dtos(reg: &mut OpenApiRegistry) {
    // R505: use register_schema_value to bypass SchemaRef::Inline’s
    // `#[serde(flatten)]` which silently drops `type: "object"` and
    // `required` from the wire format.
    reg.register_schema_value("Decision", decision_schema());
    // R518: companion schemas referenced by Decision.options / DecisionOption.effects.
    reg.register_schema_value("DecisionOption", decision_option_schema());
    reg.register_schema_value("DecisionEffect", decision_effect_schema());
    reg.register_schema_value("Company", company_schema());
    reg.register_schema_value("Issue", issue_schema());
    reg.register_schema_value("Agent", agent_schema());
    reg.register_schema_value("HeartbeatRun", heartbeat_run_schema());
    // R507: list shapes for the 4 collection GET endpoints.
    reg.register_schema_value("CompanyList", company_list_schema());
    reg.register_schema_value("AgentList", agent_list_schema());
    reg.register_schema_value("IssueList", issue_list_schema());
    reg.register_schema_value("DecisionList", decision_list_schema());
    // R507: approvals + pipelines (referenced by path hints).
    reg.register_schema_value("Approval", approval_schema());
    reg.register_schema_value("ApprovalList", approval_list_schema());
    reg.register_schema_value("PipelineList", pipeline_list_schema());
    // R508: domain-rich Pipeline + Routine shapes.
    reg.register_schema_value("Pipeline", pipeline_schema());
    reg.register_schema_value("Routine", routine_schema());
    reg.register_schema_value("RoutineList", routine_list_schema());
    // R509: error response shapes.
    reg.register_schema_value("ValidationError", validation_error_schema());
    reg.register_schema_value("ValidationErrorList", validation_error_list_schema());
    reg.register_schema_value("ErrorResponse", error_response_schema());
    // R510: pagination.
    reg.register_schema_value("PaginationCursor", pagination_cursor_schema());
    // R511: 4 domain shapes (Case/Goal/Inbox/Folder) + 4 list shapes.
    reg.register_schema_value("Case", case_schema());
    reg.register_schema_value("Goal", goal_schema());
    reg.register_schema_value("Inbox", inbox_schema());
    reg.register_schema_value("Folder", folder_schema());
    reg.register_schema_value("CaseList", case_list_schema());
    reg.register_schema_value("GoalList", goal_list_schema());
    reg.register_schema_value("InboxList", inbox_list_schema());
    reg.register_schema_value("FolderList", folder_list_schema());
    // R513: admin + companies sub-resources.
    reg.register_schema_value("CompanyMember", company_member_schema());
    reg.register_schema_value("Invite", invite_schema());
    reg.register_schema_value("AdminUser", admin_user_schema());
    reg.register_schema_value("CompanyMemberList", company_member_list_schema());
    reg.register_schema_value("InviteList", invite_list_schema());
    reg.register_schema_value("AdminUserList", admin_user_list_schema());
    // R522: Companies aggregation endpoints.
    reg.register_schema_value("CompanyStats", company_stats_schema());
    reg.register_schema_value("CompanyStatsList", company_stats_list_schema());
    reg.register_schema_value("CompanyTimelineResult", company_timeline_result_schema());
    reg.register_schema_value("CompanyArtifact", company_artifact_schema());
    reg.register_schema_value("CompanyArtifactList", company_artifact_list_schema());
    reg.register_schema_value("CompanyOrgChart", company_org_chart_schema());
}

/// OpenAPI schema for the upstream `Decision` shape (mirrors
/// `pc_repos::decision::DecisionRow`).
pub fn decision_schema() -> Value {
    json!({
        "type": "object",
        "description": "A decision awaiting or already resolved by an authorized actor.",
        "properties": {
            "id": { "type": "string", "format": "uuid" },
            "companyId": { "type": "string", "format": "uuid" },
            "bundleId": { "type": ["string", "null"], "format": "uuid" },
            "originAgentId": { "type": ["string", "null"], "format": "uuid" },
            "originIssueId": { "type": ["string", "null"], "format": "uuid" },
            "originRunId": { "type": ["string", "null"], "format": "uuid" },
            "ruleKey": { "type": ["string", "null"] },
            "title": { "type": "string" },
            "body": { "type": "string" },
            "options": {
                "type": "array",
                "items": { "$ref": "#/components/schemas/DecisionOption" }
            },
            "inputs": { "type": ["array", "null"] },
            "status": { "type": "string", "enum": ["open", "decided", "cancelled", "expired", "dismissed"] },
            "executionStatus": { "type": ["string", "null"] },
            "chosenOptionId": { "type": ["string", "null"] },
            "inputValues": { "type": ["object", "null"] },
            "decidedByUserId": { "type": ["string", "null"] },
            "decidedAt": { "type": ["string", "null"], "format": "date-time" },
            "expiresAt": { "type": "string", "format": "date-time" },
            "idempotencyKey": { "type": ["string", "null"] },
            "signedSpec": { "type": "string" },
            "targetSnapshots": { "type": "object" },
            "continuationPolicy": { "type": "string", "enum": ["none", "wake_origin_agent"] },
            "metadata": { "type": "object" },
            "createdAt": { "type": "string", "format": "date-time" },
            "updatedAt": { "type": "string", "format": "date-time" }
        },
        "required": ["id", "companyId", "title", "body", "options", "status", "expiresAt", "signedSpec", "continuationPolicy", "createdAt", "updatedAt"]
    })
}

/// Companion schema for the option entries inside `Decision.options`.
pub fn decision_option_schema() -> Value {
    json!({
        "type": "object",
        "description": "One option presented to the actor for selection.",
        "properties": {
            "id": { "type": "string" },
            "label": { "type": "string" },
            "description": { "type": ["string", "null"] },
            "effects": {
                "type": "array",
                "items": { "$ref": "#/components/schemas/DecisionEffect" }
            },
            "targetIds": {
                "type": "array",
                "items": { "type": "string" }
            }
        },
        "required": ["id", "label"]
    })
}

/// Companion schema for the effect entries inside `DecisionOption.effects`.
pub fn decision_effect_schema() -> Value {
    json!({
        "type": "object",
        "description": "A side-effect to be applied when the option is chosen.",
        "properties": {
            "type": {
                "type": "string",
                "enum": [
                    "comment_on_issue",
                    "update_issue_status",
                    "cancel_issue_tree",
                    "resolve_blocker",
                    "update_issue_assignee",
                    "add_issue_label",
                    "remove_issue_label"
                ]
            },
            "targetIssueId": { "type": ["string", "null"] },
            "staleness": { "type": ["string", "null"], "enum": ["lenient", "strict"] },
            "body": { "type": ["string", "null"] },
            "status": { "type": ["string", "null"] }
        },
        "required": ["type"]
    })
}

/// OpenAPI schema for `pc_repos::company::CompanyRow`.
pub fn company_schema() -> Value {
    json!({
        "type": "object",
        "description": "A company is the top-level tenant in Paperclip.",
        "properties": {
            "id": { "type": "string", "format": "uuid" },
            "name": { "type": "string" },
            "description": { "type": ["string", "null"] },
            "status": { "type": "string", "enum": ["active", "paused", "archived"] },
            "pauseReason": { "type": ["string", "null"] },
            "pausedAt": { "type": ["string", "null"], "format": "date-time" },
            "issuePrefix": { "type": "string" },
            "issueCounter": { "type": "integer", "format": "int32" },
            "budgetMonthlyCents": { "type": "integer", "format": "int32" },
            "spentMonthlyCents": { "type": "integer", "format": "int32" },
            "attachmentMaxBytes": { "type": "integer", "format": "int32" },
            "defaultResponsibleUserId": { "type": ["string", "null"] },
            "requireBoardApprovalForNewAgents": { "type": "boolean" },
            "feedbackDataSharingEnabled": { "type": "boolean" },
            "feedbackDataSharingConsentAt": { "type": ["string", "null"], "format": "date-time" },
            "feedbackDataSharingConsentByUserId": { "type": ["string", "null"] },
            "feedbackDataSharingTermsVersion": { "type": ["string", "null"] },
            "brandColor": { "type": ["string", "null"] },
            "createdAt": { "type": "string", "format": "date-time" },
            "updatedAt": { "type": "string", "format": "date-time" }
        },
        "required": [
            "id", "name", "status", "issuePrefix", "issueCounter",
            "budgetMonthlyCents", "spentMonthlyCents", "attachmentMaxBytes",
            "requireBoardApprovalForNewAgents", "feedbackDataSharingEnabled",
            "createdAt", "updatedAt"
        ]
    })
}

/// OpenAPI schema for `pc_repos::issue::IssueRow`.
pub fn issue_schema() -> Value {
    json!({
        "type": "object",
        "description": "An issue is a unit of work tracked by Paperclip.",
        "properties": {
            "id": { "type": "string", "format": "uuid" },
            "companyId": { "type": "string", "format": "uuid" },
            "projectId": { "type": ["string", "null"], "format": "uuid" },
            "parentId": { "type": ["string", "null"], "format": "uuid" },
            "title": { "type": "string" },
            "description": { "type": ["string", "null"] },
            "status": { "type": "string" },
            "workMode": { "type": "string" },
            "priority": { "type": "string", "enum": ["low", "normal", "high", "urgent"] },
            "assigneeAgentId": { "type": ["string", "null"], "format": "uuid" },
            "assigneeUserId": { "type": ["string", "null"] },
            "issueNumber": { "type": ["integer", "null"], "format": "int32" },
            "identifier": { "type": ["string", "null"] },
            "originKind": { "type": "string" },
            "requestDepth": { "type": "integer", "format": "int32" },
            "createdAt": { "type": "string", "format": "date-time" },
            "updatedAt": { "type": "string", "format": "date-time" }
        },
        "required": [
            "id", "companyId", "title", "status", "workMode", "priority",
            "originKind", "requestDepth", "createdAt", "updatedAt"
        ]
    })
}

/// OpenAPI schema for `pc_repos::agent::AgentRow`.
pub fn agent_schema() -> Value {
    json!({
        "type": "object",
        "description": "An agent is an AI worker configured to run tasks on behalf of a company.",
        "properties": {
            "id": { "type": "string", "format": "uuid" },
            "companyId": { "type": "string", "format": "uuid" },
            "name": { "type": "string" },
            "role": { "type": "string" },
            "title": { "type": ["string", "null"] },
            "icon": { "type": ["string", "null"] },
            "status": { "type": "string", "enum": ["active", "paused", "error"] },
            "reportsTo": { "type": ["string", "null"], "format": "uuid" },
            "capabilities": { "type": ["string", "null"] },
            "adapterType": { "type": "string" },
            "adapterConfig": { "type": "object" },
            "runtimeConfig": { "type": "object" },
            "defaultEnvironmentId": { "type": ["string", "null"], "format": "uuid" },
            "budgetMonthlyCents": { "type": "integer", "format": "int32" },
            "spentMonthlyCents": { "type": "integer", "format": "int32" },
            "pauseReason": { "type": ["string", "null"] },
            "pausedAt": { "type": ["string", "null"], "format": "date-time" },
            "errorReason": { "type": ["string", "null"] },
            "permissions": { "type": "object" },
            "lastHeartbeatAt": { "type": ["string", "null"], "format": "date-time" },
            "metadata": { "type": ["object", "null"] },
            "createdAt": { "type": "string", "format": "date-time" },
            "updatedAt": { "type": "string", "format": "date-time" }
        },
        "required": [
            "id", "companyId", "name", "role", "status",
            "adapterType", "adapterConfig", "runtimeConfig",
            "budgetMonthlyCents", "spentMonthlyCents",
            "permissions", "createdAt", "updatedAt"
        ]
    })
}

/// OpenAPI schema for `pc_repos::heartbeat::HeartbeatRunSummaryRow`.
pub fn heartbeat_run_schema() -> Value {
    json!({
        "type": "object",
        "description": "A single heartbeat execution cycle for an agent.",
        "properties": {
            "id": { "type": "string", "format": "uuid" },
            "agentId": { "type": "string", "format": "uuid" },
            "status": {
                "type": "string",
                "enum": ["running", "succeeded", "failed", "cancelled", "timed_out"]
            },
            "startedAt": { "type": "string", "format": "date-time" },
            "finishedAt": { "type": ["string", "null"], "format": "date-time" },
            "prompt": { "type": ["string", "null"] },
            "error": { "type": ["string", "null"] }
        },
        "required": ["id", "agentId", "status", "startedAt"]
    })
}

/// OpenAPI schema for the array-of-Company list response (e.g. `GET /api/companies`).
pub fn company_list_schema() -> Value {
    json!({
        "type": "array",
        "items": { "$ref": "#/components/schemas/Company" }
    })
}

/// OpenAPI schema for the array-of-Agent list response (e.g. `GET /api/agents`).
pub fn agent_list_schema() -> Value {
    json!({
        "type": "array",
        "items": { "$ref": "#/components/schemas/Agent" }
    })
}

/// OpenAPI schema for the array-of-Issue list response (e.g. `GET /api/issues`).
pub fn issue_list_schema() -> Value {
    json!({
        "type": "array",
        "items": { "$ref": "#/components/schemas/Issue" }
    })
}

/// OpenAPI schema for the array-of-Decision list response (e.g. `GET /api/decisions`).
pub fn decision_list_schema() -> Value {
    json!({
        "type": "array",
        "items": { "$ref": "#/components/schemas/Decision" }
    })
}

/// R507: Approval shape (decision-style approval request).
pub fn approval_schema() -> Value {
    json!({
        "type": "object",
        "description": "An approval request awaiting decision by a board actor.",
        "properties": {
            "id": { "type": "string", "format": "uuid" },
            "companyId": { "type": "string", "format": "uuid" },
            "kind": { "type": "string" },
            "status": { "type": "string", "enum": ["pending", "approved", "rejected", "rescinded"] },
            "subjectType": { "type": "string" },
            "subjectId": { "type": "string" },
            "requestedByUserId": { "type": ["string", "null"] },
            "decidedByUserId": { "type": ["string", "null"] },
            "decidedAt": { "type": ["string", "null"], "format": "date-time" },
            "payload": { "type": "object" },
            "createdAt": { "type": "string", "format": "date-time" },
            "updatedAt": { "type": "string", "format": "date-time" }
        },
        "required": ["id", "companyId", "kind", "status", "subjectType", "subjectId", "createdAt", "updatedAt"]
    })
}

/// R507: Approval list response shape (array of Approval).
pub fn approval_list_schema() -> Value {
    json!({
        "type": "array",
        "items": { "$ref": "#/components/schemas/Approval" }
    })
}

/// R507: Pipeline list response shape (array of Pipeline).
/// Pipeline shape itself is not yet defined; we use a placeholder object.
pub fn pipeline_list_schema() -> Value {
    json!({
        "type": "array",
        "items": { "$ref": "#/components/schemas/Pipeline" }
    })
}

/// R508: Pipeline shape (mirrors `pc_repos::pipeline::PipelineRow`).
pub fn pipeline_schema() -> Value {
    json!({
        "type": "object",
        "description": "A pipeline defines stage transitions for cases (issues being routed through a workflow).",
        "properties": {
            "id": { "type": "string", "format": "uuid" },
            "companyId": { "type": "string", "format": "uuid" },
            "projectId": { "type": ["string", "null"], "format": "uuid" },
            "key": { "type": "string" },
            "name": { "type": "string" },
            "description": { "type": ["string", "null"] },
            "enforceTransitions": { "type": "boolean" },
            "createdByUserId": { "type": ["string", "null"] },
            "createdByAgentId": { "type": ["string", "null"], "format": "uuid" },
            "archivedAt": { "type": ["string", "null"], "format": "date-time" },
            "createdAt": { "type": "string", "format": "date-time" },
            "updatedAt": { "type": "string", "format": "date-time" }
        },
        "required": [
            "id", "companyId", "key", "name", "enforceTransitions",
            "createdAt", "updatedAt"
        ]
    })
}

/// R508: Routine shape (mirrors `pc_repos::routine::RoutineRow`).
pub fn routine_schema() -> Value {
    json!({
        "type": "object",
        "description": "A routine is a scheduled workflow triggered on a cron schedule or manually.",
        "properties": {
            "id": { "type": "string", "format": "uuid" },
            "companyId": { "type": "string", "format": "uuid" },
            "projectId": { "type": ["string", "null"], "format": "uuid" },
            "folderId": { "type": ["string", "null"], "format": "uuid" },
            "goalId": { "type": ["string", "null"], "format": "uuid" },
            "parentIssueId": { "type": ["string", "null"], "format": "uuid" },
            "title": { "type": "string" },
            "description": { "type": ["string", "null"] },
            "assigneeAgentId": { "type": ["string", "null"], "format": "uuid" },
            "priority": { "type": "string", "enum": ["low", "normal", "high", "urgent"] },
            "status": { "type": "string", "enum": ["active", "paused", "archived"] },
            "concurrencyPolicy": { "type": "string", "enum": ["skip", "queue", "parallel"] },
            "catchUpPolicy": { "type": "string", "enum": ["none", "latest", "all"] },
            "activityGatePolicy": { "type": "string" },
            "activityGateScope": { "type": "string" },
            "originKind": { "type": "string" },
            "originId": { "type": ["string", "null"] },
            "variables": { "type": "object" },
            "env": { "type": ["object", "null"] },
            "latestRevisionId": { "type": ["string", "null"], "format": "uuid" },
            "latestRevisionNumber": { "type": "integer", "format": "int32" },
            "createdByAgentId": { "type": ["string", "null"], "format": "uuid" },
            "createdByUserId": { "type": ["string", "null"] },
            "responsibleUserId": { "type": ["string", "null"] },
            "updatedByAgentId": { "type": ["string", "null"], "format": "uuid" },
            "updatedByUserId": { "type": ["string", "null"] },
            "lastTriggeredAt": { "type": ["string", "null"], "format": "date-time" },
            "lastEnqueuedAt": { "type": ["string", "null"], "format": "date-time" },
            "createdAt": { "type": "string", "format": "date-time" },
            "updatedAt": { "type": "string", "format": "date-time" }
        },
        "required": [
            "id", "companyId", "title", "priority", "status",
            "concurrencyPolicy", "catchUpPolicy", "activityGatePolicy", "activityGateScope",
            "originKind", "variables", "latestRevisionNumber",
            "createdAt", "updatedAt"
        ]
    })
}

/// R509: ValidationError — field-level error structure used in 422
/// responses. Mirrors upstream `routes/openapi.ts` error shape.
pub fn validation_error_schema() -> Value {
    json!({
        "type": "object",
        "description": "A single field-level validation error.",
        "properties": {
            "field": {
                "type": "string",
                "description": "Dot-path to the offending field (e.g. `body.options[0].label`)."
            },
            "code": {
                "type": "string",
                "description": "Machine-readable error code (e.g. `required`, `enum_violation`, `length_exceeded`)."
            },
            "message": {
                "type": "string",
                "description": "Human-readable error message."
            }
        },
        "required": ["field", "code", "message"]
    })
}

/// R509: ValidationErrorList — wrapper returned by 422 responses.
pub fn validation_error_list_schema() -> Value {
    json!({
        "type": "object",
        "description": "Wrapper for a list of field-level validation errors returned on HTTP 422.",
        "properties": {
            "errors": {
                "type": "array",
                "items": { "$ref": "#/components/schemas/ValidationError" }
            },
            "traceId": {
                "type": ["string", "null"],
                "description": "Distributed trace id for correlating this error with server logs."
            }
        },
        "required": ["errors"]
    })
}

/// R510: Pagination cursor — server returns this in list responses so the
/// client can request the next page without scanning the full result set.
pub fn pagination_cursor_schema() -> Value {
    json!({
        "type": "object",
        "description": "Opaque pagination cursor emitted by list endpoints. Pass back verbatim to fetch the next page.",
        "properties": {
            "nextCursor": {
                "type": ["string", "null"],
                "description": "Opaque cursor token for the next page, or null when there are no more results."
            },
            "totalCount": {
                "type": ["integer", "null"],
                "format": "int64",
                "description": "Optional total count when the backend can compute it cheaply (null otherwise)."
            },
            "hasMore": {
                "type": "boolean",
                "description": "True if `nextCursor` is set."
            }
        },
        "required": ["hasMore"]
    })
}

/// R510: List response envelope — wraps a typed array with pagination metadata.
/// Mirrors the upstream `routes/openapi.ts` `*ListResponse` shape.
pub fn list_response_envelope_schema(item_schema_ref: &str) -> Value {
    json!({
        "type": "object",
        "description": "Generic list response wrapper with pagination cursor.",
        "properties": {
            "items": {
                "type": "array",
                "items": { "$ref": format!("#/components/schemas/{item_schema_ref}") }
            },
            "pagination": { "$ref": "#/components/schemas/PaginationCursor" }
        },
        "required": ["items", "pagination"]
    })
}

/// R522: Per-company stats returned by GET /api/companies/:company_id/stats.
pub fn company_stats_schema() -> Value {
    json!({
        "type": "object",
        "description": "Per-company aggregate statistics.",
        "properties": {
            "companyId": { "type": "string" },
            "agentCount": { "type": "integer", "format": "int64" },
            "activeAgentCount": { "type": "integer", "format": "int64" },
            "issueCount": { "type": "integer", "format": "int64" },
            "openIssueCount": { "type": "integer", "format": "int64" },
            "decisionsPending": { "type": "integer", "format": "int64" },
            "monthlySpendCents": { "type": "integer", "format": "int64" },
            "monthlyBudgetCents": { "type": ["integer", "null"], "format": "int64" },
            "lastActivityAt": { "type": ["string", "null"], "format": "date-time" }
        },
        "required": ["companyId", "agentCount", "issueCount", "monthlySpendCents"]
    })
}

/// R522: Global stats list returned by GET /api/companies/stats.
pub fn company_stats_list_schema() -> Value {
    json!({
        "type": "object",
        "description": "Per-company stats for every company the caller can see.",
        "properties": {
            "items": {
                "type": "array",
                "items": { "$ref": "#/components/schemas/CompanyStats" }
            }
        },
        "required": ["items"]
    })
}

/// R522: Work-timeline result returned by GET /api/companies/:company_id/timeline.
pub fn company_timeline_result_schema() -> Value {
    json!({
        "type": "object",
        "description": "Work timeline spanning actors / spans / events / edges.",
        "properties": {
            "actors": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "kind": { "type": "string", "enum": ["user", "agent"] },
                        "displayName": { "type": "string" }
                    },
                    "required": ["id", "kind", "displayName"]
                }
            },
            "spans": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "kind": { "type": "string" },
                        "startedAt": { "type": "string", "format": "date-time" },
                        "endedAt": { "type": ["string", "null"], "format": "date-time" },
                        "actorId": { "type": ["string", "null"] },
                        "issueId": { "type": ["string", "null"] },
                        "metadata": { "type": ["object", "null"] }
                    },
                    "required": ["id", "kind", "startedAt"]
                }
            },
            "events": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "kind": { "type": "string" },
                        "occurredAt": { "type": "string", "format": "date-time" },
                        "actorId": { "type": ["string", "null"] },
                        "targetId": { "type": ["string", "null"] },
                        "payload": { "type": ["object", "null"] }
                    },
                    "required": ["id", "kind", "occurredAt"]
                }
            },
            "edges": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "fromId": { "type": "string" },
                        "toId": { "type": "string" },
                        "kind": { "type": "string", "enum": ["blocks", "relates", "parent_of", "assigned_to"] }
                    },
                    "required": ["fromId", "toId", "kind"]
                }
            }
        },
        "required": ["actors", "spans", "events", "edges"]
    })
}

/// R522: A single company artifact (returned by GET /api/companies/:company_id/artifacts).
pub fn company_artifact_schema() -> Value {
    json!({
        "type": "object",
        "description": "A build artifact or work product attached to a company.",
        "properties": {
            "id": { "type": "string" },
            "kind": { "type": "string", "enum": ["build", "report", "export", "log", "other"] },
            "name": { "type": "string" },
            "sizeBytes": { "type": ["integer", "null"], "format": "int64" },
            "contentType": { "type": ["string", "null"] },
            "createdAt": { "type": "string", "format": "date-time" },
            "createdByUserId": { "type": ["string", "null"] },
            "downloadUrl": { "type": ["string", "null"] },
            "metadata": { "type": ["object", "null"] }
        },
        "required": ["id", "kind", "name", "createdAt"]
    })
}

/// R522: List of company artifacts returned by GET /api/companies/:company_id/artifacts.
pub fn company_artifact_list_schema() -> Value {
    json!({
        "type": "object",
        "description": "Paginated list of company artifacts.",
        "properties": {
            "items": {
                "type": "array",
                "items": { "$ref": "#/components/schemas/CompanyArtifact" }
            },
            "pagination": { "$ref": "#/components/schemas/PaginationCursor" }
        },
        "required": ["items", "pagination"]
    })
}

/// R522: Org-chart structure returned by GET /api/companies/:company_id/org.
pub fn company_org_chart_schema() -> Value {
    json!({
        "type": "object",
        "description": "Hierarchical org chart for a company's agents.",
        "properties": {
            "nodes": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "agentId": { "type": ["string", "null"] },
                        "userId": { "type": ["string", "null"] },
                        "name": { "type": "string" },
                        "title": { "type": ["string", "null"] },
                        "reportsTo": { "type": ["string", "null"] }
                    },
                    "required": ["id", "name"]
                }
            },
            "edges": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "fromId": { "type": "string" },
                        "toId": { "type": "string" }
                    },
                    "required": ["fromId", "toId"]
                }
            }
        },
        "required": ["nodes", "edges"]
    })
}

/// R509: ErrorResponse — generic 4xx/5xx error body for non-422 responses.
pub fn error_response_schema() -> Value {
    json!({
        "type": "object",
        "description": "Generic error body returned for 400 / 401 / 403 / 404 / 409 / 500 responses.",
        "properties": {
            "code": {
                "type": "string",
                "description": "Machine-readable error code (e.g. `unauthorized`, `not_found`, `internal_error`)."
            },
            "message": {
                "type": "string",
                "description": "Human-readable error message."
            },
            "traceId": {
                "type": ["string", "null"],
                "description": "Distributed trace id for correlating this error with server logs."
            }
        },
        "required": ["code", "message"]
    })
}

/// R508: Routine list response shape.
pub fn routine_list_schema() -> Value {
    json!({
        "type": "array",
        "items": { "$ref": "#/components/schemas/Routine" }
    })
}

/// R511: Case schema (mirrors `pc_repos::case::CaseRow`).
pub fn case_schema() -> Value {
    json!({
        "type": "object",
        "description": "A case aggregates a unit of work across issues, docs, labels, and annotations.",
        "properties": {
            "id": { "type": "string", "format": "uuid" },
            "companyId": { "type": "string", "format": "uuid" },
            "projectId": { "type": ["string", "null"], "format": "uuid" },
            "caseNumber": { "type": "integer", "format": "int32" },
            "identifier": { "type": "string", "description": "Human-readable identifier (e.g. `cm1-c24`)." },
            "caseType": { "type": "string" },
            "key": { "type": ["string", "null"] },
            "title": { "type": "string" },
            "summary": { "type": ["string", "null"] },
            "status": {
                "type": "string",
                "enum": ["draft", "in_progress", "in_review", "approved", "done", "cancelled"]
            },
            "fields": { "type": "object" },
            "parentCaseId": { "type": ["string", "null"], "format": "uuid" },
            "createdByAgentId": { "type": ["string", "null"], "format": "uuid" },
            "createdByUserId": { "type": ["string", "null"] },
            "completedAt": { "type": ["string", "null"], "format": "date-time" },
            "createdAt": { "type": "string", "format": "date-time" },
            "updatedAt": { "type": "string", "format": "date-time" }
        },
        "required": [
            "id", "companyId", "caseNumber", "identifier", "caseType",
            "title", "status", "createdAt", "updatedAt"
        ]
    })
}

/// R511: Goal schema (mirrors `pc_repos::goal::GoalRow`).
pub fn goal_schema() -> Value {
    json!({
        "type": "object",
        "description": "A goal in the company goal hierarchy (mission / company / team / project / task).",
        "properties": {
            "id": { "type": "string", "format": "uuid" },
            "companyId": { "type": "string", "format": "uuid" },
            "title": { "type": "string" },
            "description": { "type": ["string", "null"] },
            "level": {
                "type": "string",
                "enum": ["mission", "company", "team", "project", "task"]
            },
            "status": {
                "type": "string",
                "enum": ["planned", "active", "completed", "cancelled", "blocked"]
            },
            "parentId": { "type": ["string", "null"], "format": "uuid" },
            "ownerAgentId": { "type": ["string", "null"], "format": "uuid" },
            "createdAt": { "type": "string", "format": "date-time" },
            "updatedAt": { "type": "string", "format": "date-time" }
        },
        "required": ["id", "companyId", "title", "level", "status", "createdAt", "updatedAt"]
    })
}

/// R511: Inbox schema (mirrors `pc_repos::inbox::InboxDismissalRow`).
pub fn inbox_schema() -> Value {
    json!({
        "type": "object",
        "description": "A user-scoped inbox dismissal or snooze state for an inbox item.",
        "properties": {
            "id": { "type": "string", "format": "uuid" },
            "companyId": { "type": "string", "format": "uuid" },
            "userId": { "type": "string", "description": "Opaque user identity (auth subject id)." },
            "itemKey": { "type": "string", "description": "`{kind}:{scope}:{entity}` (e.g. `approval:cm1:ap42`)." },
            "kind": {
                "type": "string",
                "enum": ["dismiss", "snooze"],
                "description": "`dismiss` is permanent until restore; `snooze` expires at `snoozedUntil`."
            },
            "dismissedAt": { "type": "string", "format": "date-time" },
            "snoozedUntil": {
                "type": ["string", "null"],
                "format": "date-time",
                "description": "Required when kind=snooze; must be null when kind=dismiss."
            },
            "createdAt": { "type": "string", "format": "date-time" },
            "updatedAt": { "type": "string", "format": "date-time" }
        },
        "required": [
            "id", "companyId", "userId", "itemKey", "kind",
            "dismissedAt", "createdAt", "updatedAt"
        ]
    })
}

/// R511: Folder schema (mirrors `pc_repos::folder::FolderRow`).
pub fn folder_schema() -> Value {
    json!({
        "type": "object",
        "description": "A folder groups routines or skills. Supports nested hierarchy via `parentId`.",
        "properties": {
            "id": { "type": "string", "format": "uuid" },
            "companyId": { "type": "string", "format": "uuid" },
            "kind": {
                "type": "string",
                "enum": ["routine", "skill"],
                "description": "Folder stores routines or skills; the two trees are independent."
            },
            "parentId": { "type": ["string", "null"], "format": "uuid" },
            "name": { "type": "string" },
            "slug": { "type": "string", "description": "URL-safe slug, unique within a (company, kind, parent) scope." },
            "systemKey": { "type": ["string", "null"], "description": "Reserved for built-in folders (e.g. `inbox`)." },
            "color": { "type": ["string", "null"] },
            "position": { "type": "integer", "format": "int32", "description": "Sibling ordering hint." },
            "createdAt": { "type": "string", "format": "date-time" },
            "updatedAt": { "type": "string", "format": "date-time" }
        },
        "required": [
            "id", "companyId", "kind", "name", "slug", "position",
            "createdAt", "updatedAt"
        ]
    })
}

/// R511: Case list response shape (array of Case).
pub fn case_list_schema() -> Value {
    json!({
        "type": "array",
        "items": { "$ref": "#/components/schemas/Case" }
    })
}

/// R511: Goal list response shape (array of Goal).
pub fn goal_list_schema() -> Value {
    json!({
        "type": "array",
        "items": { "$ref": "#/components/schemas/Goal" }
    })
}

/// R511: Inbox list response shape (array of Inbox).
pub fn inbox_list_schema() -> Value {
    json!({
        "type": "array",
        "items": { "$ref": "#/components/schemas/Inbox" }
    })
}

/// R511: Folder list response shape (array of Folder).
pub fn folder_list_schema() -> Value {
    json!({
        "type": "array",
        "items": { "$ref": "#/components/schemas/Folder" }
    })
}
/// R513: CompanyMember schema (mirrors `pc_repos::company_member::CompanyMemberRow`).
pub fn company_member_schema() -> Value {
    json!({
        "type": "object",
        "description": "A user's membership in a company, with optional user fields denormalised from a LEFT JOIN.",
        "properties": {
            "id": { "type": "string", "format": "uuid" },
            "companyId": { "type": "string", "format": "uuid" },
            "principalId": { "type": "string", "description": "Opaque principal identity (user_id or agent_id)." },
            "membershipRole": { "type": "string", "description": "Role key within the company (e.g. owner, admin, member)." },
            "status": {
                "type": "string",
                "enum": ["active", "archived"],
                "description": "Whether the membership is active or archived."
            },
            "name": { "type": ["string", "null"] },
            "email": { "type": ["string", "null"] },
            "image": { "type": ["string", "null"] },
            "createdAt": { "type": "string", "format": "date-time" },
            "updatedAt": { "type": "string", "format": "date-time" }
        },
        "required": [
            "id", "companyId", "principalId", "membershipRole", "status",
            "createdAt", "updatedAt"
        ]
    })
}

/// R513: Invite schema (mirrors `pc_repos::invite::InviteRow`).
pub fn invite_schema() -> Value {
    json!({
        "type": "object",
        "description": "An invite token that grants access to a company when accepted.",
        "properties": {
            "id": { "type": "string", "format": "uuid" },
            "companyId": { "type": "string", "format": "uuid" },
            "inviteType": { "type": "string", "description": "open (anyone with link) or restricted (named recipients)." },
            "allowedJoinTypes": { "type": "string" },
            "defaultsPayload": { "type": ["object", "null"] },
            "tokenHash": { "type": "string" },
            "expiresAt": { "type": "string", "format": "date-time" },
            "invitedByUserId": { "type": ["string", "null"] },
            "revokedAt": { "type": ["string", "null"], "format": "date-time" },
            "acceptedAt": { "type": ["string", "null"], "format": "date-time" },
            "createdAt": { "type": "string", "format": "date-time" },
            "updatedAt": { "type": "string", "format": "date-time" }
        },
        "required": [
            "id", "companyId", "inviteType", "allowedJoinTypes", "tokenHash",
            "expiresAt", "createdAt", "updatedAt"
        ]
    })
}

/// R513: AdminUser schema - instance-level admin directory entry.
pub fn admin_user_schema() -> Value {
    json!({
        "type": "object",
        "description": "A user as seen by the instance admin directory (isInstanceAdmin flag included).",
        "properties": {
            "id": { "type": "string" },
            "email": { "type": ["string", "null"] },
            "name": { "type": ["string", "null"] },
            "image": { "type": ["string", "null"] },
            "emailVerifiedAt": { "type": ["string", "null"], "format": "date-time" },
            "isInstanceAdmin": { "type": "boolean", "description": "Whether the user holds the instance_admin role." },
            "createdAt": { "type": ["string", "null"], "format": "date-time" }
        },
        "required": ["id", "isInstanceAdmin"]
    })
}

/// R513: CompanyMember list response shape (array of CompanyMember).
pub fn company_member_list_schema() -> Value {
    json!({
        "type": "array",
        "items": { "$ref": "#/components/schemas/CompanyMember" }
    })
}

/// R513: Invite list response shape (array of Invite).
pub fn invite_list_schema() -> Value {
    json!({
        "type": "array",
        "items": { "$ref": "#/components/schemas/Invite" }
    })
}

/// R513: AdminUser list response shape (array of AdminUser).
pub fn admin_user_list_schema() -> Value {
    json!({
        "type": "array",
        "items": { "$ref": "#/components/schemas/AdminUser" }
    })
}

/// Wrap a JSON schema definition as a [`SchemaRef`] ready for registration.
#[must_use]
pub fn into_schema_ref(schema: &Value) -> SchemaRef {
    SchemaRef::object_with(schema.get("properties").unwrap_or(&json!({})), &[])
}

/// All DTO schema names registered by [`register_core_dtos`].
pub const CORE_DTO_NAMES: &[&str] = &[
    "Decision",
    "DecisionOption",
    "DecisionEffect",
    "Company",
    "Issue",
    "Agent",
    "HeartbeatRun",
    // R507: list response shapes for collection GET endpoints.
    "CompanyList",
    "AgentList",
    "IssueList",
    "DecisionList",
    // R507: approvals + pipelines (referenced by path hints).
    "Approval",
    "ApprovalList",
    "PipelineList",
    // R508: domain-rich Pipeline + Routine.
    "Pipeline",
    "Routine",
    "RoutineList",
    // R509: error response shapes.
    "ValidationError",
    "ValidationErrorList",
    "ErrorResponse",
    // R510: pagination.
    "PaginationCursor",
    // R511: 4 domain + 4 list shapes (8 schemas).
    "Case",
    "Goal",
    "Inbox",
    "Folder",
    "CaseList",
    "GoalList",
    "InboxList",
    "FolderList",
    // R513: admin + companies sub-resources.
    "CompanyMember",
    "Invite",
    "AdminUser",
    "CompanyMemberList",
    "InviteList",
    "AdminUserList",
    // R522: Companies aggregation endpoints.
    "CompanyStats",
    "CompanyStatsList",
    "CompanyTimelineResult",
    "CompanyArtifact",
    "CompanyArtifactList",
    "CompanyOrgChart",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r504_register_core_dtos_registers_five() {
        let mut reg = OpenApiRegistry::builder();
        register_core_dtos(&mut reg);
        let spec = reg.build();
        // R507: this test predates the list schemas; assert >= 5.
        assert!(spec.schema_count() >= 5);
    }

    #[test]
    fn r504_register_core_dtos_is_idempotent() {
        // First registration: 5 schemas.
        let mut reg1 = OpenApiRegistry::builder();
        register_core_dtos(&mut reg1);
        let before = reg1.build().schema_count();

        // Second registration on a fresh registry still 5 (no double counting).
        let mut reg2 = OpenApiRegistry::builder();
        register_core_dtos(&mut reg2);
        register_core_dtos(&mut reg2);
        let after = reg2.build().schema_count();

        // Re-registering should not double the count (same names overwrite).
        assert_eq!(before, after);
        assert!(after >= 5);
    }

    #[test]
    fn r504_core_dto_names_constant_matches_registry() {
        let mut reg = OpenApiRegistry::builder();
        register_core_dtos(&mut reg);
        let spec = reg.build();
        let v = spec.to_json_value();
        let schemas = v["components"]["schemas"].as_object().expect("schemas");
        for name in CORE_DTO_NAMES {
            assert!(
                schemas.contains_key(*name),
                "expected `{name}` in components.schemas, got keys: {:?}",
                schemas.keys().collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn r504_decision_schema_has_required_fields() {
        let v = decision_schema();
        let required = v["required"].as_array().expect("required");
        let names: Vec<&str> = required.iter().filter_map(|r| r.as_str()).collect();
        for field in ["id", "companyId", "title", "body", "options", "status"] {
            assert!(
                names.contains(&field),
                "Decision must require `{field}`, got {names:?}"
            );
        }
    }

    #[test]
    fn r504_company_schema_has_status_enum() {
        let v = company_schema();
        let status_enum = v["properties"]["status"]["enum"].as_array().expect("enum");
        let names: Vec<&str> = status_enum.iter().filter_map(|s| s.as_str()).collect();
        for value in ["active", "paused", "archived"] {
            assert!(
                names.contains(&value),
                "Company.status enum must contain `{value}`, got {names:?}"
            );
        }
    }

    #[test]
    fn r504_issue_schema_uses_nullable_pattern() {
        // Option<T> in Rust → type: ["T", "null"] in OpenAPI 3.1
        let v = issue_schema();
        let title = &v["properties"]["title"];
        assert_eq!(title["type"], "string");
        let assignee = &v["properties"]["assigneeAgentId"];
        assert_eq!(
            assignee["type"],
            serde_json::json!(["string", "null"]),
            "Option<Uuid> fields must be `[\"string\", \"null\"]`"
        );
    }

    #[test]
    fn r504_agent_schema_marks_known_status_values() {
        let v = agent_schema();
        let status = &v["properties"]["status"]["enum"].as_array().expect("enum");
        let names: Vec<&str> = status.iter().filter_map(|s| s.as_str()).collect();
        for value in ["active", "paused", "error"] {
            assert!(
                names.contains(&value),
                "Agent.status enum: `{value}` missing"
            );
        }
    }

    #[test]
    fn r504_heartbeat_run_schema_required_started_at() {
        let v = heartbeat_run_schema();
        let required = v["required"].as_array().expect("required");
        let names: Vec<&str> = required.iter().filter_map(|s| s.as_str()).collect();
        assert!(
            names.contains(&"startedAt"),
            "HeartbeatRun must require startedAt"
        );
    }

    #[test]
    fn r504_schemas_serialize_to_openapi_3_1() {
        let mut reg = OpenApiRegistry::builder();
        register_core_dtos(&mut reg);
        let spec = reg.build();
        let s = spec.to_json_string();
        assert!(s.contains("\"openapi\": \"3.1.0\""));
        assert!(s.contains("\"Decision\""));
        assert!(s.contains("\"Company\""));
        assert!(s.contains("\"Issue\""));
        assert!(s.contains("\"Agent\""));
        assert!(s.contains("\"HeartbeatRun\""));
    }

    #[test]
    fn r504_schemas_round_trip_through_yaml() {
        let mut reg = OpenApiRegistry::builder();
        register_core_dtos(&mut reg);
        let spec = reg.build();
        let y = spec.to_yaml_string().expect("yaml");
        assert!(y.contains("Decision:"));
        assert!(y.contains("Company:"));
        assert!(y.contains("Issue:"));
        assert!(y.contains("Agent:"));
        assert!(y.contains("HeartbeatRun:"));
    }

    // -------- r508: Pipeline + Routine domain schemas --------

    #[test]
    fn r508_pipeline_schema_has_required_fields() {
        let v = pipeline_schema();
        let required = v["required"].as_array().expect("required");
        let names: Vec<&str> = required.iter().filter_map(|r| r.as_str()).collect();
        for field in [
            "id",
            "companyId",
            "key",
            "name",
            "enforceTransitions",
            "createdAt",
            "updatedAt",
        ] {
            assert!(
                names.contains(&field),
                "Pipeline.required must contain `{field}`, got {names:?}"
            );
        }
    }

    #[test]
    fn r508_pipeline_schema_nullable_fields_use_array_null_pattern() {
        let v = pipeline_schema();
        // projectId is Option<Uuid>
        assert_eq!(
            v["properties"]["projectId"]["type"],
            serde_json::json!(["string", "null"])
        );
        // archivedAt is Option<Timestamp>
        assert_eq!(
            v["properties"]["archivedAt"]["type"],
            serde_json::json!(["string", "null"])
        );
        assert_eq!(v["properties"]["archivedAt"]["format"], "date-time");
    }

    #[test]
    fn r508_routine_schema_status_enum_has_three_values() {
        let v = routine_schema();
        let status_enum = v["properties"]["status"]["enum"].as_array().expect("enum");
        let names: Vec<&str> = status_enum.iter().filter_map(|s| s.as_str()).collect();
        for v in ["active", "paused", "archived"] {
            assert!(
                names.contains(&v),
                "Routine.status enum missing `{v}`, got {names:?}"
            );
        }
    }

    #[test]
    fn r508_routine_schema_concurrency_policy_enum() {
        let v = routine_schema();
        let enum_vals = v["properties"]["concurrencyPolicy"]["enum"]
            .as_array()
            .expect("enum");
        let names: Vec<&str> = enum_vals.iter().filter_map(|s| s.as_str()).collect();
        for v in ["skip", "queue", "parallel"] {
            assert!(
                names.contains(&v),
                "Routine.concurrencyPolicy enum missing `{v}`"
            );
        }
    }

    #[test]
    fn r508_routine_list_schema_uses_ref() {
        let v = routine_list_schema();
        assert_eq!(v["type"], "array");
        assert_eq!(v["items"]["$ref"], "#/components/schemas/Routine");
    }

    #[test]
    fn r508_schemas_round_trip_through_yaml() {
        let mut reg = OpenApiRegistry::builder();
        register_core_dtos(&mut reg);
        let spec = reg.build();
        let y = spec.to_yaml_string().expect("yaml");
        assert!(y.contains("Pipeline:"));
        assert!(y.contains("Routine:"));
        assert!(y.contains("RoutineList:"));
    }

    // -------- r509: error response schemas --------

    #[test]
    fn r509_validation_error_has_required_fields() {
        let v = validation_error_schema();
        let required = v["required"].as_array().expect("required");
        let names: Vec<&str> = required.iter().filter_map(|r| r.as_str()).collect();
        for field in ["field", "code", "message"] {
            assert!(
                names.contains(&field),
                "ValidationError.required must include `{field}`"
            );
        }
    }

    #[test]
    fn r509_validation_error_list_uses_array_ref() {
        let v = validation_error_list_schema();
        assert_eq!(v["type"], "object");
        assert_eq!(v["properties"]["errors"]["type"], "array");
        assert_eq!(
            v["properties"]["errors"]["items"]["$ref"],
            "#/components/schemas/ValidationError"
        );
    }

    #[test]
    fn r509_error_response_required_code_and_message() {
        let v = error_response_schema();
        let required = v["required"].as_array().expect("required");
        let names: Vec<&str> = required.iter().filter_map(|r| r.as_str()).collect();
        for field in ["code", "message"] {
            assert!(
                names.contains(&field),
                "ErrorResponse.required must include `{field}`"
            );
        }
    }

    #[test]
    fn r509_error_response_trace_id_is_nullable() {
        let v = error_response_schema();
        assert_eq!(
            v["properties"]["traceId"]["type"],
            serde_json::json!(["string", "null"])
        );
    }

    #[test]
    fn r509_schemas_round_trip_through_yaml() {
        let mut reg = OpenApiRegistry::builder();
        register_core_dtos(&mut reg);
        let spec = reg.build();
        let y = spec.to_yaml_string().expect("yaml");
        assert!(y.contains("ValidationError:"));
        assert!(y.contains("ValidationErrorList:"));
        assert!(y.contains("ErrorResponse:"));
    }

    // -------- r510: pagination cursor + list response envelope --------

    #[test]
    fn r510_pagination_cursor_required_has_more() {
        let v = pagination_cursor_schema();
        let required = v["required"].as_array().expect("required");
        let names: Vec<&str> = required.iter().filter_map(|r| r.as_str()).collect();
        assert_eq!(names, vec!["hasMore"]);
    }

    #[test]
    fn r510_pagination_cursor_next_cursor_is_nullable() {
        let v = pagination_cursor_schema();
        assert_eq!(
            v["properties"]["nextCursor"]["type"],
            serde_json::json!(["string", "null"])
        );
    }

    #[test]
    fn r510_list_response_envelope_uses_correct_ref() {
        let v = list_response_envelope_schema("Issue");
        assert_eq!(
            v["properties"]["items"]["items"]["$ref"],
            "#/components/schemas/Issue"
        );
        assert_eq!(
            v["properties"]["pagination"]["$ref"],
            "#/components/schemas/PaginationCursor"
        );
    }

    #[test]
    fn r510_list_response_envelope_required_items_and_pagination() {
        let v = list_response_envelope_schema("Company");
        let required = v["required"].as_array().expect("required");
        let names: Vec<&str> = required.iter().filter_map(|r| r.as_str()).collect();
        for f in ["items", "pagination"] {
            assert!(
                names.contains(&f),
                "ListResponseEnvelope must require `{f}`"
            );
        }
    }

    #[test]
    fn r510_schemas_round_trip_through_yaml() {
        let mut reg = OpenApiRegistry::builder();
        register_core_dtos(&mut reg);
        let spec = reg.build();
        let y = spec.to_yaml_string().expect("yaml");
        assert!(y.contains("PaginationCursor:"));
    }

    #[test]
    fn r510_register_core_dtos_registers_nineteen() {
        let mut reg = OpenApiRegistry::builder();
        register_core_dtos(&mut reg);
        let spec = reg.build();
        // R511: 8 new schemas added (Case/Goal/Inbox/Folder + 4 List arrays).
        // R513: 6 new schemas (CompanyMember + Invite + AdminUser + 3 List).
        assert_eq!(spec.schema_count(), 41);
    }

    #[test]
    fn r509_register_core_dtos_registers_eighteen() {
        let mut reg = OpenApiRegistry::builder();
        register_core_dtos(&mut reg);
        let spec = reg.build();
        // R513: 6 new schemas (CompanyMember + Invite + AdminUser + 3 List).
        assert_eq!(spec.schema_count(), 41);
    }

    #[test]
    fn r508_register_core_dtos_registers_fifteen() {
        let mut reg = OpenApiRegistry::builder();
        register_core_dtos(&mut reg);
        let spec = reg.build();
        // R513: 6 new schemas (CompanyMember + Invite + AdminUser + 3 List).
        assert_eq!(spec.schema_count(), 41);
    }

    #[test]
    fn r507_list_schemas_use_array_items_ref() {
        for (schema_fn, expected_items_ref) in [
            (company_list_schema(), "#/components/schemas/Company"),
            (agent_list_schema(), "#/components/schemas/Agent"),
            (issue_list_schema(), "#/components/schemas/Issue"),
            (decision_list_schema(), "#/components/schemas/Decision"),
        ] {
            let v = schema_fn;
            assert_eq!(v["type"], "array", "list schema must have type=array");
            assert_eq!(
                v["items"]["$ref"], expected_items_ref,
                "list items must $ref the correct single-DTO schema"
            );
        }
    }

    #[test]
    fn r507_register_core_dtos_registers_nine() {
        let mut reg = OpenApiRegistry::builder();
        register_core_dtos(&mut reg);
        let spec = reg.build();
        // R513: 6 new schemas (CompanyMember + Invite + AdminUser + 3 List).
        assert_eq!(spec.schema_count(), 41);
    }

    #[test]
    fn r507_core_dto_names_constant_has_nine_entries() {
        assert_eq!(CORE_DTO_NAMES.len(), 41);
        for name in ["CompanyList", "AgentList", "IssueList", "DecisionList"] {
            assert!(CORE_DTO_NAMES.contains(&name), "missing `{name}`");
        }
    }

    #[test]
    fn r507_list_schemas_round_trip_through_yaml() {
        let mut reg = OpenApiRegistry::builder();
        register_core_dtos(&mut reg);
        let spec = reg.build();
        let y = spec.to_yaml_string().expect("yaml");
        for name in ["CompanyList", "AgentList", "IssueList", "DecisionList"] {
            assert!(y.contains(&format!("{name}:")), "YAML missing {name}");
        }
    }

    #[test]
    fn r504_into_schema_ref_does_not_panic_on_empty_properties() {
        // Pure defensive check: even with no `properties`, the helper should
        // not panic.
        let v = json!({"type": "object"});
        let _ = into_schema_ref(&v);
    }

    // -------- r511: Case / Goal / Inbox / Folder + 4 list shapes --------

    #[test]
    fn r511_case_schema_required_core_fields() {
        let v = case_schema();
        let required = v["required"].as_array().expect("required");
        let names: Vec<&str> = required.iter().filter_map(|r| r.as_str()).collect();
        for field in [
            "id",
            "companyId",
            "caseNumber",
            "identifier",
            "caseType",
            "title",
            "status",
        ] {
            assert!(
                names.contains(&field),
                "Case.required must include `{field}`, got {names:?}"
            );
        }
    }

    #[test]
    fn r511_case_schema_status_enum_has_six_values() {
        let v = case_schema();
        let en = v["properties"]["status"]["enum"].as_array().expect("enum");
        let values: Vec<&str> = en.iter().filter_map(|e| e.as_str()).collect();
        for s in [
            "draft",
            "in_progress",
            "in_review",
            "approved",
            "done",
            "cancelled",
        ] {
            assert!(
                values.contains(&s),
                "Case.status enum missing `{s}`, got {values:?}"
            );
        }
    }

    #[test]
    fn r511_goal_schema_required_core_fields() {
        let v = goal_schema();
        let required = v["required"].as_array().expect("required");
        let names: Vec<&str> = required.iter().filter_map(|r| r.as_str()).collect();
        for field in ["id", "companyId", "title", "level", "status"] {
            assert!(
                names.contains(&field),
                "Goal.required must include `{field}`, got {names:?}"
            );
        }
    }

    #[test]
    fn r511_goal_schema_level_enum_has_five_values() {
        let v = goal_schema();
        let en = v["properties"]["level"]["enum"].as_array().expect("enum");
        let values: Vec<&str> = en.iter().filter_map(|e| e.as_str()).collect();
        for s in ["mission", "company", "team", "project", "task"] {
            assert!(
                values.contains(&s),
                "Goal.level enum missing `{s}`, got {values:?}"
            );
        }
    }

    #[test]
    fn r511_inbox_schema_required_core_fields() {
        let v = inbox_schema();
        let required = v["required"].as_array().expect("required");
        let names: Vec<&str> = required.iter().filter_map(|r| r.as_str()).collect();
        for field in [
            "id",
            "companyId",
            "userId",
            "itemKey",
            "kind",
            "dismissedAt",
        ] {
            assert!(
                names.contains(&field),
                "Inbox.required must include `{field}`, got {names:?}"
            );
        }
    }

    #[test]
    fn r511_inbox_schema_kind_enum_is_dismiss_or_snooze() {
        let v = inbox_schema();
        let en = v["properties"]["kind"]["enum"].as_array().expect("enum");
        let values: Vec<&str> = en.iter().filter_map(|e| e.as_str()).collect();
        assert_eq!(
            values,
            vec!["dismiss", "snooze"],
            "Inbox.kind enum must be exactly [dismiss, snooze]"
        );
    }

    #[test]
    fn r511_folder_schema_required_core_fields() {
        let v = folder_schema();
        let required = v["required"].as_array().expect("required");
        let names: Vec<&str> = required.iter().filter_map(|r| r.as_str()).collect();
        for field in ["id", "companyId", "kind", "name", "slug", "position"] {
            assert!(
                names.contains(&field),
                "Folder.required must include `{field}`, got {names:?}"
            );
        }
    }

    #[test]
    fn r511_folder_schema_kind_enum_is_routine_or_skill() {
        let v = folder_schema();
        let en = v["properties"]["kind"]["enum"].as_array().expect("enum");
        let values: Vec<&str> = en.iter().filter_map(|e| e.as_str()).collect();
        assert_eq!(
            values,
            vec!["routine", "skill"],
            "Folder.kind enum must be exactly [routine, skill]"
        );
    }

    #[test]
    fn r511_list_schemas_reference_correct_single_schemas() {
        for (list_name, list_value, expected_ref) in [
            ("CaseList", case_list_schema(), "Case"),
            ("GoalList", goal_list_schema(), "Goal"),
            ("InboxList", inbox_list_schema(), "Inbox"),
            ("FolderList", folder_list_schema(), "Folder"),
        ] {
            assert_eq!(
                list_value["type"], "array",
                "{list_name} must have type=array"
            );
            assert_eq!(
                list_value["items"]["$ref"],
                format!("#/components/schemas/{expected_ref}"),
                "{list_name}.items.$ref must point to {expected_ref}"
            );
        }
    }

    #[test]
    fn r511_register_core_dtos_registers_twenty_seven() {
        let mut reg = OpenApiRegistry::builder();
        register_core_dtos(&mut reg);
        let spec = reg.build();
        // R513: 6 new schemas (CompanyMember + Invite + AdminUser + 3 List).
        assert_eq!(spec.schema_count(), 41);
    }

    #[test]
    fn r511_core_dto_names_constant_has_twenty_seven_entries() {
        assert_eq!(CORE_DTO_NAMES.len(), 41);
        for name in [
            "Case",
            "Goal",
            "Inbox",
            "Folder",
            "CaseList",
            "GoalList",
            "InboxList",
            "FolderList",
        ] {
            assert!(
                CORE_DTO_NAMES.contains(&name),
                "missing `{name}` in CORE_DTO_NAMES"
            );
        }
    }

    // -------- r513: admin + companies sub-resources --------

    #[test]
    fn r513_company_member_schema_required_core_fields() {
        let v = company_member_schema();
        let required = v["required"].as_array().expect("required");
        let names: Vec<&str> = required.iter().filter_map(|r| r.as_str()).collect();
        for field in ["id", "companyId", "principalId", "membershipRole", "status"] {
            assert!(
                names.contains(&field),
                "CompanyMember.required must include `{field}`, got {names:?}"
            );
        }
    }

    #[test]
    fn r513_company_member_schema_status_enum_is_active_or_archived() {
        let v = company_member_schema();
        let en = v["properties"]["status"]["enum"].as_array().expect("enum");
        let values: Vec<&str> = en.iter().filter_map(|e| e.as_str()).collect();
        assert_eq!(
            values,
            vec!["active", "archived"],
            "CompanyMember.status enum must be exactly [active, archived]"
        );
    }

    #[test]
    fn r513_invite_schema_required_core_fields() {
        let v = invite_schema();
        let required = v["required"].as_array().expect("required");
        let names: Vec<&str> = required.iter().filter_map(|r| r.as_str()).collect();
        for field in [
            "id",
            "companyId",
            "inviteType",
            "allowedJoinTypes",
            "tokenHash",
            "expiresAt",
        ] {
            assert!(
                names.contains(&field),
                "Invite.required must include `{field}`, got {names:?}"
            );
        }
    }

    #[test]
    fn r513_invite_schema_nullable_fields_use_string_or_null() {
        let v = invite_schema();
        for field in ["invitedByUserId", "revokedAt", "acceptedAt"] {
            assert_eq!(
                v["properties"][field]["type"],
                serde_json::json!(["string", "null"]),
                "Invite.{field} must be nullable"
            );
        }
    }

    #[test]
    fn r513_admin_user_schema_required_minimum() {
        let v = admin_user_schema();
        let required = v["required"].as_array().expect("required");
        let names: Vec<&str> = required.iter().filter_map(|r| r.as_str()).collect();
        assert_eq!(
            names,
            vec!["id", "isInstanceAdmin"],
            "AdminUser.required must be exactly [id, isInstanceAdmin]"
        );
    }

    #[test]
    fn r513_list_schemas_reference_correct_single_schemas() {
        for (list_name, list_value, expected_ref) in [
            (
                "CompanyMemberList",
                company_member_list_schema(),
                "CompanyMember",
            ),
            ("InviteList", invite_list_schema(), "Invite"),
            ("AdminUserList", admin_user_list_schema(), "AdminUser"),
        ] {
            assert_eq!(
                list_value["type"], "array",
                "{list_name} must have type=array"
            );
            assert_eq!(
                list_value["items"]["$ref"],
                format!("#/components/schemas/{expected_ref}"),
                "{list_name}.items.$ref must point to {expected_ref}"
            );
        }
    }

    #[test]
    fn r513_register_core_dtos_registers_thirty_three() {
        let mut reg = OpenApiRegistry::builder();
        register_core_dtos(&mut reg);
        let spec = reg.build();
        assert_eq!(spec.schema_count(), 41);
    }

    #[test]
    fn r513_core_dto_names_constant_has_thirty_three_entries() {
        assert_eq!(CORE_DTO_NAMES.len(), 41);
        for name in [
            "CompanyMember",
            "Invite",
            "AdminUser",
            "CompanyMemberList",
            "InviteList",
            "AdminUserList",
        ] {
            assert!(
                CORE_DTO_NAMES.contains(&name),
                "missing `{name}` in CORE_DTO_NAMES"
            );
        }
    }

    #[test]
    fn r513_new_schemas_round_trip_through_yaml() {
        let mut reg = OpenApiRegistry::builder();
        register_core_dtos(&mut reg);
        let spec = reg.build();
        let y = spec.to_yaml_string().expect("yaml");
        for name in ["CompanyMember:", "Invite:", "AdminUser:"] {
            assert!(y.contains(name), "YAML missing top-level {name} key");
        }
    }
}
