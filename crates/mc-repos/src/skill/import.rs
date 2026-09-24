//! skill **导入**的批写入（`on_conflict` 四策略 + 整包事务 + 覆盖式重导入）。
//!
//! - **写者**：M6-3（**W**；`docs/57` §3.2）。
//! - **上游**：`internal/handler/skill.go` 的 `finishSkillImport` / `createImportedSkillWithName` /
//!   `createRenamedImportedSkill` / `resolveImportSkillConflict`，以及 `skill_create.go` 的
//!   `overwriteSkillWithFiles`。
//! - **四策略**（`fail` / `overwrite` / `rename` / `skip`，`validImportOnConflict`）语义：
//!   - `rename`：冲突时**再算一次**可用名（`%s-%d` 从 2 起），重试有上限（上游 50 次）——
//!     没有上限的话「同名风暴」就是死循环；
//!   - `skip`：不写库，但**保留**「已存在」的语义给响应（不能静默变成成功）；
//!   - 缺省（未传 `on_conflict`）是 `fail` ⇒ 409；`overwrite` 只在**创建者**本人时允许。
//! - **整包原子性**：正文 + 支持文件必须在一个事务里落库（上游逐文件写，但本仓不许出现
//!   「一半的包」）。支持文件用**一次 UNNEST 多值 INSERT**（stub 的行预算注释要求），
//!   再按请求顺序回读 —— 响应里的 `files` 顺序 = 请求体顺序（M6-2 的教训：上游创建响应的
//!   `files` 是 append 顺序，不是 `ORDER BY path`）。
//! - **覆盖式重导入（`overwriteSkillWithFiles`）**是**事务内再验一次权限**的：先 `SELECT`
//!   目标行、用**读到的** `created_by` 判权，再 `UPDATE`。判权只能基于事务内的那一次读，
//!   否则 READ COMMITTED 下「先读到别人的行、后写自己的内容」会成立。
//! - **本仓约定**：`self.db().pool().begin()` 显式事务（与 `write.rs` 同形）；`23505` ⇒
//!   `RepoError::Conflict`（经 `crate::workspace::map_sqlx_err`）。
//! - **不做什么**：不出网（取件在 route 层）、不做体积/文件数上限（那是 `mc-skill::archive`
//!   的判定，本文件**信任**已校验过的输入）、不碰标签（覆盖式重导入**保留**标签与
//!   `agent_skill` 绑定 —— 它们的行都没被删）。
//!
//! **状态：M6-3 已落地（LUM-1668）**。
//!
//! 行预算（门 ⑩）：桩写 300 行，落地 485 行（含 6 条用例）。

use mc_core::Id;
use serde_json::Value as Json;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use super::read::{SkillFileRow, SkillRepo, SkillRow};
use super::write::{NewSkill, SkillFileInput, SkillWithFiles};
use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

/// 上游 `maxImportRenameAttempts`。
pub const MAX_IMPORT_RENAME_ATTEMPTS: u32 = 50;

/// 上游 `importOnConflict*` 四个字面量。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictStrategy {
    /// `"fail"`：冲突 ⇒ 409（未传 `on_conflict` 时的缺省）。
    Fail,
    /// `"overwrite"`：覆盖同名 skill（只有创建者本人允许）。
    Overwrite,
    /// `"rename"`：换一个可用名（`-2`、`-3`…）。
    Rename,
    /// `"skip"`：什么都不写，响应里说明「已存在」。
    Skip,
}

impl ConflictStrategy {
    /// 上游 `validImportOnConflict` + 「空串 ⇒ fail」两步合一。
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "" | "fail" => Some(Self::Fail),
            "overwrite" => Some(Self::Overwrite),
            "rename" => Some(Self::Rename),
            "skip" => Some(Self::Skip),
            _ => None,
        }
    }

    /// 请求里显式传了 `on_conflict` ⇒ 走结构化结果（上游 `structuredResult`）。
    pub fn is_structured(raw: &str) -> bool {
        !raw.is_empty()
    }
}

