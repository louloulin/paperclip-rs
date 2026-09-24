//! skill 面**共用件**：会话/工作区解析、`load_skill_for_user`、响应投影。
//!
//! - **写者**：M6-2（**W**，本文件唯一写者；`docs/57` §3.2）。M6-3 只**读**。
//!   ⚠️ M6-3 若需要一个还不存在的 helper，**加到自己的** `import.rs` / `refresh.rs`，
//!   **不要**回头改本文件（否则两片同改一个文件 —— 这正是 anchor 要消灭的东西）。
//! - **上游**：`internal/handler/skill.go` 的 `resolveWorkspaceID` / `requireWorkspaceMember`
//!   / `loadSkillForUser`（+ `skill_create.go` 的公共校验）。
//! - **本仓约定**：
//!   - 鉴权沿用 `routes::auth_user::AuthUser` 提取器 + `workspace_role` 家族查询；
//!   - 跨工作区 / 非成员一律 **404**（不是 403）—— 与上游一致，避免探测存在性；
//!   - `load_skill_for_user` 是**唯一**的取 skill 入口：所有子文件（含 M6-3 的 import/refresh）
//!     都要走它，不要各写一份带不同过滤条件的版本；
//!   - DTO 在这里统一投影（`skill` 行 → 响应），**不要**把行结构直接 `Serialize`
//!     （列名与响应字段名不一致，且 `content` 在列表响应里应省略）。
//! - **不做什么**：不在这里做保留路径 / 二进制判定（`mc-skill` 的纯函数）、不做 bundle 哈希
//!   （`routes/daemon/skills.rs`，M6-4）。
//!
//! **状态：M6-2 已落地（LUM-1667）**。门 ⑩ 行预算：桩写「260 行」，落地约 **790 行**
//! （硬限 800 ✓）——多出来的几乎全是**投影层**：12 个响应形状 + 4 个请求体，加上
//! `ClawHub` 客户端与两条 Go 转义函数。

use std::collections::HashMap;
use std::fmt::Write as _;
use std::time::Duration;

use axum::body::Bytes;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};

use mc_core::Id;
use mc_errors::Error;
use mc_repos::skill::read::{
    SkillFileMetadataRow, SkillFileRow, SkillLabelRow, SkillRepo, SkillRow, SkillSummaryRow,
};
use mc_repos::skill::write::{NewSkill, SkillFileInput, SkillUpdate};

use crate::routes::agents::{bad_request, forbidden, not_found, parse_uuid, repo_err};
use crate::routes::inbox::resolve_workspace_id;
use crate::state::AppState;

// ---------------------------------------------------------------------------
// 响应投影
// ---------------------------------------------------------------------------

fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339()
}

fn ts_2(a: DateTime<Utc>, b: DateTime<Utc>) -> (String, String) {
    (ts(a), ts(b))
}

/// `skill.config` 的规范化（上游 `decodeSkillConfig`）：`null` ⇒ `{}`。
///
/// 迁移里该列是 `NOT NULL DEFAULT '{}'`，所以真实行不会是 `null`；但 `SQL NULL`
/// 与 JSON `null` 在 jsonb 里长得一样，上游为此显式兜了一层，这里照抄。
fn normalise_config(config: &JsonValue) -> JsonValue {
    if config.is_null() {
        serde_json::json!({})
    } else {
        config.clone()
    }
}

/// 上游 `SkillResponse`。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SkillDto {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub description: String,
    pub content: String,
    pub config: JsonValue,
    pub created_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl SkillDto {
    pub(crate) fn from_row(row: &SkillRow) -> Self {
        let (created_at, updated_at) = ts_2(row.created_at, row.updated_at);
        Self {
            id: row.id.to_string(),
            workspace_id: row.workspace_id.to_string(),
            name: row.name.clone(),
            description: row.description.clone(),
            content: row.content.clone(),
            config: normalise_config(&row.config),
            created_by: row.created_by.map(|u| u.to_string()),
            created_at,
            updated_at,
        }
    }
}

/// 上游 `SkillSummaryResponse`（= `SkillResponse` 去掉 `content`）。
///
/// `enabled` 只有 agent 面（M6-4）才填；`labels` 只有列表面才填 —— 两者都是**指针 +
/// `omitempty`**，所以「未填」在 JSON 里是**缺席**而不是 `null`，这里用
/// `Option::is_none` 复刻同一个语义。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SkillSummaryDto {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub description: String,
    pub config: JsonValue,
    pub created_by: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<LabelDto>>,
}

