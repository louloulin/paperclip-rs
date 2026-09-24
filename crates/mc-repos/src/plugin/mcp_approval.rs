//! `plugin_installation.mcp_approvals`（JSONB）的读写与交叉校验。
//!
//! - **写者**：M6-6（**W**；`docs/57` §3.2）。
//! - **上游**：`internal/handler/plugin_mcp.go` + 迁移 `369_plugin_mcp_approvals`。
//! - **形状**（`369` 的 CHECK 是契约）：
//!   `{"<hook_key>": {"tools": [{"name": …, "schema_digest": …}], "approved_at": …, "approved_by": …}}`
//!   —— 顶层是**对象**（非空对象的 CHECK；空对象合法）。
//! - **两条硬语义**：
//!   1. 采纳是**按 hook 分组**的，不是按安装整体 —— 一个 hook 的工具集变化**只**失效那一个 hook；
//!   2. `schema_digest` 是**比对依据**：远端 `tools/list` 的 schema 变了 ⇒ 该工具视为**未采纳**
//!      （要求重新采纳），不能静默沿用旧授权（`mc-mcp::client` 的 `validate_pinned_remote_mcp_tools`）。
//! - **本仓约定**：`mcp_approvals` 的行写入用**读-改-写**（JSONB 整块替换）+ 事务；
//!   `approved_by` 可空，但**写侧不得用空串冒充 NULL**（`""` 不是合法 `Id`——见
//!   `mc_core::plugin::PluginMcpApproval` 的注释）。
//! - **不做什么**：不做工具 schema 的语义比较（digest 比对即可）；不做审批流的审计表。
//!
//! # M6-6 落地说明（LUM-1671）
//!
//! 三处与桩注释的**有意**差异，都登记在 `docs/32` §9：
//!
//! 1. **写侧不用「先读再写」**：JSONB 的增/删是单条 `UPDATE` 里的
//!    `mcp_approvals || jsonb_build_object($2, $3)` / `mcp_approvals - $2`。桩写的是
//!    「读-改-写 + 事务」，但读-改-写让两个管理员**同时**批准两个不同 hook 时可能丢掉一个
//!    （read→write 窗口内的更新被整块覆盖）；单条语句语义完全等价（整块替换该 hook 的值、
//!    删掉该 hook 的键），却天然没有这个窗口。`updated_at = now()` 与上游
//!    `SetPluginMCPApprovals` 逐字相同，`RETURNING` 让「安装已消失」照上游落成 `NotFound`。
//! 2. **存的是窄形态**：上游 `mcp_approvals` 里塞的是整个 `remotemcp.Tool`（带 description /
//!    inputSchema）；本仓落 `mc_core::plugin::PluginApprovedTool`（`name` + `schema_digest`），
//!    这正是**迁移 `369` 注释里写下的形状**，也是 `mc-core` 的头表口径。少掉的字段只是
//!    「展示用」，比对（本文件唯一关心的语义）只用这两个。
//! 3. **`installation_for_workspace` / `package_file_sha256` 是 M6-6 的窄读垫片**：
//!    `installation.rs` / `package.rs` 归 M6-5，M6-6 不能改它们的文件；本文件只读
//!    「启动一个 surface / 发现一个 MCP 端点」需要的列（不含 `config` / `plugin_secret`），
//!    收敛点登记给 M6-INT（`LUM-1675`）。manifest **不在这里解析**（`plugin/mod.rs` 的约定：
//!    JSONB 原样存取），解析归 `mc-http` 的两条 route（它们才依赖 `mc-plugin-host`）。
//!
//! **状态：M6-6 已落地（LUM-1671）**。
//!
//! 行预算（门 ⑩）：桩写「200 行以内」，落地 396 行（含 `db_tests`）；非测试部分 200 行 ✓。

use serde_json::Value as JsonValue;
use uuid::Uuid;

use mc_core::plugin::PluginMcpApprovals;
use mc_core::Id;
use mc_db::Db;

use crate::workspace::map_sqlx_err;
use crate::{RepoError, Result};

/// `plugin_installation` 的 M6-6 **窄读**投影（见文件头的差异 3）。
///
/// 上游 `PluginService.InstallationForWorkspace` 返回整行；本片只取「装了什么版本、管理员同意了
/// 什么、manifest 快照是什么、开关是否打开」这几列。**故意不取** `config` / `plugin_secret`
/// （那是 M6-5/M6-7 的面）与 `token_hash`（凭据哈希没有任何读面需要它）。
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PluginInstallationRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub plugin_key: String,
    pub version: String,
    /// 安装时快照下来的 manifest（JSONB）。**原样读出**，解析归 route 层。
    pub manifest: JsonValue,
    /// 授予的 scope 数组（JSONB）。
    pub granted_scopes: JsonValue,
    /// 唯一开关（**没有** `status` 列）。
    pub enabled: bool,
    /// `mcp_approvals` 整块（JSONB）。
    pub mcp_approvals: JsonValue,
    /// 不可变版本 id：surface 的入口脚本按它取（`plugin_package_file.version_id`）。
    pub package_version_id: Uuid,
}

