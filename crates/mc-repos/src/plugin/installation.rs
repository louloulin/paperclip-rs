//! `plugin_installation` 的读写（安装 / 卸载 / 启停 / 配置 / 令牌哈希）。
//!
//! - **写者**：M6-5（**W**；`docs/57` §3.2）。M6-6…M6-8 只读。
//! - **上游**：`internal/service/plugin.go` + `internal/handler/plugin.go`（安装面九条路由）。
//! - **15 列**（`344` + `362` + `369` + `392`）：`id, workspace_id, plugin_key, version, manifest,
//!   granted_scopes, config, enabled, installed_by, created_at, updated_at, token_hash,
//!   token_rotated_at, mcp_approvals, package_version_id`。`source_url` 已被 `392` 删掉 ——
//!   **不要**在行结构里留它。
//! - **三条硬语义**：
//!   1. `enabled` 是**唯一**开关（没有 `status` 列；`PluginStatus` 只是投影，见 `mc_core::plugin`）；
//!   2. `config` 与 `manifest` 落 JSONB 时**原样存**（manifest 已由 `mc-plugin-host` 校验过）；
//!   3. `plugin_key` 与 `package_version_id` 都是 `NOT NULL` —— 安装必定来自一个已发布的包版本。
//! - **本仓约定**：`granted_scopes` 是 `JSONB` 数组；行结构里读成 `Vec<String>`（**不要**读成
//!   `mc_core::PluginScope`，`mc-core` 的 `Id` 没有 sqlx impl，领域转换在 route 层做）。
//! - **`plugin_secret` 为什么在这个文件**：桩注释原写「另一个文件是 M6-7 的 storage」，但
//!   `storage.rs` 的归属表写的是 `plugin_storage`，八张表里 `plugin_secret` **没有任何文件认领**，
//!   而 M6-5 的 `PUT …/config` 是它唯一的写者（读侧 M6-6/M6-7 只按 installation 取键名）。
//!   密钥与安装行同生共死（卸载一起删），放在这里而不是新开文件：**写集不变**，不碰
//!   `mod.rs`（M6-0 anchor 冻结）。本模块对安装行**只动** `token_hash` / `token_rotated_at` 两列；
//!   明文令牌与加密块归 `mc-plugin-host::{token,credentials}`。
//!
//! **状态：M6-5 已落地**。
//!
//! 行预算（门 ⑩）：预计 320 行以内。

use chrono::{DateTime, Utc};
use sqlx::types::Json;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::{Db, RepoError, RepoWithDb, Result};
use mc_core::Id;

/// 事务类型别名（各子文件共用；`query_as` 的 executor 取 `&mut *tx`）。
pub type Tx<'a> = Transaction<'a, Postgres>;

/// 上游 `PluginInstallation` 的列投影（15 列，顺序与 `RETURNING` 一致）。
pub const COLUMNS: &str =
    "id, workspace_id, plugin_key, version, manifest, granted_scopes, config, \
                           enabled, installed_by, token_hash, token_rotated_at, mcp_approvals, \
                           package_version_id, created_at, updated_at";

/// 一行 `plugin_installation`。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct InstallationRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub plugin_key: String,
    /// 已发布版本的版本串（**不是** manifest 里的那个：`+dev.N` 只存在于版本行）。
    pub version: String,
    /// 管理员同意过的 manifest 快照（`mc-plugin-host` 校验后原样存）。
    pub manifest: Json<serde_json::Value>,
    /// `JSONB` 数组。
    pub granted_scopes: Json<serde_json::Value>,
    /// 只含**非 secret** 字段。
    pub config: Json<serde_json::Value>,
    pub enabled: bool,
    pub installed_by: Option<Uuid>,
    /// `sha256(token)` 的 hex；`None` = 无凭据（未签发或已吊销）。
    pub token_hash: Option<String>,
    pub token_rotated_at: Option<DateTime<Utc>>,
    pub mcp_approvals: Json<serde_json::Value>,
    pub package_version_id: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl InstallationRow {
    /// 主键。
    pub fn id(&self) -> Id {
        Id(self.id)
    }

    /// 所属 workspace。
    pub fn workspace_id(&self) -> Id {
        Id(self.workspace_id)
    }

    /// 绑定的已发布版本。
    pub fn package_version_id(&self) -> Id {
        Id(self.package_version_id)
    }

    /// `granted_scopes` 解成 `Vec<String>`（坏值 ⇒ 空表，上游 `decodeScopes` 同义）。
    pub fn granted_scopes(&self) -> Vec<String> {
        serde_json::from_value(self.granted_scopes.0.clone()).unwrap_or_default()
    }

    /// `config` 解成对象（坏值 ⇒ 空对象）。
    pub fn config_object(&self) -> serde_json::Map<String, serde_json::Value> {
        self.config.0.as_object().cloned().unwrap_or_default()
    }
}