impl SkillSummaryDto {
    #[allow(clippy::too_many_arguments)] // 与上游 `SkillSummaryResponse` 的字段一一对读
    fn build(
        id: &uuid::Uuid,
        workspace_id: &uuid::Uuid,
        name: &str,
        description: &str,
        config: &JsonValue,
        created_by: Option<uuid::Uuid>,
        created_at: DateTime<Utc>,
        updated_at: DateTime<Utc>,
    ) -> Self {
        let (created_at, updated_at) = ts_2(created_at, updated_at);
        Self {
            id: id.to_string(),
            workspace_id: workspace_id.to_string(),
            name: name.to_string(),
            description: description.to_string(),
            config: normalise_config(config),
            created_by: created_by.map(|u| u.to_string()),
            created_at,
            updated_at,
            enabled: None,
            labels: None,
        }
    }

    /// 列表行（`ListSkillSummariesByWorkspace`）。
    pub(crate) fn from_summary_row(row: &SkillSummaryRow) -> Self {
        Self::build(
            &row.id,
            &row.workspace_id,
            &row.name,
            &row.description,
            &row.config,
            row.created_by,
            row.created_at,
            row.updated_at,
        )
    }

    /// 全列行（`include=metadata` 的详情面：skill 已按全列取回，投影成摘要形状）。
    pub(crate) fn from_skill_row(row: &SkillRow) -> Self {
        Self::build(
            &row.id,
            &row.workspace_id,
            &row.name,
            &row.description,
            &row.config,
            row.created_by,
            row.created_at,
            row.updated_at,
        )
    }
}

/// 上游 `SkillFileResponse`。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SkillFileDto {
    pub id: String,
    pub skill_id: String,
    pub path: String,
    pub content: String,
    pub created_at: String,
    pub updated_at: String,
}

impl SkillFileDto {
    pub(crate) fn from_row(row: &SkillFileRow) -> Self {
        let (created_at, updated_at) = ts_2(row.created_at, row.updated_at);
        Self {
            id: row.id.to_string(),
            skill_id: row.skill_id.to_string(),
            path: row.path.clone(),
            content: row.content.clone(),
            created_at,
            updated_at,
        }
    }
}

/// 上游 `SkillFileMetadataResponse`（正文换成 `size` + `content_hash`）。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SkillFileMetadataDto {
    pub id: String,
    pub skill_id: String,
    pub path: String,
    pub size: i64,
    pub content_hash: String,
    pub created_at: String,
    pub updated_at: String,
}

impl SkillFileMetadataDto {
    pub(crate) fn from_row(row: &SkillFileMetadataRow) -> Self {
        let (created_at, updated_at) = ts_2(row.created_at, row.updated_at);
        Self {
            id: row.id.to_string(),
            skill_id: row.skill_id.to_string(),
            path: row.path.clone(),
            size: row.size,
            content_hash: row.content_hash.clone(),
            created_at,
            updated_at,
        }
    }
}

/// 上游 `SkillWithFilesResponse`（嵌入式结构 ⇒ 字段**平铺**，不是 `{"skill": …}`）。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SkillWithFilesDto {
    #[serde(flatten)]
    pub skill: SkillDto,
    pub files: Vec<SkillFileDto>,
}

/// 上游 `SkillWithFileMetadataResponse`。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SkillWithFileMetadataDto {
    #[serde(flatten)]
    pub skill: SkillSummaryDto,
    pub content_size: i64,
    pub content_hash: String,
    pub files: Vec<SkillFileMetadataDto>,
}

/// 上游 `LabelResponse`（`usage_count` 恒 0：上游 `labelToResponse` 也不算）。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct LabelDto {
    pub id: String,
    pub workspace_id: String,
    pub resource_type: String,
    pub name: String,
    pub description: String,
    pub color: String,
    pub usage_count: i64,
    pub created_at: String,
    pub updated_at: String,
}

impl LabelDto {
    pub(crate) fn from_row(row: &SkillLabelRow) -> Self {
        let (created_at, updated_at) = ts_2(row.created_at, row.updated_at);
        Self {
            id: row.id.to_string(),
            workspace_id: row.workspace_id.to_string(),
            resource_type: row.resource_type.clone(),
            name: row.name.clone(),
            description: row.description.clone(),
            color: row.color.clone(),
            usage_count: 0,
            created_at,
            updated_at,
        }
    }
}

