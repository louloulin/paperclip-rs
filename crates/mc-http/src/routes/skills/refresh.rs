//! skill 刷新路由：`POST /api/skills/:id/refresh`（**1 个注册键**）。
//!
//! - **写者**：M6-3（`docs/57` §3.2）。
//! - **上游**：`internal/handler/skill_refresh.go`（`RefreshSkill` / `parseSkillOrigin` /
//!   `refreshableOriginSource` / `fetchImportedSkillFromOrigin` / `mergeSkillConfigOrigin`）。
//! - **语义**：只对**导入来的** skill 有意义 —— 重新按 `skill.config.origin.source_url` 取件并
//!   **原地覆盖**。保留 `id` / `created_by` / `created_at` / 标签连接行 / `agent_skill` 绑定；
//!   替换 name（采纳上游改名）/ description / content / config.origin / 全部支持文件。
//! - **三种失败要在取件前就分出来**（上游把校验放在任何网络调用之前，禁止调用方烧掉 45s 预算）：
//!   ① 非成员 ⇒ **404**；② 既非创建者又非 owner/admin ⇒ **403**；③ 没有可刷新的来源
//!   （手写 skill / 归档导入 / 被手工改过的 origin）⇒ **422**。
//!   ⚠️ 桩注释曾写「没有来源 ⇒ 400」—— **上游是 422** `UnprocessableEntity`，本文件按上游。
//! - **判权与导入覆盖不同**：这里用 `isAdmin || isCreator`（上游内联闭包），而
//!   `import.rs` 的 `on_conflict=overwrite` 是**仅创建者**。差别是有意的：来源 URL 钉在 skill
//!   自己身上，所以 admin 刷新注入不了任意内容。
//! - **实现复用**：取件（45s 超时 / 413 / 502 / 503 / 504 分类）与落库（事务内整批替换）
//!   全走 `import.rs` 的 `fetch_imported` + `mc_repos`，本文件只做「来源解析 + 判权 + 分发」。
//! - **不做什么**：不做定时自动刷新（那是 autopilot 的语义，本波没有）；不广播 WS 事件
//!   （沿用 M6-2 登记的偏离）。
//!
//! **状态：M6-3 已落地（LUM-1668）**。
//!
//! 行预算（门 ⑩）：桩写「180 行以内」，落地 235 行 ✓。
//!
//! 有意偏离（见 `docs/32` §9.6）：错误体一律上游**扁平** `{"error":"…"}`，**包括**取 skill
//! 失败（404）—— 本端点自己的形状，不受 M6-2 给 `crud/files/labels` 定的嵌套包封约束。

use std::collections::HashMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::routing::post;
use axum::Router;
use serde_json::Value as JsonValue;

use mc_core::Id;
use mc_errors::Error;
use mc_repos::skill::import::{ImportOverwriteInput, OverwriteError, OverwritePolicy};
use mc_skill::archive::ImportError;
use mc_skill::source::{detect_import_source, ImportSource};

use super::helpers::SkillScope;
use super::import::{
    fetch_imported, import_fetch_error_response, imported_skill_file_requests, sanitize_null_bytes,
    with_files_dto, write_error, write_json, FetchedSkill,
};
use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

/// 上游 `errSkillNotRefreshable`（逐字）。
const NOT_REFRESHABLE: &str =
    "this skill was not imported from a refreshable source (GitHub, skills.sh, or ClawHub)";
/// 上游刷新路径的 403 文案（逐字；与导入覆盖那句不同）。
const REFRESH_FORBIDDEN: &str =
    "only the skill creator or a workspace admin can update this skill from its source";

/// `POST /api/skills/:id/refresh`（M6-3 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/skills/:id/refresh", post(refresh_skill))
}

/// 上游 `skillOriginRef`：重跑一次导入所需的最小溯源信息。
#[derive(Debug, Clone, PartialEq, Eq)]
struct SkillOriginRef {
    kind: String,
    source_url: String,
}

