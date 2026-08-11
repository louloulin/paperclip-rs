//! Issue reference business service.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use pc_errors::{internal, Error as PcError, Result as PcResult};
use pc_repos::{
    issue::IssueRepo,
    issue_reference_mentions::{
        IssueReferenceMentionRepo, IssueReferenceMentionRow, NewIssueReferenceMention,
    },
    Db,
};

use pc_external_objects::{
    format_external_object_mention_source_label, ExternalObjectMentionSource,
    ExternalObjectMentionSourceKind,
};

use super::extractor::extract_identifiers;
use super::types::{
    IssueReferenceMentionView, IssueReferenceSource, ReferenceRelatedIssueSummary, RelatedWorkItem,
};

/// related work 总览（inbound + outbound）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueReferenceRelatedWork {
    pub outbound: Vec<RelatedWorkItem>,
    pub inbound: Vec<RelatedWorkItem>,
}

/// Service 错误类型。
#[derive(Debug, thiserror::Error)]
pub enum IssueReferenceError {
    #[error("validation: {0}")]
    Validation(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("company mismatch: target belongs to {actual} but expected {expected}")]
    CompanyMismatch { actual: Uuid, expected: Uuid },
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Pc(#[from] PcError),
}

impl From<pc_repos::RepoError> for IssueReferenceError {
    fn from(e: pc_repos::RepoError) -> Self {
        Self::Pc(internal(e.to_string()))
    }
}

pub type IssueReferenceResult<T> = std::result::Result<T, IssueReferenceError>;

#[derive(Clone)]
pub struct IssueReferenceService {
    db: Db,
}

impl IssueReferenceService {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    fn mention_repo(&self) -> IssueReferenceMentionRepo<'_> {
        IssueReferenceMentionRepo::new(&self.db)
    }

