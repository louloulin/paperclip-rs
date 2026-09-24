//! skill 与支持文件的**写**查询（建 / 改 / 删）。
//!
//! - **写者**：M6-2（**W**；`docs/57` §3.2）。
//! - **上游**：`internal/handler/skill.CreateSkill` / `UpdateSkill` / `DeleteSkill` /
//!   `UpsertSkillFile` / `DeleteSkillFile`，以及 `skill_create.go` 的
//!   `createSkillWithFilesInTx`（本文件是它的 SQL 半）；标签连接来自 `issue_label.sql`
//!   的 `AttachLabelToSkill` / `DetachLabelFromSkill` / `DeleteSkillLabelAssignmentsBySkill`。
//! - **两条硬语义**：
//!   1. `UNIQUE(workspace_id, name)` 撞了 ⇒ [`RepoError::Conflict`]，route 层映射 **409**
//!      （不是 400，也不是 500 —— 上游也是冲突语义）；
//!   2. 改内容与改文件是**两个动作**：`skill.content` 是正文列，`skill_file` 是支持文件；
//!      一次 PUT 里两件都变时要在**一个事务**里（否则会留下「正文新、文件旧」的中间态）。
//! - **本仓约定**：写用 `Db::pool().begin()` 显式事务（`Db` 没有 `begin()` 转发，
//!   也不接受外部 `&mut PgConnection` —— 与 `agent` / `project` 仓储同形）；
//!   删除是**硬删** + 依赖 `ON DELETE CASCADE`（迁移 `008` 已声明），不要手写级联。
//! - **不做的事**（在 route 层，别搬进来）：
//!   - 保留路径过滤（`SKILL.md` 不落 `skill_file`）：要用 `mc_skill::reserved`，
//!     而 `mc-repos` **不依赖** `mc-skill`（依赖方向不允许，根 `Cargo.toml` 已冻结）；
//!   - 权限判定（工作区成员 / 角色）、审计日志（本仓没有这张表）。
//!
//! **状态：M6-2 已落地（LUM-1667）**。
//!
//! 行预算（门 ⑩）：预计 280 行以内。

use mc_core::Id;
use serde_json::Value as Json;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::read::{SkillFileRow, SkillRepo, SkillRow};
use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

/// `CreateSkill` 的入参（上游 `skillCreateInput` 的 SQL 半）。
///
/// `config` 由调用方填好默认的 `{}`：上游 `createSkillWithFilesInTx` 把
/// `json.Marshal(nil)` 的 `null` 显式改写成 `{}`，本仓在 route 层做同一件事。
#[derive(Debug, Clone)]
pub struct NewSkill {
    pub workspace_id: Id,
    pub name: String,
    pub description: String,
    pub content: String,
    pub config: Json,
    pub created_by: Option<Id>,
}

/// `UpdateSkill` 的补丁：`None` = 不改（上游 `pgtype.Text{Valid:false}`）。
///
/// `config` 是 `Option<Json>` 而不是 `pgtype`：上游 `UpdateSkillParams.Config` 是
/// `[]byte`，`nil` 在 SQL 里就是 NULL ⇒ `COALESCE` 保留旧值。
#[derive(Debug, Clone, Default)]
pub struct SkillUpdate {
    pub name: Option<String>,
    pub description: Option<String>,
    pub content: Option<String>,
    pub config: Option<Json>,
}

/// 一条支持文件（上游 `CreateSkillFileRequest{Path, Content}`）。
#[derive(Debug, Clone)]
pub struct SkillFileInput {
    pub path: String,
    pub content: String,
}

/// 建 / 改 skill 的返回：skill 行 + 该 skill 的**当前**文件行。
///
/// 对应上游 `SkillWithFilesResponse{Files []SkillFileResponse}` —— 注意它用的是
/// **带正文**的 `SkillFileResponse`（元数据形态只出现在 `include=metadata` 与
/// `GET /files`），所以这里取 `SkillFileRow` 而不是 metadata 行。
#[derive(Debug, Clone)]
pub struct SkillWithFiles {
    pub skill: SkillRow,
    pub files: Vec<SkillFileRow>,
}

/// 去掉 `\0`（上游 `util.SanitizeTextForPostgres`）。
///
/// 本仓 `property::validation::sanitize_null_bytes` 是 `pub(crate)`，但
/// `property.rs` 的 `mod validation;` 是**私有**的 ⇒ 从 `skill::write` 不可达，
/// 故在此留一份 3 行副本（已登记 `docs/32` §9.6）。上游的另一半
/// `strings.ToValidUTF8(s, "\u{FFFD}")` 在 Rust 里是恒等变换（`&str` 已是合法 UTF-8），
/// 所以这里**逐字等价**，不是近似。
fn sanitize_null_bytes(text: &str) -> String {
    text.replace('\0', "")
}

