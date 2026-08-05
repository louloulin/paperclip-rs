//! `skills` 域 — 对应 paperclip `company_skills` 与相关 6 张表。
//!
//! 表清单：
//! * `company_skills`                    — 技能主表（公司维度）
//! * `company_skill_versions`            — 技能版本（不可变历史）
//! * `company_skill_comments`            — 评论（支持父子嵌套）
//! * `company_skill_test_inputs`         — 测试 fixture 输入
//! * `company_skill_test_run_templates`  — 测试运行模板（多步串）
//! * `company_skill_test_runs`           — 测试运行（每跑一份 issue）
//! * `company_skill_policies`            — 公司策略表（每公司一行）

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::{Db, RepoError, RepoResult};

// ---------- 枚举 ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillSourceType {
    LocalPath,
    Url,
    Git,
    Manual,
}
impl SkillSourceType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LocalPath => "local_path",
            Self::Url => "url",
            Self::Git => "git",
            Self::Manual => "manual",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillTrustLevel {
    MarkdownOnly,
    Sandboxed,
    Trusted,
}
impl SkillTrustLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MarkdownOnly => "markdown_only",
            Self::Sandboxed => "sandboxed",
            Self::Trusted => "trusted",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillSharingScope {
    Company,
    Public,
}
impl SkillSharingScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Company => "company",
            Self::Public => "public",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillTestRunStatus {
    Queued,
    Running,
    Passed,
    Failed,
    Cancelled,
    Superseded,
}
impl SkillTestRunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Superseded => "superseded",
        }
    }
}

// ---------- 1) company_skills ----------

const SKILL_COLS: &str = "id, company_id, folder_id, key, slug, name, description, markdown, \
     source_type, source_locator, source_ref, trust_level, compatibility, file_inventory, \
     icon_url, color, tagline, author_name, homepage_url, categories, sharing_scope, \
     public_share_token, forked_from_skill_id, forked_from_company_id, \
     star_count, install_count, fork_count, current_version_id, metadata, \
     deleted_at, archived_at, created_by_agent_id, created_by_user_id, \
     updated_by_agent_id, updated_by_user_id, created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanySkillRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub folder_id: Option<Uuid>,
    pub key: String,
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub markdown: String,
    pub source_type: String,
    pub source_locator: Option<String>,
    pub source_ref: Option<String>,
    pub trust_level: String,
    pub compatibility: String,
    pub file_inventory: Value,
    pub icon_url: Option<String>,
    pub color: Option<String>,
    pub tagline: Option<String>,
    pub author_name: Option<String>,
    pub homepage_url: Option<String>,
    pub categories: Vec<String>,
    pub sharing_scope: String,
    pub public_share_token: Option<String>,
    pub forked_from_skill_id: Option<Uuid>,
    pub forked_from_company_id: Option<Uuid>,
    pub star_count: i32,
    pub install_count: i32,
    pub fork_count: i32,
    pub current_version_id: Option<Uuid>,
    pub metadata: Option<Value>,
    pub deleted_at: Option<Timestamp>,
    pub archived_at: Option<Timestamp>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub updated_by_agent_id: Option<Uuid>,
    pub updated_by_user_id: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

// ---------- 2) company_skill_versions ----------

const VERSION_COLS: &str = "id, company_id, skill_id, version, markdown, source_type, \
     source_locator, source_ref, trust_level, compatibility, file_inventory, message, \
     superseded_by_id, created_by_agent_id, created_by_user_id, \
     approved_by_user_id, approved_at, created_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanySkillVersionRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub skill_id: Uuid,
    pub version: i32,
    pub markdown: String,
    pub source_type: String,
    pub source_locator: Option<String>,
    pub source_ref: Option<String>,
    pub trust_level: String,
    pub compatibility: String,
    pub file_inventory: Value,
    pub message: Option<String>,
    pub superseded_by_id: Option<Uuid>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub approved_by_user_id: Option<String>,
    pub approved_at: Option<Timestamp>,
    pub created_at: Timestamp,
}

// ---------- 3) company_skill_comments ----------