/// 上游 `AllowOverwrite func(userID string, skill db.Skill) bool` 的两个实现。
///
/// 两者都在**事务内**用刚读到的行判定，不是调用方先算好再传进来。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverwritePolicy {
    /// 上游 `canOverwriteSkillByLocalImport`：**只有创建者**（导入冲突覆盖）。
    CreatorOnly,
    /// 上游 `RefreshSkill` 的内联闭包：创建者**或**工作区 owner/admin（刷新）。
    CreatorOrAdmin { is_admin: bool },
}

impl OverwritePolicy {
    /// `user_id` 是发起人；`created_by` 是**事务内读到的**目标行创建者。
    pub fn allows(&self, user_id: Id, created_by: Option<Id>) -> bool {
        let is_creator = created_by == Some(user_id);
        match self {
            Self::CreatorOnly => is_creator,
            Self::CreatorOrAdmin { is_admin } => *is_admin || is_creator,
        }
    }
}

/// 上游 `skillOverwriteInput`。
#[derive(Debug, Clone)]
pub struct ImportOverwriteInput {
    pub workspace_id: Id,
    pub target_skill_id: Id,
    /// 发起人（判权用）。
    pub user_id: Id,
    /// 判权策略。
    pub policy: OverwritePolicy,
    /// 非空且与目标现名不符 ⇒ [`OverwriteError::NameMismatch`]（防「客户端拿着过期的
    /// `target_skill_id` 把 A 的内容写进 B」——上游注释原文）。
    pub expected_name: String,
    /// 非空且与目标现名不同 ⇒ 改名（刷新的「采纳上游改名」）。空 ⇒ `COALESCE` 保留原名。
    pub new_name: String,
    /// 覆盖式重导入**总是**写 description / content / config（上游三个字段都是 `Valid: true`）。
    pub description: String,
    pub content: String,
    pub config: Json,
    /// **整批替换**（先删光再插入）：源里已经没有的文件必须消失。
    pub files: Vec<SkillFileInput>,
}

/// 覆盖式重导入的失败分类（上游四个哨兵错误 + 通用 `RepoError`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverwriteError {
    /// `errSkillOverwriteNotFound` ⇒ 409 `target skill no longer exists`。
    NotFound,
    /// `errSkillOverwriteForbidden` ⇒ 403。
    Forbidden,
    /// `errSkillOverwriteNameMismatch` ⇒ 409。
    NameMismatch,
    /// `errSkillOverwriteNameConflict` ⇒ 409。
    NameConflict,
    /// 其他（route 层映射 500）。
    Repo(String),
}

impl OverwriteError {
    /// 上游 `skillImportOverwriteFailure` 的 `(status, reason)` 表（供 route 层复用）。
    pub fn import_http(&self) -> (u16, String) {
        match self {
            Self::NotFound => (409, "target skill no longer exists".to_string()),
            Self::Forbidden => (
                403,
                "only the skill creator can overwrite this skill".to_string(),
            ),
            Self::NameMismatch => (
                409,
                "target skill name no longer matches the imported skill".to_string(),
            ),
            other => (500, format!("failed to overwrite skill: {other}")),
        }
    }
}

impl std::fmt::Display for OverwriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str("target skill not found"),
            Self::Forbidden => f.write_str("overwrite forbidden"),
            Self::NameMismatch => f.write_str("target skill name mismatch"),
            Self::NameConflict => f.write_str("target skill name conflict"),
            Self::Repo(message) => f.write_str(message),
        }
    }
}