impl SkillRepo {
    /// 建 skill + 支持文件（**一个事务**；上游 `createSkillWithFilesInTx`）。
    ///
    /// 唯一约束撞车 ⇒ [`crate::RepoError::Conflict`]；事务内任何一步失败都整体回滚
    /// （上游在 `createSkillWithFiles` 里 `defer tx.Rollback`）。
    pub async fn create(&self, new: &NewSkill, files: &[SkillFileInput]) -> Result<SkillWithFiles> {
        let mut tx = self.db().pool().begin().await.map_err(map_sqlx_err)?;
        let skill = insert_skill(&mut tx, new).await?;
        let mut rows = Vec::with_capacity(files.len());
        for file in files {
            rows.push(upsert_file_tx(&mut tx, skill.id, file).await?);
        }
        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(SkillWithFiles { skill, files: rows })
    }

    /// 改 skill（上游 `UpdateSkill`）：`COALESCE` 语义的补丁 + 可选的文件整批替换。
    ///
    /// `files = None` ⇒ 不回读、不动 `skill_file`（上游走 `ListSkillFiles` 填响应）；
    /// `files = Some(列表)` ⇒ **先 `DELETE` 掉该 skill 的全部文件**再逐条 upsert，
    /// 空列表就是「清空全部文件」。上游的 `req.Files != nil` 判据就是这条分界，
    /// 所以调用方必须区分「字段缺省」与「显式 `[]`」。
    ///
    /// 注意上游的 `WHERE` 只有 `id`（没有 `workspace_id`）：workspace 守卫在
    /// `loadSkillForUser` 里已经做过，这里不重复（否则 `RETURNING` 会多一种失败态，
    /// 与上游的 404 分支对不上）。
    pub async fn update(
        &self,
        skill_id: Id,
        patch: &SkillUpdate,
        files: Option<&[SkillFileInput]>,
    ) -> Result<SkillWithFiles> {
        let mut tx = self.db().pool().begin().await.map_err(map_sqlx_err)?;
        let skill = update_skill(&mut tx, skill_id, patch).await?;
        let rows = match files {
            Some(input) => {
                delete_files_by_skill(&mut tx, skill.id).await?;
                let mut rows = Vec::with_capacity(input.len());
                for file in input {
                    rows.push(upsert_file_tx(&mut tx, skill.id, file).await?);
                }
                rows
            }
            None => list_files_tx(&mut tx, skill.id).await?,
        };
        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(SkillWithFiles { skill, files: rows })
    }

    /// 删 skill（上游 `DeleteSkill`）：先清标签连接行，再删 skill 行（同一事务）。
    ///
    /// 与上游一致的两个细节：① `skill_to_label` **故意没有外键** ⇒ 必须显式清
    /// （`skill_file` / `agent_skill` 有 `ON DELETE CASCADE`，不手写）；
    /// ② `DELETE` 不看 `rows_affected` —— 上游在同一请求里已经 `loadSkillForUser`
    /// 过一次（404 早就在那里出），这里再判一次只会把并发删除变成另一种状态码。
    pub async fn delete(&self, workspace_id: Id, skill_id: Id) -> Result<()> {
        let mut tx = self.db().pool().begin().await.map_err(map_sqlx_err)?;
        sqlx::query("DELETE FROM skill_to_label WHERE skill_id = $1")
            .bind(skill_id.0)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
        sqlx::query("DELETE FROM skill WHERE id = $1 AND workspace_id = $2")
            .bind(skill_id.0)
            .bind(workspace_id.0)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(())
    }

    /// 单文件 upsert（上游 `UpsertSkillFile`）：`ON CONFLICT (skill_id, path)` ⇒ 覆盖。
    ///
    /// 返回**带正文**的行（上游 `PutSkillFiles` 的 200 响应体就是 `SkillFileResponse`）。
    pub async fn upsert_file(&self, skill_id: Id, file: &SkillFileInput) -> Result<SkillFileRow> {
        let mut tx = self.db().pool().begin().await.map_err(map_sqlx_err)?;
        let row = upsert_file_tx(&mut tx, skill_id.0, file).await?;
        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(row)
    }

