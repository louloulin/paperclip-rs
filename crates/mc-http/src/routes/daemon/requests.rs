//! `*/result` 上报面（M3-7 / LUM-1438）—— 闭环的**第二段**。
//!
//! 覆盖 4 条 daemon 面路由（`docs/16` §6.1 表 A 第 16–19 行）：
//!
//! | 路由 | upstream |
//! |---|---|
//! | `POST …/runtimes/:runtimeId/update/:updateId/result` | `ReportUpdateResult` `runtime_update.go:292` |
//! | `POST …/runtimes/:runtimeId/models/:requestId/result` | `ReportModelListResult` `runtime_models.go:482` |
//! | `POST …/runtimes/:runtimeId/local-skills/:requestId/result` | `ReportLocalSkillListResult` `runtime_local_skills.go:721` |
//! | `POST …/runtimes/:runtimeId/local-skills/import/:requestId/result` | `ReportLocalSkillImportResult` `runtime_local_skills.go:785` |
//!
//! 用户面 `POST Initiate*` 与 `GET Get*Request` 在 `routes::runtimes::async_requests`
//! （本切片新增）；三段共享同一个 [`crate::daemon_requests::RequestStore`]。
//!
//! ## 四条 handler 共有的骨架
//!
//! 1. `G_rt`（`requireDaemonRuntimeAccess`：载 runtime → workspace 门，行不存在即 404）；
//! 2. **先取已有请求再解码 body** —— 顺序是契约的一部分：请求已在终态时上报被
//!    静默忽略并回 200 `{"status":"ok"}`，此时一个畸形 body 也不该报 400；
//! 3. 请求 id 不存在、或属于**别的 runtime** ⇒ 404（后者是跨 runtime 越权探测面）；
//! 4. `completed` → `Complete`，其余（`failed`/`timeout`…）→ `Fail`；`running` 是
//!    daemon 的「我收到了」进度信号，**no-op**；其它值 → 400 `invalid status: X`；
//! 5. 台账写失败一律 5xx（`failed to persist completion` / `failed to persist failure`
//!    / `failed to persist import completion`），让 daemon 重试而不是把上报吞掉。
//!
//! ## 本地导入（`/local-skills/import/:requestId/result`）
//!
//! 这条是四条里唯一**有副作用**的：它把 daemon 从宿主读出的本地技能落成 workspace
//! skill。`create` 与 `overwrite` 两条路径的权限、名字守卫、冲突语义都按上游复刻：
//!
//! - `action == "overwrite"`：调 [`DaemonRepo::overwrite_skill_with_files`]，
//!   存在性 / creator / 名字**在同一个事务里重新验**（用户确认与上报之间的漂移干净失败，
//!   绝不回落 create）；
//! - `action == ""`（create）：先按 `(workspace, name)` 探测同名。命中即终态 ——
//!   发起时选了 `supports_conflict` 的客户端拿到 `conflict` 终态 + 结构化信息
//!   （`conflict` **不是错误**，`error` 保持为空），老客户端仍拿 `failed` 与老文案。
//!
//! 上报体里的 `files` 先过 [`skills::validate_file_path`]（绝对路径与 `..` 逃逸直接丢弃，
//! 与上游 `validateFilePath` 同款），再写库；`config.origin` 信封同样复用 [`skills::local_import_config`]。
//!
//! 本切片**不**做的部分（逐条登记在 `docs/32` 的偏离表）：`skill:created` /
//! `skill:updated` 事件发布（本地无事件总线，daemon 侧靠 `GET …/import/{id}` 拿终态）；
//! create 路径在 `Complete` 失败后「回滚已写入的 skill」（本地台账是内存的、
//! `Complete` 不会失败，保留该分支只会是不可达代码）。
//!
//! 上游的 `update` 结果上报里那个 `fallback` 字段（模型目录是「发现失败后的静态
//! 替身」）只用于服务端目录缓存，本地无缓存 ⇒ 读入即忽略。

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::Json;
use mc_core::Id;
use mc_repos::daemon::{DaemonRepo, OverwriteOutcome, SkillRow, SkillWithFilesRow};
use mc_repos::RepoError;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use super::dto::{decode_body, sanitize};
use super::scope::{
    internal, not_found, parse_path_id, require_runtime_access, validation, DaemonAuth,
};
use super::skills::{local_import_config, validate_file_path};
use crate::daemon_requests::{LocalSkillImportAction, PendingRequest, RequestStore};
use crate::error::ApiResult;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// 公共前置
// ---------------------------------------------------------------------------

