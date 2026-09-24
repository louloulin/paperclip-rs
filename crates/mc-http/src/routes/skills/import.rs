//! skill 导入路由：`POST /api/skills/import`（**1 个注册键**）。
//!
//! - **写者**：M6-3（`docs/57` §3.2）。
//! - **上游**：`internal/handler/skill.go` 的导入段（`ImportSkill` / `finishSkillImport` /
//!   `resolveImportSkillConflict` / `createRenamedImportedSkill` / `importFetchErrorResponse`）
//!   + `skill_import_archive.go`（multipart 包）。
//! - **两种入参形态**（同一套「校验 → 入库」尾巴）：
//!   ① `{"url": "https://…"}`（或裸 slug，默认 clawhub）⇒ **出网取件**；
//!   ② `multipart/form-data` 上传 zip 包 ⇒ 内存内解包（`mc_skill::archive`）。
//!   ⚠️ 请求体字段名是 **`url`**（上游 `ImportSkillRequest` 的 json tag），不是 `source`。
//!
//! | 注册键 | 方法 | 上游 |
//! | --- | :-: | --- |
//! | `/api/skills/import` | POST | `router.go:2237` |
//!
//! - **出网面（本仓唯一需要出网的 skill 路径）**：取件全在 `import/fetch.rs` + `import/github*`
//!   （`reqwest` 已在依赖里，不新增 HTTP 客户端依赖）；总超时 45s、单请求 30s；失败映射
//!   413 / 504 / 503 / 502（`import_fetch_error_response`）。
//! - **zip 面**：`mc_skill::archive::parse_skill_archive`（纯逻辑、内存内解包）；文件数 / 体积
//!   超限 = **整包失败**（此路径下 400，URL 路径下 413 —— 与上游一致）。
//! - **入库**：`mc_repos::skill::import`（整包一个事务；四策略 `fail`/`overwrite`/`rename`/`skip`，
//!   缺省 `fail`）。
//! - **结构化 vs 旧形态**：请求里**显式传了** `on_conflict` 才回 `SkillImportResult`
//!   （`status`/`reason`/`skill`/`existing_skill`）；否则维持旧客户端契约 —— 成功回裸
//!   `SkillWithFilesResponse`，重名回 `{"error": …, "existing_skill": …}` 的 409。archive 路径
//!   **永远**结构化（上游注释：它没有需要兼容的旧客户端）。
//! - **本文件的位置分工**：本文件只管「HTTP 形状 + 冲突策略编排 + 落库调用」；出网细节在
//!   `import/fetch.rs`（端点 / 原始文件下载 / ClawHub）与 `import/github/`（api.github.com +
//!   `raw.githubusercontent.com` 支撑文件）。拆分已登记 `docs/32` §9。
//! - **不做什么**：不做 git clone（上游没有这条路径）；不做导入任务异步化（同步返回）；
//!   不广播 WS 事件（沿用 M6-2 登记的偏离）。
//!
//! **状态：M6-3 已落地（LUM-1668）**。
//!
//! 行预算（门 ⑩）：桩写「520 行以内」，落地 452 行 ✓。
//!
//! 有意偏离（见 `docs/32` §9.6）：① 本文件与 `refresh.rs` 的错误体沿用上游**扁平**
//! `{"error":"…"}`（`helpers::upstream_unavailable` 已是同款先例，M6-2 已登记）；
//! ② JSON 请求体加了 1 MiB 上限（上游不设限）⇒ 超限报 400 `invalid request body`；
//! ③ 整轮 45s 超时由 `tokio::time::timeout` 产生，折成 `ImportFailure::Timeout`（上游用
//! `context.DeadlineExceeded`），状态码与文案逐字不变；④ 保留路径（`SKILL.md`）的过滤放在
//! **请求映射**这一步（`imported_skill_file_requests`），上游只在 create 分支过滤、overwrite
//! 分支不过滤 —— 本仓两条分支都过滤（更严，不会把 SKILL.md 写进 `skill_file`）。

