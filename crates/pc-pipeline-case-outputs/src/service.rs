#![forbid(unsafe_code)]
//! Pipeline case outputs DB glue（与 Node \`pipelineCaseOutputsService\` 1:1）。
//!
//! 当前 R639.1 范围：\`.list_case_outputs()\` —— 投影 sources + documents 部分。
//! work_products / attachments 子集留 R639.2 轮次。

use crate::pure::{preview_for, sort_outputs_in_place, source_document_path, source_issue_path};
use crate::types::{
    PipelineCaseOutputItem, PipelineCaseOutputItemKind, PipelineCaseOutputsResponse,
    SourceTrustMetadata,
};
use pc_core::Timestamp;
use pc_repos::Db;
use serde_json::Value;
use uuid::Uuid;

/// 单个 source 行：pipelineCaseIssueLinks JOIN issues。
#[derive(Debug, Clone)]
pub struct CaseOutputSourceRow {
    pub link_id: Uuid,
    pub company_id: Uuid,
    pub case_id: Uuid,
    pub issue_id: Uuid,
    pub role: String,
    pub issue_identifier: Option<String>,
    pub issue_title: String,
    pub issue_status: String,
    pub issue_source_trust: Option<Value>,
    pub created_by_run_id: Option<Uuid>,
    pub linked_at: Timestamp,
}

/// 投影 sources（pipeline_case_issue_links JOIN issues 中未 retired 且 issue 未 cancel）。
pub async fn list_sources(
    db: &Db,
    company_id: Uuid,
    case_id: Uuid,
) -> sqlx::Result<Vec<CaseOutputSourceRow>> {
    let rows: Vec<(Uuid, Uuid, Uuid, Uuid, String, Option<String>, String, String, Option<Value>, Option<Uuid>, Timestamp)> = sqlx::query_as(
        "SELECT pcil.id, pcil.company_id, pcil.case_id, pcil.issue_id, pcil.role, \
                i.identifier, i.title, i.status::text, i.source_trust, \
                pcil.created_by_run_id, pcil.created_at \
         FROM pipeline_case_issue_links pcil \
         INNER JOIN issues i ON i.id = pcil.issue_id \
         WHERE pcil.company_id = $1 AND pcil.case_id = $2 \
           AND pcil.retired_at IS NULL \
           AND i.cancelled_at IS NULL \
           AND i.status <> 'cancelled' \
         ORDER BY pcil.created_at DESC, pcil.id DESC"
    )
    .bind(company_id)
    .bind(case_id)
    .fetch_all(db.pool())
    .await?;
    Ok(rows.into_iter().map(|r| CaseOutputSourceRow {
        link_id: r.0,
        company_id: r.1,
        case_id: r.2,
        issue_id: r.3,
        role: r.4,
        issue_identifier: r.5,
        issue_title: r.6,
        issue_status: r.7,
        issue_source_trust: r.8,
        created_by_run_id: r.9,
        linked_at: r.10,
    }).collect())
}

/// 投影 documents（issue_documents JOIN documents LEFT JOIN document_revisions）。
#[derive(Debug, Clone)]
pub struct CaseOutputDocumentRow {
    pub issue_id: Uuid,
    pub document_id: Uuid,
    pub document_key: String,
    pub document_title: Option<String>,
    pub format: Option<String>,
    pub latest_body: Option<String>,
    pub latest_revision_id: Option<Uuid>,
    pub latest_revision_number: Option<i32>,
    pub source_trust: Option<Value>,
    pub created_by_agent_id: Option<Uuid>,
    pub updated_by_agent_id: Option<Uuid>,
    pub latest_revision_created_by_run_id: Option<Uuid>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

pub async fn list_documents_for_issues(
    db: &Db,
    company_id: Uuid,
    issue_ids: &[Uuid],
) -> sqlx::Result<Vec<CaseOutputDocumentRow>> {
    if issue_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<(Uuid, Uuid, String, Option<String>, Option<String>, Option<String>, Option<Uuid>, Option<i32>, Option<Value>, Option<Uuid>, Option<Uuid>, Option<Uuid>, Timestamp, Timestamp)> = sqlx::query_as(
        "SELECT idoc.issue_id, d.id, idoc.key, d.title, d.format, d.latest_body, \
                d.latest_revision_id, d.latest_revision_number, d.source_trust, \
                d.created_by_agent_id, d.updated_by_agent_id, \
                dr.created_by_run_id, d.created_at, d.updated_at \
         FROM issue_documents idoc \
         INNER JOIN documents d ON d.id = idoc.document_id AND d.company_id = idoc.company_id \
         LEFT JOIN document_revisions dr ON dr.id = d.latest_revision_id AND dr.company_id = d.company_id \
         WHERE idoc.company_id = $1 AND idoc.issue_id = ANY($2::uuid[]) \
           AND idoc.key <> ALL(ARRAY['plan','comments','comments_thread'])"
    )
    .bind(company_id)
    .bind(issue_ids)
    .fetch_all(db.pool())
    .await?;
    Ok(rows.into_iter().map(|r| CaseOutputDocumentRow {
        issue_id: r.0,
        document_id: r.1,
        document_key: r.2,
        document_title: r.3,
        format: r.4,
        latest_body: r.5,
        latest_revision_id: r.6,
        latest_revision_number: r.7,
        source_trust: r.8,
        created_by_agent_id: r.9,
        updated_by_agent_id: r.10,
        latest_revision_created_by_run_id: r.11,
        created_at: r.12,
        updated_at: r.13,
    }).collect())
}

/// 验证 case 存在并返回 pipeline_id。
pub async fn get_case_pipeline_id(
    db: &Db,
    company_id: Uuid,
    case_id: Uuid,
) -> sqlx::Result<Option<Uuid>> {
    let row: Option<(Uuid,)> = sqlx::query_as(
        "SELECT pipeline_id FROM pipeline_cases WHERE company_id = $1 AND id = $2",
    )
    .bind(company_id)
    .bind(case_id)
    .fetch_optional(db.pool())
    .await?;
    Ok(row.map(|(p,)| p))
}

/// 获取公司的 issue_prefix。
pub async fn get_company_issue_prefix(
    db: &Db,
    company_id: Uuid,
) -> sqlx::Result<Option<String>> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT issue_prefix FROM companies WHERE id = $1",
    )
    .bind(company_id)
    .fetch_optional(db.pool())
    .await?;
    Ok(row.map(|(p,)| p))
}

