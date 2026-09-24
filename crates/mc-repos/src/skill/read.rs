//! skill 的**读**查询（列表 / 详情 / 支持文件 / 标签连接行）。
//!
//! - **写者**：M6-2（**W**；`docs/57` §3.2）。M6-3 / M6-4 只读，**不得**在本文件加查询 ——
//!   各切片加在自己的文件里（`import.rs` / `binding.rs`）。
//! - **上游**：`db/queries/skill.sql` 的读面 + `db/queries/issue_label.sql` 的
//!   `ListLabelsBySkill` / `ListLabelsForSkills`。
//! - **本仓约定**：读用 `&PgPool`（`self.db.pool()`）；列表必须带 `workspace_id` 收窄
//!   （跨工作区读 = 越权，上游一律 404 而不是 403）；派生字段（标签）用**批量查询**
//!   而不是 N+1（列表页最多几十行，N+1 会在真库 e2e 里直接超时）。
//! - **不做什么**：不在这里拼 DTO（响应投影在 `routes/skills` 的 DTO 层）；不读 `agent_skill`
//!   （授权面在 `binding.rs`，M6-4）。
//!
//! `SkillRepo` 落在本文件（`mod.rs` 是 anchor，冻结 ⇒ 不能另立 `repo.rs`）。
//!
//! **状态：M6-2 已落地（LUM-1667）**。
//!
//! 行预算（门 ⑩）：预计 260 行以内。

use chrono::{DateTime, Utc};
use mc_core::Id;
use mc_db::Db;
use serde_json::Value as Json;
use sqlx::FromRow;
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

/// `skill` 全列（迁移 `008` + `368`，共 **10** 列）。
///
/// `FromRow` 派生按**列名**取值 ⇒ 这里字段顺序无关；`pq` 返回的物理顺序里
/// `plugin_installation_id` 排在末尾（`368` 是 `ALTER TABLE ... ADD COLUMN`）。
#[derive(Debug, Clone, FromRow)]
pub struct SkillRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    pub description: String,
    /// SKILL.md 正文。
    pub content: String,
    /// JSONB，NOT NULL DEFAULT `{}`。
    pub config: Json,
    /// 可空：`CREATE TABLE skill` 里 `created_by UUID REFERENCES "user"(id)` 无 NOT NULL。
    pub created_by: Option<Uuid>,
    /// 可空：非 NULL 表示由插件安装贡献（`368`，M6-4 的卸载要靠它反查）。
    pub plugin_installation_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl SkillRow {
    /// 主键。
    pub fn id(&self) -> Id {
        Id(self.id)
    }

    /// 所属 workspace。
    pub fn workspace_id(&self) -> Id {
        Id(self.workspace_id)
    }
}

/// 列表行：`ListSkillSummariesByWorkspace` 的形状（**不含 `content`**）。
///
/// 上游把它单列一条 SQL（GH #2174：SKILL.md 动辄 50–200KB，列表页带上正文会把
/// CLI 拖到 15s 超时）。因此这里也不能图省事用 [`SkillRow`] 再丢字段 ——
/// `FromRow` 是按列名取值，缺 `content` 列会直接运行期报错。
#[derive(Debug, Clone, FromRow)]
pub struct SkillSummaryRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    pub description: String,
    pub config: Json,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `skill_file` 行（6 列，含正文）。
