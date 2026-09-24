//! 插件包：`plugin_package` / `plugin_package_version` / `plugin_package_file` 的读写。
//!
//! - **写者**：M6-5（**W**；`docs/57` §3.2）。
//! - **上游**：`internal/handler/plugin_packages.go`（+ `392_plugin_package_publishing` 的语义）。
//! - **7 / 9 / 7 列**：见 `mc_repos::plugin` 的表；两个 digest 列都是**纯 hex**（`char_length = 64`
//!   的 CHECK），带 `sha256:` 前缀会直接撞约束（bundle 的**线上形态**才有前缀，见 `mc_core::skill`）。
//! - **两条硬语义**：
//!   1. **版本不可变**：`plugin_package_version` 一旦落库不得 UPDATE（老设计里的
//!      `enforce_plugin_release_immutable()` 触发器已被 `344` drop，但**语义保留** ——
//!      本地靠「不写 UPDATE」而不是靠触发器）；本文件**没有任何** UPDATE 版本/文件的语句；
//!   2. `content` 是 **BYTEA**：行结构用 `Vec<u8>`（不要 `String`，否则非 UTF-8 包直接炸）。
//! - **本仓约定**：`workspace_id` 在 `plugin_package_version` 里是**冗余列**（`392` 故意加的，
//!   便于按工作区收窄查询）—— 写入时必须与 `plugin_package.workspace_id` 一致，别只写一处。
//! - **不做什么**：不做校验（zip 白名单 / 体积上限在 `mc-plugin-host::bundle`，本文件信任入参）；
//!   不做安装（`installation.rs`）。
//!
//! **状态：M6-5 已落地**。
//!
//! 行预算（门 ⑩）：预计 300 行以内。

use chrono::{DateTime, Utc};
use sqlx::types::Json;
use uuid::Uuid;

use super::installation::Tx;
use crate::workspace::map_sqlx_err;
use crate::{Db, RepoError, RepoWithDb, Result};
use mc_core::Id;

const PACKAGE_COLUMNS: &str =
    "id, workspace_id, plugin_key, name, created_by, created_at, updated_at";
const VERSION_COLUMNS: &str = "id, package_id, workspace_id, version, manifest, digest, \
                               size_bytes, published_by, created_at";
const FILE_COLUMNS: &str = "id, version_id, path, content, size_bytes, sha256, created_at";

/// 一行 `plugin_package`（一个可发布的插件身份）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PackageRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub plugin_key: String,
    pub name: String,
    pub created_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl PackageRow {
    pub fn id(&self) -> Id {
        Id(self.id)
    }
}

/// 一行 `plugin_package_version`（**不可变**）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PackageVersionRow {
    pub id: Uuid,
    pub package_id: Uuid,
    pub workspace_id: Uuid,
    pub version: String,
    pub manifest: Json<serde_json::Value>,
    /// 纯 hex（64 字符）。
    pub digest: String,
    pub size_bytes: i64,
    pub published_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

impl PackageVersionRow {
    pub fn id(&self) -> Id {
        Id(self.id)
    }
}

/// 一行 `plugin_package_file`。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PackageFileRow {
    pub id: Uuid,
    pub version_id: Uuid,
    pub path: String,
    /// BYTEA（**不是** `String`）。
    pub content: Vec<u8>,
    pub size_bytes: i64,
    /// 纯 hex（64 字符）。
    pub sha256: String,
    pub created_at: DateTime<Utc>,
}

/// `plugin_package*` 的读口。
#[derive(Debug, Clone)]
pub struct PackageRepo {
    db: Db,
}

impl RepoWithDb for PackageRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

impl PackageRepo {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 一个 workspace 的全部已发布包（上游 `ListWorkspacePluginPackages`）。
    pub async fn list(&self, workspace_id: Id) -> Result<Vec<PackageRow>> {
        sqlx::query_as::<_, PackageRow>(&format!(
            "SELECT {PACKAGE_COLUMNS} FROM plugin_package WHERE workspace_id = $1 \
             ORDER BY created_at ASC, plugin_key ASC"
        ))
        .bind(workspace_id.0)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 主键 + workspace 收窄（`GetWorkspacePluginPackage`）。
    pub async fn get(&self, workspace_id: Id, id: Id) -> Result<PackageRow> {
        sqlx::query_as::<_, PackageRow>(&format!(
            "SELECT {PACKAGE_COLUMNS} FROM plugin_package WHERE workspace_id = $1 AND id = $2"
        ))
        .bind(workspace_id.0)
        .bind(id.0)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)
    }

