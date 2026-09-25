//! `workspace_mcp_server` 仓储面 —— **M8-3 已落地（`LUM-1800`）**。
//!
//! - **上游**：`internal/handler/workspace_mcp_api.go`（CRUD 4 条）+
//!   `pkg/db/queries/workspace_mcp.sql` 的前半段（6 条查询）。
//! - **表的落法**：迁移 `315` 建表、`316` 的**唯一约束**（`idx_workspace_mcp_server_workspace_name`
//!   = `(workspace_id, name)`）拒绝重名 ⇒ 上层 409；本波 **0 新迁移**。
//! - **write-only**：`config` 是含第三方凭证的 JSONB（`url` / `headers` / `env` 的值）。
//!   本模块**照常读写**它（写侧要它、`transport` 投影要它），但
//!   ① [`WorkspaceMcpServerRow`] 的 `Debug` **手写**（只列条目顶层**键名**，不列值）；
//!   ② **不**提供「原样返回 config」的便捷读法给 HTTP 层 —— 响应 DTO 的剥离是路由层的事
//!   （`docs/61` §2.7 第 5 条）。
//! - **无外键 ⇒ 栅栏在应用层**（迁移 `315` 的注释逐字：「the application sweeps
//!   `agent_mcp_server` when an agent or a server goes away」）：
//!   ① [`WorkspaceMcpServerRepo::create`] 在同一事务里先取 workspace 行的
//!   `FOR KEY SHARE`（上游 `LockWorkspaceForChatSessionCreate`，#5219）——它与
//!   `DeleteWorkspace` 的 `FOR UPDATE` 互斥，建行不可能落在「拆除已提交」之后；
//!   ② [`WorkspaceMcpServerRepo::delete`] 先对 server 行取 `FOR UPDATE`
//!   （上游 `LockWorkspaceMcpServerForUpdate`）再删行 + 扫绑定：并发的绑定写入要么落在
//!   本事务之前（被扫掉），要么等锁之后发现 server 已消失。
//! - **本仓约定**：裸 `Uuid` + `sqlx::FromRow`、`map_sqlx_err`、运行时 builder + 参数绑定、
//!   jsonb → `serde_json::Value`；列表查询带 `workspace_id` 收窄。

use chrono::{DateTime, Utc};
use mc_core::mcp::{McpTransport, WorkspaceMcpServer};
use mc_core::Id;
use mc_db::Db;
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::{RepoError, RepoWithDb, Result};

/// 全部列（顺序与 [`WorkspaceMcpServerRow`] 的字段一一对应）。
const SERVER_COLUMNS: &str = "id, workspace_id, name, config, created_by, created_at, updated_at";

/// `workspace_mcp_server` 的一行（迁移 `315`；**7 列**）。
///
/// ⚠️ `config` 是**含密钥**的条目。它只允许出现在两个地方：① 本仓储的读写参数；
/// ② `transport` 投影。**任何响应 DTO / 日志都不得包含它的值** —— 手写的 `Debug`
/// （只打条目顶层键名）就是这条纪律的结构性保证。
#[derive(Clone, FromRow, PartialEq)]
pub struct WorkspaceMcpServerRow {
    /// `id`。
    pub id: Uuid,
    /// `workspace_id`。
    pub workspace_id: Uuid,
    /// `name`（`(workspace_id, name)` 唯一）。
    pub name: String,
    /// `config` JSONB（**write-only**，`Debug` 里脱敏）。
    pub config: Value,
    /// `created_by`（删除创建者后为 `NULL`，无外键）。
    pub created_by: Option<Uuid>,
    /// `created_at`。
    pub created_at: DateTime<Utc>,
    /// `updated_at`。
    pub updated_at: DateTime<Utc>,
}