mod fetch;
mod github;

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::extract::{FromRequest, Multipart, Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};

use mc_core::Id;
use mc_repos::skill::import::{ConflictStrategy, ImportOverwriteInput, OverwritePolicy};
use mc_repos::skill::read::SkillRow;
use mc_repos::skill::write::{NewSkill, SkillFileInput, SkillWithFiles};
use mc_repos::RepoError;
use mc_skill::archive::{parse_skill_archive, ImportedSkill, MAX_IMPORT_ARCHIVE_UPLOAD_SIZE};

use super::helpers::{
    decode_body, validate_file_path, SkillDto, SkillFileDto, SkillScope, SkillWithFilesDto,
};

pub(super) use self::fetch::{fetch_imported, import_fetch_error_response, FetchedSkill};
#[cfg(feature = "test-util")]
pub use self::fetch::{set_source_endpoints, SourceEndpoints};

use crate::routes::auth_user::AuthUser;
use crate::state::AppState;

/// JSON 请求体上限。上游对 URL 导入**不设限**（只给 archive 设了 16 MiB），这里按
/// 「一行 URL + 一个枚举」的实际体量给 1 MiB —— 防的是无上限读内存，不是业务上限。
const JSON_IMPORT_BODY_LIMIT: usize = 1 << 20;

/// `POST /api/skills/import`（M6-3 落地）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/skills/import", post(import_skill))
}

// ---------------------------------------------------------------------------
// 请求 / 响应形状（上游 `ImportSkillRequest` / `SkillImportResult` / `ExistingSkillIdentity`）
// ---------------------------------------------------------------------------

/// 上游 `ImportSkillRequest`：字段名是 `url`（裸 slug 也走这里，默认 `ClawHub`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct SkillImportRequest {
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub on_conflict: String,
}

/// 上游 `ExistingSkillIdentity`。
///
/// `created_by` / `can_overwrite` 都是 Go 的 `omitempty`：空串 / `false` **不出现在 JSON 里**
/// （`can_overwrite: false` 是「你不能覆盖」，客户端据此隐藏按钮）。
#[derive(Debug, Clone, Serialize)]
pub(super) struct ExistingSkillIdentityDto {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    pub can_overwrite: bool,
}

/// 上游 `SkillImportResult`：`status` 恒在，其余三个都 `omitempty`。
#[derive(Debug, Clone, Serialize)]
pub(super) struct SkillImportResultDto {
    pub status: &'static str,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill: Option<SkillWithFilesDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub existing_skill: Option<ExistingSkillIdentityDto>,
}

impl SkillImportResultDto {
    fn new(status: &'static str) -> Self {
        Self {
            status,
            reason: String::new(),
            skill: None,
            existing_skill: None,
        }
    }

    fn reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = reason.into();
        self
    }

    fn skill(mut self, skill: SkillWithFilesDto) -> Self {
        self.skill = Some(skill);
        self
    }

    fn existing(mut self, existing: ExistingSkillIdentityDto) -> Self {
        self.existing_skill = Some(existing);
        self
    }
}

/// 上游 `skillImportConflictReason`（逐字）。
const IMPORT_CONFLICT_REASON: &str = "a skill with this name already exists; use --on-conflict \
                                      overwrite to replace it or --on-conflict rename to import a copy";
/// 上游 `skillImportOverwriteFailure` 的 403 文案（逐字）。
const OVERWRITE_FORBIDDEN: &str = "only the skill creator can overwrite this skill";

// serde 的 `skip_serializing_if` 只接受 `fn(&T) -> bool`，所以这里必须传引用。
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(value: &bool) -> bool {
    !*value
}

/// 上游 `writeJSON`：指定状态码 + JSON 体。
pub(super) fn write_json<T: Serialize>(status: u16, body: &T) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(body)).into_response()
}