/// attach / detach / list 三条标签路由共用的 `{"labels": […]}` 包。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct LabelsDto {
    pub labels: Vec<LabelDto>,
}

/// 上游 `SkillSearchCandidateResponse`：三个可空字段（`repo` / `install_count` /
/// `github_stars`）**没有 `omitempty`** ⇒ 未填时是 JSON `null`。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SkillSearchCandidateDto {
    pub name: String,
    pub url: String,
    pub source: String,
    pub repo: Option<String>,
    pub install_count: Option<i64>,
    pub github_stars: Option<i64>,
    pub description: String,
}

// ---------------------------------------------------------------------------
// 请求体
// ---------------------------------------------------------------------------

/// 上游 `CreateSkillFileRequest`。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct SkillFileInputDto {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub content: String,
}

/// 上游 `CreateSkillRequest`。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct CreateSkillRequest {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub content: String,
    /// `null` 与缺省等价（上游 `Config any` 的 nil）。
    #[serde(default)]
    pub config: Option<JsonValue>,
    /// `null` 与缺省等价；`[]` 是**空清单**（不是缺省）。
    #[serde(default)]
    pub files: Option<Vec<SkillFileInputDto>>,
}

/// 上游 `UpdateSkillRequest`：三个 `*string` ⇒ `null`/缺省 = 不改，`""` = **清空**。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct UpdateSkillRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub config: Option<JsonValue>,
    #[serde(default)]
    pub files: Option<Vec<SkillFileInputDto>>,
}

/// 请求体解码（`CreateSkill` / `UpdateSkill` / `UpsertSkillFile` 共用）。
///
/// 对齐上游 `json.NewDecoder(...).Decode(&req)` 的三条行为：空 body / 语法错误、
/// **非对象**（显式判 —— `serde` 派生的结构体 visitor 也吃数组，Go 那边 `[]` 是 400）、
/// 以及字面 `null` ⇒ 零值（交给各自的必填校验）。详见 `docs/32` §9.6。
pub(crate) fn decode_body<T: DeserializeOwned + Default>(body: &Bytes) -> Result<T, Error> {
    let value: JsonValue =
        serde_json::from_slice(body).map_err(|_| bad_request("invalid request body"))?;
    match value {
        JsonValue::Null => Ok(T::default()),
        JsonValue::Object(_) => {
            serde_json::from_value(value).map_err(|_| bad_request("invalid request body"))
        }
        _ => Err(bad_request("invalid request body")),
    }
}

/// 上游 `AttachLabelRequest`（字段名是 **`label_id`**，不是 `labelId`）。
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct AttachLabelRequest {
    #[serde(default)]
    pub label_id: Option<String>,
}

impl CreateSkillRequest {
    /// 建库入参：`config` 缺省补 `{}`（上游 `createSkillWithFilesInTx` 的 nil→`{}`）。
    pub(crate) fn to_new_skill(&self, workspace_id: Id, created_by: Id) -> NewSkill {
        NewSkill {
            workspace_id,
            name: self.name.clone(),
            description: self.description.clone(),
            content: self.content.clone(),
            config: self.config.clone().unwrap_or_else(|| serde_json::json!({})),
            created_by: Some(created_by),
        }
    }
}

