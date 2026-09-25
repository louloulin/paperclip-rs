//! `agent_mcp_server` 仓储面 —— **M8-3 已落地（`LUM-1800`）**。
//!
//! - **上游**：`internal/handler/workspace_mcp_api.go`（agent 面 4 条）+
//!   `pkg/db/queries/workspace_mcp.sql` 的后半段（6 条查询）。
//! - **语义**：主键 `(agent_id, server_id)`；绑定**就是**授权 —— 一个库条目能不能被某个
//!   agent 用，唯一判据是这里有没有一行（且 `enabled = TRUE`，`ListEnabledAgentMcpServers`
//!   是 claim 路径的唯一入口）。建一个库条目**不给任何人**，这正是这套形状的全部意义。
//! - **幂等**：绑定 / 解绑 / 启停都是幂等的（重复 add 不报错也不重复插行
//!   —— 上游 `ON CONFLICT DO NOTHING`）。
//! - **无 FK ⇒ 栅栏在应用层**（迁移 `315`）：[`AgentMcpBindingRepo::add`] 在同一事务里
//!   先 `FOR KEY SHARE` workspace（上游 `LockWorkspaceForChatSessionCreate`）、再对 server 行
//!   `FOR SHARE`（上游 `LockWorkspaceMcpServerForShare`）。第二条语句同时承担
//!   **作用域校验**（跨 workspace 的 server id ⇒ 404）与**删除协议**：它与
//!   `WorkspaceMcpServerRepo::delete` 的 `FOR UPDATE` 互斥，所以绑定不可能插在
//!   「删 server 已扫过绑定、尚未提交」的窗口里。
//! - **凭据纪律**：列表查询要 `workspace_mcp_server.config`（`transport` 投影要它），
//!   所以两行类型都**手写 `Debug`**（只列条目键名，不列值）。
//! - **本仓约定**：裸 `Uuid` + `sqlx::FromRow`、`map_sqlx_err`、运行时 builder + 参数绑定。

use chrono::{DateTime, Utc};
use mc_core::mcp::{McpBinding, McpTransport};
use mc_core::Id;
use mc_db::Db;
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::{RepoError, RepoWithDb, Result};

/// `ListAgentMcpServers` 的列：库条目的展示列 + 绑定的 `enabled`（上游那句 JOIN 逐字）。
const BINDING_COLUMNS: &str = "s.id, s.workspace_id, s.name, s.config, s.created_at, \
                               s.updated_at, ams.enabled";

/// 「绑定给某 agent 的一个库条目」的一行 —— `GET /api/agents/{id}/mcp-servers` 的响应单元。
///
/// ⚠️ `enabled` 是**绑定的**真实值，且查询**不带** `ams.enabled = TRUE` 过滤：列表要显示
/// 被关掉的绑定（`enabled = false`），只有 claim 路径
/// （[`AgentMcpBindingRepo::list_enabled_for_agent`]）才加这个谓词。上游两条 SQL 的口径差
/// 就这一处。
///
/// ⚠️ `config` 是**含密钥**的条目 ⇒ 手写 `Debug`（只列键名）。
#[derive(Clone, FromRow, PartialEq)]
pub struct AgentMcpServerRow {
    /// `workspace_mcp_server.id`（= 绑定里的 `server_id`）。
    pub id: Uuid,
    /// `workspace_mcp_server.workspace_id`。
    pub workspace_id: Uuid,
    /// `workspace_mcp_server.name`。
    pub name: String,
    /// `workspace_mcp_server.config` JSONB（**write-only**，`Debug` 里脱敏）。
    pub config: Value,
    /// `workspace_mcp_server.created_at`。
    pub created_at: DateTime<Utc>,
    /// `workspace_mcp_server.updated_at`。
    pub updated_at: DateTime<Utc>,
    /// `agent_mcp_server.enabled`。
    pub enabled: bool,
}

impl std::fmt::Debug for AgentMcpServerRow {
    /// 手写脱敏（**不派生**）：理由同 `WorkspaceMcpServerRow`。
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut keys: Vec<&str> = self
            .config
            .as_object()
            .map(|object| object.keys().map(String::as_str).collect())
            .unwrap_or_default();
        keys.sort_unstable();
        formatter
            .debug_struct("AgentMcpServerRow")
            .field("id", &self.id)
            .field("workspace_id", &self.workspace_id)
            .field("name", &self.name)
            .field("config", &"<redacted, write-only>")
            .field("config_keys", &keys)
            .field("created_at", &self.created_at)
            .field("updated_at", &self.updated_at)
            .field("enabled", &self.enabled)
            .finish()
    }
}