impl PluginInstallationRow {
    pub fn id(&self) -> Id {
        Id::from(self.id)
    }

    pub fn workspace_id(&self) -> Id {
        Id::from(self.workspace_id)
    }

    pub fn package_version_id(&self) -> Id {
        Id::from(self.package_version_id)
    }

    /// 授予的 scope 列表，**坏值折成空表**（上游 `decodeScopes` 就是 `json.Unmarshal` 失败即
    /// `nil`）。折成空表是**fail-closed**：`net:` 域取空 ⇒ 出网调用被拒，而不是「无限制」。
    pub fn granted_scopes(&self) -> Vec<String> {
        serde_json::from_value(self.granted_scopes.clone()).unwrap_or_default()
    }
}

/// `plugin_installation.mcp_approvals` + M6-6 两条窄读的仓储。
pub struct PluginApprovalRepo {
    db: Db,
}

impl PluginApprovalRepo {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 上游 `InstallationForWorkspace(ctx, workspaceID, installationID)`：按
    /// **workspace + 安装 id** 收窄，两者任一不匹配都是 [`RepoError::NotFound`]
    /// （跨工作区读安装 = 不存在，不是 403）。
    ///
    /// 非 UUID 串在上游由 `util.ParseUUID` 判掉、同样折成 NotFound（"plugin installation
    /// not found"）；本仓额外**接受首尾空白**（`docs/51:177` 已登记这一类偏差）。
    ///
    /// # Errors
    ///
    /// [`RepoError::NotFound`]（无此行 / id 非法）、[`RepoError::Db`]。
    pub async fn installation_for_workspace(
        &self,
        workspace_id: Id,
        installation_id: &str,
    ) -> Result<PluginInstallationRow> {
        let Ok(installation_id) = Uuid::parse_str(installation_id.trim()) else {
            return Err(RepoError::NotFound);
        };
        let row = sqlx::query_as::<_, PluginInstallationRow>(
            "SELECT id, workspace_id, plugin_key, version, manifest, granted_scopes, enabled, \
             mcp_approvals, package_version_id \
             FROM plugin_installation \
             WHERE id = $1 AND workspace_id = $2",
        )
        .bind(installation_id)
        .bind(workspace_id.0)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        row.ok_or(RepoError::NotFound)
    }

    /// 读整块 `mcp_approvals`。
    ///
    /// 反序列化失败 ⇒ **空表**（上游 `decodeMCPApprovals` 吞掉 `json.Unmarshal` 的错误）。
    /// 好处是与上游逐字同语义（一列坏数据不会让整个读面 500）；代价是坏数据读起来与「什么都没
    /// 批准」无异 ⇒ 调用被拒（fail-closed），而不是放开。
    ///
    /// # Errors
    ///
    /// [`RepoError::Db`]。
    pub async fn approvals(&self, installation_id: Id) -> Result<PluginMcpApprovals> {
        let raw: Option<JsonValue> =
            sqlx::query_scalar("SELECT mcp_approvals FROM plugin_installation WHERE id = $1")
                .bind(installation_id.0)
                .fetch_optional(self.db.pool())
                .await
                .map_err(map_sqlx_err)?;
        Ok(raw
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default())
    }

    /// 批准（`Some`）或撤回（`None`）**一个 hook** 的采纳清单。
    ///
    /// 撤回 = 删掉该 hook 的键（上游：空的已批准列表不存成空数组，那读起来像「批准了，只是
    /// 里面没东西」）。同一个安装的**其它 hook 一个字节都不动** —— 这就是迁移 `369` 那份设计
    /// 的全部意义（一个 hook 的工具集变化只失效它自己）。
    ///
    /// 单条语句，语义上是整块替换（见文件头的差异 1）。
    ///
    /// # Errors
    ///
    /// [`RepoError::NotFound`]（安装行在读到写之间消失，照上游 `:one` 的 `ErrNoRows`）、
    /// [`RepoError::Db`]。
    pub async fn set_hook_approval(
        &self,
        installation_id: Id,
        hook_key: &str,
        approval: Option<&mc_core::plugin::PluginMcpApproval>,
    ) -> Result<()> {
        let keep = approval.is_some();
        // `None` 分支不会被求值，但 `$3::jsonb` 需要一个具体类型 ⇒ 给 JSON `null`。
        let value = match approval {
            Some(approval) => {
                serde_json::to_value(approval).map_err(|e| RepoError::Db(e.to_string()))?
            }
            None => JsonValue::Null,
        };
        sqlx::query(
            "UPDATE plugin_installation \
             SET mcp_approvals = CASE WHEN $3 THEN mcp_approvals || jsonb_build_object($2, $4::jsonb) \
                                      ELSE mcp_approvals - $2 END, \
                 updated_at = now() \
             WHERE id = $1 \
             RETURNING mcp_approvals",
        )
        .bind(installation_id.0)
        .bind(hook_key)
        .bind(keep)
        .bind(value)
        .fetch_one(self.db.pool())
        .await
        .map_err(|e| match e {
            sqlx::Error::RowNotFound => RepoError::NotFound,
            other => map_sqlx_err(other),
        })?;
        Ok(())
    }