impl UpdateSkillRequest {
    pub(crate) fn patch(&self) -> SkillUpdate {
        SkillUpdate {
            name: self.name.clone(),
            description: self.description.clone(),
            content: self.content.clone(),
            config: self.config.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// 路径 / 内容校验
// ---------------------------------------------------------------------------

/// 上游 `validateFilePath`：空、绝对路径、`Clean` 后以 `..` 开头都拒（`..foo` 的怪癖
/// 逐字复刻，见 `docs/32` §9.6）。`Clean` 走 `mc_skill::reserved` 的 Go 移植，
/// **不**用 `Path::clean`（两者对尾随 `/.`、重复分隔符处理不同）。
pub(crate) fn validate_file_path(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    if path.starts_with('/') {
        return false;
    }
    !mc_skill::reserved::clean_path(path).starts_with("..")
}

/// 请求体里的文件清单 → 仓储入参，并**跳过**保留路径（上游 `IsReservedContentPath`）。
///
/// 保留路径（`SKILL.md`）是正文列 `skill.content` 的地盘：可以出现在请求体里、会被
/// `validate_file_path` 校验，但**不落 `skill_file`**；上游 create/update 各写一遍，
/// 这里收成一份。
pub(crate) fn supported_files(files: &[SkillFileInputDto]) -> Vec<SkillFileInput> {
    files
        .iter()
        .filter(|f| !mc_skill::reserved::is_reserved_content_path(&f.path))
        .map(|f| SkillFileInput {
            path: f.path.clone(),
            content: f.content.clone(),
        })
        .collect()
}

/// 上游 `contentHash`：裸十六进制 SHA-256（**不是** bundle 形态的 `sha256:…`），与 SQL 侧
/// `encode(sha256(convert_to(content,'UTF8')),'hex')` 同值 ⇒ 两处哈希可比较。
pub(crate) fn content_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}

/// 上游 `resolveSkillInclude`：`?include=` → 是否内联正文。缺省（与 `content`）都内联 ——
/// 已有客户端在读 `content`，翻默认值会让他们静默收到不同形状。
pub(crate) fn resolve_include(query: &HashMap<String, String>) -> Result<bool, Error> {
    match query.get("include").map(String::as_str).map(str::trim) {
        None | Some("" | "content") => Ok(true),
        Some("metadata") => Ok(false),
        Some(_) => Err(bad_request(
            r#"invalid include: expected "content" or "metadata""#,
        )),
    }
}

// ---------------------------------------------------------------------------
// 调用上下文
// ---------------------------------------------------------------------------

/// 一次请求的「workspace + 调用者 + repo」三元组（上游 `resolveWorkspaceID` 家族）。
///
/// **角色不在 `resolve` 里查**：上游只有 `canManageSkill` 查成员身份，`ListSkills` /
/// `GetSkill` / `ListSkillFiles` 连成员都不查（只按 `workspace_id` 收窄）——提前查会把
/// 「非成员读列表」从 200 变成 404。
pub(crate) struct SkillScope {
    pub(crate) workspace_id: Id,
    pub(crate) user_id: Id,
    pub(crate) repo: SkillRepo,
    state: std::sync::Arc<AppState>,
}

impl SkillScope {
    pub(crate) fn resolve(
        state: &std::sync::Arc<AppState>,
        user: &crate::routes::auth_user::AuthUser,
        headers: &HeaderMap,
        query: &HashMap<String, String>,
    ) -> Result<Self, Error> {
        Ok(Self {
            workspace_id: resolve_workspace_id(headers, query)?,
            user_id: user.id(),
            repo: SkillRepo::new(state.db.clone()),
            state: state.clone(),
        })
    }

    /// 上游 `loadSkillForUser`：workspace 收窄 + 主键取值；取不到 ⇒ 404 `skill not found`。
    ///
    /// 偏离（已登记 `docs/32` §9.6）：上游把 `GetSkillInWorkspace` 的**任何**错误都折成
    /// 404，本仓按既有切片口径区分为 `Db` ⇒ 500（拿不到库不等于不存在）。
    pub(crate) async fn load_skill(&self, raw_id: &str) -> Result<SkillRow, Error> {
        let skill_id = Id(parse_uuid(raw_id, "skill id")?);
        self.repo
            .get_in_workspace(self.workspace_id, skill_id)
            .await
            .map_err(|e| repo_err(e, "skill"))
    }

    /// 上游 `canManageSkill`：非成员 ⇒ 404，owner/admin 放行，创建者放行，其余 403。
    /// 上游白名单 `("owner","admin","member")` 含 `member` ⇒ 它实际只校验「是不是成员」，
    /// 403 那条才是真正的语义。
    pub(crate) async fn require_can_manage(&self, skill: &SkillRow) -> Result<(), Error> {
        let role: Option<String> =
            sqlx::query_scalar("SELECT role FROM member WHERE workspace_id = $1 AND user_id = $2")
                .bind(self.workspace_id.0)
                .bind(self.user_id.0)
                .fetch_optional(self.state.db.pool())
                .await
                .map_err(|e| Error::Database(e.to_string()))?;

        match role.as_deref() {
            // 非成员：上游 requireWorkspaceRole 用 notFoundMsg="skill not found" ⇒ 404。
            None => Err(not_found("skill")),
            Some("owner" | "admin") => Ok(()),
            Some(_) if skill.created_by == Some(self.user_id.0) => Ok(()),
            Some(_) => Err(forbidden("only the skill creator can manage this skill")),
        }
    }
}

// ---------------------------------------------------------------------------
// ClawHub 搜索客户端（上游 `searchClawHubSkills`）
// ---------------------------------------------------------------------------

/// 上游 `clawHubAPIBase`。
pub(crate) const CLAWHUB_API_BASE: &str = "https://clawhub.ai/api/v1";
/// 上游 `clawHubSearchStatsLimit`：只给前 10 条补水安装量（每条一次出站请求）。
pub(crate) const CLAWHUB_STATS_LIMIT: usize = 10;
/// 上游 `http.Client{Timeout: 30 * time.Second}`。
pub(crate) const CLAWHUB_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Default, Deserialize)]
struct ClawhubSearchResponse {
    /// 缺省与 `null` 都当空表；**每个字段都必须可缺省** —— 线上响应不保证键齐全，
    /// 少一个键就整轮 502 是过度严格。
    #[serde(default)]
    results: Option<Vec<ClawhubSearchResult>>,
}