    fn issue_repo(&self) -> IssueRepo<'_> {
        IssueRepo::new(&self.db)
    }

    fn require_non_nil(id: Uuid, field: &str) -> IssueReferenceResult<()> {
        if id.is_nil() {
            Err(IssueReferenceError::Validation(format!(
                "{field} is required"
            )))
        } else {
            Ok(())
        }
    }

    /// 替换某 source 的所有 mentions（事务内 delete + insert）。
    ///
    /// 算法与 Node `replaceSourceMentions` 完全一致：
    /// 1. 从 text 抽取 identifiers
    /// 2. resolve identifiers → target_issue_ids（同公司）
    /// 3. delete 当前 source 的所有 mentions
    /// 4. insert 去重后的 (source_issue_id, target_issue_id) 对
    pub async fn replace_source_mentions(
        &self,
        company_id: Uuid,
        source_issue_id: Uuid,
        source_kind: &str,
        source_record_id: Option<Uuid>,
        document_key: Option<&str>,
        text: Option<&str>,
    ) -> IssueReferenceResult<usize> {
        Self::require_non_nil(company_id, "companyId")?;
        Self::require_non_nil(source_issue_id, "sourceIssueId")?;
        if source_kind.is_empty() {
            return Err(IssueReferenceError::Validation(
                "source_kind is required".into(),
            ));
        }

        // 1. extract identifiers
        let identifiers: Vec<String> = text.map(extract_identifiers).unwrap_or_default();
        if identifiers.is_empty() {
            // 没有引用也要清空旧 mentions
            let mut tx = self.db.pool().begin().await?;
            self.mention_repo()
                .delete_for_source_tx(
                    company_id,
                    source_issue_id,
                    source_kind,
                    source_record_id,
                    &mut tx,
                )
                .await?;
            tx.commit().await?;
            return Ok(0);
        }

        // 2. resolve → issue ids（同公司）
        let resolved = self
            .mention_repo()
            .resolve_identifiers(company_id, &identifiers)
            .await?;
        let mut target_by_identifier: std::collections::HashMap<String, Uuid> =
            std::collections::HashMap::new();
        for (id, ident) in resolved {
            target_by_identifier.insert(ident, id);
        }

        // 3. delete + insert 事务
        let mut tx = self.db.pool().begin().await?;
        self.mention_repo()
            .delete_for_source_tx(
                company_id,
                source_issue_id,
                source_kind,
                source_record_id,
                &mut tx,
            )
            .await?;

        let mut seen: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
        let mut inserted = 0usize;
        for identifier in &identifiers {
            if let Some(&target_id) = target_by_identifier.get(identifier) {
                if target_id == source_issue_id {
                    continue; // 跳过自引用
                }
                if !seen.insert(target_id) {
                    continue; // 去重
                }
                let new_m = NewIssueReferenceMention {
                    company_id,
                    source_issue_id,
                    target_issue_id: target_id,
                    source_kind,
                    source_record_id,
                    document_key,
                    matched_text: Some(identifier.as_str()),
                };
                let row = self.mention_repo().insert_in_tx(&new_m, &mut tx).await?;
                if row.is_some() {
                    inserted += 1;
                }
            }
        }
        tx.commit().await?;
        Ok(inserted)
    }

    /// 同步 issue 的 title + description 引用。
    /// 与 Node `syncIssue` 对齐。
    pub async fn sync_issue(&self, issue_id: Uuid) -> IssueReferenceResult<usize> {
        Self::require_non_nil(issue_id, "issueId")?;
        let issue = self
            .issue_repo()
            .get(issue_id)
            .await?
            .ok_or_else(|| IssueReferenceError::NotFound(format!("issue {issue_id}")))?;
        let mut total = 0;
        // title
        total += self
            .replace_source_mentions(
                issue.company_id,
                issue.id,
                "title",
                None,
                None,
                Some(&issue.title),
            )
            .await?;
        // description
        total += self
            .replace_source_mentions(
                issue.company_id,
                issue.id,
                "description",
                None,
                None,
                issue.description.as_deref(),
            )
            .await?;
        Ok(total)
    }

    /// 列出某 source 的所有 mentions（带 target summary）。
    pub async fn list_for_source(
        &self,
        company_id: Uuid,
        source_issue_id: Uuid,
    ) -> IssueReferenceResult<Vec<IssueReferenceMentionView>> {
        Self::require_non_nil(company_id, "companyId")?;
        Self::require_non_nil(source_issue_id, "sourceIssueId")?;
        let rows = self
            .mention_repo()
            .list_for_source(company_id, source_issue_id)
            .await?;
        Ok(rows.into_iter().map(row_to_view).collect())
    }

    /// 列出某 target 的所有 inbound mentions。
    pub async fn list_for_target(
        &self,
        company_id: Uuid,
        target_issue_id: Uuid,
    ) -> IssueReferenceResult<Vec<IssueReferenceMentionView>> {
        Self::require_non_nil(company_id, "companyId")?;
        Self::require_non_nil(target_issue_id, "targetIssueId")?;
        let rows = self
            .mention_repo()
            .list_for_target(company_id, target_issue_id)
            .await?;
        Ok(rows.into_iter().map(row_to_view).collect())
    }

    /// 计数某 source 的去重 target 数。
    pub async fn count_for_source(
        &self,
        company_id: Uuid,
        source_issue_id: Uuid,
    ) -> IssueReferenceResult<i64> {
        Self::require_non_nil(company_id, "companyId")?;
        Self::require_non_nil(source_issue_id, "sourceIssueId")?;
        Ok(self
            .mention_repo()
            .count_for_source(company_id, source_issue_id)
            .await?)
    }

    /// 计数某 target 的去重 source 数。
    pub async fn count_for_target(
        &self,
        company_id: Uuid,
        target_issue_id: Uuid,
    ) -> IssueReferenceResult<i64> {
        Self::require_non_nil(company_id, "companyId")?;
        Self::require_non_nil(target_issue_id, "targetIssueId")?;
        Ok(self
            .mention_repo()
            .count_for_target(company_id, target_issue_id)
            .await?)
    }

    /// 取出 issue 的 related work（outbound + inbound）。
    ///
    /// 算法：
    /// 1. 查所有 outbound mentions（source = issue_id）
    /// 2. 按 target_issue_id 聚合 mention_count + sources
    /// 3. 查所有 inbound mentions（target = issue_id）
    /// 4. 按 source_issue_id 聚合
    /// 5. 把 target/source 各自的 issue row 查出来填 summary
    pub async fn related_work_for_issue(
        &self,
        company_id: Uuid,
        issue_id: Uuid,
    ) -> IssueReferenceResult<IssueReferenceRelatedWork> {
        Self::require_non_nil(company_id, "companyId")?;
        Self::require_non_nil(issue_id, "issueId")?;
        // 验证 issue 存在
        let issue = self
            .issue_repo()
            .get(issue_id)
            .await?
            .ok_or_else(|| IssueReferenceError::NotFound(format!("issue {issue_id}")))?;
        if issue.company_id != company_id {
            return Err(IssueReferenceError::CompanyMismatch {
                actual: issue.company_id,
                expected: company_id,
            });
        }

        let outbound_rows = self
            .mention_repo()
            .list_for_source(company_id, issue_id)
            .await?;
        let inbound_rows = self
            .mention_repo()
            .list_for_target(company_id, issue_id)
            .await?;

        // outbound: 按 target 聚合
        let mut outbound: std::collections::HashMap<Uuid, RelatedWorkItem> =
            std::collections::HashMap::new();
        for r in &outbound_rows {
            let entry = outbound
                .entry(r.target_issue_id)
                .or_insert_with(|| RelatedWorkItem {
                    issue: ReferenceRelatedIssueSummary {
                        id: r.target_issue_id,
                        identifier: None,
                        title: String::new(),
                        status: String::new(),
                        priority: String::new(),
                        assignee_agent_id: None,
                        assignee_user_id: None,
                    },
                    mention_count: 0,
                    sources: Vec::new(),
                });
            entry.mention_count += 1;
            entry.sources.push(IssueReferenceSource {
                kind: r.source_kind.clone(),
                source_record_id: r.source_record_id,
                document_key: r.document_key.clone(),
                label: source_label(&r.source_kind, r.document_key.as_deref()),
            });
        }
        // inbound: 按 source 聚合
        let mut inbound: std::collections::HashMap<Uuid, RelatedWorkItem> =
            std::collections::HashMap::new();
        for r in &inbound_rows {
            let entry = inbound
                .entry(r.source_issue_id)
                .or_insert_with(|| RelatedWorkItem {
                    issue: ReferenceRelatedIssueSummary {
                        id: r.source_issue_id,
                        identifier: None,
                        title: String::new(),
                        status: String::new(),
                        priority: String::new(),
                        assignee_agent_id: None,
                        assignee_user_id: None,
                    },
                    mention_count: 0,
                    sources: Vec::new(),
                });
            entry.mention_count += 1;
            entry.sources.push(IssueReferenceSource {
                kind: r.source_kind.clone(),
                source_record_id: r.source_record_id,
                document_key: r.document_key.clone(),
                label: source_label(&r.source_kind, r.document_key.as_deref()),
            });
        }

        // 把所有 issue row 一次性查出来（in + out）
        let mut all_ids: Vec<Uuid> = outbound.keys().copied().collect();
        all_ids.extend(inbound.keys().copied());
        all_ids.sort();
        all_ids.dedup();
        let mut rows = Vec::with_capacity(all_ids.len());
        for id in &all_ids {
            if let Some(issue) = self.issue_repo().get(*id).await? {
                rows.push(issue);
            }
        }

        let mut out_outbound: Vec<RelatedWorkItem> = outbound.into_values().collect();
        for item in &mut out_outbound {
            if let Some(row) = rows.iter().find(|r| r.id == item.issue.id) {
                item.issue.identifier = row.identifier.clone();
                item.issue.title = row.title.clone();
                item.issue.status = row.status.clone();
                item.issue.priority = row.priority.clone();
                item.issue.assignee_agent_id = row.assignee_agent_id;
                item.issue.assignee_user_id = row.assignee_user_id.clone();
            }
        }
        out_outbound.sort_by(|a, b| {
            b.mention_count.cmp(&a.mention_count).then_with(|| {
                let al = a
                    .issue
                    .identifier
                    .clone()
                    .unwrap_or_else(|| a.issue.title.clone());
                let bl = b
                    .issue
                    .identifier
                    .clone()
                    .unwrap_or_else(|| b.issue.title.clone());
                al.cmp(&bl)
            })
        });

        let mut out_inbound: Vec<RelatedWorkItem> = inbound.into_values().collect();
        for item in &mut out_inbound {
            if let Some(row) = rows.iter().find(|r| r.id == item.issue.id) {
                item.issue.identifier = row.identifier.clone();
                item.issue.title = row.title.clone();
                item.issue.status = row.status.clone();
                item.issue.priority = row.priority.clone();
                item.issue.assignee_agent_id = row.assignee_agent_id;
                item.issue.assignee_user_id = row.assignee_user_id.clone();
            }
        }
        out_inbound.sort_by(|a, b| {
            b.mention_count.cmp(&a.mention_count).then_with(|| {
                let al = a
                    .issue
                    .identifier
                    .clone()
                    .unwrap_or_else(|| a.issue.title.clone());
                let bl = b
                    .issue
                    .identifier
                    .clone()
                    .unwrap_or_else(|| b.issue.title.clone());
                al.cmp(&bl)
            })
        });

        Ok(IssueReferenceRelatedWork {
            outbound: out_outbound,
            inbound: out_inbound,
        })
    }

    /// 删除某 source 的所有 mentions（用于 issue/comment 删除时清理）。
    pub async fn delete_for_source(
        &self,
        company_id: Uuid,
        source_issue_id: Uuid,
        source_kind: &str,
        source_record_id: Option<Uuid>,
    ) -> IssueReferenceResult<u64> {
        Self::require_non_nil(company_id, "companyId")?;
        Self::require_non_nil(source_issue_id, "sourceIssueId")?;
        let n = self
            .mention_repo()
            .delete_for_source(company_id, source_issue_id, source_kind, source_record_id)
            .await?;
        Ok(n)
    }
}