    /// 已安装版本里某个文件的 sha256（纯 hex）—— surface 启动要拿它当启动令牌的 `digest`。
    ///
    /// 上游 `GetPluginPackageFile(...)` 取整行（含 `content`）；本片**只要摘要**，所以只选一列：
    /// 启动路径不读脚本字节（那是 M6-7 的 `ServePluginSurface`），也就不该把包体拉进内存。
    ///
    /// 缺行返回 `Ok(None)`：调用方要按上游文案（"the installed version does not contain %q"）
    /// 落 404，而不是落成 500。
    ///
    /// # Errors
    ///
    /// [`RepoError::Db`]。
    pub async fn package_file_sha256(&self, version_id: Id, path: &str) -> Result<Option<String>> {
        let sha: Option<String> = sqlx::query_scalar(
            "SELECT sha256 FROM plugin_package_file WHERE version_id = $1 AND path = $2",
        )
        .bind(version_id.0)
        .bind(path)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(sha)
    }
}

// ---------------------------------------------------------------------------
// PG 集成测试（需要真库）
//
// `plugin_*` 八张表在迁移里**没有一条外键**（`344` / `362` / `369` / `392`），所以夹具不需要
// workspace / user 行 —— 也正是这样，这里能只验本表的两条核心语义（按 hook 分组、撤回）。
// ---------------------------------------------------------------------------
#[cfg(test)]
mod db_tests {
    use super::*;
    use mc_core::plugin::{PluginApprovedTool, PluginMcpApproval};

    struct Fixture {
        db: Db,
        workspace_id: Id,
        installation_id: Id,
        version_id: Id,
    }

    impl Fixture {
        async fn new(db: Db) -> Self {
            let workspace_id = Id::from(Uuid::new_v4());
            let installation_id = Id::from(Uuid::new_v4());
            let version_id = Id::from(Uuid::new_v4());
            sqlx::query(
                "INSERT INTO plugin_installation \
                 (id, workspace_id, plugin_key, version, manifest, granted_scopes, package_version_id) \
                 VALUES ($1, $2, 'com.example.demo', '1.0.0', '{}'::jsonb, '[\"net:mcp.example.com\"]'::jsonb, $3)",
            )
            .bind(installation_id.0)
            .bind(workspace_id.0)
            .bind(version_id.0)
            .execute(db.pool())
            .await
            .expect("insert installation");
            Self {
                db,
                workspace_id,
                installation_id,
                version_id,
            }
        }

        async fn cleanup(&self) {
            let _ = sqlx::query("DELETE FROM plugin_package_file WHERE version_id = $1")
                .bind(self.version_id.0)
                .execute(self.db.pool())
                .await;
            let _ = sqlx::query("DELETE FROM plugin_installation WHERE id = $1")
                .bind(self.installation_id.0)
                .execute(self.db.pool())
                .await;
        }
    }

    async fn setup() -> Option<Fixture> {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        let db = Db::connect(&url, 4, 1).await.ok()?;
        Some(Fixture::new(db).await)
    }

    macro_rules! fixture {
        () => {
            match setup().await {
                Some(v) => v,
                None => {
                    eprintln!("skipping: set MULTICA_TEST_DATABASE_URL to run");
                    return;
                }
            }
        };
    }