impl SkillRepo {
    /// 上游 `GetSkillByWorkspaceAndName`：同名查询（导入冲突判定 / 409 体的唯一读点）。
    pub async fn find_by_name(&self, workspace_id: Id, name: &str) -> Result<Option<SkillRow>> {
        let row = sqlx::query_as::<_, SkillRow>(
            "SELECT id, workspace_id, name, description, content, config, created_by, \
                    plugin_installation_id, created_at, updated_at \
             FROM skill WHERE workspace_id = $1 AND name = $2",
        )
        .bind(workspace_id.0)
        .bind(name)
        .fetch_optional(self.db().pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(row)
    }

    /// 上游 `createImportedSkillWithName` ⇒ `createSkillWithFiles`：建包（一个事务）。
    ///
    /// `23505` ⇒ [`crate::RepoError::Conflict`]（route 层据此走冲突分支）。
    pub async fn create_imported(
        &self,
        new: &NewSkill,
        files: &[SkillFileInput],
    ) -> Result<SkillWithFiles> {
        let mut tx = self.db().pool().begin().await.map_err(map_sqlx_err)?;
        let skill = insert_skill(&mut tx, new).await?;
        let rows = insert_files(&mut tx, skill.id, files).await?;
        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(SkillWithFiles { skill, files: rows })
    }

    /// 上游 `createRenamedImportedSkill`：`%s-%d` 从 `2` 起试到 `maxImportRenameAttempts`。
    ///
    /// 只有**唯一约束**撞车才继续试下一号；其他错误立刻返回（上游 `isUniqueViolation` 判据）。
    pub async fn create_renamed_imported(
        &self,
        new: &NewSkill,
        files: &[SkillFileInput],
        base_name: &str,
    ) -> Result<SkillWithFiles> {
        let mut candidate = new.clone();
        for suffix in 2..(MAX_IMPORT_RENAME_ATTEMPTS + 2) {
            candidate.name = format!("{base_name}-{suffix}");
            match self.create_imported(&candidate, files).await {
                Ok(created) => return Ok(created),
                // 名字被占就换下一个后缀（上游 `createRenamedImportedSkill` 的循环）。
                Err(crate::RepoError::Conflict) => {}
                Err(other) => return Err(other),
            }
        }
        Err(crate::RepoError::Db(format!(
            "failed to find an available renamed skill name after {MAX_IMPORT_RENAME_ATTEMPTS} attempts"
        )))
    }

    /// 上游 `overwriteSkillWithFiles`：**事务内**再验一次权限，然后整批替换正文与文件。
    ///
    /// 保留的：`id` / `workspace_id` / `created_by` / `created_at` / `plugin_installation_id`、
    /// 标签连接行、`agent_skill` 绑定（这些行都没被碰）。
    /// 替换的：`name`（仅当 `new_name` 给出且不同）/ `description` / `content` / `config` / 全部文件。
    pub async fn overwrite_imported(
        &self,
        input: &ImportOverwriteInput,
    ) -> std::result::Result<SkillWithFiles, OverwriteError> {
        let mut tx = self
            .db()
            .pool()
            .begin()
            .await
            .map_err(|error| OverwriteError::Repo(error.to_string()))?;

        let existing = select_in_workspace(&mut tx, input.workspace_id.0, input.target_skill_id.0)
            .await
            .map_err(|error| match error {
                crate::RepoError::NotFound => OverwriteError::NotFound,
                other => OverwriteError::Repo(other.to_string()),
            })?;

        if !input
            .policy
            .allows(input.user_id, existing.created_by.map(Id))
        {
            return Err(OverwriteError::Forbidden);
        }
        if !input.expected_name.is_empty() && existing.name != input.expected_name {
            return Err(OverwriteError::NameMismatch);
        }

        // 空 new_name 或与现名相同 ⇒ 不改名（`COALESCE` 保留原名，避免唯一名抖动）。
        let new_name = if input.new_name.is_empty() || input.new_name == existing.name {
            None
        } else {
            Some(input.new_name.clone())
        };
        let skill = update_skill_content(&mut tx, existing.id, new_name.as_deref(), input)
            .await
            .map_err(|error| match error {
                // READ COMMITTED 下并发的已提交 DELETE 会让 UPDATE 命中 0 行。
                crate::RepoError::NotFound => OverwriteError::NotFound,
                // 上游只在 `newName.Valid` 时才把唯一约束撞车归为 NameConflict。
                crate::RepoError::Conflict if new_name.is_some() => OverwriteError::NameConflict,
                other => OverwriteError::Repo(other.to_string()),
            })?;

        delete_files(&mut tx, skill.id)
            .await
            .map_err(|error| OverwriteError::Repo(error.to_string()))?;
        let files = insert_files(&mut tx, skill.id, &input.files)
            .await
            .map_err(|error| OverwriteError::Repo(error.to_string()))?;

        tx.commit()
            .await
            .map_err(|error| OverwriteError::Repo(error.to_string()))?;
        Ok(SkillWithFiles { skill, files })
    }
}

// ---------------------------------------------------------------------------
// 事务内的小步
// ---------------------------------------------------------------------------

type Tx<'a> = Transaction<'a, Postgres>;

/// 上游 `util.SanitizeTextForPostgres`（`write.rs` 里有同形副本，这里不为它建可见性）。
fn sanitize_null_bytes(text: &str) -> String {
    text.replace('\0', "")
}

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

/// 建包时的支持文件：一次 `UNNEST` 多值 `INSERT`，再按**请求顺序**回读。
///
/// 回读顺序用 `WITH ORDINALITY` 钉死（`RETURNING` 的行序没有 SQL 保证 ⇒ 不能靠它）。
async fn insert_files(
    tx: &mut Tx<'_>,
    skill_id: Uuid,
    files: &[SkillFileInput],
) -> Result<Vec<SkillFileRow>> {
    let paths: Vec<String> = files.iter().map(|f| sanitize_null_bytes(&f.path)).collect();
    let contents: Vec<String> = files
        .iter()
        .map(|f| sanitize_null_bytes(&f.content))
        .collect();
    sqlx::query_as::<_, SkillFileRow>(
        "WITH input(path, content, ord) AS ( \
             SELECT * FROM unnest($2::text[], $3::text[]) WITH ORDINALITY \
         ), inserted AS ( \
             INSERT INTO skill_file (skill_id, path, content) \
             SELECT $1::uuid, input.path, input.content FROM input \
             RETURNING id, skill_id, path, content, created_at, updated_at \
         ) \
         SELECT inserted.id, inserted.skill_id, inserted.path, inserted.content, \
                inserted.created_at, inserted.updated_at \
         FROM inserted JOIN input ON input.path = inserted.path \
         ORDER BY input.ord",
    )
    .bind(skill_id)
    .bind(&paths)
    .bind(&contents)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_sqlx_err)
}