    /// 按主键读文件行（上游 `GetSkillFile`）：route 层用它做「文件属于该 skill」的守卫。
    pub async fn get_file(&self, file_id: Id) -> Result<SkillFileRow> {
        sqlx::query_as::<_, SkillFileRow>(
            "SELECT id, skill_id, path, content, created_at, updated_at \
             FROM skill_file WHERE id = $1",
        )
        .bind(file_id.0)
        .fetch_one(self.db().pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 删单个文件行（上游 `DeleteSkillFile`，`WHERE id = $1`，硬删）。
    pub async fn delete_file(&self, file_id: Id) -> Result<()> {
        sqlx::query("DELETE FROM skill_file WHERE id = $1")
            .bind(file_id.0)
            .execute(self.db().pool())
            .await
            .map_err(map_sqlx_err)?;
        Ok(())
    }

    /// 挂标签（上游 `AttachLabelToSkill`）：带 skill + label 双重 `EXISTS` 守卫的
    /// `INSERT ... ON CONFLICT DO NOTHING` ⇒ 重复挂载静默幂等。
    ///
    /// 返回真的插入了几行（`0` = 幂等重放或守卫未命中；route 层在调用前已用
    /// `issue_label` 的 `resource_type` 判过 404，所以守卫未命中不是正常路径）。
    pub async fn attach_label(&self, skill_id: Id, label_id: Id, workspace_id: Id) -> Result<u64> {
        sqlx::query(
            "INSERT INTO skill_to_label (skill_id, label_id) \
             SELECT $1::uuid, $2::uuid \
             WHERE EXISTS (SELECT 1 FROM skill s \
                           WHERE s.id = $1::uuid AND s.workspace_id = $3::uuid) \
               AND EXISTS (SELECT 1 FROM issue_label l \
                           WHERE l.id = $2::uuid AND l.workspace_id = $3::uuid \
                             AND l.resource_type = 'skill') \
             ON CONFLICT DO NOTHING",
        )
        .bind(skill_id.0)
        .bind(label_id.0)
        .bind(workspace_id.0)
        .execute(self.db().pool())
        .await
        .map_err(map_sqlx_err)
        .map(|done| done.rows_affected())
    }

    /// 摘标签（上游 `DetachLabelFromSkill`）：无 404（未挂过 = 删 0 行 = 成功）。
    ///
    /// 上游的 `DetachLabelFromSkill` **不带** `resource_type` 谓词（只带 skill 的
    /// workspace `EXISTS`），这里逐字照抄。
    pub async fn detach_label(&self, skill_id: Id, label_id: Id, workspace_id: Id) -> Result<u64> {
        sqlx::query(
            "DELETE FROM skill_to_label \
             WHERE skill_id = $1 AND label_id = $2 \
               AND EXISTS (SELECT 1 FROM skill s \
                           WHERE s.id = $1::uuid AND s.workspace_id = $3::uuid)",
        )
        .bind(skill_id.0)
        .bind(label_id.0)
        .bind(workspace_id.0)
        .execute(self.db().pool())
        .await
        .map_err(map_sqlx_err)
        .map(|done| done.rows_affected())
    }
}

// ---------------------------------------------------------------------------
// 事务内的小步（每个都是上游一条 SQL 的逐字移植）
// ---------------------------------------------------------------------------

type Tx<'a> = Transaction<'a, Postgres>;

async fn insert_skill(tx: &mut Tx<'_>, new: &NewSkill) -> Result<SkillRow> {
    sqlx::query_as::<_, SkillRow>(
        "INSERT INTO skill (workspace_id, name, description, content, config, created_by) \
         VALUES ($1, $2, $3, $4, $5::jsonb, $6::uuid) \
         RETURNING id, workspace_id, name, description, content, config, created_by, \
                   plugin_installation_id, created_at, updated_at",
    )
    .bind(new.workspace_id.0)
    .bind(sanitize_null_bytes(&new.name))
    .bind(sanitize_null_bytes(&new.description))
    .bind(sanitize_null_bytes(&new.content))
    .bind(&new.config)
    .bind(new.created_by.map(Id::as_uuid))
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx_err)
}

async fn update_skill(tx: &mut Tx<'_>, skill_id: Id, patch: &SkillUpdate) -> Result<SkillRow> {
    sqlx::query_as::<_, SkillRow>(
        "UPDATE skill SET \
             name = COALESCE($2, name), \
             description = COALESCE($3, description), \
             content = COALESCE($4, content), \
             config = COALESCE($5::jsonb, config), \
             updated_at = now() \
         WHERE id = $1 \
         RETURNING id, workspace_id, name, description, content, config, created_by, \
                   plugin_installation_id, created_at, updated_at",
    )
    .bind(skill_id.0)
    .bind(patch.name.as_deref().map(sanitize_null_bytes))
    .bind(patch.description.as_deref().map(sanitize_null_bytes))
    .bind(patch.content.as_deref().map(sanitize_null_bytes))
    .bind(patch.config.as_ref())
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx_err)
}

async fn upsert_file_tx(
    tx: &mut Tx<'_>,
    skill_id: Uuid,
    file: &SkillFileInput,
) -> Result<SkillFileRow> {
    sqlx::query_as::<_, SkillFileRow>(
        "INSERT INTO skill_file (skill_id, path, content) VALUES ($1, $2, $3) \
         ON CONFLICT (skill_id, path) DO UPDATE SET content = EXCLUDED.content, \
                                                    updated_at = now() \
         RETURNING id, skill_id, path, content, created_at, updated_at",
    )
    .bind(skill_id)
    .bind(sanitize_null_bytes(&file.path))
    .bind(sanitize_null_bytes(&file.content))
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx_err)
}

async fn delete_files_by_skill(tx: &mut Tx<'_>, skill_id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM skill_file WHERE skill_id = $1")
        .bind(skill_id)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx_err)?;
    Ok(())
}

async fn list_files_tx(tx: &mut Tx<'_>, skill_id: Uuid) -> Result<Vec<SkillFileRow>> {
    sqlx::query_as::<_, SkillFileRow>(
        "SELECT id, skill_id, path, content, created_at, updated_at \
         FROM skill_file WHERE skill_id = $1 ORDER BY path ASC",
    )
    .bind(skill_id)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_sqlx_err)
}