/// `{"status":"ok"}` —— 四条 handler 成功/幂等时唯一的响应体。
fn ok_body() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

/// 上报前置：`G_rt` → 取请求 → 归属校验 → 终态判定。
///
/// 返回 `Ok(None)` 表示**该请求已是终态**：调用方直接回 200 `{"status":"ok"}`，
/// 连 body 都不解码（见模块文档第 2 条）。
async fn open_request(
    state: &AppState,
    auth: &DaemonAuth,
    raw_runtime_id: &str,
    raw_request_id: &str,
    not_found_msg: &str,
) -> ApiResult<Option<PendingRequest>> {
    let runtime = require_runtime_access(state, auth, raw_runtime_id, "runtime not found").await?;
    // 上游直接拿请求里的字符串去查台账；「不是 uuid」与「查不到」是同一个 404。
    let Ok(request_id) = parse_path_id("request_id", raw_request_id) else {
        return Err(not_found(not_found_msg));
    };
    let Some(request) = state.daemon_requests.get(request_id) else {
        return Err(not_found(not_found_msg));
    };
    if request.runtime_id != runtime.id {
        return Err(not_found(not_found_msg));
    }
    if !request.is_open() {
        return Ok(None);
    }
    Ok(Some(request))
}

/// 「标记失败」这条收尾路径：写失败状态，成功即 200 `{"status":"ok"}`。
///
/// 台账写失败必须 500（文案由调用方给，指名失败面），否则 daemon 以为上报落地了。
fn fail_and_ok(
    store: &RequestStore,
    id: Id,
    error: impl Into<String>,
    persist_error: &'static str,
) -> ApiResult<Json<Value>> {
    store
        .fail(id, error.into())
        .map_err(|_| internal(persist_error))?;
    Ok(ok_body())
}

/// 上报体的 `status` 是否表示「已完成」。
fn is_completed(status: &str) -> bool {
    status == "completed"
}

// ---------------------------------------------------------------------------
// 1. update
// ---------------------------------------------------------------------------

/// upstream `ReportUpdateResult` 的请求体（`runtime_update.go:317`）。
#[derive(Debug, Clone, Default, Deserialize)]
struct ReportUpdateBody {
    #[serde(default)]
    status: String,
    #[serde(default)]
    output: String,
    #[serde(default)]
    error: String,
}

/// `POST /api/daemon/runtimes/:runtimeId/update/:updateId/result`。
pub(crate) async fn report_update_result(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Path((runtime_id, update_id)): Path<(String, String)>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let Some(request) =
        open_request(&state, &auth, &runtime_id, &update_id, "update not found").await?
    else {
        return Ok(ok_body());
    };
    let body: ReportUpdateBody = decode_body(&body)?;
    match body.status.as_str() {
        "completed" => {
            // `UpdateRequest.output` 带 `omitempty`：空串 ⇒ 线上根本没有 `output` 键。
            let result = if body.output.is_empty() {
                json!({})
            } else {
                json!({ "output": body.output })
            };
            state
                .daemon_requests
                .complete(request.id, result)
                .map_err(|_| internal("failed to persist completion"))?;
        }
        "failed" => {
            return fail_and_ok(
                &state.daemon_requests,
                request.id,
                body.error,
                "failed to persist failure",
            );
        }
        // 进度信号：`PopPending` 时服务端已把状态置成 `running`，这里只确认收到。
        "running" => {}
        other => return Err(validation(format!("invalid status: {other}"))),
    }
    Ok(ok_body())
}

// ---------------------------------------------------------------------------
// 2. model list
// ---------------------------------------------------------------------------

/// upstream `ReportModelListResult` 的请求体（`runtime_models.go:509`）。
#[derive(Debug, Clone, Default, Deserialize)]
struct ReportModelListBody {
    #[serde(default)]
    status: String,
    #[serde(default)]
    models: Vec<Value>,
    #[serde(default)]
    unavailable_models: Vec<Value>,
    #[serde(default)]
    supported: Option<bool>,
    #[serde(default)]
    error: String,
    /// 见模块文档末尾：本地无目录缓存，读入即忽略。
    #[serde(default)]
    #[allow(dead_code)]
    fallback: bool,
}