#[derive(Debug, Clone, FromRow)]
pub struct SkillFileRow {
    pub id: Uuid,
    pub skill_id: Uuid,
    pub path: String,
    pub content: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl SkillFileRow {
    /// 主键。
    pub fn id(&self) -> Id {
        Id(self.id)
    }
}

/// `ListSkillFileMetadata` 的形状：正文不出库，`size` / `content_hash` 在 PG 里算。
#[derive(Debug, Clone, FromRow)]
pub struct SkillFileMetadataRow {
    pub id: Uuid,
    pub skill_id: Uuid,
    pub path: String,
    /// `octet_length(content)::bigint` = UTF-8 字节数（不是字符数）。
    pub size: i64,
    /// `encode(sha256(convert_to(content,'UTF8')),'hex')`，裸十六进制（无 `sha256:` 前缀）。
    pub content_hash: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `issue_label` × `skill_to_label` 的**扁平**行。
///
/// 单条与批量两条 SQL 都取 `stl.skill_id` ⇒ 一个结构体喂两个查询；扁平而不是
/// `(Uuid, LabelRow)` 是因为 `FromRow` 不支持嵌套行结构。
#[derive(Debug, Clone, FromRow)]
pub struct SkillLabelRow {
    /// 连接行上的 skill（批量查询时用来分组）。
    pub skill_id: Uuid,
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub resource_type: String,
    pub name: String,
    pub description: String,
    pub color: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl SkillLabelRow {
    /// 连接行上的 skill id。
    pub fn skill_id(&self) -> Id {
        Id(self.skill_id)
    }
}

const SKILL_COLUMNS: &str = "id, workspace_id, name, description, content, config, created_by, \
                             plugin_installation_id, created_at, updated_at";
const SKILL_SUMMARY_COLUMNS: &str =
    "id, workspace_id, name, description, config, created_by, created_at, updated_at";
const SKILL_FILE_COLUMNS: &str = "id, skill_id, path, content, created_at, updated_at";
/// ⚠️ `convert_to(content, 'UTF8')`，**不是** `content::bytea`：后者会走 bytea 的
/// 输入解析器，把 `\x41` 读成转义而不是字面 4 个字符（上游注释里的原始事故）。
const SKILL_FILE_METADATA_COLUMNS: &str = "id, skill_id, path, \
                                           octet_length(content)::bigint AS size, \
                                           encode(sha256(convert_to(content, 'UTF8')), 'hex') \
                                           AS content_hash, created_at, updated_at";
const SKILL_LABEL_COLUMNS: &str = "stl.skill_id, l.id, l.workspace_id, l.resource_type, l.name, \
                                   l.description, l.color, l.created_at, l.updated_at";

/// `SkillRepo`（写方法在 `write.rs` 的 `impl` 块里）。
#[derive(Clone)]
pub struct SkillRepo {
    db: Db,
}

impl SkillRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// workspace 的 skill 摘要列表（上游 `ListSkillSummariesByWorkspace`，`name ASC`）。
    pub async fn list_summaries(&self, workspace_id: Id) -> Result<Vec<SkillSummaryRow>> {
        let sql = format!(
            "SELECT {SKILL_SUMMARY_COLUMNS} FROM skill WHERE workspace_id = $1 ORDER BY name ASC"
        );
        sqlx::query_as::<_, SkillSummaryRow>(&sql)
            .bind(workspace_id.0)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 详情（上游 `GetSkillInWorkspace`：workspace 收窄 ⇒ 越权读与不存在同一条 404）。
    pub async fn get_in_workspace(&self, workspace_id: Id, skill_id: Id) -> Result<SkillRow> {
        let sql = format!("SELECT {SKILL_COLUMNS} FROM skill WHERE id = $1 AND workspace_id = $2");
        sqlx::query_as::<_, SkillRow>(&sql)
            .bind(skill_id.0)
            .bind(workspace_id.0)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 支持文件全量（上游 `ListSkillFiles`，`path ASC`，正文一并取出）。
    pub async fn list_files(&self, skill_id: Id) -> Result<Vec<SkillFileRow>> {
        let sql = format!(
            "SELECT {SKILL_FILE_COLUMNS} FROM skill_file WHERE skill_id = $1 ORDER BY path ASC"
        );
        sqlx::query_as::<_, SkillFileRow>(&sql)
            .bind(skill_id.0)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 支持文件元数据（上游 `ListSkillFileMetadata`，`path ASC`，**正文不出库**）。
    pub async fn list_file_metadata(&self, skill_id: Id) -> Result<Vec<SkillFileMetadataRow>> {
        let sql = format!(
            "SELECT {SKILL_FILE_METADATA_COLUMNS} FROM skill_file \
             WHERE skill_id = $1 ORDER BY path ASC"
        );
        sqlx::query_as::<_, SkillFileMetadataRow>(&sql)
            .bind(skill_id.0)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 单 skill 已挂的标签（上游 `ListLabelsBySkill`，`LOWER(name) ASC`）。
    ///
    /// `resource_type = 'skill'` 是 SQL 里的硬谓词：别的资源类型的标签**即使**在
    /// `skill_to_label` 里出现也不会被读出来（`attach` 侧另有同口径守卫）。
    pub async fn list_labels(&self, workspace_id: Id, skill_id: Id) -> Result<Vec<SkillLabelRow>> {
        let sql = format!(
            "SELECT {SKILL_LABEL_COLUMNS} FROM issue_label l \
             JOIN skill_to_label stl ON stl.label_id = l.id \
             WHERE stl.skill_id = $1 AND l.workspace_id = $2 AND l.resource_type = 'skill' \
             ORDER BY LOWER(l.name) ASC"
        );
        sqlx::query_as::<_, SkillLabelRow>(&sql)
            .bind(skill_id.0)
            .bind(workspace_id.0)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 批量标签（上游 `ListLabelsForSkills`）：列表页一次取回，避免每行一条 SQL。
    ///
    /// 空 `skill_ids` 直接返回空表（不上 SQL：`ANY('{}')` 虽然也对，但省一次往返，
    /// 且与上游 `labelsBySkill` 的 `len(skillIDs) == 0` 早退同形）。
    pub async fn list_labels_for_skills(
        &self,
        workspace_id: Id,
        skill_ids: &[Uuid],
    ) -> Result<Vec<SkillLabelRow>> {
        if skill_ids.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT {SKILL_LABEL_COLUMNS} FROM issue_label l \
             JOIN skill_to_label stl ON stl.label_id = l.id \
             WHERE stl.skill_id = ANY($1::uuid[]) AND l.workspace_id = $2 \
               AND l.resource_type = 'skill' \
             ORDER BY stl.skill_id, LOWER(l.name) ASC"
        );
        sqlx::query_as::<_, SkillLabelRow>(&sql)
            .bind(skill_ids)
            .bind(workspace_id.0)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }
}

impl RepoWithDb for SkillRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}
