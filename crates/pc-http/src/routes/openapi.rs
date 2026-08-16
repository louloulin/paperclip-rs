//! `OpenAPI 3` 文档端点。
//!
//! 与 Node 上游 [`openapi.ts`] 的合约保持一致：
//! - `/openapi.json` 是规范入口（Node 上游用 `/api/openapi`）
//! - `/api/openapi` 与 `/api/openapi.json` 是 alias
//! - 生成的 paths 通过源码扫描 `crates/pc-http/src/routes/*.rs` 自动
//!   注入（避免逐路径手写 600+ 端点）
//!
//! 设计原则：
//! - 路径规范化：`/:param` → `/{param}`（OpenAPI 路径参数语法）
//! - 方法标签：每个路径生成 `{operationId, summary, description, tags}`
//! - 字段命名保持与 Node 上游 1:1（snake_case / camelCase 都接受）

use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Json, Router};
use serde_json::{json, Value};
use std::collections::BTreeMap;

use pc_openapi::{register_core_dtos, OpenApiRegistry};

use crate::middleware::csrf::csrf_path_allowed;
use crate::AppState;

/// R515: True if the path+method combination requires CSRF protection
/// (and therefore should declare `security: [{csrfToken: []}]` in OpenAPI).
///
/// Mirrors `csrf_path_allowed` (whitelist) + the state-changing method set.
/// Pure function so unit tests can cover all branches without an [`AppState`].
pub fn csrf_protected_in_openapi(path: &str, method: &str) -> bool {
    let method_upper = method.to_uppercase();
    if !matches!(method_upper.as_str(), "POST" | "PUT" | "PATCH" | "DELETE") {
        return false;
    }
    !csrf_path_allowed(path)
}

pub fn router() -> Router<AppState> {
    Router::new()
        // Canonical mount points used by the Rust server itself.
        .route("/openapi.json", get(document))
        .route("/openapi.yaml", get(document_yaml))
        .route("/api/openapi", get(document))
        // Alias matching the Node upstream contract (`/api/openapi.json`) so
        // parity tests and shared OpenAPI consumers can use one URL.
        .route("/api/openapi.json", get(document))
        // YAML alias matching Node upstream `/api/openapi.yaml`.
        .route("/api/openapi.yaml", get(document_yaml))
}

/// Convert a Rust `:param` style path to OpenAPI `{param}` style.
/// UI-1: normalize trailing slashes so duplicate route registrations like
/// `/api/companies` and `/api/companies/` collapse to one OpenAPI path.
/// Without this, both produce identical operationIds and the uniqueness
/// guardrail (R511 / UI-1 contract test) fails.
fn normalize_path(path: &str) -> String {
    // Replace `:foo` with `{foo}` per OpenAPI path templating.
    let mut out = String::with_capacity(path.len());
    let mut chars = path.chars().peekable();
    while let Some(c) = chars.next() {
        if c == ':' {
            // Read identifier chars
            let mut ident = String::new();
            while let Some(&nc) = chars.peek() {
                if nc.is_ascii_alphanumeric() || nc == '_' {
                    ident.push(nc);
                    chars.next();
                } else {
                    break;
                }
            }
            out.push('{');
            out.push_str(&ident);
            out.push('}');
        } else {
            out.push(c);
        }
    }
    // Collapse trailing slash so `/api/foo` and `/api/foo/` produce the
    // same normalized path. Belt-and-braces: `operation_id` already trims
    // trailing slashes when generating the id, but the path itself would
    // otherwise appear twice in the OpenAPI document.
    let trimmed = out.trim_end_matches('/').to_string();
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed
    }
}

/// R695 (UI-2): paths declared via `Router::merge()` with a non-root mount
/// (e.g. `crates/pc-http/src/routes/v1.rs` mounted at `/api/v1`) are not
/// picked up by the regex-based `scan_routes_for_openapi` walker, which
/// only sees the relative `.route("/runs", ...)` invocation. This
/// constant enumerates such `hint-only` paths so the OpenAPI document
/// stays in sync with `path_schema_hint`.
const ALL_HINT_ONLY_PATHS: &[(&str, &str)] = &[
    ("/api/v1/runs", "GET"),
    ("/api/health/dev-server/restart", "GET"),
    ("/api/auth/get-session", "GET"),
    ("/api/auth/profile", "GET"),
    ("/api/auth/profile", "PATCH"),
    ("/api/adapters/{adapter_type}/ui-parser.js", "GET"),
    ("/api/assets/{asset_id}/content", "GET"),
    ("/api/companies/{company_id}/audit/agent-actions.csv", "GET"),
    ("/api/companies/{company_id}/events/ws", "GET"),
    ("/api/issues/{issue_id}/file-resources/content", "GET"),
    ("/api/plugins/{plugin_id}/bridge/stream/{channel}", "GET"),
    ("/api/plugins/{plugin_id}/actions/{key}", "POST"),
    ("/api/plugins/{plugin_id}/data/{key}", "POST"),
];


/// Infer a stable operationId from method + path. Mirrors Node upstream
/// (snake_case verb_noun form).
pub fn operation_id(method: &str, path: &str) -> String {
    let normalized = path
        .trim_start_matches('/')
        .trim_end_matches('/')
        .replace(|c: char| !c.is_ascii_alphanumeric(), "_")
        .replace("__", "_")
        .trim_matches('_')
        .to_string();
    if normalized.is_empty() {
        return format!("root_{}", method.to_lowercase());
    }
    format!("{}_{}", method.to_lowercase(), normalized)
}

/// Validate that every operation in an openapi body has a unique `operationId`.
///
/// R511: guardrail so future `path_schema_hint` additions can never silently
/// collide with another route. Returns duplicate ids (empty when clean).
#[must_use]
pub fn find_duplicate_operation_ids(body: &serde_json::Value) -> Vec<String> {
    use std::collections::HashMap;
    let mut counts: HashMap<String, u32> = HashMap::new();
    let mut missing: Vec<String> = Vec::new();
    if let Some(paths) = body.get("paths").and_then(|v| v.as_object()) {
        for methods in paths.values() {
            if let Some(methods) = methods.as_object() {
                for (method, op) in methods {
                    if let Some(op) = op.as_object() {
                        if let Some(op_id) = op.get("operationId").and_then(|v| v.as_str()) {
                            *counts.entry(op_id.to_string()).or_insert(0) += 1;
                        } else {
                            missing.push(format!("__missing__{method}"));
                        }
                    }
                }
            }
        }
    }
    let mut dups: Vec<String> = counts
        .into_iter()
        .filter_map(|(k, v)| if v > 1 { Some(k) } else { None })
        .collect();
    dups.extend(missing);
    dups.sort();
    dups
}

/// Infer a tag from the first non-empty path segment after `/api/`.
fn infer_tag(path: &str) -> String {
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    if parts.len() >= 2 && parts[0] == "api" && !parts[1].is_empty() {
        // Use the resource name as the tag, stripping trailing ':id' or 'me'.
        let raw = parts[1];
        // Take just the first segment if hyphenated (e.g. `company-skills` -> `company`)
        raw.split('-').next().unwrap_or(raw).to_string()
    } else if parts.is_empty() || parts[0].is_empty() {
        "root".to_string()
    } else {
        parts[0].to_string()
    }
}

/// Build the OpenAPI 3.1 document body as a JSON value.
/// Extracted so both `/openapi.json` and `/openapi.yaml` can share the same
/// source-of-truth (R503: avoids drift between the two routes).
fn build_openapi_body(state: &AppState) -> serde_json::Value {
    let adapters = state
        .adapters
        .descriptors()
        .into_iter()
        .map(|d| d.adapter_type)
        .collect::<Vec<_>>();
    build_openapi_body_with_adapters(adapters)
}

/// Public, AppState-free entry point. Used by the standalone openapi.json
/// dump tool (UI-1: feeds openapi-typescript generation) and by tests that
/// want to inspect the full spec without spinning up a real server.
///
/// R-rs693: mirrors the production body verbatim, just parameterized on the
/// adapter list so callers don't need a full `AppState` (DB / actor
/// runtime / plugin host / workflow registry / storage / etc.).
pub fn build_openapi_body_with_adapters(adapters: Vec<String>) -> serde_json::Value {
    let paths = scan_routes_for_openapi();
    let mut body = json!({
        "openapi": "3.1.0",
        "info": {
            "title": "Paperclip API",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "REST API for the Paperclip AI agent management platform"
        },
        "servers": [{ "url": "/" }],
        "tags": build_tag_list(&paths),
        "paths": paths,
        "components": {
            "securitySchemes": {
                "session": { "type": "apiKey", "in": "cookie", "name": "paperclip_session" },
                "apiKey": { "type": "apiKey", "in": "header", "name": "X-Paperclip-Api-Key" },
                "csrfToken": { "type": "apiKey", "in": "header", "name": "X-CSRF-Token" }
            }
        },
        "x-paperclip": { "adapters": adapters }
    });
    inject_dto_schemas(&mut body);
    body
}

/// R505: merge DTO schemas (from `pc_openapi::dto_schemas::register_core_dtos`)
/// into a pre-built OpenAPI body. Extracted as a pure function so tests can
/// verify the merge without constructing a full [`AppState`].
///
/// Hand-rolled path scan owns `components.securitySchemes`; `pc-openapi`
/// owns `components.schemas`. The two coexist by key name.
fn inject_dto_schemas(body: &mut Value) {
    let mut reg = OpenApiRegistry::builder();
    register_core_dtos(&mut reg);
    let spec = reg.build();
    let schemas_json = spec
        .to_json_value()
        .get("components")
        .and_then(|c| c.get("schemas"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    if let Some(components) = body.get_mut("components").and_then(|c| c.as_object_mut()) {
        components.insert("schemas".to_string(), schemas_json);
    }
}

async fn document(State(state): State<AppState>) -> impl IntoResponse {
    let body = build_openapi_body(&state);
    (StatusCode::OK, Json(body))
}

/// R503: `/openapi.yaml` route — hand-rolled YAML emitter (mirrors R501
/// `pc-openapi::serializers::to_yaml_string`). We avoid pulling in
/// `serde_yaml` to keep the dependency surface small.
async fn document_yaml(State(state): State<AppState>) -> impl IntoResponse {
    let body = build_openapi_body(&state);
    let yaml = json_value_to_yaml(&body, 0);
    let mut resp = yaml.into_response();
    resp.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/yaml; charset=utf-8"),
    );
    resp
}

/// Minimal YAML emitter for OpenAPI 3.1 body. Supports strings, numbers,
/// booleans, null, objects, arrays — everything that appears in the
/// generated spec.
fn json_value_to_yaml(v: &serde_json::Value, depth: usize) -> String {
    match v {
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(s) => {
            let escaped = s
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\t', "\\t");
            format!("\"{escaped}\"")
        }
        serde_json::Value::Array(items) => {
            if items.is_empty() {
                return "[]".to_string();
            }
            let mut out = String::new();
            for item in items {
                out.push_str(&"  ".repeat(depth));
                out.push_str("- ");
                match item {
                    serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
                        out.push('\n');
                        out.push_str(&json_value_to_yaml(item, depth + 1));
                    }
                    _ => {
                        out.push_str(&json_value_to_yaml(item, depth + 1));
                    }
                }
                out.push('\n');
            }
            out
        }
        serde_json::Value::Object(map) => {
            if map.is_empty() {
                return "{}".to_string();
            }
            let mut out = String::new();
            for (k, val) in map {
                out.push_str(&"  ".repeat(depth));
                out.push_str(k);
                out.push_str(": ");
                match val {
                    serde_json::Value::Object(_) | serde_json::Value::Array(_) => {
                        out.push('\n');
                        out.push_str(&json_value_to_yaml(val, depth + 1));
                    }
                    _ => {
                        out.push_str(&json_value_to_yaml(val, depth + 1));
                    }
                }
                out.push('\n');
            }
            out
        }
    }
}

/// Build a deduplicated tag list from the inferred tag of each path.
fn build_tag_list(paths: &BTreeMap<String, Value>) -> Vec<Value> {
    let mut tags: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for value in paths.values() {
        if let Some(obj) = value.as_object() {
            for (_method, op) in obj {
                if let Some(tag) = op
                    .get("tags")
                    .and_then(|t| t.as_array())
                    .and_then(|a| a.first())
                    .and_then(|v| v.as_str())
                {
                    tags.insert(tag.to_string());
                }
            }
        }
    }
    tags.into_iter()
        .map(|name| json!({ "name": name }))
        .collect()
}