/// \`list_case_outputs\` 的最小可用版本（仅 sources + documents 子集）。
///
/// 与 Node \`pipelineCaseOutputsService.listCaseOutputs\` 1:1 对齐核心 sources + documents
/// 部分，work_products / attachments 子集留 R639.2 轮次。
pub async fn list_case_outputs(
    db: &Db,
    company_id: Uuid,
    case_id: Uuid,
) -> sqlx::Result<Option<PipelineCaseOutputsResponse>> {
    let pipeline_id = get_case_pipeline_id(db, company_id, case_id).await?;
    let issue_prefix = get_company_issue_prefix(db, company_id).await?;
    if pipeline_id.is_none() || issue_prefix.is_none() {
        return Ok(None);
    }
    let prefix = issue_prefix.unwrap_or_default();

    let sources = list_sources(db, company_id, case_id).await?;
    let issue_ids: Vec<Uuid> = sources.iter().map(|s| s.issue_id).collect();
    let document_rows = list_documents_for_issues(db, company_id, &issue_ids).await?;

    let mut items: Vec<PipelineCaseOutputItem> = Vec::new();
    for s in &sources {
        let source_trust = s.issue_source_trust.as_ref().and_then(parse_source_trust);
        let source_path = source_issue_path(&prefix, s.issue_identifier.as_deref(), &s.issue_id.to_string());
        // documents belonging to this source issue
        for d in document_rows.iter().filter(|d| d.issue_id == s.issue_id) {
            let doc_source_trust = d.source_trust.as_ref().and_then(parse_source_trust).or(source_trust.clone());
            let preview = preview_for(d.latest_body.as_deref(), doc_source_trust.as_ref());
            let doc_path = source_document_path(&prefix, s.issue_identifier.as_deref(), &s.issue_id.to_string(), &d.document_key);
            items.push(PipelineCaseOutputItem {
                id: format!("document:{}", d.document_id),
                kind: PipelineCaseOutputItemKind::Document,
                title: d.document_title.clone().unwrap_or_else(|| d.document_key.clone()),
                source_issue_id: s.issue_id.to_string(),
                source_issue_identifier: s.issue_identifier.clone(),
                source_issue_path: source_path.clone(),
                source_issue_title: s.issue_title.clone(),
                source_issue_status: s.issue_status.clone(),
                source_role: s.role.clone(),
                source_trust: doc_source_trust,
                source_run_id: d.latest_revision_created_by_run_id.map(|id| id.to_string()).or_else(|| s.created_by_run_id.map(|id| id.to_string())),
                source_agent_id: d.updated_by_agent_id.map(|id| id.to_string()).or_else(|| d.created_by_agent_id.map(|id| id.to_string())),
                preview,
                created_at: d.created_at.as_datetime().to_rfc3339(),
                updated_at: d.updated_at.as_datetime().to_rfc3339(),
                document_id: Some(d.document_id.to_string()),
                document_key: Some(d.document_key.clone()),
                document_title: d.document_title.clone(),
                format: d.format.clone(),
                latest_revision_id: d.latest_revision_id.map(|id| id.to_string()),
                latest_revision_number: d.latest_revision_number,
                document_path: Some(doc_path),
                work_product_id: None,
                r#type: None,
                provider: None,
                external_id: None,
                url: None,
                status: None,
                review_state: None,
                attachment_id: None,
                filename: None,
                content_type: None,
                content_path: None,
                download_path: None,
                body: None,
            });
        }
    }
    sort_outputs_in_place(&mut items);
    Ok(Some(PipelineCaseOutputsResponse {
        company_id: Some(company_id.to_string()),
        case_id: Some(case_id.to_string()),
        generated_at: Timestamp::now().as_datetime().to_rfc3339(),
        items,
    }))
}

fn parse_source_trust(value: &Value) -> Option<SourceTrustMetadata> {
    serde_json::from_value(value.clone()).ok()
}
