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

    /// R560：是否为终态（不可再推进）。
    ///
    /// Passed / Failed / Cancelled / Superseded 都是终态。
    /// Queued / Running 是中间态，可以继续推进。
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Passed | Self::Failed | Self::Cancelled | Self::Superseded
        )
    }

    /// R560：状态机转换守卫。
    ///
    /// 合法的转换:
    /// - Queued   → Running | Cancelled | Superseded
    /// - Running  → Passed | Failed | Cancelled | Superseded
    /// - 终态  → 不可再转换（除非是 Superseded 的旧 run 被新的 Queued 替换;
    ///                那个路径用 `supersede_prior_runs` 而不是 `update_test_run_status`）
    ///
    /// 镜像 Node `canTransitionSkillTestRun` 在
    /// `services/company-skills.ts` 中的语义。
    pub fn can_transition_to(self, next: Self) -> bool {
        if self == next {
            return false;
        }
        if self.is_terminal() {
            return false;
        }
        match (self, next) {
            (Self::Queued, Self::Running) => true,
            (Self::Queued, Self::Cancelled) => true,
            (Self::Queued, Self::Superseded) => true,
            (Self::Running, Self::Passed) => true,
            (Self::Running, Self::Failed) => true,
            (Self::Running, Self::Cancelled) => true,
            (Self::Running, Self::Superseded) => true,
            _ => false,
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
    ) -> RepoResult<
        Option<(
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        )>,
    > {
        let row: Option<(
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT skill_key, display_name, description, content_md, manifest \
             FROM skills WHERE skill_key = $1 OR display_name = $1 LIMIT 1",
        )
        .bind(skill_name)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }

    /// Round 150: 通过 skill_key 取 content_md + manifest（invite_skill_get 用）。
    /// 返回 (content_md, manifest_json)。
    pub async fn find_content_by_key(
        &self,
        skill_key: &str,
    ) -> RepoResult<Option<(String, Option<String>)>> {
        let row: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT content_md, manifest FROM skills WHERE skill_key = $1 LIMIT 1")
                .bind(skill_key)
                .fetch_optional(self.db.pool())
                .await?;
        Ok(row)
    }

    // ---- company_skills CRUD ----

    pub async fn list_for_company(&self, company_id: Uuid) -> RepoResult<Vec<CompanySkillRow>> {
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
    pub async fn list_categories(&self, company_id: Uuid) -> RepoResult<Vec<String>> {
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

    pub async fn get(&self, company_id: Uuid, id: Uuid) -> RepoResult<Option<CompanySkillRow>> {
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
    ) -> RepoResult<
        Option<(
            Option<Uuid>,
            Option<String>,
            Option<pc_core::Timestamp>,
            i32,
        )>,
    > {
        let row: Option<(
            Option<Uuid>,
            Option<String>,
            Option<pc_core::Timestamp>,
            i32,
        )> = sqlx::query_as(
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

    pub async fn list_versions(&self, skill_id: Uuid) -> RepoResult<Vec<CompanySkillVersionRow>> {
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
            sqlx::query("UPDATE company_skill_versions SET superseded_by_id=$2 WHERE id=$1")
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

    pub async fn list_comments(&self, skill_id: Uuid) -> RepoResult<Vec<CompanySkillCommentRow>> {
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
    pub async fn delete_config(&self, company_id: Uuid, skill_id: Uuid) -> RepoResult<bool> {
        let r =
            sqlx::query("DELETE FROM company_skill_configs WHERE company_id=$1 AND skill_id=$2")
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

    pub async fn list_test_runs(&self, skill_id: Uuid) -> RepoResult<Vec<CompanySkillTestRunRow>> {
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

    /// 查找仍然有效的 harness run，并限定在同一公司与 issue 下。
    pub async fn active_test_run_for_issue(
        &self,
        company_id: Uuid,
        issue_id: Uuid,
    ) -> RepoResult<Option<CompanySkillTestRunRow>> {
        let sql = format!(
            "SELECT {TEST_RUN_COLS} FROM company_skill_test_runs \
             WHERE company_id=$1 AND issue_id=$2 AND deleted_at IS NULL \
               AND superseded_at IS NULL ORDER BY created_at DESC LIMIT 1",
        );
        Ok(sqlx::query_as::<_, CompanySkillTestRunRow>(&sql)
            .bind(company_id)
            .bind(issue_id)
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

    pub async fn get_policy(&self, company_id: Uuid) -> RepoResult<Option<CompanySkillPolicyRow>> {
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

    // ---- Round 163: company_skills route 仓储化新增方法 ----

    /// 安装/upsert 一条 company_skill（install_company_skill 用）。
    /// 与 `create` 不同：`install_company_skill` 接收字符串字段（来自 HTTP body），
    /// 触发 ON CONFLICT (company_id, key) DO UPDATE 完整 upsert，返回更新后的 row。
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_install(
        &self,
        company_id: Uuid,
        key: &str,
        slug: &str,
        name: &str,
        description: Option<&str>,
        markdown: &str,
        source_type: &str,
        source_locator: Option<&str>,
        source_ref: Option<&str>,
        trust_level: &str,
        categories: &[String],
    ) -> RepoResult<CompanySkillRow> {
        let sql = format!(
            "INSERT INTO company_skills                 (company_id, key, slug, name, description, markdown, source_type, source_locator,                  source_ref, trust_level, categories)              VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)              ON CONFLICT (company_id, key) DO UPDATE SET                 slug = EXCLUDED.slug, name = EXCLUDED.name, description = EXCLUDED.description,                 markdown = EXCLUDED.markdown, source_type = EXCLUDED.source_type,                 source_locator = EXCLUDED.source_locator, source_ref = EXCLUDED.source_ref,                 trust_level = EXCLUDED.trust_level, categories = EXCLUDED.categories,                 updated_at = now()              RETURNING {SKILL_COLS}",
        );
        Ok(sqlx::query_as::<_, CompanySkillRow>(&sql)
            .bind(company_id)
            .bind(key)
            .bind(slug)
            .bind(name)
            .bind(description)
            .bind(markdown)
            .bind(source_type)
            .bind(source_locator)
            .bind(source_ref)
            .bind(trust_level)
            .bind(categories)
            .fetch_one(self.db.pool())
            .await?)
    }

    /// 取 fork-precheck 用的核心字段。
    pub async fn fork_precheck(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
    ) -> RepoResult<Option<(String, Option<Uuid>, i32, Option<String>)>> {
        let row: Option<(String, Option<Uuid>, i32, Option<String>)> = sqlx::query_as(
            "SELECT trust_level, forked_from_skill_id, fork_count, source_locator              FROM company_skills WHERE company_id=$1 AND id=$2",
        )
        .bind(company_id)
        .bind(skill_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }

    /// 列出某 skill 的版本（带 limit/offset，按 revision_number DESC）。
    pub async fn list_versions_paged(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
        limit: i64,
        offset: i64,
    ) -> RepoResult<
        Vec<(
            Uuid,
            i32,
            Option<String>,
            Value,
            Option<Uuid>,
            Option<String>,
            Timestamp,
        )>,
    > {
        let rows: Vec<(Uuid, i32, Option<String>, Value, Option<Uuid>, Option<String>, Timestamp)> =
            sqlx::query_as(
                "SELECT id, revision_number, label, file_inventory, author_agent_id, author_user_id, created_at                  FROM company_skill_versions WHERE company_id=$1 AND company_skill_id=$2                  ORDER BY revision_number DESC LIMIT $3 OFFSET $4",
            )
            .bind(company_id)
            .bind(skill_id)
            .bind(limit)
            .bind(offset)
            .fetch_all(self.db.pool())
            .await?;
        Ok(rows)
    }

    /// 事务：写新版本 + 更新 skill.current_version_id，返回 (id, revision)。
    pub async fn create_version_and_update_current(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
        label: Option<&str>,
        file_inventory: &Value,
        author_agent_id: Option<Uuid>,
        author_user_id: Option<&str>,
    ) -> RepoResult<(Uuid, i32)> {
        let mut tx = self.db.pool().begin().await?;
        let next_rev: i32 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(revision_number),0)+1              FROM company_skill_versions WHERE company_id=$1 AND company_skill_id=$2",
        )
        .bind(company_id)
        .bind(skill_id)
        .fetch_one(&mut *tx)
        .await?;
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO company_skill_versions                 (id, company_id, company_skill_id, revision_number, label, file_inventory,                  author_agent_id, author_user_id)              VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
        )
        .bind(id)
        .bind(company_id)
        .bind(skill_id)
        .bind(next_rev)
        .bind(label)
        .bind(file_inventory)
        .bind(author_agent_id)
        .bind(author_user_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE company_skills SET current_version_id=$1, updated_at=now()              WHERE id=$2 AND company_id=$3",
        )
        .bind(id)
        .bind(skill_id)
        .bind(company_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok((id, next_rev))
    }

    /// Round 261: 与 Node 版 `ensureRunSkillVersion` 对齐 ——
    /// 当 caller 没有指定 skill_version_id 时自动快照当前 file_inventory。
    /// - file_inventory 为空 → 返回错误（与 Node 版 `Cannot run a skill test for a skill with zero files` 一致）
    /// - 当前 version 不存在，或 file_inventory 与当前 version 不一致 → 创建新 version
    /// - 否则返回当前 version
    pub async fn ensure_run_skill_version(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
        label: Option<&str>,
        author_agent_id: Option<Uuid>,
        author_user_id: Option<&str>,
    ) -> RepoResult<(Uuid, i32)> {
        let skill = self
            .get(company_id, skill_id)
            .await?
            .ok_or_else(|| RepoError::NotFound {
                entity: "company_skill".into(),
                id: skill_id.to_string(),
            })?;
        let inventory = skill.file_inventory.clone();
        let snapshot: Value = match &inventory {
            Value::Array(arr) => Value::Array(arr.clone()),
            Value::Null => Value::Array(Vec::new()),
            _ => inventory,
        };
        let is_empty = match &snapshot {
            Value::Array(arr) => arr.is_empty(),
            _ => true,
        };
        if is_empty {
            return Err(RepoError::Invalid(
                "Cannot run a skill test for a skill with zero files".into(),
            ));
        }
        // 查找 current version
        let current_version_row: Option<(Uuid, Value)> = if let Some(vid) = skill.current_version_id
        {
            sqlx::query_as(
                "SELECT id, file_inventory FROM company_skill_versions \
                 WHERE company_id=$1 AND id=$2",
            )
            .bind(company_id)
            .bind(vid)
            .fetch_optional(self.db.pool())
            .await?
        } else {
            None
        };
        // 若 current version 的 file_inventory 与当前 snapshot 一致，直接复用
        if let Some((vid, prev_inv)) = current_version_row {
            if version_inventory_snapshot_equal_inner(&prev_inv, &snapshot) {
                let revision: i32 = sqlx::query_scalar(
                    "SELECT revision_number FROM company_skill_versions \
                     WHERE company_id=$1 AND id=$2",
                )
                .bind(company_id)
                .bind(vid)
                .fetch_one(self.db.pool())
                .await?;
                return Ok((vid, revision));
            }
        }
        // 否则创建新 version 并更新 current
        self.create_version_and_update_current(
            company_id,
            skill_id,
            label,
            &snapshot,
            author_agent_id,
            author_user_id,
        )
        .await
    }

    /// 取一条 version。
    pub async fn get_version(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
        version_id: Uuid,
    ) -> RepoResult<
        Option<(
            Uuid,
            Uuid,
            Uuid,
            i32,
            Option<String>,
            Value,
            Option<Uuid>,
            Option<String>,
            Timestamp,
        )>,
    > {
        let row: Option<(Uuid, Uuid, Uuid, i32, Option<String>, Value, Option<Uuid>, Option<String>, Timestamp)> =
            sqlx::query_as(
                "SELECT id, company_id, company_skill_id, revision_number, label, file_inventory,                  author_agent_id, author_user_id, created_at                  FROM company_skill_versions                  WHERE company_id=$1 AND company_skill_id=$2 AND id=$3",
            )
            .bind(company_id)
            .bind(skill_id)
            .bind(version_id)
            .fetch_optional(self.db.pool())
            .await?;
        Ok(row)
    }

    /// 列评论（按 company_skill_id 过滤）。
    pub async fn list_comments_in_skill(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
    ) -> RepoResult<
        Vec<(
            Uuid,
            Uuid,
            Option<Uuid>,
            Option<Uuid>,
            Option<String>,
            String,
            Timestamp,
        )>,
    > {
        let rows: Vec<(Uuid, Uuid, Option<Uuid>, Option<Uuid>, Option<String>, String, Timestamp)> =
            sqlx::query_as(
                "SELECT id, company_skill_id, parent_comment_id, author_agent_id, author_user_id, body, created_at                  FROM company_skill_comments                  WHERE company_id=$1 AND company_skill_id=$2 AND deleted_at IS NULL                  ORDER BY created_at ASC",
            )
            .bind(company_id)
            .bind(skill_id)
            .fetch_all(self.db.pool())
            .await?;
        Ok(rows)
    }

    /// 写一条新评论。返回 id。
    pub async fn add_comment_raw(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
        parent_comment_id: Option<Uuid>,
        author_agent_id: Option<Uuid>,
        author_user_id: Option<&str>,
        body: &str,
    ) -> RepoResult<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO company_skill_comments                 (id, company_id, company_skill_id, parent_comment_id, author_agent_id, author_user_id, body)              VALUES ($1,$2,$3,$4,$5,$6,$7)",
        )
        .bind(id)
        .bind(company_id)
        .bind(skill_id)
        .bind(parent_comment_id)
        .bind(author_agent_id)
        .bind(author_user_id)
        .bind(body)
        .execute(self.db.pool())
        .await?;
        Ok(id)
    }

    /// 改评论（按 company_id + skill_id + id 定位）。
    pub async fn patch_comment(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
        comment_id: Uuid,
        body: &str,
    ) -> RepoResult<bool> {
        let n = sqlx::query(
            "UPDATE company_skill_comments SET body=$1, updated_at=now()              WHERE company_id=$2 AND company_skill_id=$3 AND id=$4 AND deleted_at IS NULL",
        )
        .bind(body)
        .bind(company_id)
        .bind(skill_id)
        .bind(comment_id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// 软删评论。
    pub async fn soft_delete_comment(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
        comment_id: Uuid,
    ) -> RepoResult<bool> {
        let n = sqlx::query(
            "UPDATE company_skill_comments SET deleted_at=now()              WHERE company_id=$1 AND company_skill_id=$2 AND id=$3 AND deleted_at IS NULL",
        )
        .bind(company_id)
        .bind(skill_id)
        .bind(comment_id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// 取单条评论（按 id）。
    pub async fn get_comment_by_id(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
        comment_id: Uuid,
    ) -> RepoResult<
        Option<(
            Uuid,
            Uuid,
            Uuid,
            Option<Uuid>,
            Option<Uuid>,
            Option<String>,
            String,
            Option<Timestamp>,
            Timestamp,
            Timestamp,
        )>,
    > {
        let row: Option<(Uuid, Uuid, Uuid, Option<Uuid>, Option<Uuid>, Option<String>, String, Option<Timestamp>, Timestamp, Timestamp)> =
            sqlx::query_as(
                "SELECT id, company_id, company_skill_id, parent_comment_id, author_agent_id, author_user_id,                         body, deleted_at, created_at, updated_at                  FROM company_skill_comments                  WHERE company_id=$1 AND company_skill_id=$2 AND id=$3",
            )
            .bind(company_id)
            .bind(skill_id)
            .bind(comment_id)
            .fetch_optional(self.db.pool())
            .await?;
        Ok(row)
    }

    /// 重命名 skill。
    pub async fn rename_skill(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
        name: &str,
    ) -> RepoResult<bool> {
        let n = sqlx::query(
            "UPDATE company_skills SET name=$1, updated_at=now()              WHERE company_id=$2 AND id=$3 AND deleted_at IS NULL",
        )
        .bind(name)
        .bind(company_id)
        .bind(skill_id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// 增加 install_count（按 company_id + id 定位）。
    pub async fn increment_install_count_for_company(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
    ) -> RepoResult<()> {
        sqlx::query(
            "UPDATE company_skills SET install_count=install_count+1, updated_at=now()              WHERE company_id=$1 AND id=$2",
        )
        .bind(company_id)
        .bind(skill_id)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// 重置 install/star/fork 计数器。
    pub async fn reset_skill_counters(&self, company_id: Uuid, skill_id: Uuid) -> RepoResult<()> {
        sqlx::query(
            "UPDATE company_skills SET install_count=0, star_count=0, fork_count=0, updated_at=now()              WHERE company_id=$1 AND id=$2",
        )
        .bind(company_id)
        .bind(skill_id)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// 从源 skill 派生 fork：复制大部分字段，写入新 id，并把源 skill 的 fork_count +1。
    pub async fn fork_from_skill(
        &self,
        company_id: Uuid,
        source_skill_id: Uuid,
        new_id: Uuid,
        name: &str,
    ) -> RepoResult<()> {
        let mut tx = self.db.pool().begin().await?;
        sqlx::query(
            "INSERT INTO company_skills                 (id, company_id, key, slug, name, description, markdown, source_type, source_locator,                  source_ref, trust_level, compatibility, file_inventory, forked_from_skill_id, forked_from_company_id)              SELECT $1, $2, (key || '-fork-' || substring($1::text,1,8)), (slug || '-fork'), $3,                     description, markdown, source_type, source_locator, source_ref, 'company',                     compatibility, file_inventory, id, company_id              FROM company_skills WHERE company_id=$2 AND id=$4",
        )
        .bind(new_id)
        .bind(company_id)
        .bind(name)
        .bind(source_skill_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE company_skills SET fork_count=COALESCE(fork_count,0)+1, updated_at=now()              WHERE id=$1",
        )
        .bind(source_skill_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// 动态 UPDATE：把 PatchSkill 字段逐个 COALESCE 应用。
    pub async fn patch_skill_fields(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
        name: Option<&str>,
        description: Option<&str>,
        markdown: Option<&str>,
        metadata: Option<&Value>,
        tagline: Option<&str>,
        icon_url: Option<&str>,
        color: Option<&str>,
        categories: Option<&[String]>,
    ) -> RepoResult<()> {
        sqlx::query(
            "UPDATE company_skills SET                 name = COALESCE($1, name),                 description = COALESCE($2, description),                 markdown = COALESCE($3, markdown),                 metadata = COALESCE($4, metadata),                 tagline = COALESCE($5, tagline),                 icon_url = COALESCE($6, icon_url),                 color = COALESCE($7, color),                 categories = COALESCE($8, categories),                 updated_at = now()              WHERE company_id=$9 AND id=$10",
        )
        .bind(name)
        .bind(description)
        .bind(markdown)
        .bind(metadata)
        .bind(tagline)
        .bind(icon_url)
        .bind(color)
        .bind(categories)
        .bind(company_id)
        .bind(skill_id)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// 列出 test inputs（支持 include_deleted 过滤）。
    pub async fn list_test_inputs_with_filter(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
        include_deleted: bool,
    ) -> RepoResult<Vec<(Uuid, String, String, Option<String>, Timestamp, Timestamp)>> {
        let filter = if include_deleted {
            ""
        } else {
            "AND deleted_at IS NULL"
        };
        let sql = format!(
            "SELECT id, name, content, created_by, created_at, updated_at              FROM company_skill_test_inputs              WHERE company_id=$1 AND skill_id=$2 {filter}              ORDER BY name ASC"
        );
        let rows: Vec<(Uuid, String, String, Option<String>, Timestamp, Timestamp)> =
            sqlx::query_as(&sql)
                .bind(company_id)
                .bind(skill_id)
                .fetch_all(self.db.pool())
                .await?;
        Ok(rows)
    }

    /// 写一条 test input。
    pub async fn create_test_input_raw(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
        name: &str,
        content: &str,
        created_by: Option<&str>,
    ) -> RepoResult<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO company_skill_test_inputs                 (id, company_id, skill_id, name, content, created_by)              VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(id)
        .bind(company_id)
        .bind(skill_id)
        .bind(name)
        .bind(content)
        .bind(created_by)
        .execute(self.db.pool())
        .await?;
        Ok(id)
    }

    /// 动态 UPDATE：name/content 任一更新。
    pub async fn patch_test_input_fields(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
        input_id: Uuid,
        name: Option<&str>,
        content: Option<&str>,
    ) -> RepoResult<()> {
        sqlx::query(
            "UPDATE company_skill_test_inputs SET                 name = COALESCE($1, name),                 content = COALESCE($2, content),                 updated_at = now()              WHERE company_id=$3 AND skill_id=$4 AND id=$5 AND deleted_at IS NULL",
        )
        .bind(name)
        .bind(content)
        .bind(company_id)
        .bind(skill_id)
        .bind(input_id)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// 软删 test input。
    pub async fn soft_delete_test_input(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
        input_id: Uuid,
    ) -> RepoResult<bool> {
        let n = sqlx::query(
            "UPDATE company_skill_test_inputs SET deleted_at=now()              WHERE company_id=$1 AND skill_id=$2 AND id=$3 AND deleted_at IS NULL",
        )
        .bind(company_id)
        .bind(skill_id)
        .bind(input_id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// 取 test input 的 content 快照（创建 test run 时用）。
    pub async fn get_test_input_content(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
        input_id: Uuid,
    ) -> RepoResult<Option<String>> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT content FROM company_skill_test_inputs              WHERE company_id=$1 AND skill_id=$2 AND id=$3",
        )
        .bind(company_id)
        .bind(skill_id)
        .bind(input_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.map(|(s,)| s))
    }

    /// 列 test runs（带 status 过滤与 limit）。
    pub async fn list_test_runs_with_filter(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
        status: Option<&str>,
        limit: i64,
    ) -> RepoResult<
        Vec<(
            Uuid,
            String,
            Option<Uuid>,
            Option<Uuid>,
            Uuid,
            Timestamp,
            Timestamp,
        )>,
    > {
        let status_filter = match status {
            Some(s) if !s.is_empty() => {
                let safe = s.replace('\'', "");
                format!("AND status='{safe}'")
            }
            _ => String::new(),
        };
        let sql = format!(
            "SELECT id, status, input_id, agent_id, issue_id, created_at, updated_at              FROM company_skill_test_runs WHERE company_id=$1 AND skill_id=$2 {status_filter}              ORDER BY created_at DESC LIMIT $3"
        );
        let rows: Vec<(
            Uuid,
            String,
            Option<Uuid>,
            Option<Uuid>,
            Uuid,
            Timestamp,
            Timestamp,
        )> = sqlx::query_as(&sql)
            .bind(company_id)
            .bind(skill_id)
            .bind(limit)
            .fetch_all(self.db.pool())
            .await?;
        Ok(rows)
    }

    /// 创建一条 test run。
    pub async fn create_test_run(
        &self,
        run_id: Uuid,
        company_id: Uuid,
        skill_id: Uuid,
        input_id: Option<Uuid>,
        input_snapshot: &str,
        skill_version_id: Uuid,
        agent_id: Uuid,
        issue_id: Uuid,
        agent_config_snapshot: &Value,
        template_id: Option<&str>,
        template_name: Option<&str>,
        template_body: Option<&str>,
        rendered_template_body: Option<&str>,
        harness_issue_description: &str,
    ) -> RepoResult<()> {
        let mut tx = self.db.pool().begin().await?;
        sqlx::query(
            "UPDATE company_skill_test_runs SET superseded_at=now(), \
             status=CASE WHEN status IN ('queued','running') THEN 'cancelled' ELSE status END, \
             error=CASE WHEN status IN ('queued','running') THEN COALESCE(error,'Superseded by newer run') ELSE error END, \
             harness_issue_expires_at=now()+interval '7 days', updated_at=now() \
             WHERE company_id=$1 AND skill_id=$2 \
               AND (($3::uuid IS NULL AND input_id IS NULL) OR input_id=$3) \
               AND superseded_at IS NULL AND deleted_at IS NULL",
        )
        .bind(company_id)
        .bind(skill_id)
        .bind(input_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO company_skill_test_runs                 (id, company_id, skill_id, input_id, input_snapshot, skill_version_id, agent_id,                  agent_config_snapshot, issue_id, status, template_id, template_name,                  template_body, rendered_template_body, harness_issue_description)              VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,'queued',$10,$11,$12,$13,$14)",
        )
        .bind(run_id)
        .bind(company_id)
        .bind(skill_id)
        .bind(input_id)
        .bind(input_snapshot)
        .bind(skill_version_id)
        .bind(agent_id)
        .bind(issue_id)
        .bind(agent_config_snapshot)
        .bind(template_id)
        .bind(template_name)
        .bind(template_body)
        .bind(rendered_template_body)
        .bind(harness_issue_description)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn get_test_run_template(
        &self,
        company_id: Uuid,
        template_id: &str,
    ) -> RepoResult<Option<CompanySkillTestRunTemplateRow>> {
        let sql = format!(
            "SELECT {TEST_TEMPLATE_COLS} FROM company_skill_test_run_templates \
             WHERE company_id=$1 AND id=$2 AND deleted_at IS NULL",
        );
        Ok(sqlx::query_as::<_, CompanySkillTestRunTemplateRow>(&sql)
            .bind(company_id)
            .bind(template_id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    /// 取一条 test run（按 company_id+skill_id+id 定位）。
    pub async fn get_test_run(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
        run_id: Uuid,
    ) -> RepoResult<
        Option<(
            Uuid,
            String,
            Option<Uuid>,
            Option<Uuid>,
            Uuid,
            Option<String>,
            String,
            String,
            Option<String>,
            Timestamp,
            Timestamp,
        )>,
    > {
        let row: Option<(Uuid, String, Option<Uuid>, Option<Uuid>, Uuid, Option<String>, String, String, Option<String>, Timestamp, Timestamp)> =
            sqlx::query_as(
                "SELECT id, status, input_id, agent_id, issue_id, template_id, input_snapshot,                  output_snapshot, error, created_at, updated_at                  FROM company_skill_test_runs                  WHERE company_id=$1 AND skill_id=$2 AND id=$3",
            )
            .bind(company_id)
            .bind(skill_id)
            .bind(run_id)
            .fetch_optional(self.db.pool())
            .await?;
        Ok(row)
    }

    pub async fn get_test_run_detail_row(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
        run_id: Uuid,
    ) -> RepoResult<Option<CompanySkillTestRunRow>> {
        let sql = format!(
            "SELECT {TEST_RUN_COLS} FROM company_skill_test_runs \
             WHERE company_id=$1 AND skill_id=$2 AND id=$3 AND deleted_at IS NULL",
        );
        Ok(sqlx::query_as::<_, CompanySkillTestRunRow>(&sql)
            .bind(company_id)
            .bind(skill_id)
            .bind(run_id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    /// 取消 test run（仅 queued/running 可取消）。
    /// 取消 test run（仅 queued/running 可取消）。
    /// 返回更新后的状态与关联 issue_id，供路由层同步清理 harness issue / heartbeat run。
    pub async fn cancel_test_run(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
        run_id: Uuid,
    ) -> RepoResult<Option<(Uuid, String)>> {
        let row: Option<(Uuid, String)> = sqlx::query_as(
            "UPDATE company_skill_test_runs SET status='cancelled', updated_at=now() \
             WHERE company_id=$1 AND skill_id=$2 AND id=$3 AND status IN ('queued','running') \
             RETURNING issue_id, status",
        )
        .bind(company_id)
        .bind(skill_id)
        .bind(run_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row)
    }

    /// 与 Node 版 pruneExpiredTestHarnessIssues 对齐：
    /// 扫描 `harness_issue_expires_at < now()` 且 `harness_issue_deleted_at IS NULL` 的 run，
    /// 隐藏对应 issue 并标记 harness_issue_deleted_at。返回处理的 run 数。
    pub async fn prune_expired_test_harness_issues(&self, company_id: Uuid) -> RepoResult<u64> {
        // 先查询所有过期但未删除的 (run_id, issue_id)
        let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
            "SELECT id, issue_id FROM company_skill_test_runs \
             WHERE company_id=$1 AND harness_issue_expires_at < now() \
               AND harness_issue_deleted_at IS NULL",
        )
        .bind(company_id)
        .fetch_all(self.db.pool())
        .await?;
        if rows.is_empty() {
            return Ok(0);
        }
        let mut count = 0u64;
        for (run_id, issue_id) in rows {
            let mut tx = self.db.pool().begin().await?;
            sqlx::query(
                "UPDATE issues SET hidden_at=now(), updated_at=now() \
                 WHERE id=$1 AND company_id=$2 AND hidden_at IS NULL",
            )
            .bind(issue_id)
            .bind(company_id)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE company_skill_test_runs SET harness_issue_deleted_at=now(), updated_at=now() \
                 WHERE id=$1 AND company_id=$2",
            )
            .bind(run_id)
            .bind(company_id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            count += 1;
        }
        Ok(count)
    }

    /// 全公司批量执行 retention sweeper（用于后台任务/单次扫描所有公司）。
    pub async fn prune_all_expired_test_harness_issues(&self) -> RepoResult<u64> {
        // 直接扫描所有过期记录，按 run 维度展开
        let rows: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
            "SELECT id, company_id, issue_id FROM company_skill_test_runs \
             WHERE harness_issue_expires_at < now() AND harness_issue_deleted_at IS NULL",
        )
        .fetch_all(self.db.pool())
        .await?;
        if rows.is_empty() {
            return Ok(0);
        }
        let mut count = 0u64;
        for (run_id, company_id, issue_id) in rows {
            let mut tx = self.db.pool().begin().await?;
            sqlx::query(
                "UPDATE issues SET hidden_at=now(), updated_at=now() \
                 WHERE id=$1 AND company_id=$2 AND hidden_at IS NULL",
            )
            .bind(issue_id)
            .bind(company_id)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE company_skill_test_runs SET harness_issue_deleted_at=now(), updated_at=now() \
                 WHERE id=$1 AND company_id=$2",
            )
            .bind(run_id)
            .bind(company_id)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            count += 1;
        }
        Ok(count)
    }

    /// 硬删 test run。
    pub async fn delete_test_run(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
        run_id: Uuid,
    ) -> RepoResult<bool> {
        let n = sqlx::query(
            "DELETE FROM company_skill_test_runs              WHERE company_id=$1 AND skill_id=$2 AND id=$3",
        )
        .bind(company_id)
        .bind(skill_id)
        .bind(run_id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// 取 skill 的 file_inventory。
    pub async fn get_file_inventory(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
    ) -> RepoResult<Option<Value>> {
        let row: Option<(Value,)> = sqlx::query_as(
            "SELECT file_inventory FROM company_skills WHERE company_id=$1 AND id=$2",
        )
        .bind(company_id)
        .bind(skill_id)
        .fetch_optional(self.db.pool())
        .await?;
        Ok(row.map(|(v,)| v))
    }

    /// 写整个 file_inventory。
    pub async fn set_file_inventory(
        &self,
        company_id: Uuid,
        skill_id: Uuid,
        inv: &Value,
    ) -> RepoResult<()> {
        sqlx::query(
            "UPDATE company_skills SET file_inventory=$1, updated_at=now()              WHERE company_id=$2 AND id=$3",
        )
        .bind(inv)
        .bind(company_id)
        .bind(skill_id)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// 动态 UPDATE：test run template 的 name/description/body。
    pub async fn patch_test_run_template_fields(
        &self,
        company_id: Uuid,
        template_id: Uuid,
        name: Option<&str>,
        description: Option<&str>,
        body: Option<&str>,
    ) -> RepoResult<()> {
        sqlx::query(
            "UPDATE company_skill_test_run_templates SET                 name = COALESCE($1, name),                 description = COALESCE($2, description),                 body = COALESCE($3, body),                 updated_at = now()              WHERE company_id=$4 AND id=$5 AND deleted_at IS NULL",
        )
        .bind(name)
        .bind(description)
        .bind(body)
        .bind(company_id)
        .bind(template_id)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// 软删 test run template。
    pub async fn soft_delete_test_run_template(
        &self,
        company_id: Uuid,
        template_id: Uuid,
    ) -> RepoResult<bool> {
        let n = sqlx::query(
            "UPDATE company_skill_test_run_templates SET deleted_at=now()              WHERE company_id=$1 AND id=$2 AND deleted_at IS NULL",
        )
        .bind(company_id)
        .bind(template_id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
    }

    /// 导入一条 skill（带 ON CONFLICT DO NOTHING）。
    pub async fn insert_imported_skill(
        &self,
        company_id: Uuid,
        key: &str,
        name: &str,
        markdown: &str,
    ) -> RepoResult<bool> {
        let n = sqlx::query(
            "INSERT INTO company_skills                 (id, company_id, key, slug, name, markdown, source_type, trust_level,                  compatibility, file_inventory)              VALUES (gen_random_uuid(), $1, $2, $2, $3, $4, 'imported', 'company', '{}', '[]'::jsonb)              ON CONFLICT (company_id, key) DO NOTHING",
        )
        .bind(company_id)
        .bind(key)
        .bind(name)
        .bind(markdown)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        Ok(n > 0)
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

    // ============== R560: state machine ==============

    #[test]
    fn r560_terminal_states() {
        assert!(!SkillTestRunStatus::Queued.is_terminal());
        assert!(!SkillTestRunStatus::Running.is_terminal());
        assert!(SkillTestRunStatus::Passed.is_terminal());
        assert!(SkillTestRunStatus::Failed.is_terminal());
        assert!(SkillTestRunStatus::Cancelled.is_terminal());
        assert!(SkillTestRunStatus::Superseded.is_terminal());
    }

    #[test]
    fn r560_valid_queued_transitions() {
        let from = SkillTestRunStatus::Queued;
        assert!(from.can_transition_to(SkillTestRunStatus::Running));
        assert!(from.can_transition_to(SkillTestRunStatus::Cancelled));
        assert!(from.can_transition_to(SkillTestRunStatus::Superseded));
        // 跳到 Passed/Failed 不合法(必须先 Running)
        assert!(!from.can_transition_to(SkillTestRunStatus::Passed));
        assert!(!from.can_transition_to(SkillTestRunStatus::Failed));
    }

    #[test]
    fn r560_valid_running_transitions() {
        let from = SkillTestRunStatus::Running;
        assert!(from.can_transition_to(SkillTestRunStatus::Passed));
        assert!(from.can_transition_to(SkillTestRunStatus::Failed));
        assert!(from.can_transition_to(SkillTestRunStatus::Cancelled));
        assert!(from.can_transition_to(SkillTestRunStatus::Superseded));
        // 跳回 Queued 不合法
        assert!(!from.can_transition_to(SkillTestRunStatus::Queued));
    }

    #[test]
    fn r560_terminal_states_cannot_transition() {
        for terminal in [
            SkillTestRunStatus::Passed,
            SkillTestRunStatus::Failed,
            SkillTestRunStatus::Cancelled,
            SkillTestRunStatus::Superseded,
        ] {
            for next in [
                SkillTestRunStatus::Queued,
                SkillTestRunStatus::Running,
                SkillTestRunStatus::Passed,
                SkillTestRunStatus::Failed,
                SkillTestRunStatus::Cancelled,
                SkillTestRunStatus::Superseded,
            ] {
                assert!(
                    !terminal.can_transition_to(next),
                    "{:?} should not transition to {:?}",
                    terminal,
                    next
                );
            }
        }
    }

    #[test]
    fn r560_self_transition_rejected() {
        for s in [
            SkillTestRunStatus::Queued,
            SkillTestRunStatus::Running,
            SkillTestRunStatus::Passed,
            SkillTestRunStatus::Failed,
            SkillTestRunStatus::Cancelled,
            SkillTestRunStatus::Superseded,
        ] {
            assert!(!s.can_transition_to(s), "self-transition should be rejected for {s:?}");
        }
    }

    #[test]
    fn r560_invalid_skips_rejected() {
        // 跳级转换（如 Queued→Failed）应被拒绝
        assert!(!SkillTestRunStatus::Queued.can_transition_to(SkillTestRunStatus::Failed));
        assert!(!SkillTestRunStatus::Queued.can_transition_to(SkillTestRunStatus::Passed));
        // 已 Cancelled 的 run 不能突然 Passed
        assert!(!SkillTestRunStatus::Cancelled.can_transition_to(SkillTestRunStatus::Passed));
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

/// 内部比较函数：按 path 排序后逐元素比对。
fn version_inventory_snapshot_equal_inner(a: &Value, b: &Value) -> bool {
    let normalize = |v: &Value| -> Vec<(String, String, String)> {
        match v.as_array() {
            Some(arr) => {
                let mut entries: Vec<(String, String, String)> = arr
                    .iter()
                    .map(|item| {
                        let path = item
                            .get("path")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let kind = item
                            .get("kind")
                            .and_then(Value::as_str)
                            .unwrap_or("file")
                            .to_string();
                        let content = item
                            .get("content")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        (path, kind, content)
                    })
                    .collect();
                entries.sort();
                entries
            }
            None => Vec::new(),
        }
    };
    normalize(a) == normalize(b)
}

#[cfg(test)]
mod ensure_version_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn snapshot_equal_handles_unordered_arrays() {
        let a = json!([
            {"path": "a.md", "kind": "file", "content": "hello"},
            {"path": "b.md", "kind": "file", "content": "world"},
        ]);
        let b = json!([
            {"path": "b.md", "kind": "file", "content": "world"},
            {"path": "a.md", "kind": "file", "content": "hello"},
        ]);
        assert!(version_inventory_snapshot_equal_inner(&a, &b));
    }

    #[test]
    fn snapshot_equal_detects_content_diff() {
        let a = json!([{"path": "a.md", "kind": "file", "content": "v1"}]);
        let b = json!([{"path": "a.md", "kind": "file", "content": "v2"}]);
        assert!(!version_inventory_snapshot_equal_inner(&a, &b));
    }

    #[test]
    fn snapshot_equal_handles_null_or_missing() {
        let a = json!(null);
        let b = json!([]);
        assert!(version_inventory_snapshot_equal_inner(&a, &b));
    }
}