#[derive(Debug, Default, Deserialize)]
struct ClawhubSearchResult {
    #[serde(default)]
    slug: String,
    #[serde(default, rename = "displayName")]
    display_name: String,
    #[serde(default)]
    summary: String,
    #[serde(default, rename = "ownerHandle")]
    owner_handle: String,
}

#[derive(Debug, Default, Deserialize)]
struct ClawhubGetSkillResponse {
    #[serde(default)]
    skill: ClawhubSkill,
}

#[derive(Debug, Default, Deserialize)]
struct ClawhubSkill {
    #[serde(default)]
    stats: ClawhubSkillStats,
}

#[derive(Debug, Default, Deserialize)]
struct ClawhubSkillStats {
    #[serde(default, rename = "installsAllTime")]
    installs_all_time: i64,
    #[serde(default, rename = "installsCurrent")]
    installs_current: i64,
}

/// 上游 `searchClawHubSkills`：搜索 +（前 10 条）安装量补水。错误一律是**字符串**而不是
/// `Error` —— 调用方要把它塞进 502 的扁平体（形状与本仓标准错误体不同）。
pub(crate) async fn search_clawhub_skills(
    client: &reqwest::Client,
    query: &str,
) -> Result<Vec<SkillSearchCandidateDto>, String> {
    let url = format!("{CLAWHUB_API_BASE}/search?q={}", query_escape(query));
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("failed to reach ClawHub: {e}"))?;
    if resp.status() != reqwest::StatusCode::OK {
        return Err(format!(
            "ClawHub search returned status {}",
            resp.status().as_u16()
        ));
    }
    let body: ClawhubSearchResponse = resp
        .json()
        .await
        .map_err(|_| "failed to parse ClawHub search response".to_string())?;

    let results = body.results.unwrap_or_default();
    let mut candidates = Vec::with_capacity(results.len());
    for (index, result) in results.iter().enumerate() {
        // 空 slug 直接跳过；注意下标用的是**原始**下标（上游 `for i, r := range`），
        // 所以被跳过的条目仍占掉一个补水名额。
        if result.slug.is_empty() {
            continue;
        }
        let mut candidate = SkillSearchCandidateDto {
            name: if result.display_name.is_empty() {
                result.slug.clone()
            } else {
                result.display_name.clone()
            },
            url: clawhub_skill_url(&result.owner_handle, &result.slug),
            source: "clawhub.ai".to_string(),
            repo: None,
            install_count: None,
            github_stars: None,
            description: result.summary.clone(),
        };
        if index < CLAWHUB_STATS_LIMIT {
            if let Some(count) = fetch_clawhub_install_count(client, &result.slug).await {
                candidate.install_count = Some(count);
            }
        }
        candidates.push(candidate);
    }
    Ok(candidates)
}

/// 上游 `buildClawHubSkillURL`。
pub(crate) fn clawhub_skill_url(owner_handle: &str, slug: &str) -> String {
    if owner_handle.is_empty() {
        return format!("https://clawhub.ai/{}", path_escape(slug));
    }
    format!(
        "https://clawhub.ai/{}/{}",
        path_escape(owner_handle),
        path_escape(slug)
    )
}