/// 新建一行安装（`CreatePluginInstallation`）。
pub struct NewInstallation<'a> {
    pub workspace_id: Id,
    pub plugin_key: &'a str,
    pub package_version_id: Id,
    pub version: &'a str,
    pub manifest: &'a serde_json::Value,
    pub granted_scopes: &'a serde_json::Value,
    pub installed_by: Id,
}

/// 升级（`UpdatePluginInstallationManifest`）的补丁。
pub struct UpgradeInstallation<'a> {
    pub package_version_id: Id,
    pub version: &'a str,
    pub manifest: &'a serde_json::Value,
    pub granted_scopes: &'a serde_json::Value,
    /// 已按新 manifest **剪枝**过的配置（`pruneConfig`）。
    pub config: &'a serde_json::Value,
}

/// `plugin_installation` 的读口 + 事务内写口。
#[derive(Debug, Clone)]
pub struct InstallationRepo {
    db: Db,
}

impl RepoWithDb for InstallationRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

impl InstallationRepo {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 一个 workspace 的全部安装（上游 `ListWorkspacePluginInstallations`）。
    pub async fn list(&self, workspace_id: Id) -> Result<Vec<InstallationRow>> {
        sqlx::query_as::<_, InstallationRow>(&format!(
            "SELECT {COLUMNS} FROM plugin_installation WHERE workspace_id = $1 \
             ORDER BY created_at ASC, plugin_key ASC"
        ))
        .bind(workspace_id.0)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 主键 + workspace 收窄；取不到 ⇒ [`RepoError::NotFound`]。
    pub async fn get(&self, workspace_id: Id, id: Id) -> Result<InstallationRow> {
        sqlx::query_as::<_, InstallationRow>(&format!(
            "SELECT {COLUMNS} FROM plugin_installation WHERE workspace_id = $1 AND id = $2"
        ))
        .bind(workspace_id.0)
        .bind(id.0)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)
    }

    /// 按 `plugin_key` 找（`GetWorkspacePluginInstallationByKey`）；没有 ⇒ `None`。
    pub async fn find_by_key(
        &self,
        workspace_id: Id,
        plugin_key: &str,
    ) -> Result<Option<InstallationRow>> {
        sqlx::query_as::<_, InstallationRow>(&format!(
            "SELECT {COLUMNS} FROM plugin_installation WHERE workspace_id = $1 AND plugin_key = $2"
        ))
        .bind(workspace_id.0)
        .bind(plugin_key)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 已有的 secret 键名（`ListPluginSecretKeys`）：**只有名字**，永不回值。
    pub async fn secret_keys(&self, installation_id: Id) -> Result<Vec<String>> {
        sqlx::query_scalar::<_, String>(
            "SELECT key FROM plugin_secret WHERE installation_id = $1 ORDER BY key ASC",
        )
        .bind(installation_id.0)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 按 id 直接取（卸载/令牌路径不需要 workspace 收窄的重复查）。
    pub async fn get_by_id(&self, id: Id) -> Result<InstallationRow> {
        sqlx::query_as::<_, InstallationRow>(&format!(
            "SELECT {COLUMNS} FROM plugin_installation WHERE id = $1"
        ))
        .bind(id.0)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)
    }
}

// ---------------------------------------------------------------------------
// 事务内写口（上游把这些都放在调用方的事务里）
// ---------------------------------------------------------------------------

/// 取出一个 workspace 的 `(workspace_id, plugin_key)` 建议锁。
///
/// 上游 `LockPluginPackageKey` 用 `pg_advisory_xact_lock(hashtext(workspace:key))`：
/// 发布 / 安装 / 删包三条路径都先取它，把「同一个插件」的并发写串行化。
pub async fn lock_plugin_key_tx(tx: &mut Tx<'_>, workspace_id: Id, plugin_key: &str) -> Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1))")
        .bind(format!("{}:{}", workspace_id.0, plugin_key))
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx_err)
        .map(|_| ())
}