impl AgentMcpServerRow {
    /// 库条目 id（= 绑定里的 `server_id`）。
    #[must_use]
    pub fn server_id(&self) -> Id {
        Id(self.id)
    }

    /// 条目声明的 transport —— **领域层窄口径**（同 `WorkspaceMcpServerRow::transport` 的
    /// 说明：响应用的**不是**它，而是 `mcp_transport_of` 那个无损的字符串投影）。
    #[must_use]
    pub fn transport(&self) -> Option<McpTransport> {
        McpTransport::from_config(&self.config)
    }

    /// 领域投影（`mc_core::mcp::McpBinding`）。
    #[must_use]
    pub fn to_binding(&self, agent_id: Id) -> McpBinding {
        McpBinding {
            agent_id,
            server_id: self.server_id(),
            enabled: self.enabled,
            created_at: mc_core::Timestamp::from_unix(self.created_at.timestamp()),
        }
    }
}

/// `agent_mcp_server` 的仓储。
#[derive(Clone)]
pub struct AgentMcpBindingRepo {
    db: Db,
}

impl RepoWithDb for AgentMcpBindingRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

impl AgentMcpBindingRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 上游 `ListAgentMcpServers`：该 agent 的**全部**绑定（含 `enabled = false`），
    /// 按 `s.name ASC`。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn list_for_agent(&self, agent_id: Id) -> Result<Vec<AgentMcpServerRow>> {
        let sql = format!(
            "SELECT {BINDING_COLUMNS} FROM workspace_mcp_server s \
             JOIN agent_mcp_server ams ON ams.server_id = s.id \
             WHERE ams.agent_id = $1 ORDER BY s.name ASC"
        );
        sqlx::query_as::<_, AgentMcpServerRow>(&sql)
            .bind(agent_id.0)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `ListEnabledAgentMcpServers`（**claim 路径**）：只有「绑定 + 启用」的条目会到达
    /// runtime，返回 `(name, config)` 按 `s.name ASC` —— 它的唯一消费者是
    /// `mc_core::mcp::overlay::resolve_agent_mcp_config`。
    ///
    /// ⚠️ 本波**没有**调用点：claim 的 agent 载荷（上游 `daemon.go:2470-2505`）在本仓
    /// 属 M3-7 面且尚未实现「agent 数据块」⇒ 这是 R-M8-9 同性质的**登记缺口**
    /// （见 `docs/32` §16），不是遗漏。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn list_enabled_for_agent(&self, agent_id: Id) -> Result<Vec<(String, Value)>> {
        let sql = "SELECT s.name, s.config FROM workspace_mcp_server s \
                   JOIN agent_mcp_server ams ON ams.server_id = s.id \
                   WHERE ams.agent_id = $1 AND ams.enabled = TRUE ORDER BY s.name ASC";
        sqlx::query_as::<_, (String, Value)>(sql)
            .bind(agent_id.0)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 上游 `AddAgentMcpServer`：一个事务里
    /// 「`FOR KEY SHARE` workspace → `FOR SHARE` server 行（**同时**是作用域校验与删除协议）
    /// → `INSERT … ON CONFLICT DO NOTHING`」。
    ///
    /// 幂等：重复 add 不报错（`ON CONFLICT DO NOTHING`）、不会重复插行、**不**把已关掉的
    /// 绑定重新打开（`enabled` 列默认 `TRUE`，冲突时不动它）。
    ///
    /// 跨 workspace 的 `server_id` ⇒ [`RepoError::NotFound`]（上游 404
    /// `MCP server not found in this workspace`）—— 与「不存在」同判，不泄露存在性。
    ///
    /// # Errors
    ///
    /// 见上。
    pub async fn add(&self, agent_id: Id, workspace_id: Id, server_id: Id) -> Result<()> {
        let mut tx = self.db.pool().begin().await.map_err(map_sqlx_err)?;
        let locked: Option<(Uuid,)> =
            sqlx::query_as("SELECT id FROM workspace WHERE id = $1 FOR KEY SHARE")
                .bind(workspace_id.0)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_sqlx_err)?;
        if locked.is_none() {
            return Err(RepoError::NotFound);
        }
        // 作用域校验 AND the delete protocol in one statement：
        // 行必须属于该 agent 的 workspace（别的租户的 id 会跨租户绑定），
        // 且 FOR SHARE 与 DeleteWorkspaceMcpServer 的 FOR UPDATE 互斥。
        let scoped: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM workspace_mcp_server \
             WHERE id = $1 AND workspace_id = $2 FOR SHARE",
        )
        .bind(server_id.0)
        .bind(workspace_id.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;
        if scoped.is_none() {
            return Err(RepoError::NotFound);
        }

        sqlx::query(
            "INSERT INTO agent_mcp_server (agent_id, server_id) VALUES ($1, $2) \
             ON CONFLICT DO NOTHING",
        )
        .bind(agent_id.0)
        .bind(server_id.0)
        .execute(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;
        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(())
    }

    /// 上游 `SetAgentMcpServerEnabled`：**只**动 `enabled`，不删绑定
    /// （关掉再打开不必重新找一遍）。
    ///
    /// 返回受影响行数（`0` ⇒ 绑定不存在，上层 404）。连开两次同一值是**幂等**的：
    /// 第二次仍然改到那一行（`UPDATE` 命中 1 行），不插新行。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn set_enabled(&self, agent_id: Id, server_id: Id, enabled: bool) -> Result<u64> {
        let outcome = sqlx::query(
            "UPDATE agent_mcp_server SET enabled = $3 \
             WHERE agent_id = $1 AND server_id = $2",
        )
        .bind(agent_id.0)
        .bind(server_id.0)
        .bind(enabled)
        .execute(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(outcome.rows_affected())
    }

    /// 上游 `RemoveAgentMcpServer`：只摘绑定，**库条目本身不动**。
    ///
    /// 返回受影响行数（`0` ⇒ 绑定不存在，上层 404）。
    ///
    /// # Errors
    ///
    /// 库错误 ⇒ [`crate::RepoError`]。
    pub async fn remove(&self, agent_id: Id, server_id: Id) -> Result<u64> {
        let outcome =
            sqlx::query("DELETE FROM agent_mcp_server WHERE agent_id = $1 AND server_id = $2")
                .bind(agent_id.0)
                .bind(server_id.0)
                .execute(self.db.pool())
                .await
                .map_err(map_sqlx_err)?;
        Ok(outcome.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use serde_json::json;

    fn epoch() -> DateTime<Utc> {
        Utc.timestamp_opt(0, 0).single().expect("epoch")
    }

    fn row(enabled: bool) -> AgentMcpServerRow {
        AgentMcpServerRow {
            id: Uuid::from_u128(7),
            workspace_id: Uuid::nil(),
            name: "linear".into(),
            config: json!({"type":"http","url":"https://secret.example",
                           "headers":{"Authorization":"Bearer sk-live-do-not-log"}}),
            created_at: epoch(),
            updated_at: epoch(),
            enabled,
        }
    }

    /// **凭据纪律**：手写 `Debug` 不得回显条目的值（`docs/61` §2.4 判据 1）。
    #[test]
    fn debug_redacts_the_entry_values() {
        let rendered = format!("{:?}", row(true));
        assert!(!rendered.contains("sk-live-do-not-log"), "{rendered}");
        assert!(!rendered.contains("secret.example"), "{rendered}");
        assert!(rendered.contains("<redacted, write-only>"));
        assert!(rendered.contains("headers"), "键名应当可读: {rendered}");
        assert!(rendered.contains("enabled: true"), "{rendered}");
    }

    /// transport 投影与领域投影（含 `enabled` 落到 `McpBinding`）。
    #[test]
    fn projections_carry_the_binding_state() {
        assert_eq!(row(true).transport(), Some(McpTransport::Http));
        let binding = row(false).to_binding(Id::from(Uuid::from_u128(9)));
        assert_eq!(binding.server_id, Id::from(Uuid::from_u128(7)));
        assert_eq!(binding.agent_id, Id::from(Uuid::from_u128(9)));
        assert!(!binding.enabled);
    }
}