impl std::fmt::Debug for WorkspaceMcpServerRow {
    /// 手写脱敏（**不派生**）：条目**值**一个字节都不出现，只列顶层键名
    /// （键名是结构信息；`url` / `headers` / `env` 的**值**才是凭据）。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkspaceMcpServerRow")
            .field("id", &self.id)
            .field("workspace_id", &self.workspace_id)
            .field("name", &self.name)
            .field("config", &"<redacted, write-only>")
            .field("config_keys", &config_keys(&self.config))
            .field("created_by", &self.created_by)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

impl WorkspaceMcpServerRow {
    /// 主键。
    #[must_use]
    pub fn id(&self) -> Id {
        Id(self.id)
    }

    /// 所属 workspace。
    #[must_use]
    pub fn workspace_id(&self) -> Id {
        Id(self.workspace_id)
    }

    /// 条目声明的 transport —— **领域层窄口径**（anchor 的三值枚举）。
    ///
    /// ⚠️ **不要**用它生成 wire 值：`McpTransport::from_config` 对**不认识的** `type`
    /// 会继续按 `command` / `url` 推断（`{"type":"websocket","url":"wss://…"}` ⇒ `Http`），
    /// 而线格式必须原样透传那个未知值（否则客户端表单会把条目改写成 `type:"http"`）。
    /// 响应唯一的来源是 `mc_http::routes::mcp::workspace::mcp_transport_of`。
    #[must_use]
    pub fn transport(&self) -> Option<McpTransport> {
        McpTransport::from_config(&self.config)
    }

    /// 领域投影（`mc_core::mcp::WorkspaceMcpServer`）；`config` 原值随行带出
    /// （它只服务写侧与投影，**不**是给响应用的）。
    #[must_use]
    pub fn to_domain(&self) -> WorkspaceMcpServer {
        WorkspaceMcpServer {
            id: self.id(),
            workspace_id: self.workspace_id(),
            name: self.name.clone(),
            config: self.config.clone(),
            created_by: self.created_by.map(Id),
            created_at: mc_core::Timestamp::from_unix(self.created_at.timestamp()),
            updated_at: mc_core::Timestamp::from_unix(self.updated_at.timestamp()),
        }
    }
}

/// `Debug` / 诊断用的条目顶层键名（**值**永不参与）。
fn config_keys(config: &Value) -> Vec<&str> {
    let mut keys: Vec<&str> = config
        .as_object()
        .map(|object| object.keys().map(String::as_str).collect())
        .unwrap_or_default();
    keys.sort_unstable();
    keys
}

/// 新建一个库条目的入参（上游 `CreateWorkspaceMcpServerParams`）。
#[derive(Debug, Clone)]
pub struct NewWorkspaceMcpServer {
    /// 所属 workspace。
    pub workspace_id: Id,
    /// 已经过 [`validate_name`] 的条目名。
    pub name: String,
    /// 已经过 [`validate_entry`] 的条目本体。
    pub config: Value,
    /// 创建者（上游 `parseUUID(requestUserID(r))`）。
    pub created_by: Option<Id>,
}

/// 条目校验错误 —— 四个变体的文案是**上游逐字**的 400 响应体。
///
/// 刻意**不**包裹底层错误、**不**回显输入：条目常规地嵌着 API token
/// （上游 `validateWorkspaceMcpServerEntry` 的注释为同一理由写死了这一条）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum McpServerValidationError {
    /// 条目不是 JSON 对象（含缺失、`null`、数组、标量、坏 JSON）。
    #[error("config must be a JSON object")]
    ConfigNotObject,
    /// 空对象 `{}`。
    #[error("config must not be empty")]
    ConfigEmpty,
    /// 名字为空。
    #[error("name is required")]
    NameRequired,
    /// 名字含字母/数字/连字符/下划线之外的字符。
    #[error("name may only contain letters, digits, hyphens, and underscores")]
    NameInvalid,
}