/// `POST /api/daemon/runtimes/:runtimeId/models/:requestId/result`。
pub(crate) async fn report_model_list_result(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Path((runtime_id, request_id)): Path<(String, String)>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let Some(request) =
        open_request(&state, &auth, &runtime_id, &request_id, "request not found").await?
    else {
        return Ok(ok_body());
    };
    let body: ReportModelListBody = decode_body(&body)?;
    if !is_completed(&body.status) {
        return fail_and_ok(
            &state.daemon_requests,
            request.id,
            body.error,
            "failed to persist failure",
        );
    }
    // 老 daemon 会漏 `supported`；缺省 true，别让 UI 以为「这个 runtime 不支持选模型」。
    let mut result = json!({ "supported": body.supported.unwrap_or(true) });
    let fields = result.as_object_mut().expect("json! 构造的对象");
    // `models` / `unavailable_models` 都带 `omitempty`：空数组不上线。
    if !body.models.is_empty() {
        fields.insert("models".into(), Value::Array(body.models));
    }
    if !body.unavailable_models.is_empty() {
        fields.insert(
            "unavailable_models".into(),
            Value::Array(body.unavailable_models),
        );
    }
    state
        .daemon_requests
        .complete(request.id, result)
        .map_err(|_| internal("failed to persist completion"))?;
    Ok(ok_body())
}

// ---------------------------------------------------------------------------
// 3. local skill list
// ---------------------------------------------------------------------------

/// upstream `ReportLocalSkillListResult` 的请求体（`runtime_local_skills.go:738`）。
#[derive(Debug, Clone, Default, Deserialize)]
struct ReportLocalSkillListBody {
    #[serde(default)]
    status: String,
    #[serde(default)]
    skills: Vec<Value>,
    #[serde(default)]
    supported: Option<bool>,
    #[serde(default)]
    mcp_servers: Vec<Value>,
    #[serde(default)]
    mcp_supported: Option<bool>,
    #[serde(default)]
    error: String,
}

/// `POST /api/daemon/runtimes/:runtimeId/local-skills/:requestId/result`。
pub(crate) async fn report_local_skill_list_result(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Path((runtime_id, request_id)): Path<(String, String)>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let Some(request) =
        open_request(&state, &auth, &runtime_id, &request_id, "request not found").await?
    else {
        return Ok(ok_body());
    };
    let body: ReportLocalSkillListBody = decode_body(&body)?;
    if !is_completed(&body.status) {
        return fail_and_ok(
            &state.daemon_requests,
            request.id,
            body.error,
            "failed to persist failure",
        );
    }
    // 两个「无条件 bool」的缺省值**不同**：`supported` 缺省 true（老 daemon 确实
    // 报了本地技能），`mcp_supported` 缺省 false（老 daemon 没扫过 MCP，不能假装支持）。
    let mut result = json!({
        "supported": body.supported.unwrap_or(true),
        "mcp_supported": body.mcp_supported.unwrap_or(false),
    });
    let fields = result.as_object_mut().expect("json! 构造的对象");
    if !body.skills.is_empty() {
        fields.insert("skills".into(), Value::Array(body.skills));
    }
    if !body.mcp_servers.is_empty() {
        fields.insert("mcp_servers".into(), Value::Array(body.mcp_servers));
    }
    state
        .daemon_requests
        .complete(request.id, result)
        .map_err(|_| internal("failed to persist completion"))?;
    Ok(ok_body())
}

// ---------------------------------------------------------------------------
// 4. local skill import
// ---------------------------------------------------------------------------

/// upstream `reportedRuntimeLocalSkill`（`runtime_local_skills.go:522`）。
#[derive(Debug, Clone, Default, Deserialize)]
struct ReportedLocalSkill {
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    content: String,
    /// 宿主上的原始路径。只作 `config.origin.source_path` 存档，**不**参与任何 IO。
    #[serde(default)]
    source_path: String,
    /// 发现来源（`claude` / `codex` …）。
    #[serde(default)]
    provider: String,
    #[serde(default)]
    files: Vec<ReportedSkillFile>,
}