/// 插入安装行；唯一索引撞车 ⇒ [`RepoError::Conflict`]（并发装同一个插件）。
pub async fn insert_tx(tx: &mut Tx<'_>, new: &NewInstallation<'_>) -> Result<InstallationRow> {
    sqlx::query_as::<_, InstallationRow>(&format!(
        "INSERT INTO plugin_installation \
           (workspace_id, plugin_key, package_version_id, version, manifest, granted_scopes, installed_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING {COLUMNS}"
    ))
    .bind(new.workspace_id.0)
    .bind(new.plugin_key)
    .bind(new.package_version_id.0)
    .bind(new.version)
    .bind(Json(new.manifest.clone()))
    .bind(Json(new.granted_scopes.clone()))
    .bind(new.installed_by.0)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx_err)
}

/// 原地升级：换绑版本、快照与已同意的 scope，并写回剪枝后的配置。
pub async fn upgrade_tx(
    tx: &mut Tx<'_>,
    id: Id,
    patch: &UpgradeInstallation<'_>,
) -> Result<InstallationRow> {
    sqlx::query_as::<_, InstallationRow>(&format!(
        "UPDATE plugin_installation \
            SET package_version_id = $2, version = $3, manifest = $4, granted_scopes = $5, \
                config = $6, updated_at = now() \
          WHERE id = $1 RETURNING {COLUMNS}"
    ))
    .bind(id.0)
    .bind(patch.package_version_id.0)
    .bind(patch.version)
    .bind(Json(patch.manifest.clone()))
    .bind(Json(patch.granted_scopes.clone()))
    .bind(Json(patch.config.clone()))
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx_err)
}

/// 写 `config`（`UpdatePluginInstallationConfig`）；secret 字段**不在这里**。
pub async fn set_config_tx(
    tx: &mut Tx<'_>,
    id: Id,
    config: &serde_json::Value,
) -> Result<InstallationRow> {
    sqlx::query_as::<_, InstallationRow>(&format!(
        "UPDATE plugin_installation SET config = $2, updated_at = now() \
          WHERE id = $1 RETURNING {COLUMNS}"
    ))
    .bind(id.0)
    .bind(Json(config.clone()))
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx_err)
}

/// 启停（`SetPluginInstallationEnabled`）。
pub async fn set_enabled_tx(tx: &mut Tx<'_>, id: Id, enabled: bool) -> Result<InstallationRow> {
    sqlx::query_as::<_, InstallationRow>(&format!(
        "UPDATE plugin_installation SET enabled = $2, updated_at = now() \
          WHERE id = $1 RETURNING {COLUMNS}"
    ))
    .bind(id.0)
    .bind(enabled)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx_err)
}

/// 写/清 `token_hash`：`Some` = 签发或轮换（同时刷 `token_rotated_at`），
/// `None` = 吊销（`SetPluginInstallationToken` 的空 `pgtype.Text`）。
pub async fn set_token_hash_tx(tx: &mut Tx<'_>, id: Id, token_hash: Option<&str>) -> Result<()> {
    sqlx::query(
        "UPDATE plugin_installation \
            SET token_hash = $2, token_rotated_at = CASE WHEN $2::text IS NULL THEN NULL ELSE now() END, \
                updated_at = now() \
          WHERE id = $1",
    )
    .bind(id.0)
    .bind(token_hash)
    .execute(&mut **tx)
    .await
    .map_err(map_sqlx_err)
    .map(|_| ())
}