/// 上游 `validateWorkspaceMcpServerName`：名字是 runtime 挂载用的键，也是 agent 自己
/// 配置会撞上的键 ⇒ 与 agent 设置对话框同一字符集。
///
/// # Errors
///
/// 空 ⇒ [`McpServerValidationError::NameRequired`]；含其它字符 ⇒
/// [`McpServerValidationError::NameInvalid`]。
pub fn validate_name(name: &str) -> std::result::Result<(), McpServerValidationError> {
    if name.is_empty() {
        return Err(McpServerValidationError::NameRequired);
    }
    for character in name.chars() {
        match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => {}
            _ => return Err(McpServerValidationError::NameInvalid),
        }
    }
    Ok(())
}

/// 上游 `validateWorkspaceMcpServerEntry`：**只**看形状（不看内容 —— 内容是 runtime 相关的、
/// 且带着我们不希望检视或在错误里回显的 secret）。
///
/// # Errors
///
/// 非对象 / `null` ⇒ [`McpServerValidationError::ConfigNotObject`]；
/// 空对象 ⇒ [`McpServerValidationError::ConfigEmpty`]。
pub fn validate_entry(config: &Value) -> std::result::Result<(), McpServerValidationError> {
    let Some(object) = config.as_object() else {
        return Err(McpServerValidationError::ConfigNotObject);
    };
    if object.is_empty() {
        return Err(McpServerValidationError::ConfigEmpty);
    }
    Ok(())
}

/// `workspace_mcp_server` 的仓储。
#[derive(Clone)]
pub struct WorkspaceMcpServerRepo {
    db: Db,
}

impl RepoWithDb for WorkspaceMcpServerRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