/// 上游 `fetchClawHubInstallCount`：任何失败都只记 warn 并留 `None`（搜索不因补水失败）。
async fn fetch_clawhub_install_count(client: &reqwest::Client, slug: &str) -> Option<i64> {
    let url = format!("{CLAWHUB_API_BASE}/skills/{}", path_escape(slug));
    let resp = match client.get(&url).send().await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!(slug, error = %e, "clawhub search: failed to fetch skill details");
            return None;
        }
    };
    if resp.status() != reqwest::StatusCode::OK {
        tracing::warn!(
            slug,
            status = resp.status().as_u16(),
            "clawhub search: skill details returned non-200"
        );
        return None;
    }
    let detail: ClawhubGetSkillResponse = match resp.json().await {
        Ok(detail) => detail,
        Err(e) => {
            tracing::warn!(slug, error = %e, "clawhub search: failed to parse skill details");
            return None;
        }
    };
    let stats = detail.skill.stats;
    if stats.installs_all_time > 0 {
        Some(stats.installs_all_time)
    } else {
        Some(stats.installs_current)
    }
}

// ---------------------------------------------------------------------------
// 转义（本 crate 没有 `url` 依赖 ⇒ 手写 Go 的两条最小转义）
// ---------------------------------------------------------------------------

/// Go `url.QueryEscape`：非保留字符里空格变 `+`（表单编码），其余不合法字节 `%XX`。
///
/// 只覆盖 `A-Za-z0-9-_.~` 放行 —— 与 Go 的 `shouldEscape(encodeQueryComponent)` 一致
/// （`+` 本身也要转成 `%2B`，否则会被对端读成空格）。
pub(crate) fn query_escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            b' ' => out.push('+'),
            other => {
                // `write!` 到 `String` 不会失败（`fmt::Write` 的 `String` 实现是 Infallible）
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}

/// Go `url.PathEscape`（`encodePathSegment`）：`/ ; , ?` 要转义，`$ & + : = @` 放行。
pub(crate) fn path_escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.as_bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~'
            | b'$'
            | b'&'
            | b'+'
            | b':'
            | b'='
            | b'@' => out.push(*byte as char),
            other => {
                // `write!` 到 `String` 不会失败（`fmt::Write` 的 `String` 实现是 Infallible）
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 502（上游 SearchSkills 的扁平错误体）
// ---------------------------------------------------------------------------

/// 上游 `writeJSON(w, http.StatusBadGateway, map[string]string{"code":…, "error":…})`。
/// ⚠️ 这条**故意**不是本仓的 `{"error":{…}}` 形状：契约就是扁平的，故返回裸 `Response`。
pub(crate) fn upstream_unavailable(message: &str) -> Response {
    (
        axum::http::StatusCode::BAD_GATEWAY,
        Json(serde_json::json!({
            "code": "upstream_unavailable",
            "error": message,
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn include_defaults_to_content_and_rejects_anything_else() {
        // `mc_errors::Error` 没有 `PartialEq` ⇒ 比 `Result` 要解出来各自断言
        assert!(resolve_include(&q(&[])).unwrap());
        assert!(resolve_include(&q(&[("include", "")])).unwrap());
        assert!(resolve_include(&q(&[("include", " content ")])).unwrap());
        assert!(!resolve_include(&q(&[("include", "metadata")])).unwrap());
        let err = resolve_include(&q(&[("include", "files")])).unwrap_err();
        assert_eq!(err.http_status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[test]
    fn content_hash_is_bare_hex_sha256_of_the_bytes() {
        // 与 SQL `encode(sha256(convert_to(content,'UTF8')),'hex')` 同值（不带 `sha256:` 前缀）。
        assert_eq!(
            content_hash(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            content_hash("hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn escapes_follow_go_query_and_path_rules() {
        // Go：`QueryEscape` 空格→`+`、`+`→`%2B`；`PathEscape` 空格→`%20`、`/`→`%2F`。
        assert_eq!(query_escape("a b+c/d&e"), "a+b%2Bc%2Fd%26e");
        assert_eq!(path_escape("a b+c/d&e"), "a%20b+c%2Fd&e");
        assert_eq!(query_escape("react"), "react");
    }

    #[test]
    fn clawhub_url_skips_the_owner_segment_when_absent() {
        assert_eq!(
            clawhub_skill_url("acme", "react-tips"),
            "https://clawhub.ai/acme/react-tips"
        );
        assert_eq!(
            clawhub_skill_url("", "react-tips"),
            "https://clawhub.ai/react-tips"
        );
    }

    #[test]
    fn config_null_normalises_to_an_empty_object() {
        assert_eq!(normalise_config(&JsonValue::Null), serde_json::json!({}));
        assert_eq!(
            normalise_config(&serde_json::json!({"a": 1})),
            serde_json::json!({"a": 1})
        );
    }
}