/// upsert 一个加密过的 secret（`UpsertPluginSecret`）。
pub async fn upsert_secret_tx(
    tx: &mut Tx<'_>,
    installation_id: Id,
    key: &str,
    ciphertext: &[u8],
) -> Result<()> {
    sqlx::query(
        "INSERT INTO plugin_secret (installation_id, key, ciphertext) VALUES ($1, $2, $3) \
         ON CONFLICT (installation_id, key) \
         DO UPDATE SET ciphertext = EXCLUDED.ciphertext, updated_at = now()",
    )
    .bind(installation_id.0)
    .bind(key)
    .bind(ciphertext)
    .execute(&mut **tx)
    .await
    .map_err(map_sqlx_err)
    .map(|_| ())
}

/// 清一个 secret（空串提交 = 清除，`DeletePluginSecret`）。
pub async fn delete_secret_tx(tx: &mut Tx<'_>, installation_id: Id, key: &str) -> Result<()> {
    sqlx::query("DELETE FROM plugin_secret WHERE installation_id = $1 AND key = $2")
        .bind(installation_id.0)
        .bind(key)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx_err)
        .map(|_| ())
}

/// 删掉该安装的**全部** secret（`DeletePluginSecretsByInstallation`；升级剪枝与卸载共用）。
pub async fn delete_all_secrets_tx(tx: &mut Tx<'_>, installation_id: Id) -> Result<()> {
    sqlx::query("DELETE FROM plugin_secret WHERE installation_id = $1")
        .bind(installation_id.0)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx_err)
        .map(|_| ())
}

/// 该安装是否还有其它 secret（升级剪枝后判定用；键名集合由调用方算）。
pub async fn count_installations_of_versions_tx(tx: &mut Tx<'_>, package_id: Id) -> Result<i64> {
    // 删包前的守卫：任何安装仍指着这些版本 ⇒ 拒（`CountInstallationsOfPackageVersions`）。
    sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM plugin_installation i \
           JOIN plugin_package_version v ON v.id = i.package_version_id \
          WHERE v.package_id = $1",
    )
    .bind(package_id.0)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx_err)
}