/// upstream `CreateSkillFileRequest`（`skill.go:285`）。
#[derive(Debug, Clone, Default, Deserialize)]
struct ReportedSkillFile {
    #[serde(default)]
    path: String,
    #[serde(default)]
    content: String,
}

/// upstream `ReportLocalSkillImportResult` 的请求体（`runtime_local_skills.go:804`）。
#[derive(Debug, Clone, Default, Deserialize)]
struct ReportLocalSkillImportBody {
    #[serde(default)]
    status: String,
    #[serde(default)]
    skill: Option<ReportedLocalSkill>,
    #[serde(default)]
    error: String,
}

/// 本地导入报告里，服务端**自己**算出来的写入入参（与 daemon 报的无关于此）。
struct ImportInputs {
    /// runtime 所属 workspace。
    workspace_id: Id,
    /// 发起导入的用户（`initiator_user_id`；缺失即台账坏了）。
    creator: Id,
    /// 落库用的名字（用户面显式值优先）。
    name: String,
    /// 落库用的描述（用户面显式值优先）。
    description: String,
    /// `config.origin` 信封。
    config: Value,
    /// 已过 [`validate_file_path`] 的支持文件。
    files: Vec<(String, String)>,
}

/// `POST /api/daemon/runtimes/:runtimeId/local-skills/import/:requestId/result`。
pub(crate) async fn report_local_skill_import_result(
    State(state): State<Arc<AppState>>,
    auth: DaemonAuth,
    Path((runtime_id, request_id)): Path<(String, String)>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    let Some(request) =
        open_request(&state, &auth, &runtime_id, &request_id, "request not found").await?
    else {
        return Ok(ok_body());
    };
    let body: ReportLocalSkillImportBody = decode_body(&body)?;
    if !is_completed(&body.status) {
        return fail_and_ok(
            &state.daemon_requests,
            request.id,
            body.error,
            "failed to persist failure",
        );
    }
    let Some(reported) = body.skill else {
        return fail_and_ok(
            &state.daemon_requests,
            request.id,
            "daemon returned an empty skill bundle",
            "failed to persist failure",
        );
    };

    // 上游把 `creator_id` 反解 UUID：存进来的必须是合法 uuid，否则 500 —— 这是服务端
    // 自己的台账坏了（重试不会变好），不是调用方的参数问题。
    let Some(creator) = request.initiator_user_id else {
        let message = "stored local skill import creator_id is invalid";
        let _ = state.daemon_requests.fail(request.id, message);
        return Err(internal(message));
    };

    let inputs = ImportInputs {
        workspace_id: request.workspace_id,
        creator,
        name: request
            .name
            .clone()
            .unwrap_or_else(|| reported.name.clone()),
        description: request
            .description
            .clone()
            .unwrap_or_else(|| reported.description.clone()),
        config: local_import_config(&runtime_id, &reported.provider, &reported.source_path),
        files: reported
            .files
            .iter()
            .filter(|file| validate_file_path(&file.path))
            .map(|file| (sanitize(&file.path), sanitize(&file.content)))
            .collect(),
    };

    let repo = DaemonRepo::new(&state.db);
    if request.action.as_deref() == Some(LocalSkillImportAction::Overwrite.as_str()) {
        return overwrite_imported_skill(
            &state,
            request.id,
            &repo,
            &reported,
            inputs,
            request.target_skill_id,
        )
        .await;
    }
    create_imported_skill(
        &state,
        request.id,
        &repo,
        &reported,
        inputs,
        request.supports_conflict,
    )
    .await
}