    /// 按 key 找包（`GetWorkspacePluginPackageByKey`）。
    pub async fn find_by_key(
        &self,
        workspace_id: Id,
        plugin_key: &str,
    ) -> Result<Option<PackageRow>> {
        sqlx::query_as::<_, PackageRow>(&format!(
            "SELECT {PACKAGE_COLUMNS} FROM plugin_package WHERE workspace_id = $1 AND plugin_key = $2"
        ))
        .bind(workspace_id.0)
        .bind(plugin_key)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 一个包的全部版本（`ListPluginPackageVersions`；最新在前）。
    pub async fn versions(&self, package_id: Id) -> Result<Vec<PackageVersionRow>> {
        sqlx::query_as::<_, PackageVersionRow>(&format!(
            "SELECT {VERSION_COLUMNS} FROM plugin_package_version WHERE package_id = $1 \
             ORDER BY created_at DESC"
        ))
        .bind(package_id.0)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 版本 + workspace 收窄（`GetWorkspacePluginPackageVersion`）；取不到 ⇒ `NotFound`。
    pub async fn get_version(&self, workspace_id: Id, id: Id) -> Result<PackageVersionRow> {
        sqlx::query_as::<_, PackageVersionRow>(&format!(
            "SELECT {VERSION_COLUMNS} FROM plugin_package_version WHERE workspace_id = $1 AND id = $2"
        ))
        .bind(workspace_id.0)
        .bind(id.0)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)
    }

    /// 版本里的一个文件（`GetPluginPackageFile`）。
    pub async fn file(&self, version_id: Id, path: &str) -> Result<PackageFileRow> {
        sqlx::query_as::<_, PackageFileRow>(&format!(
            "SELECT {FILE_COLUMNS} FROM plugin_package_file WHERE version_id = $1 AND path = $2"
        ))
        .bind(version_id.0)
        .bind(path)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)
    }
}

// ---------------------------------------------------------------------------
// 事务内写口
// ---------------------------------------------------------------------------

/// upsert 包身份（`upsertPackage`）：名字跟着**最新发布的那个版本**走，key 不动。
///
/// 键存在但名字变了 ⇒ 只 UPDATE 名字；不存在 ⇒ INSERT（并发时唯一索引兜底 ⇒ `Conflict`）。
pub async fn upsert_package_tx(
    tx: &mut Tx<'_>,
    workspace_id: Id,
    user_id: Id,
    plugin_key: &str,
    name: &str,
) -> Result<PackageRow> {
    let existing = sqlx::query_as::<_, PackageRow>(&format!(
        "SELECT {PACKAGE_COLUMNS} FROM plugin_package WHERE workspace_id = $1 AND plugin_key = $2"
    ))
    .bind(workspace_id.0)
    .bind(plugin_key)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_sqlx_err)?;

    if let Some(row) = existing {
        if row.name == name {
            return Ok(row);
        }
        return sqlx::query_as::<_, PackageRow>(&format!(
            "UPDATE plugin_package SET name = $2, updated_at = now() WHERE id = $1 \
             RETURNING {PACKAGE_COLUMNS}"
        ))
        .bind(row.id)
        .bind(name)
        .fetch_one(&mut **tx)
        .await
        .map_err(map_sqlx_err);
    }

    sqlx::query_as::<_, PackageRow>(&format!(
        "INSERT INTO plugin_package (workspace_id, plugin_key, name, created_by) \
         VALUES ($1, $2, $3, $4) RETURNING {PACKAGE_COLUMNS}"
    ))
    .bind(workspace_id.0)
    .bind(plugin_key)
    .bind(name)
    .bind(user_id.0)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx_err)
}

/// 列出该包已有版本（版本串去重用；`publish` 与 `upgrade` 都在锁内调它）。
pub async fn versions_tx(tx: &mut Tx<'_>, package_id: Id) -> Result<Vec<PackageVersionRow>> {
    sqlx::query_as::<_, PackageVersionRow>(&format!(
        "SELECT {VERSION_COLUMNS} FROM plugin_package_version WHERE package_id = $1 \
         ORDER BY created_at DESC"
    ))
    .bind(package_id.0)
    .fetch_all(&mut **tx)
    .await
    .map_err(map_sqlx_err)
}