/// 卸载：按上游顺序删掉**应用层拥有**的全部从属行，最后删安装行。
///
/// 没有外键/级联（仓库策略），所以这些 delete 必须在**同一个**事务里：中途失败会留下
/// 谁也够不着的行。`skill` 只删本安装贡献的那些（`plugin_installation_id` 收窄 ⇒ 人写的 skill
/// 不受影响）。
pub async fn delete_cascade_tx(tx: &mut Tx<'_>, installation_id: Id) -> Result<()> {
    let id = installation_id.0;
    // ① hook 日程（M6-8 的表；卸载必须带走，否则留下一代够不着的行）。
    sqlx::query("DELETE FROM plugin_hook_schedule WHERE installation_id = $1")
        .bind(id)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx_err)?;
    // ② 插件状态。
    sqlx::query("DELETE FROM plugin_storage WHERE installation_id = $1")
        .bind(id)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx_err)?;
    // ③ 密钥。
    delete_all_secrets_tx(tx, installation_id).await?;
    // ④ 调用记录（M6-6/M6-8 读面）。
    sqlx::query("DELETE FROM plugin_invocation WHERE installation_id = $1")
        .bind(id)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx_err)?;
    // ⑤ 本安装贡献的 skill（`368` 的列；人写的 `NULL` 行不动）。
    sqlx::query("DELETE FROM skill WHERE plugin_installation_id = $1")
        .bind(id)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx_err)?;
    // ⑥ 安装行。
    sqlx::query("DELETE FROM plugin_installation WHERE id = $1")
        .bind(id)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx_err)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 真库集成测试（`#[ignore]` + `MULTICA_TEST_DATABASE_URL`，gate ⑥ 拉起）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod db_tests {
    use super::*;
    use crate::plugin::package::{insert_version_tx, upsert_package_tx, NewVersion};
    use serde_json::json;
    use std::env;
    use uuid::Uuid;

    struct Fixture {
        db: Db,
        workspace_id: Id,
        user_id: Id,
        package_version_id: Id,
    }

    async fn setup() -> Option<Fixture> {
        let url = env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        let db = Db::connect(&url, 4, 1).await.ok()?;
        let workspace_id: Uuid = sqlx::query_scalar(
            "INSERT INTO workspace(name, slug) VALUES ('itest-m65', $1) RETURNING id",
        )
        .bind(format!("itest-m65-{}", Uuid::new_v4()))
        .fetch_one(db.pool())
        .await
        .ok()?;
        let user_id: Uuid = sqlx::query_scalar(
            r#"INSERT INTO "user"(name, email) VALUES ('itest-m65', $1) RETURNING id"#,
        )
        .bind(format!("itest-m65-{}@example.com", Uuid::new_v4()))
        .fetch_one(db.pool())
        .await
        .ok()?;
        let workspace_id = Id::from(workspace_id);
        let user_id = Id::from(user_id);
        let mut tx = db.pool().begin().await.ok()?;
        let package = upsert_package_tx(&mut tx, workspace_id, user_id, "hello.plugin", "Hello")
            .await
            .ok()?;
        let version = insert_version_tx(
            &mut tx,
            &NewVersion {
                package_id: package.id(),
                workspace_id,
                version: "1.0.0",
                manifest: &json!({"name": "Hello"}),
                digest: &"a".repeat(64),
                size_bytes: 12,
                published_by: user_id,
            },
        )
        .await
        .ok()?;
        tx.commit().await.ok()?;
        Some(Fixture {
            db,
            workspace_id,
            user_id,
            package_version_id: version.id(),
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

    macro_rules! new_install {
        ($fx:expr, $key:expr) => {
            NewInstallation {
                workspace_id: $fx.workspace_id,
                plugin_key: $key,
                package_version_id: $fx.package_version_id,
                version: "1.0.0",
                manifest: &json!({"name": "Hello"}),
                granted_scopes: &json!([]),
                installed_by: $fx.user_id,
            }
        };
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_install_crud_and_conflict() {
        let fx = fixture!();
        let repo = InstallationRepo::new(fx.db.clone());
        let mut tx = fx.db.pool().begin().await.expect("tx");
        let row = insert_tx(&mut tx, &new_install!(&fx, "hello.plugin"))
            .await
            .expect("insert");
        tx.commit().await.expect("commit");

        assert!(row.enabled);
        assert_eq!(row.version, "1.0.0");
        assert_eq!(row.package_version_id, fx.package_version_id.0);
        assert!(row.granted_scopes().is_empty());

        assert_eq!(repo.list(fx.workspace_id).await.expect("list").len(), 1);
        assert_eq!(
            repo.find_by_key(fx.workspace_id, "hello.plugin")
                .await
                .expect("find")
                .expect("row")
                .id(),
            row.id()
        );
        assert_eq!(
            repo.get(fx.workspace_id, row.id()).await.expect("get").id(),
            row.id()
        );

        // 同 key 再装 = 唯一索引冲突（不是新建）。
        let mut tx = fx.db.pool().begin().await.expect("tx");
        let dup = insert_tx(&mut tx, &new_install!(&fx, "hello.plugin")).await;
        assert!(matches!(dup, Err(RepoError::Conflict)));
        tx.rollback().await.expect("rollback");

        // 配置 / 启停 / 升级。
        let mut tx = fx.db.pool().begin().await.expect("tx");
        let configured = set_config_tx(&mut tx, row.id(), &json!({"greeting": "hi"}))
            .await
            .expect("config");
        assert_eq!(configured.config_object()["greeting"], "hi");
        let off = set_enabled_tx(&mut tx, row.id(), false)
            .await
            .expect("disable");
        assert!(!off.enabled);
        let upgraded = upgrade_tx(
            &mut tx,
            row.id(),
            &UpgradeInstallation {
                package_version_id: fx.package_version_id,
                version: "2.0.0",
                manifest: &json!({"name": "Hello", "version": 2}),
                granted_scopes: &json!(["issues:read"]),
                config: &json!({}),
            },
        )
        .await
        .expect("upgrade");
        assert_eq!(upgraded.version, "2.0.0");
        assert_eq!(upgraded.granted_scopes(), vec!["issues:read".to_string()]);
        tx.commit().await.expect("commit");

        teardown(&fx).await;
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_token_rotate_then_revoke_is_nullable() {
        let fx = fixture!();
        let repo = InstallationRepo::new(fx.db.clone());
        let mut tx = fx.db.pool().begin().await.expect("tx");
        let row = insert_tx(&mut tx, &new_install!(&fx, "token.plugin"))
            .await
            .expect("insert");
        tx.commit().await.expect("commit");
        assert!(row.token_hash.is_none() && row.token_rotated_at.is_none());

        // rotate：写入哈希并刷时间戳。
        let mut tx = fx.db.pool().begin().await.expect("tx");
        set_token_hash_tx(&mut tx, row.id(), Some(&"f".repeat(64)))
            .await
            .expect("rotate");
        tx.commit().await.expect("commit");
        let rotated = repo.get_by_id(row.id()).await.expect("get");
        assert_eq!(rotated.token_hash.as_deref(), Some("f".repeat(64).as_str()));
        assert!(rotated.token_rotated_at.is_some());

        // revoke：两列一起清空（幂等：再来一次仍是同一结果）。
        for _ in 0..2 {
            let mut tx = fx.db.pool().begin().await.expect("tx");
            set_token_hash_tx(&mut tx, row.id(), None)
                .await
                .expect("revoke");
            tx.commit().await.expect("commit");
        }
        let revoked = repo.get_by_id(row.id()).await.expect("get");
        assert!(revoked.token_hash.is_none() && revoked.token_rotated_at.is_none());

        teardown(&fx).await;
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_secret_upsert_list_delete_and_cascade() {
        let fx = fixture!();
        let repo = InstallationRepo::new(fx.db.clone());
        let mut tx = fx.db.pool().begin().await.expect("tx");
        let row = insert_tx(&mut tx, &new_install!(&fx, "secret.plugin"))
            .await
            .expect("insert");
        tx.commit().await.expect("commit");

        let mut tx = fx.db.pool().begin().await.expect("tx");
        upsert_secret_tx(&mut tx, row.id(), "api_key", b"sealed-1")
            .await
            .expect("upsert");
        upsert_secret_tx(&mut tx, row.id(), "api_key", b"sealed-2")
            .await
            .expect("re-upsert");
        upsert_secret_tx(&mut tx, row.id(), "other", b"sealed-3")
            .await
            .expect("upsert");
        tx.commit().await.expect("commit");
        assert_eq!(
            repo.secret_keys(row.id()).await.expect("keys"),
            vec!["api_key".to_string(), "other".to_string()]
        );
        let ciphertext: Vec<u8> = sqlx::query_scalar(
            "SELECT ciphertext FROM plugin_secret WHERE installation_id = $1 AND key = 'api_key'",
        )
        .bind(row.id().0)
        .fetch_one(fx.db.pool())
        .await
        .expect("ciphertext");
        assert_eq!(ciphertext, b"sealed-2");

        let mut tx = fx.db.pool().begin().await.expect("tx");
        delete_secret_tx(&mut tx, row.id(), "other")
            .await
            .expect("delete");
        tx.commit().await.expect("commit");
        assert_eq!(
            repo.secret_keys(row.id()).await.expect("keys"),
            vec!["api_key".to_string()]
        );

        // 卸载级联把密钥一起带走。
        let mut tx = fx.db.pool().begin().await.expect("tx");
        delete_cascade_tx(&mut tx, row.id()).await.expect("cascade");
        tx.commit().await.expect("commit");
        assert!(repo.get_by_id(row.id()).await.is_err());
        let left: i64 =
            sqlx::query_scalar("SELECT count(*) FROM plugin_secret WHERE installation_id = $1")
                .bind(row.id().0)
                .fetch_one(fx.db.pool())
                .await
                .expect("count");
        assert_eq!(left, 0);

        teardown(&fx).await;
    }
}