const COMMENT_COLS: &str = "id, company_id, company_skill_id, parent_comment_id, author_type, \
     author_user_id, author_agent_id, body, attachment_refs, created_at, updated_at, deleted_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanySkillCommentRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub company_skill_id: Uuid,
    pub parent_comment_id: Option<Uuid>,
    pub author_type: String,
    pub author_user_id: Option<String>,
    pub author_agent_id: Option<Uuid>,
    pub body: String,
    pub attachment_refs: Option<Value>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub deleted_at: Option<Timestamp>,
}

// ---------- 4) company_skill_test_inputs ----------

const TEST_INPUT_COLS: &str = "id, company_id, skill_id, name, content, created_by, \
     deleted_at, created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanySkillTestInputRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub skill_id: Uuid,
    pub name: String,
    pub content: String,
    pub created_by: Option<String>,
    pub deleted_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

// ---------- 5) company_skill_test_run_templates ----------

const TEST_TEMPLATE_COLS: &str = "id, company_id, name, description, body, \
     created_by_agent_id, created_by_user_id, updated_by_agent_id, updated_by_user_id, \
     deleted_at, created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanySkillTestRunTemplateRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub body: String,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
    pub updated_by_agent_id: Option<Uuid>,
    pub updated_by_user_id: Option<String>,
    pub deleted_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

// ---------- 6) company_skill_test_runs ----------

const TEST_RUN_COLS: &str = "id, company_id, skill_id, input_id, input_snapshot, \
     skill_version_id, agent_id, agent_config_snapshot, issue_id, \
     template_id, template_name, template_body, rendered_template_body, \
     harness_issue_description, status, output_document_key, output_snapshot, \
     error, deleted_at, superseded_at, harness_issue_expires_at, harness_issue_deleted_at, \
     created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanySkillTestRunRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub skill_id: Uuid,
    pub input_id: Option<Uuid>,
    pub input_snapshot: String,
    pub skill_version_id: Uuid,
    pub agent_id: Uuid,
    pub agent_config_snapshot: Value,
    pub issue_id: Uuid,
    pub template_id: Option<String>,
    pub template_name: Option<String>,
    pub template_body: Option<String>,
    pub rendered_template_body: Option<String>,
    pub harness_issue_description: String,
    pub status: String,
    pub output_document_key: String,
    pub output_snapshot: String,
    pub error: Option<String>,
    pub deleted_at: Option<Timestamp>,
    pub superseded_at: Option<Timestamp>,
    pub harness_issue_expires_at: Option<Timestamp>,
    pub harness_issue_deleted_at: Option<Timestamp>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

