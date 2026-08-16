//! workspace-runtime HTTP routes (R665).
//!
//! 与 Node `services/workspace-runtime.ts` + `services/workspace-runtime-read-model.ts` 对齐：
//! - `/api/workspace-runtime/readiness-timeout` — 计算 readiness 探测超时（pure function）
//! - `/api/workspace-runtime/is-dev-service`    — 判断是否为 paperclip-dev 服务
//! - `/api/workspace-runtime/realization/parse` — 反序列化 realization 请求 JSON
//! - `/api/workspace-runtime/realization/build` — 从已知结构构建 realization 请求（dry-run）
//!
//! 这些端点让 UI / external clients 调用 pc-core 的 pure-function helpers，
//! 避免 Node 端对应函数缺失导致的 backend 一致性漂移。

use axum::{
    extract::State,
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/workspace-runtime/readiness-timeout",
            post(readiness_timeout),
        )
        .route(
            "/api/workspace-runtime/is-dev-service",
            post(is_dev_service),
        )
        .route(
            "/api/workspace-runtime/realization/parse",
            post(realization_parse),
        )
        .route(
            "/api/workspace-runtime/realization/build",
            post(realization_build),
        )
        // 健康检查端点（无 auth 阻塞，便于运维）
        .route("/api/workspace-runtime/health", get(health))
}

// ============================================================================
// readiness timeout
// ============================================================================

#[derive(Debug, Deserialize)]
struct ReadinessTimeoutInput {
    /// service 配置（任意 JSON object），与 Node `service` 入参对齐
    service: HashMap<String, Value>,
}

#[derive(Debug, Serialize)]
struct ReadinessTimeoutOutput {
    /// 计算出的 readiness 探测超时（秒）
    timeout_sec: u64,
    /// 是否命中 dev-server 启发式（true → 90s，false → 30s 或显式配置）
    dev_server_heuristic: bool,
}