/// 上游 `writeError`：**扁平** `{"error": "…"}`。
#[derive(Serialize)]
struct FlatErrorBody<'a> {
    error: &'a str,
}

pub(super) fn write_error(status: u16, message: &str) -> Response {
    write_json(status, &FlatErrorBody { error: message })
}

/// 上游 `sanitizeNullBytes`（`skill::write` 里那份是私有的，留一份 3 行副本，
/// 已登记 `docs/32` §9.6）。
pub(super) fn sanitize_null_bytes(text: &str) -> String {
    text.replace('\0', "")
}

/// 上游 `createSkillWithFiles` 的响应投影（`crud.rs` 那份是同文件的私有函数，这里再投影一次）。
pub(super) fn with_files_dto(created: &SkillWithFiles) -> SkillWithFilesDto {
    SkillWithFilesDto {
        skill: SkillDto::from_row(&created.skill),
        files: created.files.iter().map(SkillFileDto::from_row).collect(),
    }
}

// ---------------------------------------------------------------------------
// POST /api/skills/import
// ---------------------------------------------------------------------------

/// `POST /api/skills/import`（上游 `ImportSkill`）。
///
/// 分支只按 `Content-Type` 判一次（上游 `isMultipartForm`）：`multipart/form-data` 前缀 ⇒
/// 归档路径，其余一律走 JSON —— 包括 `Content-Type` 缺失或写错的情况（那时 JSON 解码失败
/// ⇒ 400 `invalid request body`，与上游同）。
pub(super) async fn import_skill(
    State(state): State<Arc<AppState>>,
    auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
    request: Request,
) -> Response {
    let scope = match SkillScope::resolve(&state, &auth, &headers, &query) {
        Ok(scope) => scope,
        Err(error) => return crate::error::ApiError(error).into_response(),
    };

    let is_multipart = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_ascii_lowercase)
        .is_some_and(|value| value.starts_with("multipart/form-data"));

    if is_multipart {
        return import_skill_from_archive(&state, &scope, request).await;
    }

    let (_, body) = request.into_parts();
    let Ok(bytes) = to_bytes(body, JSON_IMPORT_BODY_LIMIT).await else {
        return write_error(400, "invalid request body");
    };
    // `decode_body` 已处理「空体 / 语法错 / 非对象（`[]` 不能被当成「字段全缺省」）/ 字面 null」
    // 四种情形（上游 `json.NewDecoder(...).Decode` 的口径，见 `docs/32` §9.6）。
    let payload: SkillImportRequest = match decode_body(&bytes) {
        Ok(payload) => payload,
        Err(_) => return write_error(400, "invalid request body"),
    };
    let Some(strategy) = ConflictStrategy::parse(&payload.on_conflict) else {
        return write_error(
            400,
            "on_conflict must be one of: fail, overwrite, rename, skip",
        );
    };
    let structured = ConflictStrategy::is_structured(&payload.on_conflict);

    // 源判定失败 ⇒ **400**（上游在取件之前做这一步，所以这里是 400 不是 502）。
    let (source, normalized) = match mc_skill::source::detect_import_source(&payload.url) {
        Ok(detected) => detected,
        Err(error) => return write_error(400, &error.0),
    };

    let fetched = match fetch_imported(source, &normalized).await {
        Ok(fetched) => fetched,
        Err(error) => {
            let (status, message) = import_fetch_error_response(&error);
            return write_error(status, &message);
        }
    };
    finish_skill_import(&scope, strategy, structured, &fetched).await
}