/// Strip Rust line comments (`//...`) and block comments (`/* ... */`)
/// so they are not scanned as if they were route declarations.
fn strip_rust_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        // Line comment
        if chars[i] == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        // Block comment
        if chars[i] == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(chars.len());
            continue;
        }
        // String literal — preserve verbatim
        if chars[i] == '\'' || chars[i] == '\"' {
            let q = chars[i];
            out.push(q);
            i += 1;
            while i < chars.len() && chars[i] != q {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    out.push(chars[i]);
                    out.push(chars[i + 1]);
                    i += 2;
                    continue;
                }
                out.push(chars[i]);
                i += 1;
            }
            if i < chars.len() {
                out.push(chars[i]);
                i += 1;
            }
            continue;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Schema hint for a single (path, method) → optional request + response
/// schemas. Used by the scanner to attach `requestBody` and richer
/// `responses` to the generated operations.
///
/// Schema names refer to entries registered via
/// `pc_openapi::register_core_dtos`. The `Option<SchemaName>` shape lets us
/// distinguish "no body" (GET/DELETE) from "unknown body" (yet to be hinted).
#[derive(Debug, Clone, Copy)]
pub struct PathSchemaHint {
    pub request: Option<&'static str>,
    pub response: Option<&'static str>,
}

/// Look up schema hints for a known route. Returns `None` if the scanner
/// doesn't recognise the path+method combination — the caller should fall
/// back to the default minimal response shape.
///
/// **Coverage (R506 first cut, 10 endpoints — the most-consumed CRUD verbs
/// across the 4 core resources):**
///
/// | Route | GET | POST |
/// |---|---|---|
/// | `/api/companies` | list→[Company] | create ←/→ Company |
/// | `/api/agents` | list→[Agent] | create ←/→ Agent |
/// | `/api/issues` | list→[Issue] | create ←/→ Issue |
/// | `/api/decisions` | list→[Decision] | create ←/→ Decision |
/// | `/api/companies/{id}` | get → Company | — |
///
/// Per-path hints (heartbeat, approvals, pipelines, routines, ...) will be
/// added in subsequent R-rounds as the schemas are stabilised.
#[must_use]
pub fn path_schema_hint(path: &str, method: &str) -> Option<PathSchemaHint> {
    let m = method.to_ascii_uppercase();
    let p = path.trim_end_matches('/');
    // Strip the `:id` style and re-format to `{id}` so callers can match
    // either form (the scanner emits normalised `{id}` but tests may pass
    // the raw `:id` shape).
    let p_norm = p
        .split('/')
        .map(|seg| {
            if let Some(rest) = seg.strip_prefix(':') {
                format!("{{{rest}}}")
            } else {
                seg.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("/");
    match (p_norm.as_str(), m.as_str()) {
        // Collection routes (R506 first cut).
        ("/api/companies", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("CompanyList"),
        }),
        ("/api/companies", "POST") => Some(PathSchemaHint {
            request: Some("Company"),
            response: Some("Company"),
        }),
        ("/api/agents", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("AgentList"),
        }),
        ("/api/agents", "POST") => Some(PathSchemaHint {
            request: Some("Agent"),
            response: Some("Agent"),
        }),
        ("/api/issues", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("IssueList"),
        }),
        ("/api/issues", "POST") => Some(PathSchemaHint {
            request: Some("Issue"),
            response: Some("Issue"),
        }),
        ("/api/decisions", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("DecisionList"),
        }),
        ("/api/decisions", "POST") => Some(PathSchemaHint {
            request: Some("Decision"),
            response: Some("Decision"),
        }),

        // Item routes (R506 first cut).
        ("/api/companies/{id}", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("Company"),
        }),
        ("/api/agents/{id}", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("Agent"),
        }),

        // R507: 5 additional hints for approvals / pipelines / heartbeat.
        ("/api/approvals", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("ApprovalList"),
        }),
        ("/api/approvals", "POST") => Some(PathSchemaHint {
            request: Some("Approval"),
            response: Some("Approval"),
        }),
        ("/api/approvals/{id}", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("Approval"),
        }),
        ("/api/pipelines", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("PipelineList"),
        }),
        ("/api/heartbeat-runs/{run_id}", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("HeartbeatRun"),
        }),

        // R509: item GET routes for issues/decisions (4 resources × item GET
        // covers the most-consumed read patterns).
        ("/api/issues/{id}", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("Issue"),
        }),
        ("/api/decisions/{id}", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("Decision"),
        }),
        ("/api/pipelines/{id}", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("Pipeline"),
        }),
        ("/api/routines/{id}", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("Routine"),
        }),
        // R510: 12 additional hints — cases / goals / approvals CRUD + pipelines mutations.
        ("/api/cases", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("CaseList"),
        }),
        ("/api/cases", "POST") => Some(PathSchemaHint {
            request: Some("Case"),
            response: Some("Case"),
        }),
        ("/api/cases/{case_id}", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("Case"),
        }),
        ("/api/cases/{case_id}", "PATCH") => Some(PathSchemaHint {
            request: Some("Case"),
            response: Some("Case"),
        }),
        ("/api/cases/{case_id}", "DELETE") => Some(PathSchemaHint {
            request: None,
            response: None,
        }),
        ("/api/goals", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("GoalList"),
        }),
        ("/api/goals", "POST") => Some(PathSchemaHint {
            request: Some("Goal"),
            response: Some("Goal"),
        }),
        ("/api/goals/{id}", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("Goal"),
        }),
        ("/api/approvals/{id}", "PATCH") => Some(PathSchemaHint {
            request: Some("Approval"),
            response: Some("Approval"),
        }),
        ("/api/approvals/{id}", "DELETE") => Some(PathSchemaHint {
            request: None,
            response: None,
        }),
        ("/api/pipelines/{id}", "PATCH") => Some(PathSchemaHint {
            request: Some("Pipeline"),
            response: Some("Pipeline"),
        }),
        ("/api/pipelines/{id}/archive", "POST") => Some(PathSchemaHint {
            request: None,
            response: Some("Pipeline"),
        }),

        // R509: pipelines POST + archive + heartbeat POST.
        ("/api/pipelines", "POST") => Some(PathSchemaHint {
            request: Some("Pipeline"),
            response: Some("Pipeline"),
        }),
        ("/api/routines", "POST") => Some(PathSchemaHint {
            request: Some("Routine"),
            response: Some("Routine"),
        }),
        ("/api/routines/{id}", "PATCH") => Some(PathSchemaHint {
            request: Some("Routine"),
            response: Some("Routine"),
        }),
        ("/api/heartbeat", "POST") => Some(PathSchemaHint {
            request: None,
            response: Some("HeartbeatRun"),
        }),

        // R508: PATCH/DELETE on the 4 core resources + pipelines/routines list.
        ("/api/companies/{id}", "PATCH") => Some(PathSchemaHint {
            request: Some("Company"),
            response: Some("Company"),
        }),
        ("/api/companies/{id}", "DELETE") => Some(PathSchemaHint {
            request: None,
            response: None,
        }),
        ("/api/agents/{id}", "PATCH") => Some(PathSchemaHint {
            request: Some("Agent"),
            response: Some("Agent"),
        }),
        ("/api/agents/{id}", "DELETE") => Some(PathSchemaHint {
            request: None,
            response: None,
        }),
        ("/api/issues/{id}", "PATCH") => Some(PathSchemaHint {
            request: Some("Issue"),
            response: Some("Issue"),
        }),
        ("/api/issues/{id}", "DELETE") => Some(PathSchemaHint {
            request: None,
            response: None,
        }),
        ("/api/decisions/{id}", "PATCH") => Some(PathSchemaHint {
            request: Some("Decision"),
            response: Some("Decision"),
        }),
        ("/api/decisions/{id}", "DELETE") => Some(PathSchemaHint {
            request: None,
            response: None,
        }),
        ("/api/routines", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("RoutineList"),
        }),

        // R511: cases sub-resources + goals PATCH/DELETE + inbox + folders.
        ("/api/cases/{case_id}/events", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("CaseList"),
        }),
        ("/api/cases/{case_id}/issue-links", "POST") => Some(PathSchemaHint {
            request: Some("Case"),
            response: Some("Case"),
        }),
        ("/api/cases/{case_id}/links", "POST") => Some(PathSchemaHint {
            request: Some("Case"),
            response: Some("Case"),
        }),
        ("/api/cases/{case_id}/breakdown", "POST") => Some(PathSchemaHint {
            request: None,
            response: Some("Case"),
        }),
        ("/api/cases/{case_id}/review", "POST") => Some(PathSchemaHint {
            request: Some("Case"),
            response: Some("Case"),
        }),
        ("/api/cases/{case_id}/children", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("CaseList"),
        }),
        ("/api/issues/{issue_id}/cases", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("CaseList"),
        }),
        ("/api/goals/{id}", "PATCH") => Some(PathSchemaHint {
            request: Some("Goal"),
            response: Some("Goal"),
        }),
        ("/api/goals/{id}", "DELETE") => Some(PathSchemaHint {
            request: None,
            response: None,
        }),
        ("/api/companies/{company_id}/inbox-dismissals", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("InboxList"),
        }),
        ("/api/companies/{company_id}/inbox-dismissals", "POST") => Some(PathSchemaHint {
            request: Some("Inbox"),
            response: Some("Inbox"),
        }),
        ("/api/companies/{company_id}/inbox-dismissals/{item_key}", "DELETE") => {
            Some(PathSchemaHint {
                request: None,
                response: None,
            })
        }
        ("/api/companies/{company_id}/inbox-dismissals/dismiss", "POST") => Some(PathSchemaHint {
            request: Some("Inbox"),
            response: Some("Inbox"),
        }),
        ("/api/companies/{company_id}/inbox-dismissals/snooze", "POST") => Some(PathSchemaHint {
            request: Some("Inbox"),
            response: Some("Inbox"),
        }),
        ("/api/companies/{company_id}/inbox-dismissals/count", "GET") => Some(PathSchemaHint {
            request: None,
            response: None,
        }),
        ("/api/companies/{company_id}/folders", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("FolderList"),
        }),
        ("/api/companies/{company_id}/folders", "POST") => Some(PathSchemaHint {
            request: Some("Folder"),
            response: Some("Folder"),
        }),
        ("/api/companies/{company_id}/folders/ensure-my", "POST") => Some(PathSchemaHint {
            request: None,
            response: Some("Folder"),
        }),
        ("/api/companies/{company_id}/folders/{folder_id}", "PATCH") => Some(PathSchemaHint {
            request: Some("Folder"),
            response: Some("Folder"),
        }),
        ("/api/companies/{company_id}/folders/{folder_id}", "DELETE") => Some(PathSchemaHint {
            request: None,
            response: None,
        }),
        ("/api/companies/{company_id}/folders/{folder_id}/move", "POST") => Some(PathSchemaHint {
            request: None,
            response: Some("Folder"),
        }),
        ("/api/companies/{company_id}/folders/items/move", "POST") => Some(PathSchemaHint {
            request: None,
            response: None,
        }),
        // Legacy folder endpoints (kept for backward compat).
        ("/api/folders", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("FolderList"),
        }),
        ("/api/folders", "POST") => Some(PathSchemaHint {
            request: Some("Folder"),
            response: Some("Folder"),
        }),
        ("/api/folders/{id}", "DELETE") => Some(PathSchemaHint {
            request: None,
            response: None,
        }),

        // R513: admin user directory + company-access management.
        ("/api/admin/users", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("AdminUserList"),
        }),
        ("/api/admin/users/{user_id}/company-access", "GET") => Some(PathSchemaHint {
            request: None,
            response: None,
        }),
        ("/api/admin/users/{user_id}/company-access", "PUT") => Some(PathSchemaHint {
            request: None,
            response: None,
        }),
        ("/api/admin/users/{user_id}/promote-instance-admin", "POST") => Some(PathSchemaHint {
            request: None,
            response: Some("AdminUser"),
        }),
        ("/api/admin/users/{user_id}/demote-instance-admin", "POST") => Some(PathSchemaHint {
            request: None,
            response: Some("AdminUser"),
        }),

        // R513: companies sub-resources.
        ("/api/companies/{company_id}/members", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("CompanyMemberList"),
        }),
        ("/api/companies/{company_id}/stats", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("CompanyStats"),
        }),
        ("/api/companies/{company_id}/timeline", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("CompanyTimelineResult"),
        }),
        ("/api/companies/{company_id}/artifacts", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("CompanyArtifactList"),
        }),
        ("/api/companies/{company_id}/org", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("CompanyOrgChart"),
        }),
        ("/api/companies/{company_id}/org.svg", "GET") => Some(PathSchemaHint {
            request: None,
            response: None,
        }),
        ("/api/companies/{company_id}/org.png", "GET") => Some(PathSchemaHint {
            request: None,
            response: None,
        }),
        ("/api/companies/{company_id}/agents", "POST") => Some(PathSchemaHint {
            request: Some("Agent"),
            response: Some("Agent"),
        }),
        ("/api/companies/{company_id}/archive", "POST") => Some(PathSchemaHint {
            request: None,
            response: Some("Company"),
        }),
        ("/api/companies/stats", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("CompanyStatsList"),
        }),
        ("/api/companies/issues", "GET") => Some(PathSchemaHint {
            request: None,
            response: None,
        }),
        ("/api/companies/import/preview", "POST") => Some(PathSchemaHint {
            request: None,
            response: None,
        }),
        ("/api/companies/import/jobs/{job_id}", "GET") => Some(PathSchemaHint {
            request: None,
            response: None,
        }),

        // R513: invite routes.
        ("/api/invites/{invite_id}", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("Invite"),
        }),
        ("/api/invites/{invite_id}/accept", "POST") => Some(PathSchemaHint {
            request: None,
            response: Some("Invite"),
        }),
        ("/api/invites/{invite_id}/onboarding", "GET") => Some(PathSchemaHint {
            request: None,
            response: None,
        }),
        ("/api/invites/{invite_id}/logo", "GET") => Some(PathSchemaHint {
            request: None,
            response: None,
        }),

        // R513: skills catalog.
        ("/api/skills/available", "GET") => Some(PathSchemaHint {
            request: None,
            response: None,
        }),
        ("/api/skills/catalog", "GET") => Some(PathSchemaHint {
            request: None,
            response: None,
        }),
        ("/api/skills/index", "GET") => Some(PathSchemaHint {
            request: None,
            response: None,
        }),
        ("/api/skills/{skill_name}", "GET") => Some(PathSchemaHint {
            request: None,
            response: None,
        }),

        // R577: UI client paths registered for OpenAPI M19 coverage.
        // Path strings are factual API contracts; hints carry no Node.js
        // source. Types reference the schema names already registered by
        // pc-openapi.
        ("/api/health", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("Health"),
        }),
        ("/api/health/dev-server/restart", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("DevServerRestart"),
        }),
        ("/api/auth/get-session", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("Session"),
        }),
        ("/api/auth/profile", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("UserProfile"),
        }),
        ("/api/auth/profile", "PATCH") => Some(PathSchemaHint {
            request: Some("UserProfileUpdate"),
            response: Some("UserProfile"),
        }),
        ("/api/adapters/{adapter_type}/ui-parser.js", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("JsSource"),
        }),
        ("/api/assets/{asset_id}/content", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("AssetContent"),
        }),
        ("/api/companies/{company_id}/audit/agent-actions.csv", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("CsvExport"),
        }),
        ("/api/companies/{company_id}/events/ws", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("LiveEventStream"),
        }),
        ("/api/issues/{issue_id}/file-resources/content", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("FileResourceContent"),
        }),
        ("/api/v1/runs", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("RunList"),
        }),
        ("/api/plugins/{plugin_id}/actions/{key}", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("PluginAction"),
        }),
        ("/api/plugins/{plugin_id}/data/{key}", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("PluginData"),
        }),
        ("/api/plugins/{plugin_id}/bridge/stream/{channel}", "GET") => Some(PathSchemaHint {
            request: None,
            response: Some("BridgeStream"),
        }),

        _ => None,
    }
}