async fn readiness_timeout(
    State(_state): State<AppState>,
    Json(body): Json<ReadinessTimeoutInput>,
) -> (StatusCode, Json<ReadinessTimeoutOutput>) {
    let timeout_sec =
        pc_core::workspace_runtime_readiness::resolve_workspace_runtime_readiness_timeout_sec(
            &body.service,
        );
    let explicit = body
        .service
        .get("readiness")
        .and_then(|v| v.get("timeoutSec"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let command = body
        .service
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let heuristic = explicit == 0
        && pc_core::workspace_runtime_readiness::looks_like_workspace_dev_server_command(&command);
    (
        StatusCode::OK,
        Json(ReadinessTimeoutOutput {
            timeout_sec,
            dev_server_heuristic: heuristic,
        }),
    )
}

// ============================================================================
// is-dev-service
// ============================================================================

#[derive(Debug, Deserialize)]
struct IsDevServiceInput {
    service_name: Option<String>,
    command: Option<String>,
}

#[derive(Debug, Serialize)]
struct IsDevServiceOutput {
    is_dev: bool,
    reason: String,
}

async fn is_dev_service(
    State(_state): State<AppState>,
    Json(body): Json<IsDevServiceInput>,
) -> (StatusCode, Json<IsDevServiceOutput>) {
    let is_dev = pc_core::workspace_runtime_readiness::is_paperclip_dev_runtime_service(
        body.service_name.as_deref(),
        body.command.as_deref(),
    );
    let reason = if is_dev {
        "name_or_command_matches_paperclip_dev".to_string()
    } else {
        "no_match".to_string()
    };
    (StatusCode::OK, Json(IsDevServiceOutput { is_dev, reason }))
}

// ============================================================================
// realization parse
// ============================================================================

#[derive(Debug, Deserialize)]
struct RealizationParseInput {
    /// 任意 JSON（Version=1 解析规则严格匹配）
    value: Value,
}

#[derive(Debug, Serialize)]
struct RealizationParseOutput {
    /// 解析结果（None 表示 version 不匹配或关键字段缺失）
    parsed: Option<Value>,
    version_matched: bool,
}

async fn realization_parse(
    State(_state): State<AppState>,
    Json(body): Json<RealizationParseInput>,
) -> (StatusCode, Json<RealizationParseOutput>) {
    let version_matched = body
        .value
        .get("version")
        .and_then(|v| v.as_i64())
        == Some(1);
    let parsed = pc_core::workspace_realization::read_workspace_realization_request(Some(&body.value))
        .map(|req| serde_json::to_value(req).unwrap_or(Value::Null));
    (
        StatusCode::OK,
        Json(RealizationParseOutput {
            parsed,
            version_matched,
        }),
    )
}

// ============================================================================
// realization build (dry-run; 不需要 DB)
// ============================================================================

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct RealizationBuildInput {
    company_id: String,
    environment_id: String,
    heartbeat_run_id: String,
    adapter_type: String,
    /// Source 信息
    source_kind: String,
    source_strategy: String,
    source_local_path: String,
    source_project_id: Option<String>,
    source_project_workspace_id: Option<String>,
    source_repo_url: Option<String>,
    source_repo_ref: Option<String>,
    source_branch_name: Option<String>,
    source_worktree_path: Option<String>,
    /// Runtime overlay
    provision_command: Option<String>,
    teardown_command: Option<String>,
    cleanup_command: Option<String>,
    /// Optional workspaceRuntime overlay
    workspace_runtime_overlay: Option<Value>,
}

#[derive(Debug, Serialize)]
struct RealizationBuildOutput {
    request: Value,
    version: i64,
}

async fn realization_build(
    State(_state): State<AppState>,
    Json(body): Json<RealizationBuildInput>,
) -> (StatusCode, Json<RealizationBuildOutput>) {
    // 直接构造 WorkspaceRealizationRequest JSON，与 Node 1:1 对齐。
    let mut source_obj = json!({
        "kind": body.source_kind,
        "strategy": body.source_strategy,
        "localPath": body.source_local_path,
    });
    if let Some(p) = &body.source_project_id {
        source_obj["projectId"] = json!(p);
    }
    if let Some(p) = &body.source_project_workspace_id {
        source_obj["projectWorkspaceId"] = json!(p);
    }
    if let Some(p) = &body.source_repo_url {
        source_obj["repoUrl"] = json!(p);
    }
    if let Some(p) = &body.source_repo_ref {
        source_obj["repoRef"] = json!(p);
    }
    if let Some(p) = &body.source_branch_name {
        source_obj["branchName"] = json!(p);
    }
    if let Some(p) = &body.source_worktree_path {
        source_obj["worktreePath"] = json!(p);
    }

    let mut overlay_obj = json!({});
    if let Some(c) = &body.provision_command {
        overlay_obj["provisionCommand"] = json!(c);
    }
    if let Some(c) = &body.teardown_command {
        overlay_obj["teardownCommand"] = json!(c);
    }
    if let Some(c) = &body.cleanup_command {
        overlay_obj["cleanupCommand"] = json!(c);
    }
    if let Some(wsr) = &body.workspace_runtime_overlay {
        overlay_obj["workspaceRuntime"] = wsr.clone();
    }

    let request = json!({
        "version": 1,
        "adapterType": body.adapter_type,
        "companyId": body.company_id,
        "environmentId": body.environment_id,
        "heartbeatRunId": body.heartbeat_run_id,
        "source": source_obj,
        "runtimeOverlay": overlay_obj,
    });
    (
        StatusCode::OK,
        Json(RealizationBuildOutput {
            request,
            version: 1,
        }),
    )
}

// ============================================================================
// health (公开路径，由 require_board_layer 白名单豁免)
// ============================================================================

async fn health() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "subsystem": "workspace-runtime",
        "endpoints": [
            "/api/workspace-runtime/readiness-timeout",
            "/api/workspace-runtime/is-dev-service",
            "/api/workspace-runtime/realization/parse",
            "/api/workspace-runtime/realization/build",
        ],
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use crate::AppState;

    fn test_state() -> AppState {
        // Construct minimal AppState for unit tests. In pc-http, AppState::new()
        // requires DB; for pure helpers we only need a placeholder. We can use
        // `Default::default()` if available; otherwise skip DB-dependent tests
        // and rely on pc-core unit tests + integration tests for these helpers.
        unimplemented!("unit tests for these helpers live in pc-core + integration tests")
    }

    #[test]
    fn readiness_timeout_uses_explicit_value() {
        let mut svc = HashMap::new();
        svc.insert("command".to_string(), json!("npm run dev"));
        let mut readiness = serde_json::Map::new();
        readiness.insert("timeoutSec".to_string(), json!(120));
        svc.insert("readiness".to_string(), json!(readiness));
        let out = pc_core::workspace_runtime_readiness::resolve_workspace_runtime_readiness_timeout_sec(&svc);
        assert_eq!(out, 120);
    }

    #[test]
    fn readiness_timeout_dev_server_heuristic_90s() {
        let mut svc = HashMap::new();
        svc.insert("command".to_string(), json!("npm run dev"));
        let out = pc_core::workspace_runtime_readiness::resolve_workspace_runtime_readiness_timeout_sec(&svc);
        assert_eq!(out, 90);
    }

    #[test]
    fn is_dev_service_matches_paperclip_dev() {
        assert!(pc_core::workspace_runtime_readiness::is_paperclip_dev_runtime_service(
            Some("paperclip-dev"), Some("node server")
        ));
        assert!(pc_core::workspace_runtime_readiness::is_paperclip_dev_runtime_service(
            Some("paperclip-dev-once"), None
        ));
        assert!(!pc_core::workspace_runtime_readiness::is_paperclip_dev_runtime_service(
            Some("postgres"), Some("postgres")
        ));
    }

    #[test]
    fn realization_parse_rejects_wrong_version() {
        let v = json!({"version": 2});
        assert!(pc_core::workspace_realization::read_workspace_realization_request(Some(&v)).is_none());
    }

    #[test]
    fn realization_parse_accepts_v1() {
        let v = json!({
            "version": 1,
            "adapterType": "test",
            "companyId": "00000000-0000-0000-0000-000000000001",
            "environmentId": "00000000-0000-0000-0000-000000000002",
            "heartbeatRunId": "00000000-0000-0000-0000-000000000003",
            "source": {
                "kind": "task_session",
                "strategy": "git_worktree",
                "localPath": "/tmp/test",
            },
            "runtimeOverlay": {},
        });
        assert!(pc_core::workspace_realization::read_workspace_realization_request(Some(&v)).is_some());
    }
}