    fn approval(name: &str, digest: &str) -> PluginMcpApproval {
        PluginMcpApproval {
            tools: vec![PluginApprovedTool {
                name: name.into(),
                schema_digest: digest.into(),
            }],
            approved_at: mc_core::Timestamp::now(),
            approved_by: None,
        }
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_installation_is_scoped_to_the_workspace() {
        let fx = fixture!();
        let repo = PluginApprovalRepo::new(fx.db.clone());

        let row = repo
            .installation_for_workspace(fx.workspace_id, &fx.installation_id.0.to_string())
            .await
            .expect("found");
        assert_eq!(row.id, fx.installation_id.0);
        assert_eq!(row.plugin_key, "com.example.demo");
        assert_eq!(row.version, "1.0.0");
        assert_eq!(row.package_version_id, fx.version_id.0);
        assert!(row.enabled);
        assert_eq!(row.granted_scopes(), vec!["net:mcp.example.com"]);

        // 另一个 workspace 问同一台安装：不存在（不是 403）。
        let other = Id::from(Uuid::new_v4());
        assert!(matches!(
            repo.installation_for_workspace(other, &fx.installation_id.0.to_string())
                .await,
            Err(RepoError::NotFound)
        ));
        // 非 UUID / 未知 UUID 同样 NotFound。
        assert!(matches!(
            repo.installation_for_workspace(fx.workspace_id, "not-a-uuid")
                .await,
            Err(RepoError::NotFound)
        ));
        assert!(matches!(
            repo.installation_for_workspace(fx.workspace_id, &Uuid::new_v4().to_string())
                .await,
            Err(RepoError::NotFound)
        ));
        // 接受首尾空白（本仓 `parse_uuid` 家族的既有偏差）。
        assert!(repo
            .installation_for_workspace(fx.workspace_id, &format!("  {}  ", fx.installation_id.0))
            .await
            .is_ok());

        fx.cleanup().await;
    }

    /// 迁移 `369` 的核心语义：**按 hook 分组**。批准 hook A 之后再批准 hook B，A 的清单
    /// 必须原样还在；撤回 A 之后 B 也必须原样还在。
    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_approvals_are_per_hook_and_withdrawal_leaves_the_others_alone() {
        let fx = fixture!();
        let repo = PluginApprovalRepo::new(fx.db.clone());

        assert!(repo
            .approvals(fx.installation_id)
            .await
            .expect("empty")
            .is_empty());

        let alpha = approval("search", "aa11");
        repo.set_hook_approval(fx.installation_id, "alpha", Some(&alpha))
            .await
            .expect("approve alpha");
        let beta = approval("lookup", "bb22");
        repo.set_hook_approval(fx.installation_id, "beta", Some(&beta))
            .await
            .expect("approve beta");

        let stored = repo.approvals(fx.installation_id).await.expect("read back");
        assert_eq!(stored.len(), 2);
        // 逐字段回读：窄形态（name + schema_digest）与 approved_at 都要能往返。
        assert_eq!(stored["alpha"].tools, alpha.tools);
        assert_eq!(stored["alpha"].approved_at, alpha.approved_at);
        assert_eq!(stored["alpha"].approved_by, None);
        assert_eq!(stored["beta"].tools, beta.tools);

        // 重新批准同一 hook = 整块替换它自己，不动别人。
        let alpha2 = approval("search", "aa99");
        repo.set_hook_approval(fx.installation_id, "alpha", Some(&alpha2))
            .await
            .expect("re-approve alpha");
        let stored = repo.approvals(fx.installation_id).await.expect("read back");
        assert_eq!(stored["alpha"].tools, alpha2.tools);
        assert_eq!(stored["beta"].tools, beta.tools);

        // 撤回：键消失，另一个键还在（不是把整块清空）。
        repo.set_hook_approval(fx.installation_id, "alpha", None)
            .await
            .expect("withdraw alpha");
        let stored = repo.approvals(fx.installation_id).await.expect("read back");
        assert!(!stored.contains_key("alpha"));
        assert_eq!(stored["beta"].tools, beta.tools);

        // 未知安装 ⇒ NotFound（上游 `:one` 的 ErrNoRows）。
        assert!(matches!(
            repo.set_hook_approval(Id::from(Uuid::new_v4()), "alpha", Some(&alpha))
                .await,
            Err(RepoError::NotFound)
        ));

        fx.cleanup().await;
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_package_file_sha256_reads_one_column() {
        let fx = fixture!();
        let repo = PluginApprovalRepo::new(fx.db.clone());
        let sha = "a".repeat(64);

        sqlx::query(
            "INSERT INTO plugin_package_file (version_id, path, content, size_bytes, sha256) \
             VALUES ($1, 'dist/surface.js', 'x'::bytea, 1, $2)",
        )
        .bind(fx.version_id.0)
        .bind(&sha)
        .execute(fx.db.pool())
        .await
        .expect("insert file");

        assert_eq!(
            repo.package_file_sha256(fx.version_id, "dist/surface.js")
                .await
                .expect("hit"),
            Some(sha)
        );
        assert_eq!(
            repo.package_file_sha256(fx.version_id, "dist/missing.js")
                .await
                .expect("miss"),
            None
        );

        fx.cleanup().await;
    }
}