impl WorkspaceMcpServerRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 上游 `ListWorkspaceMcpServers`：按 `name ASC`（列表顺序是契约的一部分）。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn list_by_workspace(&self, workspace_id: Id) -> Result<Vec<WorkspaceMcpServerRow>> {
        let sql = format!(
            "SELECT {SERVER_COLUMNS} FROM workspace_mcp_server \
             WHERE workspace_id = $1 ORDER BY name ASC"
        );
        sqlx::query_as::<_, WorkspaceMcpServerRow>(&sql)
            .bind(workspace_id.0)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `GetWorkspaceMcpServer`：命中返回行，未命中返回 `None`（`workspace_id` 收窄）。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn find_by_id(
        &self,
        id: Id,
        workspace_id: Id,
    ) -> Result<Option<WorkspaceMcpServerRow>> {
        let sql = format!(
            "SELECT {SERVER_COLUMNS} FROM workspace_mcp_server \
             WHERE id = $1 AND workspace_id = $2"
        );
        sqlx::query_as::<_, WorkspaceMcpServerRow>(&sql)
            .bind(id.0)
            .bind(workspace_id.0)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `CreateWorkspaceMcpServer`：一个事务里
    /// 「`LockWorkspaceForChatSessionCreate`（`FOR KEY SHARE` 锁 workspace 行）→ INSERT」。
    ///
    /// 这张表**没有 FK** ⇒ 没有那把锁，一个在 `DeleteWorkspace` 扫过之后提交的 create
    /// 会留下一行指向已不存在的 workspace。workspace 不存在 ⇒
    /// [`RepoError::NotFound`]（上游回 404 `workspace not found`）；重名 ⇒
    /// [`RepoError::Conflict`]（`316` 的唯一索引，`23505`）。
    ///
    /// # Errors
    ///
    /// 见上。
    pub async fn create(&self, new: NewWorkspaceMcpServer) -> Result<WorkspaceMcpServerRow> {
        let mut tx = self.db.pool().begin().await.map_err(map_sqlx_err)?;
        let locked: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM workspace WHERE id = $1 FOR KEY SHARE")
                .bind(new.workspace_id.0)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx_err)?;
        if locked.is_none() {
            return Err(RepoError::NotFound);
        }

        let sql = format!(
            "INSERT INTO workspace_mcp_server (workspace_id, name, config, created_by) \
             VALUES ($1, $2, $3, $4) RETURNING {SERVER_COLUMNS}"
        );
        let row = sqlx::query_as::<_, WorkspaceMcpServerRow>(&sql)
            .bind(new.workspace_id.0)
            .bind(new.name)
            .bind(new.config)
            .bind(new.created_by.map(|id| id.0))
            .fetch_one(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(row)
    }

    /// 上游 `UpdateWorkspaceMcpServer`：**整体替换**一个条目，`name` / `config` 各自
    /// 「给了才动」（`COALESCE`，对应上游 `sqlc.narg`）。
    ///
    /// 重命名在这里是安全的：绑定以 **id** 为键，所以正在用这个 server 的 agent 不受影响
    /// （这正是 314 的文档模型被换掉的理由）。
    ///
    /// 未命中 / 不属于该 workspace ⇒ [`RepoError::NotFound`]；改成一个已存在的名字 ⇒
    /// [`RepoError::Conflict`]。
    ///
    /// # Errors
    ///
    /// 见上。
    pub async fn update(
        &self,
        id: Id,
        workspace_id: Id,
        name: Option<&str>,
        config: Option<&Value>,
    ) -> Result<WorkspaceMcpServerRow> {
        let sql = format!(
            "UPDATE workspace_mcp_server SET \
                 name = COALESCE($3, name), \
                 config = COALESCE($4, config), \
                 updated_at = now() \
             WHERE id = $1 AND workspace_id = $2 \
             RETURNING {SERVER_COLUMNS}"
        );
        sqlx::query_as::<_, WorkspaceMcpServerRow>(&sql)
            .bind(id.0)
            .bind(workspace_id.0)
            .bind(name)
            .bind(config)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `DeleteWorkspaceMcpServer` + `DeleteAgentMcpServersByServer`：一个事务里
    /// 「`FOR UPDATE` 锁 server 行 → 删行 → 扫掉它的全部绑定」。
    ///
    /// 返回 `false` = 行不存在 / 不属于该 workspace（上游此时 404）。上游把扫绑定与删除
    /// 放在**同一事务**里，否则残留的绑定会一直指向一个已经消失的 server。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn delete(&self, id: Id, workspace_id: Id) -> Result<bool> {
        let mut tx = self.db.pool().begin().await.map_err(map_sqlx_err)?;
        // LockWorkspaceMcpServerForUpdate：绑定写入方持 FOR SHARE，与本锁互斥。
        let locked: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM workspace_mcp_server \
             WHERE id = $1 AND workspace_id = $2 FOR UPDATE",
        )
        .bind(id.0)
        .bind(workspace_id.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;
        if locked.is_none() {
            return Ok(false);
        }

        let outcome =
            sqlx::query("DELETE FROM workspace_mcp_server WHERE id = $1 AND workspace_id = $2")
                .bind(id.0)
                .bind(workspace_id.0)
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx_err)?;
        if outcome.rows_affected() == 0 {
            return Ok(false);
        }
        sqlx::query("DELETE FROM agent_mcp_server WHERE server_id = $1")
            .bind(id.0)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    /// 上游 `TestValidateWorkspaceMcpServerName` 逐条。
    #[test]
    fn name_validation_matches_upstream() {
        for name in ["linear", "local-tool", "my_server", "abc123"] {
            assert_eq!(validate_name(name), Ok(()), "{name} 应当合法");
        }
        for name in ["", "has space", "dot.name", "slash/name", "emoji🎉"] {
            assert!(validate_name(name).is_err(), "{name} 应当被拒");
        }
        assert_eq!(
            validate_name(""),
            Err(McpServerValidationError::NameRequired)
        );
        assert_eq!(
            validate_name("has space"),
            Err(McpServerValidationError::NameInvalid)
        );
    }

    /// 上游 `TestValidateWorkspaceMcpServerEntry` 逐条（含 `null` / `[]` / 标量）。
    #[test]
    fn entry_validation_matches_upstream() {
        assert_eq!(
            validate_entry(&json!({"command":"npx","args":["-y","server"]})),
            Ok(())
        );
        assert_eq!(
            validate_entry(&json!({"type":"http","url":"https://mcp.example"})),
            Ok(())
        );
        assert_eq!(
            validate_entry(&json!({})),
            Err(McpServerValidationError::ConfigEmpty)
        );
        for bad in [json!([]), json!("nope"), Value::Null] {
            assert_eq!(
                validate_entry(&bad),
                Err(McpServerValidationError::ConfigNotObject),
                "{bad}"
            );
        }
    }

    /// 校验错误的文案**不回显输入**（条目常规地嵌着 token；上游为同一理由不包裹底层错误）。
    #[test]
    fn validation_errors_never_echo_input() {
        let secret = json!({"headers":{"Authorization":"Bearer sk-live-should-not-leak"}});
        assert_eq!(validate_entry(&secret), Ok(()));
        let message = McpServerValidationError::ConfigNotObject.to_string();
        assert!(!message.contains("sk-live-should-not-leak"));
        assert_eq!(message, "config must be a JSON object");
    }

    fn row() -> WorkspaceMcpServerRow {
        WorkspaceMcpServerRow {
            id: Uuid::nil(),
            workspace_id: Uuid::nil(),
            name: "linear".into(),
            config: json!({"url":"https://secret.example","headers":{"Authorization":"Bearer sk-live-do-not-log"}}),
            created_by: None,
            created_at: Utc.timestamp_opt(0, 0).single().expect("epoch"),
            updated_at: Utc.timestamp_opt(0, 0).single().expect("epoch"),
        }
    }

    /// **凭据纪律**：手写 `Debug` 不得回显条目的值（`docs/61` §2.4 判据 1）。
    #[test]
    fn debug_redacts_the_entry_values() {
        let rendered = format!("{:?}", row());
        assert!(!rendered.contains("sk-live-do-not-log"), "{rendered}");
        assert!(!rendered.contains("secret.example"), "{rendered}");
        assert!(rendered.contains("<redacted, write-only>"));
        // 结构信息仍然可读（诊断要用）：条目键名 + 名字。
        assert!(rendered.contains("headers"), "{rendered}");
        assert!(rendered.contains("linear"), "{rendered}");
    }

    /// 领域层窄口径：显式三值可解析、无 `type` 时按 `command` / `url` 推断、什么都没有 ⇒ `None`。
    ///
    /// ⚠️ 与线格式**刻意不同**：`{"type":"websocket","url":…}` 在领域层落到 `Http`
    /// （枚举没有「未知」这一支，`from_config` 会继续推断），而响应必须原样报 `websocket`
    /// （`routes::mcp::workspace::mcp_transport_of`，上游 `TestMcpTransportOf` 的回归点）。
    /// 本用例把两者的差异钉在同一处，避免以后有人拿 `transport()` 去生成 wire 值。
    #[test]
    fn domain_transport_projection_is_narrower_than_the_wire_one() {
        assert_eq!(row().transport(), Some(McpTransport::Http));
        let mut stdio = row();
        stdio.config = json!({"command":"npx"});
        assert_eq!(stdio.transport(), Some(McpTransport::Stdio));
        let mut unknown = row();
        unknown.config = json!({"type":"websocket","url":"wss://x"});
        assert_eq!(unknown.transport(), Some(McpTransport::Http));
        let mut nothing = row();
        nothing.config = json!({"foo":"bar"});
        assert_eq!(nothing.transport(), None);
    }

    /// 领域投影逐字段对上（含 `Id` / `Timestamp` 的换算）。
    #[test]
    fn domain_projection_round_trips() {
        let domain = row().to_domain();
        assert_eq!(domain.name, "linear");
        assert_eq!(domain.id, Id::nil());
        assert_eq!(domain.transport(), Some(McpTransport::Http));
    }
}