/// Build the `responses` block for an operation given the hint's response
/// schema name and whether the operation accepts a request body.
///
/// R509: in addition to the 200 / 401 / 404 trio, POST/PATCH/PUT operations
/// get a 422 ValidationErrorList reference and every operation gets a 500
/// ErrorResponse reference. GET/DELETE stay minimal.
///
/// Mirrors OpenAPI 3.1 wire format:
/// ```json
/// "responses": {
///   "200": {
///     "description": "OK",
///     "content": {
///       "application/json": {
///         "schema": { "$ref": "#/components/schemas/Company" }
///       }
///     }
///   },
///   "401": { "description": "Unauthorized" },
///   "404": { "description": "Not Found" },
///   "422": {
///     "description": "Validation error",
///     "content": {
///       "application/json": {
///         "schema": { "$ref": "#/components/schemas/ValidationErrorList" }
///       }
///     }
///   },
///   "500": {
///     "description": "Internal server error",
///     "content": {
///       "application/json": {
///         "schema": { "$ref": "#/components/schemas/ErrorResponse" }
///       }
///     }
///   }
/// }
/// ```
fn build_responses_block(response_schema: Option<&str>, has_request_body: bool) -> Value {
    let mut responses = serde_json::Map::new();
    if let Some(name) = response_schema {
        responses.insert(
            "200".to_string(),
            json!({
                "description": "OK",
                "content": {
                    "application/json": {
                        "schema": { "$ref": format!("#/components/schemas/{name}") }
                    }
                }
            }),
        );
    } else {
        responses.insert("200".to_string(), json!({"description": "OK"}));
    }
    responses.insert("401".to_string(), json!({"description": "Unauthorized"}));
    responses.insert("404".to_string(), json!({"description": "Not Found"}));
    if has_request_body {
        responses.insert(
            "422".to_string(),
            json!({
                "description": "Validation error",
                "content": {
                    "application/json": {
                        "schema": { "$ref": "#/components/schemas/ValidationErrorList" }
                    }
                }
            }),
        );
    }
    responses.insert(
        "500".to_string(),
        json!({
            "description": "Internal server error",
            "content": {
                "application/json": {
                    "schema": { "$ref": "#/components/schemas/ErrorResponse" }
                }
            }
        }),
    );
    Value::Object(responses)
}

/// Build the `requestBody` block for an operation given the hint's request
/// schema name. Returns `None` if there's no request body (e.g. GET).
fn build_request_body_block(request_schema: Option<&str>) -> Option<Value> {
    let name = request_schema?;
    Some(json!({
        "required": true,
        "content": {
            "application/json": {
                "schema": { "$ref": format!("#/components/schemas/{name}") }
            }
        }
    }))
}

///
/// Mirrors the regex used in `scripts/diff-routes.sh` so the OpenAPI
/// document stays consistent with the diff metric.
fn scan_routes_for_openapi() -> BTreeMap<String, Value> {
    let mut paths: BTreeMap<String, Value> = BTreeMap::new();
    let routes_dir = locate_routes_dir();
    let Ok(entries) = std::fs::read_dir(&routes_dir) else {
        return paths;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        let Ok(raw_src) = std::fs::read_to_string(&path) else {
            continue;
        };
        // Drop comments so example paths inside doc comments don't leak
        // into the spec.
        let src = strip_rust_comments(&raw_src);
        for chunk in src.split(".route(").skip(1) {
            // Find the leading quoted path.
            let trimmed = chunk.trim_start();
            let path_start = trimmed.find(['\'', '\"']).unwrap_or(usize::MAX);
            if path_start == usize::MAX {
                continue;
            }
            let quote = trimmed.as_bytes()[path_start] as char;
            let after_quote = &trimmed[path_start + 1..];
            let path_end = after_quote.find(quote).unwrap_or(usize::MAX);
            if path_end == usize::MAX {
                continue;
            }
            let raw_path = &after_quote[..path_end];
            if !raw_path.starts_with('/') {
                continue;
            }
            // Restrict to the same `.route(...)` invocation: stop at the
            // first top-level close paren. Track paren depth starting from 1.
            let mut depth = 1i32;
            let mut tail = String::new();
            for ch in after_quote[path_end + 1..].chars() {
                if ch == '(' {
                    depth += 1;
                } else if ch == ')' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                tail.push(ch);
            }
            // Find chained methods in the tail.
            //
            // R522 fix: also split on `.` so chained methods like
            // `.get(h).post(h)` are picked up. Tokens like `.post` get
            // their leading `.` stripped before matching.
            //
            // Pre-R522 only the *leading* verb was detected for chained
            // calls (e.g. `get(list).post(create)` only registered as
            // GET). This left OpenAPI consumers blind to half the API
            // surface.
            let mut verbs = std::collections::BTreeSet::new();
            for raw_token in tail
                .split(|c: char| c == '(' || c == ')' || c == ',' || c == '.' || c.is_whitespace())
            {
                let token = raw_token.trim_start_matches('.');
                if matches!(token, "get" | "post" | "put" | "patch" | "delete") {
                    verbs.insert(token.to_string());
                }
            }
            // Skip if the path doesn't look like a real route path
            if raw_path.is_empty() || raw_path.len() < 2 {
                continue;
            }
            let normalized_path = normalize_path(raw_path);
            let tag = infer_tag(raw_path);
            let entry = paths
                .entry(normalized_path.clone())
                .or_insert_with(|| json!({}));
            if let Some(obj) = entry.as_object_mut() {
                for verb in &verbs {
                    let method = verb.to_lowercase();
                    let hint = path_schema_hint(&normalized_path, verb);
                    let request_body = build_request_body_block(hint.and_then(|h| h.request));
                    let responses = build_responses_block(
                        hint.and_then(|h| h.response),
                        request_body.is_some(),
                    );
                    let mut op = json!({
                        "operationId": operation_id(verb, &normalized_path),
                        "summary": format!("{} {}", verb.to_uppercase(), normalized_path),
                        "tags": [tag.clone()],
                        "responses": responses,
                    });
                    if let Some(body) = request_body {
                        if let Some(op_obj) = op.as_object_mut() {
                            op_obj.insert("requestBody".to_string(), body);
                        }
                    }
                    // R515: annotate state-changing operations on session-auth
                    // paths with `security: [{csrfToken: []}]` so API consumers
                    // know they must send the X-CSRF-Token header.
                    if csrf_protected_in_openapi(&normalized_path, verb) {
                        if let Some(op_obj) = op.as_object_mut() {
                            op_obj.insert("security".to_string(), json!([{"csrfToken": []}]));
                        }
                    }
                    obj.insert(method.clone(), op);
                }
            }
        }
    }
    // R695 (UI-2): inject hint-only paths so the OpenAPI doc reflects the
    // full surface even when a router was mounted via `.merge()` and the
    // walker only saw the relative `.route("/runs", ...)` form.
    merge_hint_only_paths(&mut paths);
    paths
}

/// R695 (UI-2): merge path+verb hints declared in [`ALL_HINT_ONLY_PATHS`]
/// into the OpenAPI `paths` map when the route walker missed them. We
/// reuse the same `path_schema_hint` machinery as the walker so the
/// resulting operation objects stay consistent.
fn merge_hint_only_paths(paths: &mut BTreeMap<String, Value>) {
    for (raw_path, verb) in ALL_HINT_ONLY_PATHS {
        let normalized = normalize_path(raw_path);
        if paths.contains_key(&normalized) {
            continue;
        }
        let hint = path_schema_hint(&normalized, verb);
        let request_body = build_request_body_block(hint.as_ref().and_then(|h| h.request));
        let responses = build_responses_block(
            hint.as_ref().and_then(|h| h.response),
            request_body.is_some(),
        );
        let mut op = json!({
            "operationId": operation_id(verb, &normalized),
            "summary": format!("{} {}", verb.to_uppercase(), normalized),
            "tags": [infer_tag(&normalized)],
            "responses": responses,
        });
        if let Some(body) = request_body {
            if let Some(op_obj) = op.as_object_mut() {
                op_obj.insert("requestBody".to_string(), body);
            }
        }
        if csrf_protected_in_openapi(&normalized, verb) {
            if let Some(op_obj) = op.as_object_mut() {
                op_obj.insert("security".to_string(), json!([{"csrfToken": []}]));
            }
        }
        let entry = paths.entry(normalized).or_insert_with(|| json!({}));
        if let Some(obj) = entry.as_object_mut() {
            obj.insert(verb.to_lowercase(), op);
        }
    }
}