/// overwrite 路径：目标的存在性 / creator / 名字由仓储在**同一个事务**里复查。
async fn overwrite_imported_skill(
    state: &AppState,
    request_id: Id,
    repo: &DaemonRepo,
    reported: &ReportedLocalSkill,
    inputs: ImportInputs,
    target_skill_id: Option<Id>,
) -> ApiResult<Json<Value>> {
    let Some(target_skill_id) = target_skill_id else {
        let message = "stored target_skill_id is invalid";
        let _ = state.daemon_requests.fail(request_id, message);
        return Err(internal(message));
    };
    let outcome = repo
        .overwrite_skill_with_files(
            inputs.workspace_id,
            target_skill_id,
            &sanitize(&inputs.name),
            Some(inputs.creator),
            &sanitize(&inputs.description),
            &sanitize(&reported.content),
            &inputs.config,
            &inputs.files,
        )
        .await;
    match outcome {
        Ok(OverwriteOutcome::Updated(row)) => {
            state
                .daemon_requests
                .complete(request_id, json!({ "skill": skill_with_files_wire(&row) }))
                .map_err(|_| internal("failed to persist import completion"))?;
            Ok(ok_body())
        }
        // 覆写路径的四种失败都是**终态失败**（不是 conflict）：用户在确认框里点的就是
        // 「覆盖这一行」，目标漂移了只能重新发起一次导入。
        Ok(OverwriteOutcome::Missing) => fail_and_ok(
            &state.daemon_requests,
            request_id,
            "target skill no longer exists",
            "failed to persist failure",
        ),
        Ok(OverwriteOutcome::NotOwner) => fail_and_ok(
            &state.daemon_requests,
            request_id,
            "you no longer have permission to overwrite this skill",
            "failed to persist failure",
        ),
        Ok(OverwriteOutcome::NameMismatch) => fail_and_ok(
            &state.daemon_requests,
            request_id,
            "target skill name no longer matches the imported skill",
            "failed to persist failure",
        ),
        Err(err) => fail_and_ok(
            &state.daemon_requests,
            request_id,
            err.to_string(),
            "failed to persist failure",
        ),
    }
}

/// create 路径：先探同名（冲突是**终态**而非错误），再写库。
///
/// 独立成函数是因为竞态收尾要复用一遍同名探测：`create_skill_with_files` 撞上
/// `UNIQUE(workspace_id, name)` 时回 [`RepoError::Conflict`]（另一个导入赢了探测与
/// 插入之间的竞态），此时必须**再查一次**并走同一条冲突收尾，而不是把唯一键冲突
/// 当基础设施故障。
async fn create_imported_skill(
    state: &AppState,
    request_id: Id,
    repo: &DaemonRepo,
    reported: &ReportedLocalSkill,
    inputs: ImportInputs,
    supports_conflict: bool,
) -> ApiResult<Json<Value>> {
    let name = sanitize(&inputs.name);
    let existing = match repo.skill_by_name(inputs.workspace_id, &name).await {
        Ok(existing) => existing,
        // 探测失败在上游同样是「上报成功 + 请求失败」，不是 5xx（daemon 重发也没用）。
        Err(err) => {
            return fail_and_ok(
                &state.daemon_requests,
                request_id,
                format!("failed to check for existing skill: {err}"),
                "failed to persist failure",
            );
        }
    };
    if let Some(existing) = existing {
        return conflict_terminal(
            state,
            request_id,
            &existing,
            inputs.creator,
            supports_conflict,
        );
    }
    let created = repo
        .create_skill_with_files(
            inputs.workspace_id,
            &name,
            &sanitize(&inputs.description),
            &sanitize(&reported.content),
            &inputs.config,
            Some(inputs.creator),
            &inputs.files,
        )
        .await;
    match created {
        Ok(row) => {
            state
                .daemon_requests
                .complete(request_id, json!({ "skill": skill_with_files_wire(&row) }))
                .map_err(|_| internal("failed to persist import completion"))?;
            Ok(ok_body())
        }
        Err(RepoError::Conflict) => {
            // 输了竞态：重新探测，命中就走冲突收尾；没命中（对手又被删了）回老文案。
            match repo.skill_by_name(inputs.workspace_id, &name).await {
                Ok(Some(existing)) => conflict_terminal(
                    state,
                    request_id,
                    &existing,
                    inputs.creator,
                    supports_conflict,
                ),
                Ok(None) => fail_and_ok(
                    &state.daemon_requests,
                    request_id,
                    "a skill with this name already exists",
                    "failed to persist failure",
                ),
                Err(err) => fail_and_ok(
                    &state.daemon_requests,
                    request_id,
                    format!("failed to check for existing skill: {err}"),
                    "failed to persist failure",
                ),
            }
        }
        Err(err) => fail_and_ok(
            &state.daemon_requests,
            request_id,
            err.to_string(),
            "failed to persist failure",
        ),
    }
}