/// 上游 `importSkillFromArchive`：multipart 分支。
///
/// 与 URL 分支的两处差别：① 上限违例是 **400**（上游 `writeError(400, err.Error())`，不是 413）；
/// ② 结果**永远**结构化。名字取自 `SKILL.md` 的 frontmatter（缺失则回落归档/文件名），
/// origin 为空（归档导入没有可回溯的 URL ⇒ `config` 是 `{}`，刷新这类 skill 报 422）。
async fn import_skill_from_archive(
    state: &Arc<AppState>,
    scope: &SkillScope,
    request: Request,
) -> Response {
    let (parts, body) = request.into_parts();
    // 上游 `http.MaxBytesReader(w, r.Body, maxImportArchiveUploadSize)` + `ParseMultipartForm`
    // 的合并效果：读不动 / 超限都算「包不合法」⇒ 400（同一句文案）。
    let Ok(bytes) = to_bytes(body, MAX_IMPORT_ARCHIVE_UPLOAD_SIZE + 1).await else {
        return write_error(400, ARCHIVE_UPLOAD_INVALID);
    };
    let rebuilt = Request::from_parts(parts, Body::from(bytes));
    let Ok(mut multipart) = Multipart::from_request(rebuilt, state).await else {
        return write_error(400, ARCHIVE_UPLOAD_INVALID);
    };

    let mut on_conflict = String::new();
    let mut archive: Option<(Vec<u8>, String)> = None;
    loop {
        let field = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => break,
            Err(_) => return write_error(400, ARCHIVE_UPLOAD_INVALID),
        };
        // 先把名字拷出来：`field` 接下来要被消费，`name()` 的借用必须立刻结束。
        let name = field.name().map(str::to_string);
        match name.as_deref() {
            Some("on_conflict") => match field.text().await {
                Ok(text) => on_conflict = text,
                Err(_) => return write_error(400, "failed to read uploaded file"),
            },
            Some("file") => {
                let filename = field.file_name().unwrap_or_default().to_string();
                match field.bytes().await {
                    Ok(data) => archive = Some((data.to_vec(), filename)),
                    Err(_) => return write_error(400, "failed to read uploaded file"),
                }
            }
            _ => {}
        }
    }

    let Some(strategy) = ConflictStrategy::parse(&on_conflict) else {
        return write_error(
            400,
            "on_conflict must be one of: fail, overwrite, rename, skip",
        );
    };
    let Some((data, filename)) = archive else {
        return write_error(
            400,
            r#"a skill archive file is required (form field "file")"#,
        );
    };
    let imported = match parse_skill_archive(&data, &filename) {
        Ok(imported) => imported,
        Err(error) => return write_error(400, &error.message),
    };
    let fetched = FetchedSkill {
        skill: imported,
        origin: JsonValue::Null,
    };
    finish_skill_import(scope, strategy, true, &fetched).await
}

/// 上游 `ParseMultipartForm` 失败时的文案（逐字）。
const ARCHIVE_UPLOAD_INVALID: &str = "invalid multipart upload or file exceeds the size limit";

// ---------------------------------------------------------------------------
// 共用尾巴（上游 `finishSkillImport`）
// ---------------------------------------------------------------------------

/// 上游 `importedSkillFileRequests`：取件结果 → 入库文件清单。
///
/// 跳过两类路径：`validateFilePath` 不认的（绝对路径 / `..`）与**保留路径**（`SKILL.md` ——
/// 它是 `skill.content` 的地盘）。上游只在 create 分支过滤保留路径，这里两个分支都过滤
/// （见文件头偏离 ④）。
pub(super) fn imported_skill_file_requests(imported: &ImportedSkill) -> Vec<SkillFileInput> {
    imported
        .files
        .iter()
        .filter(|file| {
            validate_file_path(&file.path)
                && !mc_skill::reserved::is_reserved_content_path(&file.path)
        })
        .map(|file| SkillFileInput {
            path: file.path.clone(),
            content: file.content.clone(),
        })
        .collect()
}

