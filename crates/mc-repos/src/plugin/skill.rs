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

// ---------------------------------------------------------------------------
// 真库集成测试（`#[ignore]` + `MULTICA_TEST_DATABASE_URL`，gate ⑥ 拉起）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod db_tests {
    use super::*;
    use crate::plugin::installation::{insert_tx, NewInstallation};
    use crate::plugin::package::{insert_version_tx, upsert_package_tx, NewVersion};
    use crate::Db;
    use serde_json::json;
    use std::env;
    use uuid::Uuid;

    struct Fixture {
        db: Db,
        workspace_id: Id,
        user_id: Id,
        installation_id: Id,
    }

    async fn setup() -> Option<Fixture> {
        let url = env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        let db = Db::connect(&url, 4, 1).await.ok()?;
        let workspace_id: Uuid = sqlx::query_scalar(
            "INSERT INTO workspace(name, slug) VALUES ('itest-m65sk', $1) RETURNING id",
        )
        .bind(format!("itest-m65sk-{}", Uuid::new_v4()))
        .fetch_one(db.pool())
        .await
        .ok()?;
        let user_id: Uuid = sqlx::query_scalar(
            r#"INSERT INTO "user"(name, email) VALUES ('itest-m65sk', $1) RETURNING id"#,
        )
        .bind(format!("itest-m65sk-{}@example.com", Uuid::new_v4()))
        .fetch_one(db.pool())
        .await
        .ok()?;
        let workspace_id = Id::from(workspace_id);
        let user_id = Id::from(user_id);
        let mut tx = db.pool().begin().await.ok()?;
        let package = upsert_package_tx(&mut tx, workspace_id, user_id, "sk.plugin", "Sk")
            .await
            .ok()?;
        let version = insert_version_tx(
            &mut tx,
            &NewVersion {
                package_id: package.id(),
                workspace_id,
                version: "1.0.0",
                manifest: &json!({"name": "Sk"}),
                digest: &"a".repeat(64),
                size_bytes: 5,
                published_by: user_id,
            },
        )
        .await
        .ok()?;
        let installation = insert_tx(
            &mut tx,
            &NewInstallation {
                workspace_id,
                plugin_key: "sk.plugin",
                package_version_id: version.id(),
                version: "1.0.0",
                manifest: &json!({"name": "Sk"}),
                granted_scopes: &json!([]),
                installed_by: user_id,
            },
        )
        .await
        .ok()?;
        tx.commit().await.ok()?;
        Some(Fixture {
            db,
            workspace_id,
            user_id,
            installation_id: installation.id(),
        })
    }

    macro_rules! fixture {
        () => {
            match setup().await {
                Some(fx) => fx,
                None => {
                    eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
                    return;
                }
            }
        };
    }

    async fn teardown(fx: &Fixture) {
        let _ = sqlx::query("DELETE FROM skill WHERE workspace_id = $1")
            .bind(fx.workspace_id.0)
            .execute(fx.db.pool())
            .await;
        for sql in [
            "DELETE FROM plugin_installation WHERE workspace_id = $1",
            "DELETE FROM plugin_package_version WHERE workspace_id = $1",
            "DELETE FROM plugin_package WHERE workspace_id = $1",
        ] {
            let _ = sqlx::query(sql)
                .bind(fx.workspace_id.0)
                .execute(fx.db.pool())
                .await;
        }
        let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
            .bind(fx.workspace_id.0)
            .execute(fx.db.pool())
            .await;
        let _ = sqlx::query(r#"DELETE FROM "user" WHERE id = $1"#)
            .bind(fx.user_id.0)
            .execute(fx.db.pool())
            .await;
    }

    fn input(name: &str, content: &str) -> PluginSkillInput {
        PluginSkillInput {
            name: name.to_string(),
            description: format!("{name} skill"),
            content: content.to_string(),
        }
    }

    async fn skill_names(fx: &Fixture) -> Vec<String> {
        sqlx::query_scalar("SELECT name FROM skill WHERE plugin_installation_id = $1 ORDER BY name")
            .bind(fx.installation_id.0)
            .fetch_all(fx.db.pool())
            .await
            .expect("names")
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_sync_materializes_prunes_and_updates_in_place() {
        let fx = fixture!();

        let mut tx = fx.db.pool().begin().await.expect("tx");
        sync_tx(
            &mut tx,
            fx.workspace_id,
            fx.installation_id,
            fx.user_id,
            &[input("alpha", "A"), input("beta", "B")],
        )
        .await
        .expect("sync");
        tx.commit().await.expect("commit");
        assert_eq!(skill_names(&fx).await, vec!["alpha", "beta"]);

        // 内容变更 = 同一行 UPDATE（不是新增）。
        let mut tx = fx.db.pool().begin().await.expect("tx");
        sync_tx(
            &mut tx,
            fx.workspace_id,
            fx.installation_id,
            fx.user_id,
            &[input("alpha", "A2"), input("beta", "B")],
        )
        .await
        .expect("resync");
        tx.commit().await.expect("commit");
        let (content, count): (String, i64) = (
            sqlx::query_scalar(
                "SELECT content FROM skill WHERE plugin_installation_id = $1 AND name = 'alpha'",
            )
            .bind(fx.installation_id.0)
            .fetch_one(fx.db.pool())
            .await
            .expect("content"),
            sqlx::query_scalar("SELECT count(*) FROM skill WHERE workspace_id = $1")
                .bind(fx.workspace_id.0)
                .fetch_one(fx.db.pool())
                .await
                .expect("count"),
        );
        assert_eq!(content, "A2");
        assert_eq!(count, 2);

        // 新版本只贡献一个 ⇒ 旧的那个被剪掉。
        let mut tx = fx.db.pool().begin().await.expect("tx");
        sync_tx(
            &mut tx,
            fx.workspace_id,
            fx.installation_id,
            fx.user_id,
            &[input("alpha", "A2")],
        )
        .await
        .expect("prune");
        tx.commit().await.expect("commit");
        assert_eq!(skill_names(&fx).await, vec!["alpha"]);

        // 空集 ⇒ 本安装的 skill 全部消失（人写的行不受影响）。
        let human: Uuid = sqlx::query_scalar(
            "INSERT INTO skill(workspace_id, name, description, content) VALUES ($1, 'human', '', '') RETURNING id",
        )
        .bind(fx.workspace_id.0)
        .fetch_one(fx.db.pool())
        .await
        .expect("human skill");
        let mut tx = fx.db.pool().begin().await.expect("tx");
        sync_tx(
            &mut tx,
            fx.workspace_id,
            fx.installation_id,
            fx.user_id,
            &[],
        )
        .await
        .expect("empty sync");
        tx.commit().await.expect("commit");
        assert!(skill_names(&fx).await.is_empty());
        let survived: i64 = sqlx::query_scalar("SELECT count(*) FROM skill WHERE id = $1")
            .bind(human)
            .fetch_one(fx.db.pool())
            .await
            .expect("survived");
        assert_eq!(survived, 1);

        teardown(&fx).await;
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_sync_refuses_a_name_owned_by_another_installation() {
        let fx = fixture!();
        let mut tx = fx.db.pool().begin().await.expect("tx");
        sync_tx(
            &mut tx,
            fx.workspace_id,
            fx.installation_id,
            fx.user_id,
            &[input("shared", "mine")],
        )
        .await
        .expect("first");
        tx.commit().await.expect("commit");

        // 第二个安装想占同名 ⇒ Conflict（绝不静默抢走别人的行）。
        let other: Uuid = sqlx::query_scalar(
            "INSERT INTO plugin_installation \
               (workspace_id, plugin_key, version, manifest, package_version_id) \
             VALUES ($1, 'other.plugin', '1.0.0', '{}'::jsonb, \
                     (SELECT id FROM plugin_installation WHERE id = $2)) RETURNING id",
        )
        .bind(fx.workspace_id.0)
        .bind(fx.installation_id.0)
        .fetch_one(fx.db.pool())
        .await
        .expect("other install");
        let other = Id::from(other);

        let mut tx = fx.db.pool().begin().await.expect("tx");
        let clash = sync_tx(
            &mut tx,
            fx.workspace_id,
            other,
            fx.user_id,
            &[input("shared", "theirs")],
        )
        .await;
        assert!(matches!(clash, Err(RepoError::Conflict)));
        tx.rollback().await.expect("rollback");

        // 内容仍是原主人的。
        let content: String =
            sqlx::query_scalar("SELECT content FROM skill WHERE plugin_installation_id = $1")
                .bind(fx.installation_id.0)
                .fetch_one(fx.db.pool())
                .await
                .expect("content");
        assert_eq!(content, "mine");

        let _ = sqlx::query("DELETE FROM plugin_installation WHERE id = $1")
            .bind(other.0)
            .execute(fx.db.pool())
            .await;
        teardown(&fx).await;
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_delete_by_installation_leaves_human_skills_alone() {
        let fx = fixture!();
        let mut tx = fx.db.pool().begin().await.expect("tx");
        sync_tx(
            &mut tx,
            fx.workspace_id,
            fx.installation_id,
            fx.user_id,
            &[input("alpha", "A")],
        )
        .await
        .expect("sync");
        tx.commit().await.expect("commit");
        let human: Uuid = sqlx::query_scalar(
            "INSERT INTO skill(workspace_id, name, description, content) VALUES ($1, 'human2', '', '') RETURNING id",
        )
        .bind(fx.workspace_id.0)
        .fetch_one(fx.db.pool())
        .await
        .expect("human skill");

        let mut tx = fx.db.pool().begin().await.expect("tx");
        delete_by_installation_tx(&mut tx, fx.installation_id)
            .await
            .expect("delete");
        tx.commit().await.expect("commit");
        assert!(skill_names(&fx).await.is_empty());
        let survived: i64 = sqlx::query_scalar("SELECT count(*) FROM skill WHERE id = $1")
            .bind(human)
            .fetch_one(fx.db.pool())
            .await
            .expect("survived");
        assert_eq!(survived, 1);

        teardown(&fx).await;
    }
}