/// 一次发布的输入（`insert_version_tx`）。
///
/// 做成结构体而不是 8 个位置参数：`digest` 与 `version` 都是字符串、`workspace_id` 与
/// `published_by` 都是 `Id`，位置参数会让人一眼看不出谁是谁。
#[derive(Debug, Clone)]
pub struct NewVersion<'a> {
    /// 所属包。
    pub package_id: Id,
    /// 冗余的 workspace（读面免 join）。
    pub workspace_id: Id,
    /// 版本号（`(package_id, version)` 唯一）。
    pub version: &'a str,
    /// 该版本的 manifest 快照。
    pub manifest: &'a serde_json::Value,
    /// bundle 的 sha256 hex（64 字符）。
    pub digest: &'a str,
    /// bundle 字节数（非负）。
    pub size_bytes: i64,
    /// 发布者。
    pub published_by: Id,
}

/// 插入一个**新的**不可变版本；唯一索引撞车 ⇒ [`RepoError::Conflict`]（版本已存在）。
pub async fn insert_version_tx(tx: &mut Tx<'_>, new: &NewVersion<'_>) -> Result<PackageVersionRow> {
    sqlx::query_as::<_, PackageVersionRow>(&format!(
        "INSERT INTO plugin_package_version \
           (package_id, workspace_id, version, manifest, digest, size_bytes, published_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING {VERSION_COLUMNS}"
    ))
    .bind(new.package_id.0)
    .bind(new.workspace_id.0)
    .bind(new.version)
    .bind(Json(new.manifest.clone()))
    .bind(new.digest)
    .bind(new.size_bytes)
    .bind(new.published_by.0)
    .fetch_one(&mut **tx)
    .await
    .map_err(map_sqlx_err)
}

/// 插入一个包内文件（`content` 是 BYTEA，`sha256` 是纯 hex）。
pub async fn insert_file_tx(
    tx: &mut Tx<'_>,
    version_id: Id,
    path: &str,
    content: &[u8],
    sha256: &str,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO plugin_package_file (version_id, path, content, size_bytes, sha256) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(version_id.0)
    .bind(path)
    .bind(content)
    .bind(
        i64::try_from(content.len())
            .map_err(|_| RepoError::Db("plugin package file exceeds i64 bytes".to_string()))?,
    )
    .bind(sha256)
    .execute(&mut **tx)
    .await
    .map_err(map_sqlx_err)
    .map(|_| ())
}

/// 删包里的一个文件（卸载/删包前的显式清理；文件先走，因为它的版本马上就不存在了）。
pub async fn delete_files_by_package_tx(tx: &mut Tx<'_>, package_id: Id) -> Result<()> {
    sqlx::query(
        "DELETE FROM plugin_package_file f USING plugin_package_version v \
          WHERE f.version_id = v.id AND v.package_id = $1",
    )
    .bind(package_id.0)
    .execute(&mut **tx)
    .await
    .map_err(map_sqlx_err)
    .map(|_| ())
}

/// 删该包的全部版本行（`DeletePluginPackageVersionsByPackage`）。
pub async fn delete_versions_by_package_tx(tx: &mut Tx<'_>, package_id: Id) -> Result<()> {
    sqlx::query("DELETE FROM plugin_package_version WHERE package_id = $1")
        .bind(package_id.0)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx_err)
        .map(|_| ())
}

/// 事务内按 id 读一个版本（`GetWorkspacePluginPackageVersion`）；取不到 ⇒ `None`。
///
/// 存在的唯一理由：安装/升级在**持有 `lock_plugin_key_tx` 之后**要再确认一次版本还在
/// （上游 `requireVersionStillPublished`）。锁外那次读（[`PackageRepo::get_version`]）只是
/// 「先在无锁路径上拒掉明显非法的入参」，不能替代它 —— 并发的删包可能正好落在两者之间。
pub async fn get_version_tx(
    tx: &mut Tx<'_>,
    workspace_id: Id,
    version_id: Id,
) -> Result<Option<PackageVersionRow>> {
    sqlx::query_as::<_, PackageVersionRow>(&format!(
        "SELECT {VERSION_COLUMNS} FROM plugin_package_version WHERE workspace_id = $1 AND id = $2"
    ))
    .bind(workspace_id.0)
    .bind(version_id.0)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_sqlx_err)
}