/// 同名冲突的终态收尾：新客户端拿 `conflict`（非错误），老客户端拿 `failed`。
fn conflict_terminal(
    state: &AppState,
    request_id: Id,
    existing: &SkillRow,
    creator: Id,
    supports_conflict: bool,
) -> ApiResult<Json<Value>> {
    if !supports_conflict {
        return fail_and_ok(
            &state.daemon_requests,
            request_id,
            "a skill with this name already exists",
            "failed to persist failure",
        );
    }
    state
        .daemon_requests
        .conflict(request_id, &conflict_wire(existing, creator))
        .map_err(|_| internal("failed to persist conflict"))?;
    Ok(ok_body())
}

/// skill 行 + 文件行 → upstream `SkillWithFilesResponse`（`skill.go:133`）。
///
/// `created_by` 是 `*string`（**无** omitempty ⇒ null 也会出现）；
/// `files` 无 omitempty ⇒ 空数组也要出现。
fn skill_with_files_wire(row: &SkillWithFilesRow) -> Value {
    let skill = &row.skill;
    json!({
        "id": skill.id().to_string(),
        "workspace_id": skill.workspace_id.to_string(),
        "name": skill.name,
        "description": skill.description,
        "content": skill.content,
        "config": skill.config,
        "created_by": skill.created_by.map(|id| id.to_string()),
        "created_at": skill.created_at.to_rfc3339(),
        "updated_at": skill.updated_at.to_rfc3339(),
        "files": row
            .files
            .iter()
            .map(|file| json!({
                "id": file.id.to_string(),
                "skill_id": file.skill_id.to_string(),
                "path": file.path,
                "content": file.content,
                "created_at": file.created_at.to_rfc3339(),
                "updated_at": file.updated_at.to_rfc3339(),
            }))
            .collect::<Vec<Value>>(),
    })
}

/// upstream `LocalSkillImportConflict`（`runtime_local_skills.go:54`）。
fn conflict_wire(existing: &SkillRow, creator: Id) -> Value {
    let mut info = json!({
        "existing_skill_id": existing.id().to_string(),
        "can_overwrite": can_overwrite(Some(creator), existing),
    });
    if let Some(created_by) = existing.created_by {
        info.as_object_mut().expect("json! 构造的对象").insert(
            "existing_created_by".into(),
            Value::String(created_by.to_string()),
        );
    }
    info
}

/// upstream `canOverwriteSkillByLocalImport`（`skill.go:602`）：creator 存在且就是发起者。
fn can_overwrite(creator: Option<Id>, existing: &SkillRow) -> bool {
    match (creator, existing.created_by) {
        (Some(creator), Some(created_by)) => creator.as_uuid() == created_by,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_path_validation_is_shared_with_skill_bundles() {
        // 口径由 `skills::validate_file_path` 单点提供（本模块不再自建一份）。
        for ok in ["SKILL.md", "docs/a.md", "./a/b.md"] {
            assert!(validate_file_path(ok), "应接受 {ok}");
        }
        for bad in [
            "",
            "/etc/passwd",
            "../escape.md",
            "a/../../escape.md",
            r"\\host\share\f",
            r"C:\Users\f",
            "c:/Users/f",
        ] {
            assert!(!validate_file_path(bad), "应拒绝 {bad}");
        }
    }

    fn skill_row(created_by: Option<Id>) -> SkillRow {
        SkillRow {
            id: Id::new().as_uuid(),
            workspace_id: Id::new().as_uuid(),
            name: "s".into(),
            description: String::new(),
            content: String::new(),
            config: json!({}),
            created_by: created_by.map(mc_core::Id::as_uuid),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn can_overwrite_requires_same_creator() {
        let creator = Id::new();
        let other = Id::new();
        assert!(can_overwrite(Some(creator), &skill_row(Some(creator))));
        assert!(!can_overwrite(Some(other), &skill_row(Some(creator))));
        assert!(!can_overwrite(None, &skill_row(Some(creator))));
        assert!(!can_overwrite(Some(creator), &skill_row(None)));
    }

    #[test]
    fn conflict_wire_omits_absent_creator() {
        let creator = Id::new();
        let owned = conflict_wire(&skill_row(Some(creator)), creator);
        assert_eq!(owned["can_overwrite"], json!(true));
        assert_eq!(owned["existing_created_by"], json!(creator.to_string()));

        let orphan = conflict_wire(&skill_row(None), creator);
        assert_eq!(orphan["can_overwrite"], json!(false));
        assert!(orphan.get("existing_created_by").is_none());
    }
}
