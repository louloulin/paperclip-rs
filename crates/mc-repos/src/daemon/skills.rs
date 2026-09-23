//! skill bundle 仓储（R7 拆分自 `daemon.rs`）。
//!
//! 上游落点：`server/pkg/db/queries/skill.sql`（按 agent 解析 bundle、按名查 skill、
//! 本地技能导入的 create / overwrite 两条写路径）。

// 本文件的 `impl DaemonRepo` 是 `daemon.rs` 那个 impl 的续块（R7 800 行拆分）。

use super::*;

impl DaemonRepo {
    /// 该 agent 名下、且被点名要的 skill bundle（upstream `LoadRequestedAgentSkillBundles`）。
    ///
    /// 上游的 ref 带 `source`（`workspace` / `builtin` / `plugin`）；本仓只实现
    /// `workspace` 源（`agent_skill ⋈ skill`），因此非 `workspace` 的 ref 不会出现在
    /// 结果里，调用方据此回 404 `skill bundle not found`（与上游同一行为）。
    pub async fn skill_bundles_for_agent(
        &self,
        agent_id: Id,
        skill_ids: &[Id],
    ) -> Result<Vec<SkillBundleRow>> {
        if skill_ids.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<Uuid> = skill_ids.iter().map(|id| id.as_uuid()).collect();
        let skills = sqlx::query_as::<_, SkillRow>(
            "SELECT s.id, s.workspace_id, s.name, s.description, s.content, s.config, \
                    s.created_by, s.created_at, s.updated_at \
             FROM skill s JOIN agent_skill ask ON ask.skill_id = s.id \
             WHERE ask.agent_id = $1 AND s.id = ANY($2) \
             ORDER BY s.name ASC",
        )
        .bind(agent_id.as_uuid())
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_err)?;

        let files = sqlx::query_as::<_, SkillFileRow>(
            "SELECT id, skill_id, path, content, created_at, updated_at FROM skill_file \
             WHERE skill_id = ANY($1) ORDER BY path ASC",
        )
        .bind(&ids)
        .fetch_all(&self.pool)
        .await
        .map_err(map_sqlx_err)?;

        Ok(skills
            .into_iter()
            .map(|skill| SkillBundleRow {
                files: files
                    .iter()
                    .filter(|f| f.skill_id == skill.id)
                    .map(|f| (f.path.clone(), f.content.clone()))
                    .collect(),
                skill,
            })
            .collect())
    }

    /// 按 workspace + name 读 skill（本地导入的冲突探测；`UNIQUE(workspace_id, name)`）。
    pub async fn skill_by_name(&self, workspace_id: Id, name: &str) -> Result<Option<SkillRow>> {
        sqlx::query_as::<_, SkillRow>(
            "SELECT id, workspace_id, name, description, content, config, created_by, \
                    created_at, updated_at FROM skill \
             WHERE workspace_id = $1 AND name = $2",
        )
        .bind(workspace_id.as_uuid())
        .bind(name)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_sqlx_err)
    }

    /// 本地导入的 create 路径：新建 skill + 支持文件（单事务）。
    #[allow(clippy::too_many_arguments)]
    pub async fn create_skill_with_files(
        &self,
        workspace_id: Id,
        name: &str,
        description: &str,
        content: &str,
        config: &Value,
        created_by: Option<Id>,
        files: &[(String, String)],
    ) -> Result<SkillWithFilesRow> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_err)?;
        let (skill_id,): (Uuid,) = sqlx::query_as(
            "INSERT INTO skill (workspace_id, name, description, content, config, created_by) \
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
        )
        .bind(workspace_id.as_uuid())
        .bind(name)
        .bind(description)
        .bind(content)
        .bind(config)
        .bind(created_by.map(Id::as_uuid))
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;
        for (path, file_content) in files {
            sqlx::query(
                "INSERT INTO skill_file (skill_id, path, content) VALUES ($1, $2, $3) \
                 ON CONFLICT (skill_id, path) DO UPDATE SET content = EXCLUDED.content, \
                    updated_at = now()",
            )
            .bind(skill_id)
            .bind(path)
            .bind(file_content)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
        }
        let row = sqlx::query_as::<_, SkillRow>(
            "SELECT id, workspace_id, name, description, content, config, created_by, \
                    created_at, updated_at FROM skill WHERE id = $1",
        )
        .bind(skill_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;
        let file_rows = skill_files(&mut tx, skill_id).await?;
        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(SkillWithFilesRow {
            skill: row,
            files: file_rows,
        })
    }

    /// 本地导入的 overwrite 路径：同一个事务里**重新验**目标 skill 存在、creator 仍是
    /// 调用者、名字仍匹配 —— 用户在确认与上报之间的任何漂移都干净失败，不回落 create。
    #[allow(clippy::too_many_arguments)]
    pub async fn overwrite_skill_with_files(
        &self,
        workspace_id: Id,
        target_skill_id: Id,
        expected_name: &str,
        expect_creator: Option<Id>,
        description: &str,
        content: &str,
        config: &Value,
        files: &[(String, String)],
    ) -> Result<OverwriteOutcome> {
        let mut tx = self.pool.begin().await.map_err(map_sqlx_err)?;
        let existing = sqlx::query_as::<_, SkillRow>(
            "SELECT id, workspace_id, name, description, content, config, created_by, \
                    created_at, updated_at FROM skill \
             WHERE id = $1 AND workspace_id = $2 FOR UPDATE",
        )
        .bind(target_skill_id.as_uuid())
        .bind(workspace_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;
        let Some(existing) = existing else {
            tx.rollback().await.map_err(map_sqlx_err)?;
            return Ok(OverwriteOutcome::Missing);
        };
        // 名字守卫（上游 `errSkillOverwriteNameMismatch`）。
        if existing.name != expected_name {
            tx.rollback().await.map_err(map_sqlx_err)?;
            return Ok(OverwriteOutcome::NameMismatch);
        }
        // 只有原 creator 能覆写（上游 `canOverwriteSkillByLocalImport` 的事务内复查）。
        if expect_creator.is_some()
            && existing.created_by != expect_creator.map(mc_core::Id::as_uuid)
        {
            tx.rollback().await.map_err(map_sqlx_err)?;
            return Ok(OverwriteOutcome::NotOwner);
        }
        sqlx::query(
            "UPDATE skill SET description = $2, content = $3, config = $4, updated_at = now() \
             WHERE id = $1",
        )
        .bind(target_skill_id.as_uuid())
        .bind(description)
        .bind(content)
        .bind(config)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;
        // 覆写语义 = 文件集合全量替换（上游同款：先删后插）。
        sqlx::query("DELETE FROM skill_file WHERE skill_id = $1")
            .bind(target_skill_id.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
        for (path, file_content) in files {
            sqlx::query("INSERT INTO skill_file (skill_id, path, content) VALUES ($1, $2, $3)")
                .bind(target_skill_id.as_uuid())
                .bind(path)
                .bind(file_content)
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx_err)?;
        }
        let row = sqlx::query_as::<_, SkillRow>(
            "SELECT id, workspace_id, name, description, content, config, created_by, \
                    created_at, updated_at FROM skill WHERE id = $1",
        )
        .bind(target_skill_id.as_uuid())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;
        let file_rows = skill_files(&mut tx, target_skill_id.as_uuid()).await?;
        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(OverwriteOutcome::Updated(Box::new(SkillWithFilesRow {
            skill: row,
            files: file_rows,
        })))
    }

    // ---------------------------------------------------------------- gc probes
}