/// 上游 `parseSkillOrigin`：`config.origin.{type,source_url}`；解不出（缺键 / 类型不对 /
/// 空 type）就是「没有来源」。
///
/// `source_url` 缺失 ⇒ 空串（上游零值），由调用方再判 —— 上游把「空 URL」与「没有 origin」
/// 分成两条失败路径，最终都是 422。
fn parse_skill_origin(config: &JsonValue) -> Option<SkillOriginRef> {
    let origin = config.get("origin")?.as_object()?;
    let kind = origin.get("type")?.as_str()?.to_string();
    if kind.is_empty() {
        return None;
    }
    let source_url = origin
        .get("source_url")
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    Some(SkillOriginRef { kind, source_url })
}

/// 取件失败面：能不能刷（配置问题）与取不到（网络问题）必须分开 —— 前者 422，后者按
/// `import_fetch_error_response` 的 413/502/503/504 家族。
enum RefreshFetchError {
    /// 上游 `errSkillNotRefreshable`。
    NotRefreshable,
    /// 取件本身失败（分类沿用导入那套）。
    Fetch(ImportError),
}

/// 上游 `fetchImportedSkillFromOrigin`。
///
/// 三道闸全在出网之前：origin 类型必须可刷新 → `source_url` 非空 → `source_url` 必须
/// **解析回它自己那个源**（防手改过的 config 把 clawhub 的 URL 塞进 github origin）。
async fn fetch_imported_skill_from_origin(
    origin: SkillOriginRef,
) -> Result<FetchedSkill, RefreshFetchError> {
    let Some(expected) = ImportSource::from_origin_type(&origin.kind) else {
        return Err(RefreshFetchError::NotRefreshable);
    };
    if origin.source_url.is_empty() {
        return Err(RefreshFetchError::NotRefreshable);
    }
    let Ok((source, normalized)) = detect_import_source(&origin.source_url) else {
        return Err(RefreshFetchError::NotRefreshable);
    };
    if source != expected {
        return Err(RefreshFetchError::NotRefreshable);
    }
    fetch_imported(source, &normalized)
        .await
        .map_err(RefreshFetchError::Fetch)
}

/// 上游 `mergeSkillConfigOrigin`：**只**改 `origin` 键，其余用户配置原样保留。
///
/// 比导入覆盖路径窄：那里 config 是整批替换，这里只动溯源 —— 用户通过 API/CLI 设置的
/// 其它 config 键不该因为一次刷新被抹掉。
fn merge_skill_config_origin(existing: &JsonValue, origin: &JsonValue) -> JsonValue {
    let mut config = match existing.as_object() {
        Some(map) => map.clone(),
        None => serde_json::Map::new(),
    };
    if !origin.is_null() {
        config.insert("origin".to_string(), origin.clone());
    }
    JsonValue::Object(config)
}

/// 上游 `RefreshSkill`。
pub(super) async fn refresh_skill(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    Path(id): Path<String>,
) -> axum::response::Response {
    let scope = match SkillScope::resolve(&state, &auth, &headers, &query) {
        Ok(scope) => scope,
        Err(error) => return error_response(&error),
    };
    let skill = match scope.load_skill(&id).await {
        Ok(skill) => skill,
        Err(error) => return error_response(&error),
    };

    // 上游 `requireWorkspaceRole(..., "skill not found", "owner","admin","member")`：非成员
    // 对外与「skill 不在我这个工作区」同形（404），不泄露存在性。
    let role = match member_role(&state, scope.workspace_id, scope.user_id).await {
        Ok(role) => role,
        Err(error) => return error_response(&error),
    };
    let is_admin = role == "owner" || role == "admin";
    let is_creator = skill.created_by == Some(scope.user_id.0);
    if !is_admin && !is_creator {
        return write_error(403, REFRESH_FORBIDDEN);
    }

    // 判权先于任何出网（上游注释：别让一个越权调用者烧掉 45s 取件预算）。
    let Some(origin) = parse_skill_origin(&skill.config) else {
        return write_error(422, NOT_REFRESHABLE);
    };
    let fetched = match fetch_imported_skill_from_origin(origin).await {
        Ok(fetched) => fetched,
        Err(RefreshFetchError::NotRefreshable) => return write_error(422, NOT_REFRESHABLE),
        Err(RefreshFetchError::Fetch(error)) => {
            let (status, message) = import_fetch_error_response(&error);
            return write_error(status, &message);
        }
    };

    // 采纳上游改名（名字被上游改过时，刷新会跟着改）—— 唯一会写 `name` 的刷新分支。
    let new_name = sanitize_null_bytes(&fetched.skill.name);
    if new_name != skill.name {
        tracing::info!(
            skill_id = %skill.id,
            old_name = %skill.name,
            new_name = %new_name,
            "skill refresh: adopting upstream rename"
        );
    }

    let input = ImportOverwriteInput {
        workspace_id: scope.workspace_id,
        target_skill_id: Id(skill.id),
        user_id: scope.user_id,
        policy: OverwritePolicy::CreatorOrAdmin { is_admin },
        // 刷新不比对名字（`expected_name` 是导入覆盖用来防「拿过期 id 覆盖错行」的）。
        expected_name: String::new(),
        new_name,
        description: fetched.skill.description.clone(),
        content: fetched.skill.content.clone(),
        config: merge_skill_config_origin(&skill.config, &fetched.origin),
        files: imported_skill_file_requests(&fetched.skill),
    };
    match scope.repo.overwrite_imported(&input).await {
        Ok(updated) => write_json(200, &with_files_dto(&updated)),
        Err(error) => match error {
            OverwriteError::NotFound => write_error(404, "skill not found"),
            OverwriteError::Forbidden => write_error(403, REFRESH_FORBIDDEN),
            OverwriteError::NameConflict => write_error(
                409,
                &format!(
                    "a skill named \"{}\" already exists in this workspace",
                    input.new_name
                ),
            ),
            other => write_error(500, &format!("failed to update skill from source: {other}")),
        },
    }
}

