//! squad 面的线上形状（wire shape）与请求体（上游 `squad.go` 的
//! `SquadResponse` / `SquadMemberPreviewResponse` / `SquadMemberResponse` /
//! `SquadMemberStatusResponse` / `SquadActiveIssueBrief` / `SquadMemberStatusListResponse`）。
//!
//! 字段顺序、JSON key、可空性都按上游逐字段抄写（结构体按声明序序列化）。
//! 有意不同只有两处（详见 `routes/squads.rs` 模块文档的「有意偏离」）：
//! - `avatar_url` 原样透传（上游 `resolveAvatarURLPtr` 会签名成存储 URL，本仓没有
//!   对象存储接线，M3-5 agents 面的先例同样是原样透传）；
//! - 时间串用本仓既有约定 `to_rfc3339()`（Go 侧 `time.RFC3339`，秒级精度）。

use std::collections::HashMap;

use axum::body::Bytes;
use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use uuid::Uuid;

use mc_repos::squad::{
    SquadMemberPreviewRow, SquadMemberRow, SquadRow, DEFAULT_MEMBER_PREVIEW_LIMIT,
};

use crate::routes::agents::bad_request;

// ---------------------------------------------------------------------------
// 响应 DTO
// ---------------------------------------------------------------------------

/// 上游 `SquadResponse`。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SquadDto {
    pub(crate) id: String,
    pub(crate) workspace_id: String,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) instructions: String,
    pub(crate) avatar_url: Option<String>,
    pub(crate) leader_id: String,
    pub(crate) creator_id: String,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    pub(crate) archived_at: Option<String>,
    pub(crate) archived_by: Option<String>,
    pub(crate) member_count: u64,
    pub(crate) member_preview: Vec<SquadMemberPreviewDto>,
}

/// 上游 `SquadMemberPreviewResponse`。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SquadMemberPreviewDto {
    pub(crate) member_type: String,
    pub(crate) member_id: String,
    pub(crate) role: String,
}

/// 上游 `SquadMemberResponse`。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SquadMemberDto {
    pub(crate) id: String,
    pub(crate) squad_id: String,
    pub(crate) member_type: String,
    pub(crate) member_id: String,
    pub(crate) role: String,
    pub(crate) created_at: String,
}

/// 上游 `SquadMemberStatusListResponse`（`{members: [...]}`）。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SquadMemberStatusListDto {
    pub(crate) members: Vec<SquadMemberStatusDto>,
}

/// 上游 `SquadMemberStatusResponse`。
///
/// `status` 对非 agent 成员是 **`null`**（上游 `*string`），所以这里是 `Option`。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SquadMemberStatusDto {
    pub(crate) member_type: String,
    pub(crate) member_id: String,
    pub(crate) status: Option<&'static str>,
    pub(crate) active_issues: Vec<SquadActiveIssueDto>,
    pub(crate) last_active_at: Option<String>,
}

/// 上游 `SquadActiveIssueBrief`。
#[derive(Debug, Clone, Serialize)]
pub(crate) struct SquadActiveIssueDto {
    pub(crate) issue_id: String,
    pub(crate) identifier: String,
    pub(crate) title: String,
    pub(crate) issue_status: String,
}

fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339()
}

fn ts_opt(t: Option<DateTime<Utc>>) -> Option<String> {
    t.map(|v| v.to_rfc3339())
}

/// 上游 `squadMemberSummary`：`count` 是**全部**成员行数，`preview` 只留前 3 条。
#[derive(Debug, Clone, Default)]
pub(crate) struct SquadMemberSummary {
    pub(crate) count: u64,
    pub(crate) preview: Vec<SquadMemberPreviewDto>,
}

impl SquadMemberSummary {
    /// 上游 `addSquadMemberPreview`：先加计数，超过 3 条就不再进 preview。
    pub(crate) fn push(&mut self, member_type: &str, member_id: Uuid, role: &str) {
        self.count += 1;
        if self.preview.len() >= DEFAULT_MEMBER_PREVIEW_LIMIT {
            return;
        }
        self.preview.push(SquadMemberPreviewDto {
            member_type: member_type.to_string(),
            member_id: member_id.to_string(),
            role: role.to_string(),
        });
    }
}

impl SquadDto {
    /// 上游 `squadToResponse` + `squadToResponseWithPreview`（`member_count` 走 summary）。
    pub(crate) fn from_row(row: &SquadRow, summary: &SquadMemberSummary) -> Self {
        Self {
            id: row.id.to_string(),
            workspace_id: row.workspace_id.to_string(),
            name: row.name.clone(),
            description: row.description.clone(),
            instructions: row.instructions.clone(),
            avatar_url: row.avatar_url.clone(),
            leader_id: row.leader_id.to_string(),
            creator_id: row.creator_id.to_string(),
            created_at: ts(row.created_at),
            updated_at: ts(row.updated_at),
            archived_at: ts_opt(row.archived_at),
            archived_by: row.archived_by.map(|v| v.to_string()),
            member_count: summary.count,
            member_preview: summary.preview.clone(),
        }
    }
}