/// 上游 `finishSkillImport`：取件 / 解包之后的**唯一**入库路径。
async fn finish_skill_import(
    scope: &SkillScope,
    strategy: ConflictStrategy,
    structured: bool,
    fetched: &FetchedSkill,
) -> Response {
    let imported = &fetched.skill;
    let files = imported_skill_file_requests(imported);
    // 溯源落在 `skill.config.origin`（列表 / 详情 UI 据此显示「Imported from …」并回链）。
    let config = match &fetched.origin {
        JsonValue::Null => json!({}),
        origin => json!({ "origin": origin }),
    };
    let name = sanitize_null_bytes(&imported.name);

    // 结构化结果先查一次同名：这样 `skip` 不必先写库再回滚，而且能把「可覆盖性」算给客户端。
    if structured {
        match scope.repo.find_by_name(scope.workspace_id, &name).await {
            Ok(Some(existing)) => {
                return resolve_import_skill_conflict(
                    scope, strategy, &existing, &name, imported, &config, &files,
                )
                .await
            }
            Ok(None) => {}
            Err(error) => {
                return write_json(
                    500,
                    &SkillImportResultDto::new("failed")
                        .reason(format!("failed to check for existing skill: {error}")),
                )
            }
        }
    }

    let new = NewSkill {
        workspace_id: scope.workspace_id,
        name: name.clone(),
        description: imported.description.clone(),
        content: imported.content.clone(),
        config: config.clone(),
        created_by: Some(scope.user_id),
    };
    match scope.repo.create_imported(&new, &files).await {
        Ok(created) => {
            let dto = with_files_dto(&created);
            if structured {
                return write_json(201, &SkillImportResultDto::new("created").skill(dto));
            }
            write_json(201, &dto)
        }
        // 唯一约束撞车：结构化请求再判一次策略（并发插入只可能在这一步暴露）；
        // 旧形态请求回上游那个「带 existing_skill 的 409」。
        Err(RepoError::Conflict) => {
            if structured {
                if let Ok(Some(existing)) = scope.repo.find_by_name(scope.workspace_id, &name).await
                {
                    return resolve_import_skill_conflict(
                        scope, strategy, &existing, &name, imported, &config, &files,
                    )
                    .await;
                }
            }
            match scope
                .repo
                .find_by_name(scope.workspace_id, &name)
                .await
                .ok()
                .flatten()
            {
                Some(existing) => {
                    // 非结构化路径不知道调用者是谁 ⇒ `can_overwrite=false`（上游传空 userID）。
                    let identity = existing_skill_identity(&existing, None);
                    write_json(
                        409,
                        &json!({
                            "error": "a skill with this name already exists",
                            "existing_skill": identity,
                        }),
                    )
                }
                None => write_error(409, "a skill with this name already exists"),
            }
        }
        Err(error) => write_error(500, &format!("failed to create skill: {error}")),
    }
}

/// 上游 `existingSkillIdentity`：`can_overwrite` 由「创建者 == 调用者」判定。
///
/// `user_id` 为 `None` ⇒ 永远 `false`（上游传空串的等价物，用于旧形态 409 体）。
fn existing_skill_identity(skill: &SkillRow, user_id: Option<Id>) -> ExistingSkillIdentityDto {
    let created_by = skill.created_by.map(Id);
    ExistingSkillIdentityDto {
        id: skill.id.to_string(),
        name: skill.name.clone(),
        created_by: created_by.map(|id| id.to_string()),
        can_overwrite: matches!((user_id, created_by), (Some(user), Some(creator)) if user == creator),
    }
}