/// 上游 `requireWorkspaceRole`：非成员 ⇒ 404（文案与 skill 面一致）。
async fn member_role(state: &AppState, workspace_id: Id, user_id: Id) -> Result<String, Error> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT role FROM member WHERE workspace_id = $1 AND user_id = $2")
            .bind(workspace_id.0)
            .bind(user_id.0)
            .fetch_optional(state.db.pool())
            .await
            .map_err(|error| Error::Database(error.to_string()))?;
    row.map(|(role,)| role)
        .ok_or_else(|| crate::routes::agents::not_found("skill"))
}

/// 本端点的错误体是上游**扁平**形状：`Error` 只用来取状态码与文案。
fn error_response(error: &Error) -> axum::response::Response {
    let body = mc_errors::ErrorResponse::from(error);
    write_error(error.http_status(), &body.message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_skill_origin_reads_type_and_source_url() {
        let config =
            json!({"origin": {"type": "clawhub", "source_url": " clawhub.ai/foo ", "slug": "foo"}});
        assert_eq!(
            parse_skill_origin(&config),
            Some(SkillOriginRef {
                kind: "clawhub".into(),
                source_url: "clawhub.ai/foo".into(),
            })
        );
        // 其它键不影响解析（刷新只关心 origin）。
        assert!(parse_skill_origin(&json!({"other": 1})).is_none());
        // 空 type / 非对象 origin / 非字符串 type 都算「没有来源」。
        assert!(parse_skill_origin(&json!({"origin": {"type": ""}})).is_none());
        assert!(parse_skill_origin(&json!({"origin": "clawhub"})).is_none());
        assert!(parse_skill_origin(&json!({"origin": {"type": 7}})).is_none());
    }

    #[test]
    fn merge_skill_config_origin_preserves_other_keys() {
        let merged = merge_skill_config_origin(
            &json!({"temperature": 0.5, "origin": {"type": "github", "source_url": "old"}}),
            &json!({"type": "github", "source_url": "new"}),
        );
        assert_eq!(merged["temperature"], json!(0.5));
        assert_eq!(merged["origin"]["source_url"], json!("new"));
        // 没有 origin（归档导入 / 手写）时不动配置。
        let untouched = merge_skill_config_origin(&json!({"temperature": 0.5}), &JsonValue::Null);
        assert_eq!(untouched, json!({"temperature": 0.5}));
        // 非对象 config 等价于空配置。
        assert_eq!(
            merge_skill_config_origin(&JsonValue::Null, &json!({"type": "github"})),
            json!({"origin": {"type": "github"}})
        );
    }
}