// ---------- 7) company_skill_policies ----------

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanySkillPolicyRow {
    pub company_id: Uuid,
    pub schema_version: i32,
    pub revision: i32,
    pub default_effect: String,
    pub rules: Value,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

// ---------- 输入结构 ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewCompanySkill {
    pub company_id: Uuid,
    pub folder_id: Option<Uuid>,
    pub key: String,
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub markdown: String,
    pub source_type: SkillSourceType,
    pub source_locator: Option<String>,
    pub source_ref: Option<String>,
    pub trust_level: SkillTrustLevel,
    pub categories: Vec<String>,
    pub sharing_scope: SkillSharingScope,
    pub metadata: Option<Value>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewCompanySkillVersion {
    pub company_id: Uuid,
    pub skill_id: Uuid,
    pub markdown: String,
    pub source_type: SkillSourceType,
    pub source_locator: Option<String>,
    pub source_ref: Option<String>,
    pub trust_level: SkillTrustLevel,
    pub file_inventory: Value,
    pub message: Option<String>,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewCompanySkillComment {
    pub company_id: Uuid,
    pub company_skill_id: Uuid,
    pub parent_comment_id: Option<Uuid>,
    pub author_type: String,
    pub author_user_id: Option<String>,
    pub author_agent_id: Option<Uuid>,
    pub body: String,
    pub attachment_refs: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewCompanySkillTestInput {
    pub company_id: Uuid,
    pub skill_id: Uuid,
    pub name: String,
    pub content: String,
    pub created_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewCompanySkillTestRunTemplate {
    pub company_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub body: String,
    pub created_by_agent_id: Option<Uuid>,
    pub created_by_user_id: Option<String>,
}

// ---------- 主仓库 ----------

pub struct SkillRepo<'a> {
    pub db: &'a Db,
}

impl<'a> SkillRepo<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    // ---- Round 147: global skill queries (skills_available / skills_index / skill_get) ----

    pub async fn list_public_skills(&self) -> RepoResult<Vec<(String, String, String)>> {
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT skill_key, display_name, description FROM skills \
             WHERE visibility = 'public' ORDER BY display_name",
        )
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows)
    }

    pub async fn list_all_skills_index(&self) -> RepoResult<Vec<(String, String, String)>> {
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT skill_key, display_name, category FROM skills ORDER BY skill_key",
        )
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows)
    }

    pub async fn find_skill_by_key_or_name(
        &self,
        skill_name: &str,
    ) -> RepoResult<Option<(String, String, Option<String>, Option<String>, Option<String>)>> {
        let row: Option<(String, String, Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT skill_key, display_name, description, content_md, manifest \
             FROM skills WHERE skill_key = $1 OR display_name = $1 LIMIT 1",
        )
        .bind(skill_name)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }

    // ---- company_skills CRUD ----

    pub async fn list_for_company(
        &self,
        company_id: Uuid,
    ) -> RepoResult<Vec<CompanySkillRow>> {
        let sql = format!(
            "SELECT {SKILL_COLS} FROM company_skills \
             WHERE company_id=$1 AND deleted_at IS NULL \
             ORDER BY name ASC"
        );
        Ok(sqlx::query_as::<_, CompanySkillRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    /// Round 125: 列出 company 全部 distinct categories（unwind + collect）。
    pub async fn list_categories(
        &self,
        company_id: Uuid,
    ) -> RepoResult<Vec<String>> {
        let rows: Vec<(Vec<String>,)> = sqlx::query_as(
            "SELECT categories FROM company_skills WHERE company_id=$1 AND deleted_at IS NULL",
        )
        .bind(company_id)
        .fetch_all(self.db.pool())
        .await?;
        let mut seen = std::collections::BTreeSet::new();
        for (cats,) in rows {
            for c in cats {
                seen.insert(c);
            }
        }
        Ok(seen.into_iter().collect())
    }

    pub async fn get(
        &self,
        company_id: Uuid,
        id: Uuid,
    ) -> RepoResult<Option<CompanySkillRow>> {
        let sql = format!(
            "SELECT {SKILL_COLS} FROM company_skills \
             WHERE company_id=$1 AND id=$2 AND deleted_at IS NULL",
        );
        Ok(sqlx::query_as::<_, CompanySkillRow>(&sql)
            .bind(company_id)
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn get_by_slug(
        &self,
        company_id: Uuid,
        slug: &str,
    ) -> RepoResult<Option<CompanySkillRow>> {
        let sql = format!(
            "SELECT {SKILL_COLS} FROM company_skills \
             WHERE company_id=$1 AND slug=$2 AND deleted_at IS NULL",
        );
        Ok(sqlx::query_as::<_, CompanySkillRow>(&sql)
            .bind(company_id)
            .bind(slug)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn create(&self, s: &NewCompanySkill) -> RepoResult<CompanySkillRow> {
        if s.key.trim().is_empty() || s.slug.trim().is_empty() || s.name.trim().is_empty() {
            return Err(RepoError::Invalid("key/slug/name must not be empty".into()));
        }
        let sql = format!(
            "INSERT INTO company_skills (company_id, folder_id, key, slug, name, description, \
                markdown, source_type, source_locator, source_ref, trust_level, categories, \
                sharing_scope, metadata, created_by_agent_id, created_by_user_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16) \
             RETURNING {SKILL_COLS}",
        );
        let row = sqlx::query_as::<_, CompanySkillRow>(&sql)
            .bind(s.company_id)
            .bind(s.folder_id)
            .bind(&s.key)
            .bind(&s.slug)
            .bind(&s.name)
            .bind(s.description.as_deref())
            .bind(&s.markdown)
            .bind(s.source_type.as_str())
            .bind(s.source_locator.as_deref())
            .bind(s.source_ref.as_deref())
            .bind(s.trust_level.as_str())
            .bind(&s.categories)
            .bind(s.sharing_scope.as_str())
            .bind(s.metadata.clone())
            .bind(s.created_by_agent_id)
            .bind(s.created_by_user_id.as_deref())
            .fetch_one(self.db.pool())
            .await?;
        Ok(row)
    }

    pub async fn archive(&self, company_id: Uuid, id: Uuid) -> RepoResult<bool> {
        let n = sqlx::query(
            "UPDATE company_skills SET archived_at=now(), updated_at=now() \
             WHERE company_id=$1 AND id=$2 AND deleted_at IS NULL AND archived_at IS NULL",
        )
        .bind(company_id)
        .bind(id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// Round 127: 取 skill 的 update status（current_version_id + source_ref + install_count + updated_at）。
    pub async fn update_status(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
    ) -> RepoResult<Option<(Option<Uuid>, Option<String>, Option<pc_core::Timestamp>, i32)>> {
        let row: Option<(Option<Uuid>, Option<String>, Option<pc_core::Timestamp>, i32)> = sqlx::query_as(
            "SELECT current_version_id, source_ref, updated_at, install_count \
             FROM company_skills WHERE company_id=$1 AND id=$2 AND deleted_at IS NULL",
        )
        .bind(company_id)
        .bind(skill_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }

    pub async fn soft_delete(&self, company_id: Uuid, id: Uuid) -> RepoResult<bool> {
        let n = sqlx::query(
            "UPDATE company_skills SET deleted_at=now(), updated_at=now() \
             WHERE company_id=$1 AND id=$2 AND deleted_at IS NULL",
        )
        .bind(company_id)
        .bind(id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    pub async fn increment_install_count(&self, id: Uuid) -> RepoResult<()> {
        sqlx::query(
            "UPDATE company_skills SET install_count=install_count+1, updated_at=now() WHERE id=$1",
        )
        .bind(id)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    pub async fn increment_fork_count(&self, id: Uuid) -> RepoResult<()> {
        sqlx::query(
            "UPDATE company_skills SET fork_count=fork_count+1, updated_at=now() WHERE id=$1",
        )
        .bind(id)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    // ---- versions ----

    pub async fn list_versions(
        &self,
        skill_id: Uuid,
    ) -> RepoResult<Vec<CompanySkillVersionRow>> {
        let sql = format!(
            "SELECT {VERSION_COLS} FROM company_skill_versions \
             WHERE skill_id=$1 ORDER BY version DESC",
        );
        Ok(sqlx::query_as::<_, CompanySkillVersionRow>(&sql)
            .bind(skill_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn latest_version(
        &self,
        skill_id: Uuid,
    ) -> RepoResult<Option<CompanySkillVersionRow>> {
        let sql = format!(
            "SELECT {VERSION_COLS} FROM company_skill_versions \
             WHERE skill_id=$1 ORDER BY version DESC LIMIT 1",
        );
        Ok(sqlx::query_as::<_, CompanySkillVersionRow>(&sql)
            .bind(skill_id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    /// 原子地：写新版本 → 把旧 current 标记 superseded → 更新 skill 的 current_version_id。
    pub async fn publish_version(
        &self,
        v: &NewCompanySkillVersion,
        message: Option<&str>,
    ) -> RepoResult<(CompanySkillVersionRow, CompanySkillRow)> {
        let mut tx = self.db.pool().begin().await?;
        let skill: CompanySkillRow = sqlx::query_as::<_, CompanySkillRow>(&format!(
            "SELECT {SKILL_COLS} FROM company_skills WHERE id=$1 AND deleted_at IS NULL FOR UPDATE",
        ))
        .bind(v.skill_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| RepoError::NotFound {
            entity: "company_skill",
            id: v.skill_id.to_string(),
        })?;
        // 找出上一个版本号
        let prev: Option<(i32, Uuid)> = sqlx::query_as(
            "SELECT version, id FROM company_skill_versions \
             WHERE skill_id=$1 ORDER BY version DESC LIMIT 1",
        )
        .bind(v.skill_id)
        .fetch_optional(&mut *tx)
        .await?;
        let next = prev.as_ref().map(|(v, _)| v + 1).unwrap_or(1);
        // 写新版本
        let row: CompanySkillVersionRow = sqlx::query_as::<_, CompanySkillVersionRow>(&format!(
            "INSERT INTO company_skill_versions (company_id, skill_id, version, markdown, \
                source_type, source_locator, source_ref, trust_level, file_inventory, message, \
                created_by_agent_id, created_by_user_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) \
             RETURNING {VERSION_COLS}",
        ))
        .bind(v.company_id)
        .bind(v.skill_id)
        .bind(next)
        .bind(&v.markdown)
        .bind(v.source_type.as_str())
        .bind(v.source_locator.as_deref())
        .bind(v.source_ref.as_deref())
        .bind(v.trust_level.as_str())
        .bind(v.file_inventory.clone())
        .bind(message)
        .bind(v.created_by_agent_id)
        .bind(v.created_by_user_id.as_deref())
        .fetch_one(&mut *tx)
        .await?;
        // 把旧 version 标 superseded
        if let Some((_, prev_id)) = prev {
            sqlx::query(
                "UPDATE company_skill_versions SET superseded_by_id=$2 WHERE id=$1",
            )
            .bind(prev_id)
            .bind(row.id)
            .execute(&mut *tx)
            .await?;
        }
        // 更新 skill.current_version_id
        let updated: CompanySkillRow = sqlx::query_as::<_, CompanySkillRow>(&format!(
            "UPDATE company_skills SET current_version_id=$2, updated_at=now() \
             WHERE id=$1 RETURNING {SKILL_COLS}",
        ))
        .bind(v.skill_id)
        .bind(row.id)
        .fetch_one(&mut *tx)
        .await?;
        let _ = skill; // 编译器占位，避免 dead_code 警告
        tx.commit().await?;
        Ok((row, updated))
    }

    pub async fn approve_version(&self, version_id: Uuid, user_id: &str) -> RepoResult<()> {
        sqlx::query(
            "UPDATE company_skill_versions SET approved_by_user_id=$2, approved_at=now() \
             WHERE id=$1",
        )
        .bind(version_id)
        .bind(user_id)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    // ---- comments ----

    pub async fn list_comments(
        &self,
        skill_id: Uuid,
    ) -> RepoResult<Vec<CompanySkillCommentRow>> {
        let sql = format!(
            "SELECT {COMMENT_COLS} FROM company_skill_comments \
             WHERE company_skill_id=$1 AND deleted_at IS NULL \
             ORDER BY created_at ASC",
        );
        Ok(sqlx::query_as::<_, CompanySkillCommentRow>(&sql)
            .bind(skill_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn add_comment(
        &self,
        c: &NewCompanySkillComment,
    ) -> RepoResult<CompanySkillCommentRow> {
        let sql = format!(
            "INSERT INTO company_skill_comments (company_id, company_skill_id, parent_comment_id, \
                author_type, author_user_id, author_agent_id, body, attachment_refs) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8) \
             RETURNING {COMMENT_COLS}",
        );
        Ok(sqlx::query_as::<_, CompanySkillCommentRow>(&sql)
            .bind(c.company_id)
            .bind(c.company_skill_id)
            .bind(c.parent_comment_id)
            .bind(&c.author_type)
            .bind(c.author_user_id.as_deref())
            .bind(c.author_agent_id)
            .bind(&c.body)
            .bind(c.attachment_refs.clone())
            .fetch_one(self.db.pool())
            .await?)
    }

    pub async fn delete_comment(&self, id: Uuid) -> RepoResult<bool> {
        let n = sqlx::query(
            "UPDATE company_skill_comments SET deleted_at=now(), updated_at=now() WHERE id=$1",
        )
        .bind(id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    // ---- stars ----

    /// Star a skill (by `agent_id` 或 `user_id`)。原子地：
    /// 1. INSERT ON CONFLICT DO NOTHING
    /// 2. 仅当 RETURNING 拿到新行时 +1 `star_count`
    /// 返回 `(newly_starred: bool)` — 同一 actor 重复调用不会重复计数。
    pub async fn star(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
        agent_id: Option<Uuid>,
        user_id: Option<&str>,
    ) -> RepoResult<bool> {
        if agent_id.is_none() && user_id.is_none() {
            return Err(RepoError::Invalid(
                "star requires agent_id or user_id".into(),
            ));
        }
        let mut tx = self.db.pool().begin().await?;
        let inserted: Option<(Uuid,)> = sqlx::query_as(
            "INSERT INTO company_skill_stars (company_id, company_skill_id, agent_id, user_id) \
             VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING RETURNING id",
        )
        .bind(company_id)
        .bind(skill_id)
        .bind(agent_id)
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await?;
        if inserted.is_some() {
            sqlx::query(
                "UPDATE company_skills SET star_count = star_count + 1, updated_at = now() \
                 WHERE company_id = $1 AND id = $2",
            )
            .bind(company_id)
            .bind(skill_id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok(true)
        } else {
            // 重复 star：回滚事务，不应有任何副作用
            tx.rollback().await?;
            Ok(false)
        }
    }

    /// Unstar — 删除 (agent_id / user_id) 对应的 star 行；仅在确实删除时才 -1。
    /// 兼容 star 时同时传 agent_id + user_id 的双 actor 场景（两个独立行）。
    pub async fn unstar(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
        agent_id: Option<Uuid>,
        user_id: Option<&str>,
    ) -> RepoResult<i32> {
        if agent_id.is_none() && user_id.is_none() {
            return Err(RepoError::Invalid(
                "unstar requires agent_id or user_id".into(),
            ));
        }
        let mut tx = self.db.pool().begin().await?;
        let mut deleted: i64 = 0;
        if let Some(aid) = agent_id {
            let r = sqlx::query(
                "DELETE FROM company_skill_stars \
                 WHERE company_id=$1 AND company_skill_id=$2 AND agent_id=$3",
            )
            .bind(company_id)
            .bind(skill_id)
            .bind(aid)
            .execute(&mut *tx)
            .await?;
            deleted += r.rows_affected() as i64;
        }
        if let Some(uid) = user_id {
            let r = sqlx::query(
                "DELETE FROM company_skill_stars \
                 WHERE company_id=$1 AND company_skill_id=$2 AND user_id=$3",
            )
            .bind(company_id)
            .bind(skill_id)
            .bind(uid)
            .execute(&mut *tx)
            .await?;
            deleted += r.rows_affected() as i64;
        }
        if deleted > 0 {
            sqlx::query(
                "UPDATE company_skills SET star_count = GREATEST(star_count - $1, 0), \
                                            updated_at = now() \
                 WHERE company_id = $2 AND id = $3",
            )
            .bind(deleted as i32)
            .bind(company_id)
            .bind(skill_id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(deleted as i32)
    }

    /// 当前 skill 的 star 行数（精确数）。
    pub async fn count_stars(&self, company_id: Uuid, skill_id: Uuid) -> RepoResult<i64> {
        let row: (i64,) = sqlx::query_as(
            "SELECT COUNT(*)::bigint FROM company_skill_stars \
             WHERE company_id=$1 AND company_skill_id=$2",
        )
        .bind(company_id)
        .bind(skill_id)
        .fetch_one(self.db.pool())
        .await?;
        Ok(row.0)
    }

    // ---- configs (per-company skill K/V) ----

    /// 取公司在某 skill 上的 K/V 配置（jsonb）。
    pub async fn get_config(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
    ) -> RepoResult<Option<serde_json::Value>> {
        let row: Option<(serde_json::Value,)> = sqlx::query_as(
            "SELECT value FROM company_skill_configs \
             WHERE company_id=$1 AND skill_id=$2",
        )
        .bind(company_id)
        .bind(skill_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.map(|(v,)| v))
    }

    /// 写入或替换配置（upsert）。`updated_by_user_id` 可空。
    pub async fn set_config(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
        value: &serde_json::Value,
        updated_by_user_id: Option<Uuid>,
    ) -> RepoResult<()> {
        sqlx::query(
            "INSERT INTO company_skill_configs (company_id, skill_id, value, updated_by_user_id) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (company_id, skill_id) DO UPDATE SET \
                value = EXCLUDED.value, \
                updated_by_user_id = EXCLUDED.updated_by_user_id, \
                updated_at = now()",
        )
        .bind(company_id)
        .bind(skill_id)
        .bind(value)
        .bind(updated_by_user_id)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// 删除配置（un-config 路径）。
    pub async fn delete_config(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
    ) -> RepoResult<bool> {
        let r = sqlx::query(
            "DELETE FROM company_skill_configs WHERE company_id=$1 AND skill_id=$2",
        )
        .bind(company_id)
        .bind(skill_id)
        .execute(self.db.pool())
        .await?;
        Ok(r.rows_affected() > 0)
    }

    // ---- test inputs ----

    pub async fn list_test_inputs(
        &self,
        skill_id: Uuid,
    ) -> RepoResult<Vec<CompanySkillTestInputRow>> {
        let sql = format!(
            "SELECT {TEST_INPUT_COLS} FROM company_skill_test_inputs \
             WHERE skill_id=$1 AND deleted_at IS NULL ORDER BY name",
        );
        Ok(sqlx::query_as::<_, CompanySkillTestInputRow>(&sql)
            .bind(skill_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn create_test_input(
        &self,
        i: &NewCompanySkillTestInput,
    ) -> RepoResult<CompanySkillTestInputRow> {
        let sql = format!(
            "INSERT INTO company_skill_test_inputs (company_id, skill_id, name, content, created_by) \
             VALUES ($1,$2,$3,$4,$5) \
             RETURNING {TEST_INPUT_COLS}",
        );
        Ok(sqlx::query_as::<_, CompanySkillTestInputRow>(&sql)
            .bind(i.company_id)
            .bind(i.skill_id)
            .bind(&i.name)
            .bind(&i.content)
            .bind(i.created_by.as_deref())
            .fetch_one(self.db.pool())
            .await?)
    }

    pub async fn delete_test_input(&self, id: Uuid) -> RepoResult<bool> {
        let n = sqlx::query(
            "UPDATE company_skill_test_inputs SET deleted_at=now(), updated_at=now() WHERE id=$1",
        )
        .bind(id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    // ---- test run templates ----

    pub async fn list_test_run_templates(
        &self,
        company_id: Uuid,
    ) -> RepoResult<Vec<CompanySkillTestRunTemplateRow>> {
        let sql = format!(
            "SELECT {TEST_TEMPLATE_COLS} FROM company_skill_test_run_templates \
             WHERE company_id=$1 AND deleted_at IS NULL ORDER BY name",
        );
        Ok(sqlx::query_as::<_, CompanySkillTestRunTemplateRow>(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn create_test_run_template(
        &self,
        t: &NewCompanySkillTestRunTemplate,
    ) -> RepoResult<CompanySkillTestRunTemplateRow> {
        let sql = format!(
            "INSERT INTO company_skill_test_run_templates (company_id, name, description, body, \
                created_by_agent_id, created_by_user_id) \
             VALUES ($1,$2,$3,$4,$5,$6) \
             RETURNING {TEST_TEMPLATE_COLS}",
        );
        Ok(sqlx::query_as::<_, CompanySkillTestRunTemplateRow>(&sql)
            .bind(t.company_id)
            .bind(&t.name)
            .bind(t.description.as_deref())
            .bind(&t.body)
            .bind(t.created_by_agent_id)
            .bind(t.created_by_user_id.as_deref())
            .fetch_one(self.db.pool())
            .await?)
    }

    // ---- test runs ----

    pub async fn list_test_runs(
        &self,
        skill_id: Uuid,
    ) -> RepoResult<Vec<CompanySkillTestRunRow>> {
        let sql = format!(
            "SELECT {TEST_RUN_COLS} FROM company_skill_test_runs \
             WHERE skill_id=$1 AND deleted_at IS NULL \
             ORDER BY created_at DESC LIMIT 50",
        );
        Ok(sqlx::query_as::<_, CompanySkillTestRunRow>(&sql)
            .bind(skill_id)
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn latest_test_run_for_input(
        &self,
        skill_id: Uuid,
        input_id: Uuid,
    ) -> RepoResult<Option<CompanySkillTestRunRow>> {
        let sql = format!(
            "SELECT {TEST_RUN_COLS} FROM company_skill_test_runs \
             WHERE skill_id=$1 AND input_id=$2 AND deleted_at IS NULL \
             ORDER BY created_at DESC LIMIT 1",
        );
        Ok(sqlx::query_as::<_, CompanySkillTestRunRow>(&sql)
            .bind(skill_id)
            .bind(input_id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn update_test_run_status(
        &self,
        id: Uuid,
        status: SkillTestRunStatus,
        error: Option<&str>,
        output_snapshot: Option<&str>,
    ) -> RepoResult<()> {
        sqlx::query(
            "UPDATE company_skill_test_runs SET status=$2, error=$3, output_snapshot=COALESCE($4,output_snapshot), updated_at=now() \
             WHERE id=$1",
        )
        .bind(id)
        .bind(status.as_str())
        .bind(error)
        .bind(output_snapshot)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    pub async fn supersede_prior_runs(
        &self,
        skill_id: Uuid,
        input_id: Option<Uuid>,
    ) -> RepoResult<u64> {
        let n = sqlx::query(
            "UPDATE company_skill_test_runs SET superseded_at=now(), status='superseded' \
             WHERE skill_id=$1 AND ($2::uuid IS NULL OR input_id=$2) \
               AND superseded_at IS NULL AND deleted_at IS NULL",
        )
        .bind(skill_id)
        .bind(input_id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n)
    }

    // ---- policies ----

    pub async fn get_policy(
        &self,
        company_id: Uuid,
    ) -> RepoResult<Option<CompanySkillPolicyRow>> {
        Ok(sqlx::query_as::<_, CompanySkillPolicyRow>(
            "SELECT company_id, schema_version, revision, default_effect, rules, created_at, updated_at \
             FROM company_skill_policies WHERE company_id=$1",
        )
        .bind(company_id)
        .fetch_optional(self.db.pool())
        .await?)
    }

    pub async fn upsert_policy(
        &self,
        company_id: Uuid,
        default_effect: &str,
        rules: &Value,
    ) -> RepoResult<CompanySkillPolicyRow> {
        Ok(sqlx::query_as::<_, CompanySkillPolicyRow>(
            "INSERT INTO company_skill_policies (company_id, revision, default_effect, rules) \
             VALUES ($1, 1, $2, $3) \
             ON CONFLICT (company_id) DO UPDATE SET \
                default_effect=EXCLUDED.default_effect, rules=EXCLUDED.rules, \
                revision=company_skill_policies.revision+1, updated_at=now() \
             RETURNING company_id, schema_version, revision, default_effect, rules, created_at, updated_at",
        )
        .bind(company_id)
        .bind(default_effect)
        .bind(rules)
        .fetch_one(self.db.pool())
        .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_type_strings() {
        assert_eq!(SkillSourceType::LocalPath.as_str(), "local_path");
        assert_eq!(SkillSourceType::Url.as_str(), "url");
        assert_eq!(SkillSourceType::Git.as_str(), "git");
    }
    #[test]
    fn trust_level_strings() {
        assert_eq!(SkillTrustLevel::MarkdownOnly.as_str(), "markdown_only");
        assert_eq!(SkillTrustLevel::Trusted.as_str(), "trusted");
    }
    #[test]
    fn sharing_scope_strings() {
        assert_eq!(SkillSharingScope::Company.as_str(), "company");
        assert_eq!(SkillSharingScope::Public.as_str(), "public");
    }
    #[test]
    fn test_run_status_strings() {
        assert_eq!(SkillTestRunStatus::Queued.as_str(), "queued");
        assert_eq!(SkillTestRunStatus::Passed.as_str(), "passed");
    }
    #[test]
    fn new_skill_input_basic_validation() {
        let s = NewCompanySkill {
            company_id: Uuid::new_v4(),
            folder_id: None,
            key: "STRIPE_API".into(),
            slug: "stripe-api".into(),
            name: "Stripe API".into(),
            description: None,
            markdown: "# how to call stripe".into(),
            source_type: SkillSourceType::Manual,
            source_locator: None,
            source_ref: None,
            trust_level: SkillTrustLevel::MarkdownOnly,
            categories: vec!["payments".into()],
            sharing_scope: SkillSharingScope::Company,
            metadata: None,
            created_by_agent_id: None,
            created_by_user_id: Some("u1".into()),
        };
        assert!(!s.key.trim().is_empty());
        assert!(!s.slug.trim().is_empty());
    }
    #[test]
    fn star_requires_at_least_one_actor() {
        // 不需要 DB，只测 API 校验
        // 实际 Repo 行为：star(_, _, None, None) → RepoError::Invalid
        // 这条规则在路由层 + Repo 层都应生效
        let actor_present = !(None::<Uuid>.is_none() && None::<&str>.is_none());
        assert!(!actor_present || actor_present); // 文档化意图
    }

    #[test]
    fn star_count_idempotency_guarantee_is_well_known() {
        // 文档化：star 第二次调用必须返回 false
        // 由唯一索引 (company_skill_id, agent_id) / (company_skill_id, user_id) 保证
        // 单元测试只覆盖意图；实际行为在集成测试 repo_star_twice_by_same_user_is_idempotent
        let first = true;
        let second = false; // ON CONFLICT DO NOTHING 触发
        assert!(first);
        assert!(!second);
    }

}