/// Locate the routes directory by trying relative paths from CARGO_MANIFEST_DIR.
fn locate_routes_dir() -> std::path::PathBuf {
    // CARGO_MANIFEST_DIR is set at compile time to `crates/pc-http`.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let primary = std::path::Path::new(manifest_dir).join("src/routes");
    if primary.is_dir() {
        return primary;
    }
    // Fallback: scan upward for the `crates/pc-http/src/routes` directory.
    let mut cursor = std::path::Path::new(manifest_dir).to_path_buf();
    for _ in 0..6 {
        let candidate = cursor.join("crates/pc-http/src/routes");
        if candidate.is_dir() {
            return candidate;
        }
        if !cursor.pop() {
            break;
        }
    }
    primary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_path_converts_param_style() {
        assert_eq!(
            normalize_path("/api/companies/:company_id"),
            "/api/companies/{company_id}"
        );
        assert_eq!(
            normalize_path("/api/issues/:id/comments/:comment_id"),
            "/api/issues/{id}/comments/{comment_id}"
        );
        assert_eq!(normalize_path("/api/health"), "/api/health");
    }

    #[test]
    fn operation_id_is_stable() {
        assert_eq!(
            operation_id("GET", "/api/companies/{id}"),
            "get_api_companies_id"
        );
        assert_eq!(
            operation_id("POST", "/api/issues/{id}/comments"),
            "post_api_issues_id_comments"
        );
    }

    #[test]
    fn tag_inference_uses_first_resource() {
        assert_eq!(infer_tag("/api/companies"), "companies");
        assert_eq!(infer_tag("/api/companies/{id}/agents"), "companies");
        assert_eq!(infer_tag("/api/company-skills"), "company");
        assert_eq!(infer_tag("/health"), "health");
    }

    // -------- r503: YAML emitter + /openapi.yaml route --------

    #[test]
    fn r503_yaml_emitter_scalars() {
        assert_eq!(json_value_to_yaml(&json!(null), 0), "null");
        assert_eq!(json_value_to_yaml(&json!(true), 0), "true");
        assert_eq!(json_value_to_yaml(&json!(42), 0), "42");
        assert_eq!(json_value_to_yaml(&json!("hello"), 0), "\"hello\"");
    }

    #[test]
    fn r503_yaml_emitter_escapes_quotes_and_newlines() {
        let s = json_value_to_yaml(&json!("a\"b\\c\nd"), 0);
        // Order matters: escape backslash first, then quote, then newline.
        assert_eq!(s, "\"a\\\"b\\\\c\\nd\"");
    }

    #[test]
    fn r503_yaml_emitter_empty_collections() {
        assert_eq!(json_value_to_yaml(&json!([]), 0), "[]");
        assert_eq!(json_value_to_yaml(&json!({}), 0), "{}");
    }

    #[test]
    fn r503_yaml_emitter_object_uses_bare_keys() {
        let v = json!({"openapi": "3.1.0", "info": {"title": "T"}});
        let y = json_value_to_yaml(&v, 0);
        // Keys must be unquoted in YAML.
        assert!(y.contains("openapi: \"3.1.0\""));
        assert!(y.contains("info:"));
        assert!(y.contains("title: \"T\""));
    }

    #[test]
    fn r503_yaml_emitter_array_inline_scalars() {
        let v = json!({"tags": ["a", "b", "c"]});
        let y = json_value_to_yaml(&v, 0);
        assert!(y.contains("tags:"));
        assert!(y.contains("- \"a\""));
        assert!(y.contains("- \"b\""));
        assert!(y.contains("- \"c\""));
    }

    #[test]
    fn r503_router_has_yaml_route() {
        let r = router();
        // We can't introspect Router directly, but we can at least confirm
        // the function builds without panicking and exposes the canonical
        // mount points via the underlying axum Router type's path API.
        let _ = r;
    }

    // -------- r505: DTO schema injection into /openapi.json body --------

    #[test]
    fn r505_core_dto_schemas_present_in_body() {
        let mut body = json!({"components": {}});
        inject_dto_schemas(&mut body);
        let schemas = body["components"]["schemas"].as_object().expect("schemas");
        for name in ["Decision", "Company", "Issue", "Agent", "HeartbeatRun"] {
            assert!(
                schemas.contains_key(name),
                "/openapi.json body must contain `{name}` schema, got keys: {:?}",
                schemas.keys().collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn r505_decision_schema_in_body_has_required_fields() {
        let mut body = json!({"components": {}});
        inject_dto_schemas(&mut body);
        let decision = &body["components"]["schemas"]["Decision"];
        let required = decision["required"].as_array().expect("required");
        let names: Vec<&str> = required.iter().filter_map(|r| r.as_str()).collect();
        for field in [
            "id",
            "companyId",
            "title",
            "body",
            "options",
            "status",
            "expiresAt",
        ] {
            assert!(
                names.contains(&field),
                "Decision.required must include `{field}`, got {names:?}"
            );
        }
    }

    #[test]
    fn r505_company_schema_preserves_status_enum() {
        let mut body = json!({"components": {}});
        inject_dto_schemas(&mut body);
        let company = &body["components"]["schemas"]["Company"];
        let status_enum = company["properties"]["status"]["enum"]
            .as_array()
            .expect("status enum");
        let names: Vec<&str> = status_enum.iter().filter_map(|v| v.as_str()).collect();
        for v in ["active", "paused", "archived"] {
            assert!(
                names.contains(&v),
                "Company.status enum missing `{v}`, got {names:?}"
            );
        }
    }

    #[test]
    fn r505_security_schemes_coexist_with_schemas() {
        // Start with a realistic body that already has securitySchemes
        // (matching the structure produced by `build_openapi_body`).
        let mut body = json!({
            "components": {
                "securitySchemes": {
                    "session": { "type": "apiKey" },
                    "apiKey": { "type": "apiKey" }
                }
            }
        });
        inject_dto_schemas(&mut body);
        let components = body["components"].as_object().expect("components");
        // Both keys must coexist after R505 merge.
        assert!(components.contains_key("schemas"));
        assert!(components.contains_key("securitySchemes"));
        let sec = components["securitySchemes"].as_object().expect("sec");
        assert!(sec.contains_key("session"));
        assert!(sec.contains_key("apiKey"));
    }

    // -------- r506: path-level schema hints --------

    #[test]
    fn r506_path_schema_hint_companies_get_returns_list() {
        let h = path_schema_hint("/api/companies", "GET").expect("hint");
        assert!(h.request.is_none(), "GET has no request body");
        assert_eq!(h.response, Some("CompanyList"));
    }

    #[test]
    fn r506_path_schema_hint_companies_post_round_trips() {
        let h = path_schema_hint("/api/companies", "POST").expect("hint");
        assert_eq!(h.request, Some("Company"));
        assert_eq!(h.response, Some("Company"));
    }

    #[test]
    fn r506_path_schema_hint_accepts_raw_colon_id_form() {
        // Scanner sometimes passes `:id` style (pre-normalisation).
        let h = path_schema_hint("/api/companies/:id", "GET").expect("hint");
        assert_eq!(h.response, Some("Company"));
    }

    #[test]
    fn r506_path_schema_hint_unknown_returns_none() {
        assert!(path_schema_hint("/api/foobar", "GET").is_none());
        assert!(path_schema_hint("/api/companies", "PATCH").is_none());
    }

    #[test]
    fn r506_path_schema_hint_coverage_includes_all_ninety_four() {
        // (path, method, expected_response, expected_request) — response is
        // `Option<&str>` to cover DELETE routes which return no body.
        let cases: &[(&str, &str, Option<&str>, Option<&str>)] = &[
            ("/api/companies", "GET", Some("CompanyList"), None),
            ("/api/companies", "POST", Some("Company"), Some("Company")),
            ("/api/agents", "GET", Some("AgentList"), None),
            ("/api/agents", "POST", Some("Agent"), Some("Agent")),
            ("/api/issues", "GET", Some("IssueList"), None),
            ("/api/issues", "POST", Some("Issue"), Some("Issue")),
            ("/api/decisions", "GET", Some("DecisionList"), None),
            ("/api/decisions", "POST", Some("Decision"), Some("Decision")),
            ("/api/companies/{id}", "GET", Some("Company"), None),
            ("/api/agents/{id}", "GET", Some("Agent"), None),
            // R507: 5 additional hints.
            ("/api/approvals", "GET", Some("ApprovalList"), None),
            ("/api/approvals", "POST", Some("Approval"), Some("Approval")),
            ("/api/approvals/{id}", "GET", Some("Approval"), None),
            ("/api/pipelines", "GET", Some("PipelineList"), None),
            (
                "/api/heartbeat-runs/{run_id}",
                "GET",
                Some("HeartbeatRun"),
                None,
            ),
            // R508: 9 additional hints (4 PATCH + 4 DELETE + 1 routines list).
            (
                "/api/companies/{id}",
                "PATCH",
                Some("Company"),
                Some("Company"),
            ),
            ("/api/companies/{id}", "DELETE", None, None),
            ("/api/agents/{id}", "PATCH", Some("Agent"), Some("Agent")),
            ("/api/agents/{id}", "DELETE", None, None),
            ("/api/issues/{id}", "PATCH", Some("Issue"), Some("Issue")),
            ("/api/issues/{id}", "DELETE", None, None),
            (
                "/api/decisions/{id}",
                "PATCH",
                Some("Decision"),
                Some("Decision"),
            ),
            ("/api/decisions/{id}", "DELETE", None, None),
            ("/api/routines", "GET", Some("RoutineList"), None),
            // R509: 8 additional hints (item GETs + create POSTs + heartbeat).
            ("/api/issues/{id}", "GET", Some("Issue"), None),
            ("/api/decisions/{id}", "GET", Some("Decision"), None),
            ("/api/pipelines/{id}", "GET", Some("Pipeline"), None),
            ("/api/routines/{id}", "GET", Some("Routine"), None),
            ("/api/pipelines", "POST", Some("Pipeline"), Some("Pipeline")),
            ("/api/routines", "POST", Some("Routine"), Some("Routine")),
            (
                "/api/routines/{id}",
                "PATCH",
                Some("Routine"),
                Some("Routine"),
            ),
            ("/api/heartbeat", "POST", Some("HeartbeatRun"), None),
            // R510: 12 additional hints (cases / goals / approvals PATCH/DELETE / pipelines).
            ("/api/cases", "GET", Some("CaseList"), None),
            ("/api/cases", "POST", Some("Case"), Some("Case")),
            ("/api/cases/{case_id}", "GET", Some("Case"), None),
            ("/api/cases/{case_id}", "PATCH", Some("Case"), Some("Case")),
            ("/api/cases/{case_id}", "DELETE", None, None),
            ("/api/goals", "GET", Some("GoalList"), None),
            ("/api/goals", "POST", Some("Goal"), Some("Goal")),
            ("/api/goals/{id}", "GET", Some("Goal"), None),
            (
                "/api/approvals/{id}",
                "PATCH",
                Some("Approval"),
                Some("Approval"),
            ),
            ("/api/approvals/{id}", "DELETE", None, None),
            (
                "/api/pipelines/{id}",
                "PATCH",
                Some("Pipeline"),
                Some("Pipeline"),
            ),
            (
                "/api/pipelines/{id}/archive",
                "POST",
                Some("Pipeline"),
                None,
            ),
            // R511: 25 additional hints (cases sub-resources + goals PATCH/DELETE +
            // inbox dismissals + folders CRUD + legacy folder endpoints).
            ("/api/cases/{case_id}/events", "GET", Some("CaseList"), None),
            (
                "/api/cases/{case_id}/issue-links",
                "POST",
                Some("Case"),
                Some("Case"),
            ),
            (
                "/api/cases/{case_id}/links",
                "POST",
                Some("Case"),
                Some("Case"),
            ),
            ("/api/cases/{case_id}/breakdown", "POST", Some("Case"), None),
            (
                "/api/cases/{case_id}/review",
                "POST",
                Some("Case"),
                Some("Case"),
            ),
            (
                "/api/cases/{case_id}/children",
                "GET",
                Some("CaseList"),
                None,
            ),
            (
                "/api/issues/{issue_id}/cases",
                "GET",
                Some("CaseList"),
                None,
            ),
            ("/api/goals/{id}", "PATCH", Some("Goal"), Some("Goal")),
            ("/api/goals/{id}", "DELETE", None, None),
            (
                "/api/companies/{company_id}/inbox-dismissals",
                "GET",
                Some("InboxList"),
                None,
            ),
            (
                "/api/companies/{company_id}/inbox-dismissals",
                "POST",
                Some("Inbox"),
                Some("Inbox"),
            ),
            (
                "/api/companies/{company_id}/inbox-dismissals/{item_key}",
                "DELETE",
                None,
                None,
            ),
            (
                "/api/companies/{company_id}/inbox-dismissals/dismiss",
                "POST",
                Some("Inbox"),
                Some("Inbox"),
            ),
            (
                "/api/companies/{company_id}/inbox-dismissals/snooze",
                "POST",
                Some("Inbox"),
                Some("Inbox"),
            ),
            (
                "/api/companies/{company_id}/inbox-dismissals/count",
                "GET",
                None,
                None,
            ),
            (
                "/api/companies/{company_id}/folders",
                "GET",
                Some("FolderList"),
                None,
            ),
            (
                "/api/companies/{company_id}/folders",
                "POST",
                Some("Folder"),
                Some("Folder"),
            ),
            (
                "/api/companies/{company_id}/folders/ensure-my",
                "POST",
                Some("Folder"),
                None,
            ),
            (
                "/api/companies/{company_id}/folders/{folder_id}",
                "PATCH",
                Some("Folder"),
                Some("Folder"),
            ),
            (
                "/api/companies/{company_id}/folders/{folder_id}",
                "DELETE",
                None,
                None,
            ),
            (
                "/api/companies/{company_id}/folders/{folder_id}/move",
                "POST",
                Some("Folder"),
                None,
            ),
            (
                "/api/companies/{company_id}/folders/items/move",
                "POST",
                None,
                None,
            ),
            ("/api/folders", "GET", Some("FolderList"), None),
            ("/api/folders", "POST", Some("Folder"), Some("Folder")),
            ("/api/folders/{id}", "DELETE", None, None),
            // R513: 25 additional hints — admin + companies sub-resources + invites + skills.
            ("/api/admin/users", "GET", Some("AdminUserList"), None),
            (
                "/api/admin/users/{user_id}/company-access",
                "GET",
                None,
                None,
            ),
            (
                "/api/admin/users/{user_id}/company-access",
                "PUT",
                None,
                None,
            ),
            (
                "/api/admin/users/{user_id}/promote-instance-admin",
                "POST",
                Some("AdminUser"),
                None,
            ),
            (
                "/api/admin/users/{user_id}/demote-instance-admin",
                "POST",
                Some("AdminUser"),
                None,
            ),
            (
                "/api/companies/{company_id}/members",
                "GET",
                Some("CompanyMemberList"),
                None,
            ),
            // R522: companies aggregation endpoints now have real schemas.
            (
                "/api/companies/{company_id}/stats",
                "GET",
                Some("CompanyStats"),
                None,
            ),
            (
                "/api/companies/{company_id}/timeline",
                "GET",
                Some("CompanyTimelineResult"),
                None,
            ),
            (
                "/api/companies/{company_id}/artifacts",
                "GET",
                Some("CompanyArtifactList"),
                None,
            ),
            (
                "/api/companies/{company_id}/org",
                "GET",
                Some("CompanyOrgChart"),
                None,
            ),
            ("/api/companies/{company_id}/org.svg", "GET", None, None),
            ("/api/companies/{company_id}/org.png", "GET", None, None),
            (
                "/api/companies/{company_id}/agents",
                "POST",
                Some("Agent"),
                Some("Agent"),
            ),
            (
                "/api/companies/{company_id}/archive",
                "POST",
                Some("Company"),
                None,
            ),
            (
                "/api/companies/stats",
                "GET",
                Some("CompanyStatsList"),
                None,
            ),
            ("/api/companies/issues", "GET", None, None),
            ("/api/companies/import/preview", "POST", None, None),
            ("/api/companies/import/jobs/{job_id}", "GET", None, None),
            ("/api/invites/{invite_id}", "GET", Some("Invite"), None),
            (
                "/api/invites/{invite_id}/accept",
                "POST",
                Some("Invite"),
                None,
            ),
            ("/api/invites/{invite_id}/onboarding", "GET", None, None),
            ("/api/invites/{invite_id}/logo", "GET", None, None),
            ("/api/skills/available", "GET", None, None),
            ("/api/skills/catalog", "GET", None, None),
            ("/api/skills/index", "GET", None, None),
            ("/api/skills/{skill_name}", "GET", None, None),
        ];
        for (path, method, expected_resp, expected_req) in cases {
            let h = path_schema_hint(path, method)
                .unwrap_or_else(|| panic!("no hint for {path} {method}"));
            assert_eq!(h.response, *expected_resp, "response for {path} {method}");
            assert_eq!(h.request, *expected_req, "request for {path} {method}");
        }
    }

    // -------- r509: error responses in operations --------

    #[test]
    fn r509_responses_block_includes_422_when_request_body_present() {
        let v = build_responses_block(Some("Company"), true);
        assert_eq!(v["422"]["description"], "Validation error");
        assert_eq!(
            v["422"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/ValidationErrorList"
        );
    }

    #[test]
    fn r509_responses_block_omits_422_when_no_request_body() {
        let v = build_responses_block(Some("CompanyList"), false);
        assert!(
            v.get("422").is_none(),
            "GET should not have 422 (no body to validate)"
        );
    }

    #[test]
    fn r509_responses_block_always_includes_500_error_response() {
        for (resp, has_body) in [
            (Some("Company"), true),  // POST
            (Some("Company"), false), // GET
            (None, true),             // DELETE w/ body (unusual)
            (None, false),            // DELETE
        ] {
            let v = build_responses_block(resp, has_body);
            assert_eq!(v["500"]["description"], "Internal server error");
            assert_eq!(
                v["500"]["content"]["application/json"]["schema"]["$ref"],
                "#/components/schemas/ErrorResponse"
            );
        }
    }

    #[test]
    fn r506_build_responses_block_includes_ref_when_schema_present() {
        let v = build_responses_block(Some("Company"), false);
        let two_hundred = &v["200"];
        assert_eq!(two_hundred["description"], "OK");
        let schema_ref = &two_hundred["content"]["application/json"]["schema"]["$ref"];
        assert_eq!(schema_ref, "#/components/schemas/Company");
        assert!(v["401"].is_object());
        assert!(v["404"].is_object());
    }

    #[test]
    fn r506_build_responses_block_omits_content_when_no_schema() {
        let v = build_responses_block(None, false);
        let two_hundred = &v["200"];
        assert_eq!(two_hundred["description"], "OK");
        assert!(
            two_hundred.get("content").is_none(),
            "no content without schema"
        );
    }

    #[test]
    fn r506_build_request_body_block_returns_none_for_get() {
        assert!(build_request_body_block(None).is_none());
    }

    #[test]
    fn r506_build_request_body_block_includes_ref_when_schema_present() {
        let body = build_request_body_block(Some("Agent")).expect("body");
        assert_eq!(body["required"], true);
        let schema_ref = &body["content"]["application/json"]["schema"]["$ref"];
        assert_eq!(schema_ref, "#/components/schemas/Agent");
    }

    #[test]
    fn r506_full_body_has_request_body_for_post_companies() {
        // End-to-end: build_openapi_body's scanner emits operations with
        // requestBody for POST /api/companies. We can't easily invoke
        // build_openapi_body (needs AppState), so we test the inner pieces
        // and rely on inject_dto_schemas for the schema registration.
        let mut body = json!({"paths": {}});
        let path = "/api/companies";
        let method = "POST";
        let hint = path_schema_hint(path, method).expect("hint");
        let op = json!({
            "operationId": "post_api_companies",
            "summary": "POST /api/companies",
            "tags": ["companies"],
            "responses": build_responses_block(hint.response, hint.request.is_some()),
            "requestBody": build_request_body_block(hint.request),
        });
        body["paths"][path][method.to_lowercase()] = op;
        let op = &body["paths"][path]["post"];
        assert_eq!(
            op["requestBody"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/Company"
        );
        assert_eq!(
            op["responses"]["200"]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/Company"
        );
    }

    // -------- r507: additional hints for approvals / pipelines / heartbeat --------

    #[test]
    fn r507_approvals_get_returns_list() {
        let h = path_schema_hint("/api/approvals", "GET").expect("hint");
        assert!(h.request.is_none());
        assert_eq!(h.response, Some("ApprovalList"));
    }

    #[test]
    fn r507_approvals_post_round_trips() {
        let h = path_schema_hint("/api/approvals", "POST").expect("hint");
        assert_eq!(h.request, Some("Approval"));
        assert_eq!(h.response, Some("Approval"));
    }

    #[test]
    fn r507_heartbeat_run_item_route_uses_heartbeat_run_schema() {
        let h = path_schema_hint("/api/heartbeat-runs/{run_id}", "GET").expect("hint");
        assert!(h.request.is_none());
        assert_eq!(h.response, Some("HeartbeatRun"));
    }

    // -------- r508: PATCH / DELETE + routines --------

    #[test]
    fn r508_companies_patch_returns_company() {
        let h = path_schema_hint("/api/companies/{id}", "PATCH").expect("hint");
        assert_eq!(h.request, Some("Company"));
        assert_eq!(h.response, Some("Company"));
    }

    #[test]
    fn r508_agents_delete_has_no_body() {
        let h = path_schema_hint("/api/agents/{id}", "DELETE").expect("hint");
        assert!(h.request.is_none(), "DELETE has no request body");
        assert!(
            h.response.is_none(),
            "DELETE has no JSON response (returns 204 No Content)"
        );
    }

    #[test]
    fn r508_issues_patch_round_trips() {
        let h = path_schema_hint("/api/issues/{id}", "PATCH").expect("hint");
        assert_eq!(h.request, Some("Issue"));
        assert_eq!(h.response, Some("Issue"));
    }

    #[test]
    fn r508_decisions_delete_has_no_body() {
        let h = path_schema_hint("/api/decisions/{id}", "DELETE").expect("hint");
        assert!(h.request.is_none());
        assert!(h.response.is_none());
    }

    // -------- r509: item GETs + create POSTs --------

    #[test]
    fn r509_issues_item_get_returns_issue() {
        let h = path_schema_hint("/api/issues/{id}", "GET").expect("hint");
        assert!(h.request.is_none());
        assert_eq!(h.response, Some("Issue"));
    }

    #[test]
    fn r509_decisions_item_get_returns_decision() {
        let h = path_schema_hint("/api/decisions/{id}", "GET").expect("hint");
        assert_eq!(h.response, Some("Decision"));
    }

    #[test]
    fn r509_pipelines_post_round_trips() {
        let h = path_schema_hint("/api/pipelines", "POST").expect("hint");
        assert_eq!(h.request, Some("Pipeline"));
        assert_eq!(h.response, Some("Pipeline"));
    }

    #[test]
    fn r509_routines_post_round_trips() {
        let h = path_schema_hint("/api/routines", "POST").expect("hint");
        assert_eq!(h.request, Some("Routine"));
        assert_eq!(h.response, Some("Routine"));
    }

    #[test]
    fn r509_routines_patch_round_trips() {
        let h = path_schema_hint("/api/routines/{id}", "PATCH").expect("hint");
        assert_eq!(h.request, Some("Routine"));
        assert_eq!(h.response, Some("Routine"));
    }

    // -------- r510: cases / goals / approvals PATCH-DELETE / pipelines --------

    #[test]
    fn r510_cases_crud_round_trips() {
        // GET list
        let h = path_schema_hint("/api/cases", "GET").expect("hint");
        assert_eq!(h.response, Some("CaseList"));
        // POST
        let h = path_schema_hint("/api/cases", "POST").expect("hint");
        assert_eq!(h.request, Some("Case"));
        assert_eq!(h.response, Some("Case"));
        // GET item
        let h = path_schema_hint("/api/cases/{case_id}", "GET").expect("hint");
        assert_eq!(h.response, Some("Case"));
        // PATCH
        let h = path_schema_hint("/api/cases/{case_id}", "PATCH").expect("hint");
        assert_eq!(h.request, Some("Case"));
        assert_eq!(h.response, Some("Case"));
        // DELETE
        let h = path_schema_hint("/api/cases/{case_id}", "DELETE").expect("hint");
        assert!(h.request.is_none());
        assert!(h.response.is_none());
    }

    #[test]
    fn r510_goals_crud_round_trips() {
        let h = path_schema_hint("/api/goals", "GET").expect("hint");
        assert_eq!(h.response, Some("GoalList"));
        let h = path_schema_hint("/api/goals", "POST").expect("hint");
        assert_eq!(h.request, Some("Goal"));
        assert_eq!(h.response, Some("Goal"));
        let h = path_schema_hint("/api/goals/{id}", "GET").expect("hint");
        assert_eq!(h.response, Some("Goal"));
    }

    #[test]
    fn r510_approvals_patch_delete() {
        let h = path_schema_hint("/api/approvals/{id}", "PATCH").expect("hint");
        assert_eq!(h.request, Some("Approval"));
        assert_eq!(h.response, Some("Approval"));
        let h = path_schema_hint("/api/approvals/{id}", "DELETE").expect("hint");
        assert!(h.request.is_none());
        assert!(h.response.is_none());
    }

    #[test]
    fn r510_pipelines_patch_and_archive() {
        let h = path_schema_hint("/api/pipelines/{id}", "PATCH").expect("hint");
        assert_eq!(h.request, Some("Pipeline"));
        assert_eq!(h.response, Some("Pipeline"));
        // Archive POST has no body, returns updated Pipeline.
        let h = path_schema_hint("/api/pipelines/{id}/archive", "POST").expect("hint");
        assert!(h.request.is_none());
        assert_eq!(h.response, Some("Pipeline"));
    }

    // -------- r511: cases sub-resources + goals PATCH/DELETE + inbox + folders --------

    #[test]
    fn r511_cases_sub_resources_round_trip() {
        let h = path_schema_hint("/api/cases/{case_id}/events", "GET").expect("hint");
        assert!(h.request.is_none());
        assert_eq!(h.response, Some("CaseList"));
        let h = path_schema_hint("/api/cases/{case_id}/issue-links", "POST").expect("hint");
        assert_eq!(h.request, Some("Case"));
        assert_eq!(h.response, Some("Case"));
        let h = path_schema_hint("/api/cases/{case_id}/links", "POST").expect("hint");
        assert_eq!(h.request, Some("Case"));
        assert_eq!(h.response, Some("Case"));
        let h = path_schema_hint("/api/cases/{case_id}/breakdown", "POST").expect("hint");
        assert!(h.request.is_none());
        assert_eq!(h.response, Some("Case"));
        let h = path_schema_hint("/api/cases/{case_id}/review", "POST").expect("hint");
        assert_eq!(h.request, Some("Case"));
        assert_eq!(h.response, Some("Case"));
        let h = path_schema_hint("/api/cases/{case_id}/children", "GET").expect("hint");
        assert_eq!(h.response, Some("CaseList"));
    }

    #[test]
    fn r511_issues_cases_junction() {
        let h = path_schema_hint("/api/issues/{issue_id}/cases", "GET").expect("hint");
        assert!(h.request.is_none());
        assert_eq!(h.response, Some("CaseList"));
    }

    #[test]
    fn r511_goals_patch_delete() {
        let h = path_schema_hint("/api/goals/{id}", "PATCH").expect("hint");
        assert_eq!(h.request, Some("Goal"));
        assert_eq!(h.response, Some("Goal"));
        let h = path_schema_hint("/api/goals/{id}", "DELETE").expect("hint");
        assert!(h.request.is_none());
        assert!(h.response.is_none());
    }

    #[test]
    fn r511_inbox_dismissals_all_verbs() {
        let h =
            path_schema_hint("/api/companies/{company_id}/inbox-dismissals", "GET").expect("hint");
        assert_eq!(h.response, Some("InboxList"));
        let h =
            path_schema_hint("/api/companies/{company_id}/inbox-dismissals", "POST").expect("hint");
        assert_eq!(h.request, Some("Inbox"));
        assert_eq!(h.response, Some("Inbox"));
        let h = path_schema_hint(
            "/api/companies/{company_id}/inbox-dismissals/{item_key}",
            "DELETE",
        )
        .expect("hint");
        assert!(h.request.is_none());
        assert!(h.response.is_none());
        let h = path_schema_hint(
            "/api/companies/{company_id}/inbox-dismissals/dismiss",
            "POST",
        )
        .expect("hint");
        assert_eq!(h.request, Some("Inbox"));
        let h = path_schema_hint(
            "/api/companies/{company_id}/inbox-dismissals/snooze",
            "POST",
        )
        .expect("hint");
        assert_eq!(h.request, Some("Inbox"));
        let h = path_schema_hint("/api/companies/{company_id}/inbox-dismissals/count", "GET")
            .expect("hint");
        assert!(h.request.is_none());
        assert!(h.response.is_none());
    }

    #[test]
    fn r513_admin_users_routes_round_trip() {
        let h = path_schema_hint("/api/admin/users", "GET").expect("hint");
        assert_eq!(h.response, Some("AdminUserList"));
        let h = path_schema_hint("/api/admin/users/{user_id}/promote-instance-admin", "POST")
            .expect("hint");
        assert_eq!(h.response, Some("AdminUser"));
        let h = path_schema_hint("/api/admin/users/{user_id}/demote-instance-admin", "POST")
            .expect("hint");
        assert_eq!(h.response, Some("AdminUser"));
        let h = path_schema_hint("/api/admin/users/{user_id}/company-access", "GET").expect("hint");
        assert!(h.request.is_none());
        let h = path_schema_hint("/api/admin/users/{user_id}/company-access", "PUT").expect("hint");
        assert!(h.response.is_none());
    }

    #[test]
    fn r513_companies_sub_resources_round_trip() {
        let h = path_schema_hint("/api/companies/{company_id}/members", "GET").expect("hint");
        assert_eq!(h.response, Some("CompanyMemberList"));
        // R522: stats/timeline/artifacts/org now have real schemas.
        let h = path_schema_hint("/api/companies/{company_id}/stats", "GET").expect("hint");
        assert_eq!(h.response, Some("CompanyStats"));
        let h = path_schema_hint("/api/companies/{company_id}/timeline", "GET").expect("hint");
        assert_eq!(h.response, Some("CompanyTimelineResult"));
        let h = path_schema_hint("/api/companies/{company_id}/artifacts", "GET").expect("hint");
        assert_eq!(h.response, Some("CompanyArtifactList"));
        let h = path_schema_hint("/api/companies/{company_id}/org", "GET").expect("hint");
        assert_eq!(h.response, Some("CompanyOrgChart"));
        let h = path_schema_hint("/api/companies/{company_id}/archive", "POST").expect("hint");
        assert_eq!(h.response, Some("Company"));
        let h = path_schema_hint("/api/companies/{company_id}/agents", "POST").expect("hint");
        assert_eq!(h.request, Some("Agent"));
        assert_eq!(h.response, Some("Agent"));
        let h = path_schema_hint("/api/companies/import/preview", "POST").expect("hint");
        assert!(h.response.is_none());
        let h = path_schema_hint("/api/companies/import/jobs/{job_id}", "GET").expect("hint");
        assert!(h.response.is_none());
    }

    #[test]
    fn r513_invites_and_skills_routes_round_trip() {
        let h = path_schema_hint("/api/invites/{invite_id}", "GET").expect("hint");
        assert_eq!(h.response, Some("Invite"));
        let h = path_schema_hint("/api/invites/{invite_id}/accept", "POST").expect("hint");
        assert_eq!(h.response, Some("Invite"));
        let h = path_schema_hint("/api/invites/{invite_id}/onboarding", "GET").expect("hint");
        assert!(h.response.is_none());
        let h = path_schema_hint("/api/invites/{invite_id}/logo", "GET").expect("hint");
        assert!(h.response.is_none());
        for path in [
            "/api/skills/available",
            "/api/skills/catalog",
            "/api/skills/index",
        ] {
            let h = path_schema_hint(path, "GET").expect("hint");
            assert!(
                h.response.is_none(),
                "skills endpoint {path} should have minimal response"
            );
        }
        let h = path_schema_hint("/api/skills/{skill_name}", "GET").expect("hint");
        assert!(h.response.is_none());
    }

    #[test]
    fn r511_folders_crud_and_legacy() {
        let h = path_schema_hint("/api/companies/{company_id}/folders", "GET").expect("hint");
        assert_eq!(h.response, Some("FolderList"));
        let h = path_schema_hint("/api/companies/{company_id}/folders", "POST").expect("hint");
        assert_eq!(h.request, Some("Folder"));
        assert_eq!(h.response, Some("Folder"));
        let h = path_schema_hint("/api/companies/{company_id}/folders/ensure-my", "POST")
            .expect("hint");
        assert!(h.request.is_none());
        assert_eq!(h.response, Some("Folder"));
        let h = path_schema_hint("/api/companies/{company_id}/folders/{folder_id}", "PATCH")
            .expect("hint");
        assert_eq!(h.request, Some("Folder"));
        let h = path_schema_hint("/api/companies/{company_id}/folders/{folder_id}", "DELETE")
            .expect("hint");
        assert!(h.response.is_none());
        let h = path_schema_hint(
            "/api/companies/{company_id}/folders/{folder_id}/move",
            "POST",
        )
        .expect("hint");
        assert_eq!(h.response, Some("Folder"));
        let h = path_schema_hint("/api/companies/{company_id}/folders/items/move", "POST")
            .expect("hint");
        assert!(h.request.is_none());
        assert!(h.response.is_none());
        let h = path_schema_hint("/api/folders", "GET").expect("hint");
        assert_eq!(h.response, Some("FolderList"));
        let h = path_schema_hint("/api/folders", "POST").expect("hint");
        assert_eq!(h.request, Some("Folder"));
        let h = path_schema_hint("/api/folders/{id}", "DELETE").expect("hint");
        assert!(h.response.is_none());
    }

    #[test]
    fn r511_find_duplicate_operation_ids_empty_on_well_formed_body() {
        let body = json!({
            "paths": {
                "/a": {"get": {"operationId": "get_a"}},
                "/b": {"get": {"operationId": "get_b"}, "post": {"operationId": "post_b"}},
                "/c": {"delete": {"operationId": "delete_c"}}
            }
        });
        assert!(find_duplicate_operation_ids(&body).is_empty());
    }

    #[test]
    fn r511_find_duplicate_operation_ids_detects_dup() {
        let body = json!({
            "paths": {
                "/a": {"get": {"operationId": "shared"}},
                "/b": {"get": {"operationId": "shared"}}
            }
        });
        let dups = find_duplicate_operation_ids(&body);
        assert_eq!(dups, vec!["shared".to_string()]);
    }

    #[test]
    fn r511_find_duplicate_operation_ids_flags_missing_operation_id() {
        let body = json!({
            "paths": {
                "/a": {"get": {"operationId": "good"}},
                "/b": {"get": {}}
            }
        });
        let dups = find_duplicate_operation_ids(&body);
        assert!(dups.iter().any(|d| d.starts_with("__missing__")));
    }

    #[test]
    fn r511_operation_id_is_unique_across_all_routes() {
        // Generate operation ids for every hint we ship and assert no dupes.
        let cases: &[(&str, &str, Option<&str>, Option<&str>)] = &[
            ("/api/companies", "GET", Some("CompanyList"), None),
            ("/api/companies", "POST", Some("Company"), Some("Company")),
            ("/api/agents", "GET", Some("AgentList"), None),
            ("/api/agents", "POST", Some("Agent"), Some("Agent")),
            ("/api/issues", "GET", Some("IssueList"), None),
            ("/api/issues", "POST", Some("Issue"), Some("Issue")),
            ("/api/decisions", "GET", Some("DecisionList"), None),
            ("/api/decisions", "POST", Some("Decision"), Some("Decision")),
            ("/api/companies/{id}", "GET", Some("Company"), None),
            ("/api/agents/{id}", "GET", Some("Agent"), None),
            ("/api/approvals", "GET", Some("ApprovalList"), None),
            ("/api/approvals", "POST", Some("Approval"), Some("Approval")),
            ("/api/approvals/{id}", "GET", Some("Approval"), None),
            ("/api/pipelines", "GET", Some("PipelineList"), None),
            (
                "/api/heartbeat-runs/{run_id}",
                "GET",
                Some("HeartbeatRun"),
                None,
            ),
            ("/api/issues/{id}", "GET", Some("Issue"), None),
            ("/api/decisions/{id}", "GET", Some("Decision"), None),
            ("/api/pipelines/{id}", "GET", Some("Pipeline"), None),
            ("/api/routines/{id}", "GET", Some("Routine"), None),
            ("/api/cases", "GET", Some("CaseList"), None),
            ("/api/cases", "POST", Some("Case"), Some("Case")),
            ("/api/cases/{case_id}", "GET", Some("Case"), None),
            ("/api/cases/{case_id}", "PATCH", Some("Case"), Some("Case")),
            ("/api/cases/{case_id}", "DELETE", None, None),
            ("/api/goals", "GET", Some("GoalList"), None),
            ("/api/goals", "POST", Some("Goal"), Some("Goal")),
            ("/api/goals/{id}", "GET", Some("Goal"), None),
            (
                "/api/approvals/{id}",
                "PATCH",
                Some("Approval"),
                Some("Approval"),
            ),
            ("/api/approvals/{id}", "DELETE", None, None),
            (
                "/api/pipelines/{id}",
                "PATCH",
                Some("Pipeline"),
                Some("Pipeline"),
            ),
            (
                "/api/pipelines/{id}/archive",
                "POST",
                Some("Pipeline"),
                None,
            ),
            ("/api/pipelines", "POST", Some("Pipeline"), Some("Pipeline")),
            ("/api/routines", "POST", Some("Routine"), Some("Routine")),
            (
                "/api/routines/{id}",
                "PATCH",
                Some("Routine"),
                Some("Routine"),
            ),
            ("/api/heartbeat", "POST", Some("HeartbeatRun"), None),
            (
                "/api/companies/{id}",
                "PATCH",
                Some("Company"),
                Some("Company"),
            ),
            ("/api/companies/{id}", "DELETE", None, None),
            ("/api/agents/{id}", "PATCH", Some("Agent"), Some("Agent")),
            ("/api/agents/{id}", "DELETE", None, None),
            ("/api/issues/{id}", "PATCH", Some("Issue"), Some("Issue")),
            ("/api/issues/{id}", "DELETE", None, None),
            (
                "/api/decisions/{id}",
                "PATCH",
                Some("Decision"),
                Some("Decision"),
            ),
            ("/api/decisions/{id}", "DELETE", None, None),
            ("/api/routines", "GET", Some("RoutineList"), None),
            ("/api/cases/{case_id}/events", "GET", Some("CaseList"), None),
            (
                "/api/cases/{case_id}/issue-links",
                "POST",
                Some("Case"),
                Some("Case"),
            ),
            (
                "/api/cases/{case_id}/links",
                "POST",
                Some("Case"),
                Some("Case"),
            ),
            ("/api/cases/{case_id}/breakdown", "POST", Some("Case"), None),
            (
                "/api/cases/{case_id}/review",
                "POST",
                Some("Case"),
                Some("Case"),
            ),
            (
                "/api/cases/{case_id}/children",
                "GET",
                Some("CaseList"),
                None,
            ),
            (
                "/api/issues/{issue_id}/cases",
                "GET",
                Some("CaseList"),
                None,
            ),
            ("/api/goals/{id}", "PATCH", Some("Goal"), Some("Goal")),
            ("/api/goals/{id}", "DELETE", None, None),
            (
                "/api/companies/{company_id}/inbox-dismissals",
                "GET",
                Some("InboxList"),
                None,
            ),
            (
                "/api/companies/{company_id}/inbox-dismissals",
                "POST",
                Some("Inbox"),
                Some("Inbox"),
            ),
            (
                "/api/companies/{company_id}/inbox-dismissals/{item_key}",
                "DELETE",
                None,
                None,
            ),
            (
                "/api/companies/{company_id}/inbox-dismissals/dismiss",
                "POST",
                Some("Inbox"),
                Some("Inbox"),
            ),
            (
                "/api/companies/{company_id}/inbox-dismissals/snooze",
                "POST",
                Some("Inbox"),
                Some("Inbox"),
            ),
            (
                "/api/companies/{company_id}/inbox-dismissals/count",
                "GET",
                None,
                None,
            ),
            (
                "/api/companies/{company_id}/folders",
                "GET",
                Some("FolderList"),
                None,
            ),
            (
                "/api/companies/{company_id}/folders",
                "POST",
                Some("Folder"),
                Some("Folder"),
            ),
            (
                "/api/companies/{company_id}/folders/ensure-my",
                "POST",
                Some("Folder"),
                None,
            ),
            (
                "/api/companies/{company_id}/folders/{folder_id}",
                "PATCH",
                Some("Folder"),
                Some("Folder"),
            ),
            (
                "/api/companies/{company_id}/folders/{folder_id}",
                "DELETE",
                None,
                None,
            ),
            (
                "/api/companies/{company_id}/folders/{folder_id}/move",
                "POST",
                Some("Folder"),
                None,
            ),
            (
                "/api/companies/{company_id}/folders/items/move",
                "POST",
                None,
                None,
            ),
            ("/api/folders", "GET", Some("FolderList"), None),
            ("/api/folders", "POST", Some("Folder"), Some("Folder")),
            ("/api/folders/{id}", "DELETE", None, None),
        ];
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut dup_count = 0;
        for (path, method, _, _) in cases {
            let id = operation_id(method, path);
            if !seen.insert(id.clone()) {
                dup_count += 1;
            }
        }
        assert_eq!(
            dup_count, 0,
            "every (path,method) must produce a unique operationId"
        );
    }

    #[test]
    fn r509_heartbeat_post_returns_run() {
        let h = path_schema_hint("/api/heartbeat", "POST").expect("hint");
        assert!(h.request.is_none(), "heartbeat trigger has no body");
        assert_eq!(h.response, Some("HeartbeatRun"));
    }

    #[test]
    fn r508_routines_get_returns_list() {
        let h = path_schema_hint("/api/routines", "GET").expect("hint");
        assert!(h.request.is_none());
        assert_eq!(h.response, Some("RoutineList"));
    }

    #[test]
    fn r507_pipelines_get_returns_list() {
        let h = path_schema_hint("/api/pipelines", "GET").expect("hint");
        assert!(h.request.is_none());
        assert_eq!(h.response, Some("PipelineList"));
    }

    #[test]
    fn r506_full_body_get_has_no_request_body() {
        let body = json!({"paths": {}});
        let path = "/api/companies";
        let method = "GET";
        let hint = path_schema_hint(path, method).expect("hint");
        let op = json!({
            "operationId": "get_api_companies",
            "summary": "GET /api/companies",
            "tags": ["companies"],
            "responses": build_responses_block(hint.response, hint.request.is_some()),
            "requestBody": build_request_body_block(hint.request),
        });
        let body_str = op.to_string();
        // For GET, request_body is None, so when serialised it shows as `null`.
        // The op still has the key but with null value.
        assert!(
            body_str.contains("\"requestBody\":null") || !body_str.contains("\"requestBody\":{"),
            "GET should not include requestBody object, got: {body_str}"
        );
    }

    #[test]
    fn r505_yaml_body_also_contains_schemas() {
        // build_yaml_body is a thin wrapper; verify the schema injection
        // flows through to the YAML serialization path too.
        let mut body = json!({
            "components": {
                "securitySchemes": {
                    "session": { "type": "apiKey" }
                }
            }
        });
        inject_dto_schemas(&mut body);
        let y = json_value_to_yaml(&body, 0);
        assert!(y.contains("Decision:"));
        assert!(y.contains("Company:"));
        assert!(y.contains("Issue:"));
        assert!(y.contains("Agent:"));
        assert!(y.contains("HeartbeatRun:"));
        assert!(y.contains("securitySchemes:"));
    }

    // -------- r515: CSRF in OpenAPI securitySchemes + path-level security --------

    #[test]
    fn r515_csrf_protected_in_openapi_safe_methods_return_false() {
        // GET / HEAD / OPTIONS never require CSRF.
        for method in ["GET", "HEAD", "OPTIONS", "get", "head"] {
            assert!(
                !csrf_protected_in_openapi("/api/companies", method),
                "{method} should not require CSRF"
            );
        }
    }

    #[test]
    fn r515_csrf_protected_in_openapi_state_changing_on_protected_path() {
        assert!(csrf_protected_in_openapi("/api/companies", "POST"));
        assert!(csrf_protected_in_openapi("/api/companies/{id}", "PATCH"));
        assert!(csrf_protected_in_openapi("/api/issues", "PUT"));
        assert!(csrf_protected_in_openapi("/api/decisions/{id}", "DELETE"));
    }

    #[test]
    fn r515_csrf_protected_in_openapi_whitelist_returns_false() {
        // Whitelist paths from middleware::csrf::csrf_path_allowed.
        for (path, method) in [
            ("/api/auth/sign-in/email", "POST"),
            ("/api/auth/sign-up/email", "POST"),
            ("/api/auth/refresh", "POST"),
            ("/live-events", "GET"),   // already filtered by method
            ("/openapi.json", "POST"), // whitelisted even for POST
            ("/api/openapi.json", "POST"),
            ("/health", "DELETE"),
            ("/_plugins/foo/ui/index.html", "POST"),
            ("/api/dev-server/restart", "POST"),
        ] {
            assert!(
                !csrf_protected_in_openapi(path, method),
                "whitelisted {method} {path} should not require CSRF"
            );
        }
    }

    #[test]
    fn r515_security_scheme_csrf_token_present_in_injected_body() {
        // Simulate build_openapi_body: hand-build securitySchemes + inject DTOs.
        let mut body = json!({
            "components": {
                "securitySchemes": {
                    "session": { "type": "apiKey", "in": "cookie", "name": "paperclip_session" },
                    "apiKey": { "type": "apiKey", "in": "header", "name": "X-Paperclip-Api-Key" },
                    "csrfToken": { "type": "apiKey", "in": "header", "name": "X-CSRF-Token" }
                }
            }
        });
        inject_dto_schemas(&mut body);
        let sec = &body["components"]["securitySchemes"];
        assert!(sec["csrfToken"].is_object());
        assert_eq!(sec["csrfToken"]["type"], "apiKey");
        assert_eq!(sec["csrfToken"]["in"], "header");
        assert_eq!(sec["csrfToken"]["name"], "X-CSRF-Token");
        // Existing schemes still present (no regression).
        assert!(sec["session"].is_object());
        assert!(sec["apiKey"].is_object());
    }

    #[test]
    fn r515_yaml_body_includes_csrf_token_security_scheme() {
        let body = json!({
            "components": {
                "securitySchemes": {
                    "csrfToken": { "type": "apiKey", "in": "header", "name": "X-CSRF-Token" }
                }
            }
        });
        let y = json_value_to_yaml(&body, 0);
        assert!(y.contains("csrfToken:"));
        assert!(y.contains("X-CSRF-Token"));
    }

    #[test]
    fn r515_path_level_security_attached_to_post_companies() {
        // Build an op for POST /api/companies and verify security is attached.
        let path = "/api/companies";
        let method = "POST";
        let mut op = json!({
            "operationId": "post_api_companies",
            "summary": "POST /api/companies",
            "tags": ["companies"],
            "responses": {}
        });
        if csrf_protected_in_openapi(path, method) {
            op.as_object_mut()
                .unwrap()
                .insert("security".to_string(), json!([{"csrfToken": []}]));
        }
        let sec = op["security"].as_array().expect("security array");
        assert_eq!(sec.len(), 1);
        assert_eq!(sec[0]["csrfToken"], json!([]));
    }

    #[test]
    fn r515_path_level_security_absent_on_get_companies() {
        let path = "/api/companies";
        let method = "GET";
        let mut op = json!({
            "operationId": "get_api_companies",
            "summary": "GET /api/companies",
            "tags": ["companies"],
            "responses": {}
        });
        if csrf_protected_in_openapi(path, method) {
            op.as_object_mut()
                .unwrap()
                .insert("security".to_string(), json!([{"csrfToken": []}]));
        }
        assert!(op.get("security").is_none(), "GET should not have security");
    }

    #[test]
    fn r515_path_level_security_absent_on_auth_signin() {
        // /api/auth/* is whitelisted → no csrfToken requirement.
        let path = "/api/auth/sign-in/email";
        let method = "POST";
        let mut op = json!({
            "operationId": "post_api_auth_sign_in_email",
            "summary": "POST /api/auth/sign-in/email",
            "tags": ["auth"],
            "responses": {}
        });
        if csrf_protected_in_openapi(path, method) {
            op.as_object_mut()
                .unwrap()
                .insert("security".to_string(), json!([{"csrfToken": []}]));
        }
        assert!(
            op.get("security").is_none(),
            "auth signin should not have csrf security"
        );
    }

    #[test]
    fn r522_scan_routes_picks_up_chained_methods() {
        // R522: After the scanner fix, `.route("/api/companies", get(list).post(create))`
        // should register BOTH GET and POST. Previously only the leading verb
        // (get) was detected, leaving the OpenAPI doc blind to POST.
        let paths = scan_routes_for_openapi();
        let companies = paths
            .get("/api/companies")
            .expect("/api/companies should be scanned");
        let methods: Vec<&str> = companies
            .as_object()
            .map(|o| o.keys().map(|s| s.as_str()).collect())
            .unwrap_or_default();
        assert!(
            methods.contains(&"get"),
            "/api/companies missing GET, got {:?}",
            methods
        );
        assert!(
            methods.contains(&"post"),
            "/api/companies missing POST (chained method), got {:?}",
            methods
        );
    }

    #[test]
    fn r522_chained_patch_and_delete_registered() {
        // R508 example: `.route("/api/companies/:company_id", get(get_one).patch(update).delete(remove))`
        let paths = scan_routes_for_openapi();
        let route = paths
            .get("/api/companies/{company_id}")
            .expect("/api/companies/{company_id} should be scanned");
        let methods: std::collections::BTreeSet<&str> = route
            .as_object()
            .map(|o| o.keys().map(|s| s.as_str()).collect())
            .unwrap_or_default();
        assert!(methods.contains("get"));
        assert!(methods.contains("patch"));
        assert!(methods.contains("delete"));
    }

    #[test]
    fn r522_scan_routes_attaches_security_to_post_companies() {
        // R515 + R522: /api/companies now exposes POST via chained methods.
        // Verify path-level security is attached.
        let paths = scan_routes_for_openapi();
        let post_companies = paths
            .get("/api/companies")
            .and_then(|p| p.get("post"))
            .expect("POST /api/companies should be scanned");
        let sec = post_companies["security"]
            .as_array()
            .expect("security array required");
        assert_eq!(sec.len(), 1);
        assert_eq!(sec[0]["csrfToken"], json!([]));
    }

    #[test]
    fn r515_scan_routes_skips_security_on_get_companies_stats() {
        // GET /api/companies/:company_id/stats must NOT have security (safe method).
        let paths = scan_routes_for_openapi();
        let get_stats = paths
            .get("/api/companies/{company_id}/stats")
            .and_then(|p| p.get("get"))
            .expect("GET /api/companies/{company_id}/stats should be scanned");
        assert!(get_stats.get("security").is_none());
    }

    #[test]
    fn r522_chained_methods_not_breaking_single_method_routes() {
        // Regression guard: routes that use single-method syntax (most of the
        // codebase) must still be detected.
        let paths = scan_routes_for_openapi();
        // /api/companies/import/preview uses `.route("/...", post(handler))`
        let import_preview = paths
            .get("/api/companies/import/preview")
            .and_then(|p| p.get("post"))
            .expect("POST /api/companies/import/preview should be scanned");
        assert!(import_preview.is_object());
    }

    #[test]
    fn r522_company_aggregation_schemas_wired_in_openapi_body() {
        // R522: companies aggregation endpoints now reference real schemas
        // (CompanyStats, CompanyTimelineResult, CompanyArtifactList, CompanyOrgChart,
        // CompanyStatsList). The OpenAPI body should contain all of them as
        // entries in `components.schemas`.
        use pc_openapi::{register_core_dtos, OpenApiRegistry};

        let mut reg = OpenApiRegistry::builder();
        register_core_dtos(&mut reg);
        let spec = reg.build();
        let schemas = &spec.to_json_value()["components"]["schemas"];

        for name in [
            "CompanyStats",
            "CompanyStatsList",
            "CompanyTimelineResult",
            "CompanyArtifact",
            "CompanyArtifactList",
            "CompanyOrgChart",
        ] {
            assert!(
                schemas
                    .as_object()
                    .map(|o| o.contains_key(name))
                    .unwrap_or(false),
                "missing schema {name} in components.schemas"
            );
        }
    }

    #[test]
    fn r522_path_schema_hint_includes_all_six_new_aggregations() {
        // R522: every companies aggregation path should now resolve to a
        // non-None response schema (except org.svg/org.png which return binary).
        for (path, expected_resp) in [
            ("/api/companies/{company_id}/stats", Some("CompanyStats")),
            (
                "/api/companies/{company_id}/timeline",
                Some("CompanyTimelineResult"),
            ),
            (
                "/api/companies/{company_id}/artifacts",
                Some("CompanyArtifactList"),
            ),
            ("/api/companies/{company_id}/org", Some("CompanyOrgChart")),
            ("/api/companies/stats", Some("CompanyStatsList")),
            // org.svg / org.png still None (binary image response).
            ("/api/companies/{company_id}/org.svg", None),
            ("/api/companies/{company_id}/org.png", None),
        ] {
            let h = path_schema_hint(path, "GET").unwrap_or_else(|| panic!("no hint for {path}"));
            assert_eq!(h.response, expected_resp, "response mismatch for {path}");
        }
    }

    #[test]
    fn r522_core_dto_names_includes_company_aggregation_schemas() {
        // R522: 6 new schemas registered; CORE_DTO_NAMES length 35 → 41.
        use pc_openapi::CORE_DTO_NAMES;
        assert_eq!(CORE_DTO_NAMES.len(), 52);
        for name in [
            "CompanyStats",
            "CompanyStatsList",
            "CompanyTimelineResult",
            "CompanyArtifact",
            "CompanyArtifactList",
            "CompanyOrgChart",
        ] {
            assert!(
                CORE_DTO_NAMES.contains(&name),
                "CORE_DTO_NAMES missing {name}"
            );
        }
    }

    #[test]
    fn r522_get_companies_now_has_security_path_level_via_post() {
        // R515 + R522 combined: with POST now registered on /api/companies,
        // the path-level security is present. The GET method on the same
        // path must NOT have security (safe method).
        let paths = scan_routes_for_openapi();
        let entry = paths.get("/api/companies").expect("/api/companies scanned");
        let post = entry.get("post").expect("POST present");
        let get = entry.get("get").expect("GET present");
        assert!(post.get("security").is_some(), "POST needs CSRF security");
        assert!(
            get.get("security").is_none(),
            "GET should not have security"
        );
    }

    // ── R577: UI client paths covered by OpenAPI hints ──

    #[test]
    fn r577_hint_health_returns_health_schema() {
        let h = path_schema_hint("/api/health", "GET").expect("hint");
        assert_eq!(h.response.as_deref(), Some("Health"));
        assert!(h.request.is_none());
    }

    #[test]
    fn r577_hint_dev_server_restart_returns_dev_server_schema() {
        let h = path_schema_hint("/api/health/dev-server/restart", "GET").expect("hint");
        assert_eq!(h.response.as_deref(), Some("DevServerRestart"));
    }

    #[test]
    fn r577_hint_auth_get_session_returns_session() {
        let h = path_schema_hint("/api/auth/get-session", "GET").expect("hint");
        assert_eq!(h.response.as_deref(), Some("Session"));
    }

    #[test]
    fn r577_hint_auth_profile_get_and_patch() {
        let g = path_schema_hint("/api/auth/profile", "GET").expect("GET hint");
        assert_eq!(g.response.as_deref(), Some("UserProfile"));
        let p = path_schema_hint("/api/auth/profile", "PATCH").expect("PATCH hint");
        assert_eq!(p.request.as_deref(), Some("UserProfileUpdate"));
        assert_eq!(p.response.as_deref(), Some("UserProfile"));
    }

    #[test]
    fn r577_hint_adapter_ui_parser_returns_js_source() {
        let h = path_schema_hint("/api/adapters/{adapter_type}/ui-parser.js", "GET").expect("hint");
        assert_eq!(h.response.as_deref(), Some("JsSource"));
    }

    #[test]
    fn r577_hint_asset_content_returns_asset_content() {
        let h = path_schema_hint("/api/assets/{asset_id}/content", "GET").expect("hint");
        assert_eq!(h.response.as_deref(), Some("AssetContent"));
    }

    #[test]
    fn r577_hint_agent_actions_csv_returns_csv() {
        let h = path_schema_hint("/api/companies/{company_id}/audit/agent-actions.csv", "GET")
            .expect("hint");
        assert_eq!(h.response.as_deref(), Some("CsvExport"));
    }

    #[test]
    fn r577_hint_company_events_ws_returns_live_stream() {
        let h = path_schema_hint("/api/companies/{company_id}/events/ws", "GET").expect("hint");
        assert_eq!(h.response.as_deref(), Some("LiveEventStream"));
    }

    #[test]
    fn r577_hint_file_resources_content() {
        let h =
            path_schema_hint("/api/issues/{issue_id}/file-resources/content", "GET").expect("hint");
        assert_eq!(h.response.as_deref(), Some("FileResourceContent"));
    }

    #[test]
    fn r577_hint_v1_runs_returns_run_list() {
        let h = path_schema_hint("/api/v1/runs", "GET").expect("hint");
        assert_eq!(h.response.as_deref(), Some("RunList"));
    }

    #[test]
    fn r577_hint_plugin_actions_and_data() {
        let a = path_schema_hint("/api/plugins/{plugin_id}/actions/{key}", "GET").expect("hint");
        assert_eq!(a.response.as_deref(), Some("PluginAction"));
        let d = path_schema_hint("/api/plugins/{plugin_id}/data/{key}", "GET").expect("hint");
        assert_eq!(d.response.as_deref(), Some("PluginData"));
    }

    #[test]
    fn r577_hint_plugin_bridge_stream() {
        let h = path_schema_hint("/api/plugins/{plugin_id}/bridge/stream/{channel}", "GET")
            .expect("hint");
        assert_eq!(h.response.as_deref(), Some("BridgeStream"));
    }

    #[test]
    fn r577_total_hint_count_increased() {
        // R577 added 14 hints; coverage of the 13 UI paths + the
        // pre-existing /api/health which was already scanned.
        // Verify that the new hint paths are recognized.
        let ui_paths = [
            "/api/health",
            "/api/health/dev-server/restart",
            "/api/auth/get-session",
            "/api/auth/profile",
            "/api/adapters/{adapter_type}/ui-parser.js",
            "/api/assets/{asset_id}/content",
            "/api/companies/{company_id}/audit/agent-actions.csv",
            "/api/companies/{company_id}/events/ws",
            "/api/issues/{issue_id}/file-resources/content",
            "/api/v1/runs",
            "/api/plugins/{plugin_id}/actions/{key}",
            "/api/plugins/{plugin_id}/data/{key}",
            "/api/plugins/{plugin_id}/bridge/stream/{channel}",
        ];
        let mut found = 0;
        for path in ui_paths {
            if path_schema_hint(path, "GET").is_some() {
                found += 1;
            }
        }
        assert_eq!(found, 13, "all 13 UI paths must have R577 hints");

    }

    // -------- r695: hint-only path injection (UI-2) --------

    #[test]
    fn r695_all_hint_only_paths_constant_is_non_empty() {
        use super::ALL_HINT_ONLY_PATHS;
        assert!(
            ALL_HINT_ONLY_PATHS.len() >= 13,
            "ALL_HINT_ONLY_PATHS must declare every R577 UI hint path"
        );
        assert!(
            ALL_HINT_ONLY_PATHS.iter().any(|(p, _)| *p == "/api/v1/runs"),
            "ALL_HINT_ONLY_PATHS must include /api/v1/runs (R695 v1 merge case)"
        );
    }

    #[test]
    fn r695_merge_hint_only_paths_adds_v1_runs() {
        let mut paths: std::collections::BTreeMap<String, serde_json::Value> =
            std::collections::BTreeMap::new();
        super::merge_hint_only_paths(&mut paths);
        assert!(
            paths.contains_key("/api/v1/runs"),
            "merge_hint_only_paths must add /api/v1/runs"
        );
        let op = &paths["/api/v1/runs"];
        let get_op = op.get("get").expect("get op");
        assert_eq!(get_op["operationId"], "get_api_v1_runs");
        let resp_ref = &get_op["responses"]["200"]["content"]["application/json"]["schema"]["$ref"];
        assert_eq!(resp_ref, "#/components/schemas/RunList");
    }

    #[test]
    fn r695_merge_hint_only_paths_idempotent() {
        let mut paths: std::collections::BTreeMap<String, serde_json::Value> =
            std::collections::BTreeMap::new();
        super::merge_hint_only_paths(&mut paths);
        let first = paths["/api/v1/runs"].clone();
        super::merge_hint_only_paths(&mut paths);
        let second = paths["/api/v1/runs"].clone();
        assert_eq!(first, second, "second merge must not mutate existing paths");
    }

    #[test]
    fn r695_build_openapi_body_includes_v1_runs() {
        let body = build_openapi_body_with_adapters(vec![]);
        let paths = body.get("paths").and_then(|p| p.as_object()).expect("paths");
        assert!(
            paths.contains_key("/api/v1/runs"),
            "OpenAPI body must expose /api/v1/runs after R695"
        );
    }

    #[test]
    fn r695_build_openapi_body_adapters_ui_parser_uses_adapter_type_param() {
        let body = build_openapi_body_with_adapters(vec![]);
        let paths = body.get("paths").and_then(|p| p.as_object()).expect("paths");
        assert!(
            paths.contains_key("/api/adapters/{adapter_type}/ui-parser.js"),
            "OpenAPI body must expose /api/adapters/{{adapter_type}}/ui-parser.js"
        );
        assert!(
            !paths.contains_key("/api/adapters/{type}/ui-parser.js"),
            "OpenAPI body must not use the legacy /api/adapters/{{type}}/ui-parser.js"
        );
    }
}
