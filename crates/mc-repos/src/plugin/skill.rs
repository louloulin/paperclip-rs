//! 插件贡献的 skill：按 `skill.plugin_installation_id` 关联的读写。
//!
//! - **写者**：M6-5（**W**；`docs/57` §3.2）。
//! - **上游**：插件安装/卸载时对 `skill` 行的同步（`plugin_installation_id` 是迁移 `368` 加的列）。
//! - **为什么单独一个文件**：`skill` 表的主写者是 M6-2/M6-3（`mc_repos::skill`），但
//!   **按安装 id 收窄**的那几个查询只有 M6-5 用。放在这里而不是加到 `mc_repos::skill`，
//!   是为了保住「一个文件一个写者」（M6-5 不改 skill 目录下的文件）。
//! - **语义要点（`368` 的注释就是契约）**：
//!   - `plugin_installation_id` 可空：`NULL` = **人写的** skill，非空 = 插件贡献的；
//!   - 该列**故意没有外键**（仓库策略）：卸载插件时要**显式删**这些行，别指望级联；
//!   - 卸载 = 删行（不是标记），所以「重新安装后 skill 又回来了」是正常现象。
//! - **行身份是 `(workspace_id, name)` 且 upsert 带守卫**（上游 `plugin.sql` 的
//!   `UpsertPluginSkill`）：`DO UPDATE … WHERE skill.plugin_installation_id = EXCLUDED.…`
//!   只允许**同一个安装**覆盖自己的行；人写的、或另一个安装拥有的同名 skill 一律不覆盖。
//!   上游这里其实会拿到 `ErrNoRows` 再折成 502（见 `docs/32` §9.6 的偏离登记）——
//!   本仓折成 **409 conflict**，与它自己注释里写的意图（"must fail the install loudly"）一致。
//! - **不做什么**：不改人写的 skill（`plugin_installation_id IS NULL` 的行不属于本文件）。
//!
//! **状态：M6-5 已落地**。
//!
//! 行预算（门 ⑩）：预计 180 行以内。

use super::installation::Tx;
use crate::workspace::map_sqlx_err;
use crate::{RepoError, Result};
use mc_core::Id;

/// 一个要物化的插件 skill（内容已在调用方的事务里从版本文件读出）。
#[derive(Debug, Clone)]
pub struct PluginSkillInput {
    /// 上游口径：**manifest 的 resource key**（不是 frontmatter 里的 name）。
    pub name: String,
    /// frontmatter 的 description；空 ⇒ 调用方已回落成 `Provided by the … Plugin.`。
    pub description: String,
    /// `SKILL.md` 正文。
    pub content: String,
}

/// 先剪枝、再 upsert（上游 `InstallSkillResources`）。
///
/// 剪枝必须在写之前：改名时新名字要先腾出旧名字，反序会被 `(workspace_id, name)` 的
/// 唯一约束挡住任何「只改大小写/空白」的重命名。
///
/// `skills` 为空 = 该版本不再贡献任何 skill ⇒ 剪枝会删光本安装的旧行（上游同义）。
pub async fn sync_tx(
    tx: &mut Tx<'_>,
    workspace_id: Id,
    installation_id: Id,
    created_by: Id,
    skills: &[PluginSkillInput],
) -> Result<()> {
    let keep: Vec<String> = skills.iter().map(|skill| skill.name.clone()).collect();
    sqlx::query("DELETE FROM skill WHERE plugin_installation_id = $1 AND name <> ALL($2::text[])")
        .bind(installation_id.0)
        .bind(&keep)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx_err)?;

    for skill in skills {
        // `ON CONFLICT … DO UPDATE … WHERE` 在不满足守卫时返回 **0 行**（不报 23505）——
        // 这正是「这个名字已经属于别人」的信号。
        let row: Option<(uuid::Uuid,)> = sqlx::query_as(
            "INSERT INTO skill \
               (workspace_id, name, description, content, config, created_by, plugin_installation_id) \
             VALUES ($1, $2, $3, $4, '{}'::jsonb, $5, $6) \
             ON CONFLICT (workspace_id, name) DO UPDATE SET \
               description = EXCLUDED.description, content = EXCLUDED.content, updated_at = now() \
             WHERE skill.plugin_installation_id = EXCLUDED.plugin_installation_id \
             RETURNING id",
        )
        .bind(workspace_id.0)
        .bind(&skill.name)
        .bind(&skill.description)
        .bind(&skill.content)
        .bind(created_by.0)
        .bind(installation_id.0)
        .fetch_optional(&mut **tx)
        .await
        .map_err(map_sqlx_err)?;

        if row.is_none() {
            return Err(RepoError::Conflict);
        }
    }
    Ok(())
}

/// 删掉本安装贡献的全部 skill（卸载路径；`DeletePluginSkillsByInstallation`）。
///
/// 卸载的完整级联在 `installation::delete_cascade_tx` 里（同一个事务），这里保留独立入口是
/// 给「只摘 skill、不动安装」的后续切片（M6-6 的运行时面）用。
pub async fn delete_by_installation_tx(tx: &mut Tx<'_>, installation_id: Id) -> Result<()> {
    sqlx::query("DELETE FROM skill WHERE plugin_installation_id = $1")
        .bind(installation_id.0)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx_err)
        .map(|_| ())
}