/// 事务内读版本里的一个文件（`GetPluginPackageFile`）；取不到 ⇒ `None`。
///
/// 物化 skill 资源时必须走事务：文件与安装行要落在同一个提交里，`FILE_COLUMNS` 是私有的，
/// 路由层拼不出等价 SELECT。
pub async fn file_tx(
    tx: &mut Tx<'_>,
    version_id: Id,
    path: &str,
) -> Result<Option<PackageFileRow>> {
    sqlx::query_as::<_, PackageFileRow>(&format!(
        "SELECT {FILE_COLUMNS} FROM plugin_package_file WHERE version_id = $1 AND path = $2"
    ))
    .bind(version_id.0)
    .bind(path)
    .fetch_optional(&mut **tx)
    .await
    .map_err(map_sqlx_err)
}

/// 删包身份行（`DeletePluginPackage`）。
pub async fn delete_package_tx(tx: &mut Tx<'_>, package_id: Id) -> Result<()> {
    sqlx::query("DELETE FROM plugin_package WHERE id = $1")
        .bind(package_id.0)
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
    use crate::plugin::installation::{
        count_installations_of_versions_tx, insert_tx, NewInstallation,
    };
    use serde_json::json;
    use std::env;
    use uuid::Uuid;

    struct Fixture {
        db: Db,
        workspace_id: Id,
        user_id: Id,
    }

    async fn setup() -> Option<Fixture> {
        let url = env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        let db = Db::connect(&url, 4, 1).await.ok()?;
        let workspace_id: Uuid = sqlx::query_scalar(
            "INSERT INTO workspace(name, slug) VALUES ('itest-m65pkg', $1) RETURNING id",
        )
        .bind(format!("itest-m65pkg-{}", Uuid::new_v4()))
        .fetch_one(db.pool())
        .await
        .ok()?;
        let user_id: Uuid = sqlx::query_scalar(
            r#"INSERT INTO "user"(name, email) VALUES ('itest-m65pkg', $1) RETURNING id"#,
        )
        .bind(format!("itest-m65pkg-{}@example.com", Uuid::new_v4()))
        .fetch_one(db.pool())
        .await
        .ok()?;
        Some(Fixture {
            db,
            workspace_id: Id::from(workspace_id),
            user_id: Id::from(user_id),
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

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_upsert_package_reuses_row_and_tracks_name() {
        let fx = fixture!();
        let repo = PackageRepo::new(fx.db.clone());

        let mut tx = fx.db.pool().begin().await.expect("tx");
        let first = upsert_package_tx(&mut tx, fx.workspace_id, fx.user_id, "a.plugin", "Alpha")
            .await
            .expect("insert");
        tx.commit().await.expect("commit");

        // 同 key 再发布 = 同一 package（只跟新名字走，不新建行）。
        let mut tx = fx.db.pool().begin().await.expect("tx");
        let renamed =
            upsert_package_tx(&mut tx, fx.workspace_id, fx.user_id, "a.plugin", "Alpha v2")
                .await
                .expect("rename");
        tx.commit().await.expect("commit");
        assert_eq!(renamed.id, first.id);
        assert_eq!(renamed.name, "Alpha v2");
        assert_eq!(repo.list(fx.workspace_id).await.expect("list").len(), 1);

        teardown(&fx).await;
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_version_is_immutable_and_files_round_trip_as_bytes() {
        let fx = fixture!();
        let repo = PackageRepo::new(fx.db.clone());
        let mut tx = fx.db.pool().begin().await.expect("tx");
        let package = upsert_package_tx(&mut tx, fx.workspace_id, fx.user_id, "b.plugin", "Beta")
            .await
            .expect("package");
        let version = insert_version_tx(
            &mut tx,
            &NewVersion {
                package_id: package.id(),
                workspace_id: fx.workspace_id,
                version: "1.0.0",
                manifest: &json!({"name": "Beta"}),
                digest: &"b".repeat(64),
                size_bytes: 3,
                published_by: fx.user_id,
            },
        )
        .await
        .expect("version");
        tx.commit().await.expect("commit");

        // 重复发布同一版本 ⇒ 唯一键冲突，绝不 UPDATE。
        let mut tx = fx.db.pool().begin().await.expect("tx");
        let dup = insert_version_tx(
            &mut tx,
            &NewVersion {
                package_id: package.id(),
                workspace_id: fx.workspace_id,
                version: "1.0.0",
                manifest: &json!({"name": "Beta"}),
                digest: &"c".repeat(64),
                size_bytes: 9,
                published_by: fx.user_id,
            },
        )
        .await;
        assert!(matches!(dup, Err(RepoError::Conflict)));
        tx.rollback().await.expect("rollback");
        let versions = repo.versions(package.id()).await.expect("versions");
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].digest, "b".repeat(64));

        // 文件 content 是 BYTEA：非 UTF-8 字节能原样往返。
        let raw = vec![0xff_u8, 0x00, 0xfe];
        let mut tx = fx.db.pool().begin().await.expect("tx");
        insert_file_tx(&mut tx, version.id(), "surface.js", &raw, &"d".repeat(64))
            .await
            .expect("file");
        tx.commit().await.expect("commit");
        let file = repo.file(version.id(), "surface.js").await.expect("read");
        assert_eq!(file.content, raw);
        assert_eq!(file.size_bytes, 3);

        // 事务内读口（安装路径在持锁后用它再确认版本/取 skill 正文）。
        let mut tx = fx.db.pool().begin().await.expect("tx");
        let in_tx = get_version_tx(&mut tx, fx.workspace_id, version.id())
            .await
            .expect("version")
            .expect("present");
        assert_eq!(in_tx.id(), version.id());
        assert_eq!(
            file_tx(&mut tx, version.id(), "surface.js")
                .await
                .expect("file")
                .expect("present")
                .content,
            raw
        );
        assert!(file_tx(&mut tx, version.id(), "missing.js")
            .await
            .expect("file")
            .is_none());
        assert!(get_version_tx(&mut tx, fx.workspace_id, Id::nil())
            .await
            .expect("version")
            .is_none());
        tx.rollback().await.expect("rollback");

        teardown(&fx).await;
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_delete_package_cascade_and_installation_guard() {
        let fx = fixture!();
        let repo = PackageRepo::new(fx.db.clone());
        let mut tx = fx.db.pool().begin().await.expect("tx");
        let package = upsert_package_tx(&mut tx, fx.workspace_id, fx.user_id, "c.plugin", "Gamma")
            .await
            .expect("package");
        let version = insert_version_tx(
            &mut tx,
            &NewVersion {
                package_id: package.id(),
                workspace_id: fx.workspace_id,
                version: "1.0.0",
                manifest: &json!({"name": "Gamma"}),
                digest: &"e".repeat(64),
                size_bytes: 4,
                published_by: fx.user_id,
            },
        )
        .await
        .expect("version");
        insert_file_tx(&mut tx, version.id(), "index.js", b"x", &"f".repeat(64))
            .await
            .expect("file");
        let install = insert_tx(
            &mut tx,
            &NewInstallation {
                workspace_id: fx.workspace_id,
                plugin_key: "c.plugin",
                package_version_id: version.id(),
                version: "1.0.0",
                manifest: &json!({"name": "Gamma"}),
                granted_scopes: &json!([]),
                installed_by: fx.user_id,
            },
        )
        .await
        .expect("install");
        tx.commit().await.expect("commit");

        // 还有安装指着这些版本 ⇒ 不许删包。
        let mut tx = fx.db.pool().begin().await.expect("tx");
        let users = count_installations_of_versions_tx(&mut tx, package.id())
            .await
            .expect("count");
        assert_eq!(users, 1);
        tx.rollback().await.expect("rollback");

        let mut tx = fx.db.pool().begin().await.expect("tx");
        sqlx::query("DELETE FROM plugin_installation WHERE id = $1")
            .bind(install.id().0)
            .execute(&mut *tx)
            .await
            .expect("drop install");
        delete_files_by_package_tx(&mut tx, package.id())
            .await
            .expect("files");
        delete_versions_by_package_tx(&mut tx, package.id())
            .await
            .expect("versions");
        delete_package_tx(&mut tx, package.id())
            .await
            .expect("package");
        tx.commit().await.expect("commit");
        assert!(repo.get(fx.workspace_id, package.id()).await.is_err());

        teardown(&fx).await;
    }
}