async fn select_in_workspace(
    tx: &mut Tx<'_>,
    workspace_id: Uuid,
    skill_id: Uuid,
) -> Result<SkillRow> {
    sqlx::query_as::<_, SkillRow>(
        "SELECT id, workspace_id, name, description, content, config, created_by, \
                plugin_installation_id, created_at, updated_at \
         FROM skill WHERE id = $1 AND workspace_id = $2",
    )
    .bind(skill_id)
    .bind(workspace_id)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx_err)
}

async fn update_skill_content(
    tx: &mut Tx<'_>,
    skill_id: Uuid,
    new_name: Option<&str>,
    input: &ImportOverwriteInput,
) -> Result<SkillRow> {
    sqlx::query_as::<_, SkillRow>(
        "UPDATE skill SET \
             name = COALESCE($2, name), \
             description = $3, \
             content = $4, \
             config = $5::jsonb, \
             updated_at = now() \
         WHERE id = $1 \
         RETURNING id, workspace_id, name, description, content, config, created_by, \
                   plugin_installation_id, created_at, updated_at",
    )
    .bind(skill_id)
    .bind(new_name.map(sanitize_null_bytes))
    .bind(sanitize_null_bytes(&input.description))
    .bind(sanitize_null_bytes(&input.content))
    .bind(&input.config)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx_err)
}