/// 上游 `resolveImportSkillConflict`：同名冲突下按四策略分流。
#[allow(clippy::too_many_arguments)]
async fn resolve_import_skill_conflict(
    scope: &SkillScope,
    strategy: ConflictStrategy,
    existing: &SkillRow,
    name: &str,
    imported: &ImportedSkill,
    config: &JsonValue,
    files: &[SkillFileInput],
) -> Response {
    let identity = existing_skill_identity(existing, Some(scope.user_id));
    match strategy {
        ConflictStrategy::Skip => write_json(
            200,
            &SkillImportResultDto::new("skipped")
                .reason("a skill with this name already exists")
                .existing(identity),
        ),
        ConflictStrategy::Fail => write_json(
            409,
            &SkillImportResultDto::new("conflict")
                .reason(IMPORT_CONFLICT_REASON)
                .existing(identity),
        ),
        ConflictStrategy::Overwrite => {
            // 先判权（事务外，给客户端一个明确的 403 而不是等到事务里才失败）。
            if !OverwritePolicy::CreatorOnly.allows(scope.user_id, existing.created_by.map(Id)) {
                return write_json(
                    403,
                    &SkillImportResultDto::new("failed")
                        .reason(OVERWRITE_FORBIDDEN)
                        .existing(identity),
                );
            }
            let input = ImportOverwriteInput {
                workspace_id: scope.workspace_id,
                target_skill_id: Id(existing.id),
                user_id: scope.user_id,
                policy: OverwritePolicy::CreatorOnly,
                expected_name: name.to_string(),
                // 导入覆盖不改名（改名是 `rename` 策略的事）。
                new_name: String::new(),
                description: imported.description.clone(),
                content: imported.content.clone(),
                config: config.clone(),
                files: files.to_vec(),
            };
            match scope.repo.overwrite_imported(&input).await {
                Ok(updated) => write_json(
                    200,
                    &SkillImportResultDto::new("updated").skill(with_files_dto(&updated)),
                ),
                Err(error) => {
                    let (status, reason) = error.import_http();
                    write_json(
                        status,
                        &SkillImportResultDto::new("failed")
                            .reason(reason)
                            .existing(identity),
                    )
                }
            }
        }
        ConflictStrategy::Rename => {
            let new = NewSkill {
                workspace_id: scope.workspace_id,
                name: name.to_string(),
                description: imported.description.clone(),
                content: imported.content.clone(),
                config: config.clone(),
                created_by: Some(scope.user_id),
            };
            match scope.repo.create_renamed_imported(&new, files, name).await {
                Ok(created) => write_json(
                    201,
                    &SkillImportResultDto::new("created")
                        .reason("renamed to avoid an existing skill")
                        .skill(with_files_dto(&created))
                        .existing(identity),
                ),
                Err(error) => write_json(
                    500,
                    &SkillImportResultDto::new("failed")
                        .reason(format!("failed to create renamed skill: {error}"))
                        .existing(identity),
                ),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_request_reads_the_url_field_not_source() {
        let payload: SkillImportRequest =
            serde_json::from_str(r#"{"url":"clawhub.ai/foo","on_conflict":"skip"}"#).unwrap();
        assert_eq!(payload.url, "clawhub.ai/foo");
        assert_eq!(payload.on_conflict, "skip");
        assert_eq!(
            ConflictStrategy::parse(&payload.on_conflict),
            Some(ConflictStrategy::Skip)
        );
    }

    /// 上游 `ExistingSkillIdentity` 的 `omitempty`：`false` / 空串**不出现**在 JSON 里。
    #[test]
    fn identity_omits_false_can_overwrite() {
        let identity = ExistingSkillIdentityDto {
            id: "skill-1".into(),
            name: "demo".into(),
            created_by: None,
            can_overwrite: false,
        };
        let value = serde_json::to_value(&identity).unwrap();
        assert_eq!(value, json!({"id": "skill-1", "name": "demo"}));
    }

    #[test]
    fn import_result_omits_empty_optional_fields() {
        let value = serde_json::to_value(SkillImportResultDto::new("skipped")).unwrap();
        assert_eq!(value, json!({"status": "skipped"}));
        let value = serde_json::to_value(
            SkillImportResultDto::new("created").reason("renamed to avoid an existing skill"),
        )
        .unwrap();
        assert_eq!(
            value,
            json!({"status": "created", "reason": "renamed to avoid an existing skill"})
        );
    }
}