impl SquadMemberDto {
    /// 上游 `squadMemberToResponse`。
    pub(crate) fn from_row(row: &SquadMemberRow) -> Self {
        Self {
            id: row.id.to_string(),
            squad_id: row.squad_id.to_string(),
            member_type: row.member_type.clone(),
            member_id: row.member_id.to_string(),
            role: row.role.clone(),
            created_at: ts(row.created_at),
        }
    }
}

/// 把「按 `squad_id` 升序」的 preview 行归组（上游 `loadSquadMemberSummary` 的批量等价物）。
pub(crate) fn group_preview_by_squad(
    rows: Vec<SquadMemberPreviewRow>,
) -> HashMap<Uuid, SquadMemberSummary> {
    let mut grouped: HashMap<Uuid, SquadMemberSummary> = HashMap::new();
    for row in rows {
        grouped
            .entry(row.squad_id)
            .or_default()
            .push(&row.member_type, row.member_id, &row.role);
    }
    grouped
}

/// 上游 `buildSquadMemberSummary(rows)`（单 squad 版本）。
pub(crate) fn summary_from_rows(rows: &[SquadMemberPreviewRow]) -> SquadMemberSummary {
    let mut summary = SquadMemberSummary::default();
    for row in rows {
        summary.push(&row.member_type, row.member_id, &row.role);
    }
    summary
}

// ---------------------------------------------------------------------------
// 请求体
// ---------------------------------------------------------------------------

/// `POST /api/squads/`（上游 `CreateSquad` 的内联结构体）。
///
/// 字段全用 `Option` 只是为了容忍 JSON `null`：Go 把 `null` 解成零值（不报错），
/// serde 的 `String` 会报类型错，语义上等价于「没传」。
#[derive(Debug, Default, Deserialize)]
pub(crate) struct CreateSquadRequest {
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) leader_id: Option<String>,
    #[serde(default)]
    pub(crate) avatar_url: Option<String>,
}

/// `PUT /api/squads/:id/`（上游 `UpdateSquad` 的 `*string` 字段集）。
///
/// 语义与 Go 的指针完全一致：**缺省 = 不改**、`null` = 不改（Go 指针 nil）、
/// `""` = 改成空串。
#[derive(Debug, Default, Deserialize)]
pub(crate) struct UpdateSquadRequest {
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) description: Option<String>,
    #[serde(default)]
    pub(crate) instructions: Option<String>,
    #[serde(default)]
    pub(crate) avatar_url: Option<String>,
    #[serde(default)]
    pub(crate) leader_id: Option<String>,
}

/// `POST /api/squads/:id/members`（上游 `AddSquadMember`）。
#[derive(Debug, Default, Deserialize)]
pub(crate) struct AddSquadMemberRequest {
    #[serde(default)]
    pub(crate) member_type: Option<String>,
    #[serde(default)]
    pub(crate) member_id: Option<String>,
    #[serde(default)]
    pub(crate) role: Option<String>,
}

/// `DELETE /api/squads/:id/members`（上游 `RemoveSquadMember`）。
#[derive(Debug, Default, Deserialize)]
pub(crate) struct RemoveSquadMemberRequest {
    #[serde(default)]
    pub(crate) member_type: Option<String>,
    #[serde(default)]
    pub(crate) member_id: Option<String>,
}

/// `PATCH /api/squads/:id/members/role`（上游 `UpdateSquadMemberRole`）。
#[derive(Debug, Default, Deserialize)]
pub(crate) struct UpdateSquadMemberRoleRequest {
    #[serde(default)]
    pub(crate) member_type: Option<String>,
    #[serde(default)]
    pub(crate) member_id: Option<String>,
    #[serde(default)]
    pub(crate) role: Option<String>,
}

/// 上游 `decodeJSONBody`：body 非法 → 400 `invalid request body`。
///
/// 注意 Go 的 `null` body 会「全部字段缺失」，这里同样退化成 `T::default()`。
pub(crate) fn decode_body<T: DeserializeOwned + Default>(body: &Bytes) -> Result<T, ()> {
    let value: JsonValue = serde_json::from_slice(body).map_err(|_| ())?;
    if value.is_null() {
        return Ok(T::default());
    }
    if !value.is_object() {
        return Err(());
    }
    serde_json::from_value(value).map_err(|_| ())
}

/// [`decode_body`] 的「否则 400」形态。
pub(crate) fn decode_body_or_bad_request<T: DeserializeOwned + Default>(
    body: &Bytes,
) -> Result<T, mc_errors::Error> {
    decode_body::<T>(body).map_err(|()| bad_request("invalid request body"))
}
