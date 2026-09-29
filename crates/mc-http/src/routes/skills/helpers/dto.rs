use super::*;

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