async fn delete_files(tx: &mut Tx<'_>, skill_id: Uuid) -> Result<()> {
    sqlx::query("DELETE FROM skill_file WHERE skill_id = $1")
        .bind(skill_id)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx_err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn id(n: u8) -> Id {
        Id(Uuid::from_bytes([n; 16]))
    }

    #[test]
    fn strategy_parse_matches_valid_import_on_conflict() {
        assert_eq!(ConflictStrategy::parse(""), Some(ConflictStrategy::Fail));
        assert_eq!(
            ConflictStrategy::parse("fail"),
            Some(ConflictStrategy::Fail)
        );
        assert_eq!(
            ConflictStrategy::parse("overwrite"),
            Some(ConflictStrategy::Overwrite)
        );
        assert_eq!(
            ConflictStrategy::parse("rename"),
            Some(ConflictStrategy::Rename)
        );
        assert_eq!(
            ConflictStrategy::parse("skip"),
            Some(ConflictStrategy::Skip)
        );
        for bad in ["FAIL", "merge", "fail ", "skip,rename"] {
            assert_eq!(ConflictStrategy::parse(bad), None, "{bad} must be rejected");
        }
    }

    #[test]
    fn structured_result_is_driven_by_a_non_empty_on_conflict() {
        assert!(!ConflictStrategy::is_structured(""));
        for raw in ["fail", "overwrite", "rename", "skip"] {
            assert!(ConflictStrategy::is_structured(raw), "{raw}");
        }
    }

    #[test]
    fn creator_only_policy_needs_the_creator_row() {
        let policy = OverwritePolicy::CreatorOnly;
        assert!(policy.allows(id(1), Some(id(1))));
        assert!(!policy.allows(id(1), Some(id(2))));
        // 上游：`CreatedBy.Valid` 为假 ⇒ 谁都不能覆盖（不是「无主就能覆盖」）。
        assert!(!policy.allows(id(1), None));
    }

    #[test]
    fn creator_or_admin_policy_requires_admin_or_creator() {
        let admin = OverwritePolicy::CreatorOrAdmin { is_admin: true };
        assert!(admin.allows(id(1), Some(id(1))));
        assert!(admin.allows(id(1), Some(id(2))));
        // 上游闭包是 `isAdmin || (CreatedBy.Valid && …)` ⇒ admin 不要求创建者存在
        // （与导入覆盖的 `canOverwriteSkillByLocalImport` 不同：那条 `CreatedBy.Valid`
        // 是前置条件，无主行谁都覆盖不了）。
        assert!(admin.allows(id(1), None));

        let member = OverwritePolicy::CreatorOrAdmin { is_admin: false };
        assert!(member.allows(id(1), Some(id(1))));
        assert!(!member.allows(id(1), Some(id(2))));
    }

    #[test]
    fn overwrite_failure_http_table_matches_upstream() {
        assert_eq!(
            OverwriteError::NotFound.import_http(),
            (409, "target skill no longer exists".to_string())
        );
        assert_eq!(
            OverwriteError::Forbidden.import_http(),
            (
                403,
                "only the skill creator can overwrite this skill".to_string()
            )
        );
        assert_eq!(
            OverwriteError::NameMismatch.import_http(),
            (
                409,
                "target skill name no longer matches the imported skill".to_string()
            )
        );
        let (status, reason) = OverwriteError::NameConflict.import_http();
        assert_eq!(status, 500);
        assert!(reason.contains("failed to overwrite skill"));
    }

    #[test]
    fn overwrite_input_keeps_config_object_shape() {
        // 覆盖式重导入写的是 `{}` 形态（不是 `null`）—— 上游 `config = []byte("{}")`。
        let input = ImportOverwriteInput {
            workspace_id: id(1),
            target_skill_id: id(2),
            user_id: id(3),
            policy: OverwritePolicy::CreatorOnly,
            expected_name: "n".into(),
            new_name: String::new(),
            description: String::new(),
            content: "body".into(),
            config: json!({}),
            files: vec![],
        };
        assert_eq!(input.config, json!({}));
        assert!(input.config.is_object());
    }
}