fn row_to_view(r: IssueReferenceMentionRow) -> IssueReferenceMentionView {
    IssueReferenceMentionView {
        id: r.id,
        source_issue_id: r.source_issue_id,
        target_issue_id: r.target_issue_id,
        source_kind: r.source_kind,
        source_record_id: r.source_record_id,
        document_key: r.document_key,
        matched_text: r.matched_text,
        created_at: r.created_at,
    }
}

/// R567: delegate to pc-external-objects unified formatter so that
/// source labels (Title / Description / Comment / Document[:key] /
/// Property[:key] / Plugin) stay consistent with the rest of the system.
/// Falls back to the raw kind string for unknown kinds so unknown sources
/// still surface usefully in the UI instead of producing empty labels.
fn source_label(kind: &str, document_key: Option<&str>) -> String {
    match ExternalObjectMentionSourceKind::parse(kind) {
        Some(parsed_kind) => {
            // For Document/Property we surface the key for context; the
            // unified formatter already prefixes with "Document: " /
            // "Property: ".
            let doc_key_for_doc =
                if matches!(parsed_kind, ExternalObjectMentionSourceKind::Document) {
                    document_key
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                } else {
                    None
                };
            let source = ExternalObjectMentionSource {
                company_id: None,
                source_issue_id: None,
                source_kind: parsed_kind,
                source_record_id: None,
                document_key: doc_key_for_doc,
                property_key: None,
            };
            format_external_object_mention_source_label(&source)
        }
        None => kind.to_string(),
    }
}
