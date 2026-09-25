//! squad leader 判决记录（M2-A 尾-补 / LUM-1793）。
//!
//! 上游 `server/internal/handler/squad.go:976 RecordSquadLeaderEvaluation`（注册在
//! `server/cmd/server/router.go:2097`：`r.Post("/api/issues/{id}/squad-evaluated", …)`）。
//! 这个面**不建新表**：判决落的就是一条 `activity_log` 行（上游同名注释
//! "records … into the unified `activity_log"），表在`
//! `migrations/upstream/001_init.up.sql:156`（8 列 + `idx_activity_log_issue`）⇒ 本片
//! **0 迁移**。
//!
//! 本模块只做这一条链的两半：
//!
//! | 上游 query | 本模块 |
//! | --- | --- |
//! | `GetAgentTaskInWorkspace`（`agent.sql:731`，`JOIN agent` 做租户收窄） | [`SquadEvaluationRepo::leader_task_in_workspace`] |
//! | `CreateActivity`（`activity.sql:29`） | [`SquadEvaluationRepo::record_evaluation`] |
//!
//! **为什么另起一个文件**（而不是写进 `squad.rs` / `task/` / `agent/env.rs`）：
//! 上游 handler 位于 `squad.go`，但读的 `agent_task_queue` 是 **M3 域**表、写的
//! `activity_log` 与 M2-A 尾片的 `pins.rs` / `stats.rs` 同族 —— 三个既有文件的写者
//! 分别是 M4-2 / M3-6 / M3-x，本片按「只追加自己的文件、不改别人的行」的写集纪律新建。
//! `mc-repos` **没有**通用 activity repo（上游 `agent/env.rs` 对应的
//! `crates/mc-repos/src/agent/env.rs` 文件头也承认这一点），所以不往那里塞。
//!
//! ## 逐字照上游的四处（改了就是改契约）
//!
//! 1. **`actor_id = task.agent_id`，不是 `squad.leader_id`**（`squad.go:1117-1120`）：
//!    抑制查询 [`SUPPRESSION_LOOKUP_COLUMNS`] 是拿 `activity_log.actor_id` 对
//!    `task.agent_id` 比的，列里放 leader 会让 `no_action` 抑制**永远查不到那一行**。
//!    ⚠️ **该抑制查询在本仓还没有消费者**（上游 `service.HasSquadLeaderNoActionEvaluationForTask`
//!    的对应面未移植 ⇒ 无调用点）—— 本模块**只保证列里放的是 task 的 agent**，
//!    不实现抑制本身（偏离登记在 `docs/22-ROUTE-PARITY.md` §7）。
//! 2. **`id` 显式写 `UUIDv7`**（上游 `dbid.NewV7()`）：`activity_log.id` 的 DDL 默认值只是
//!    `gen_random_uuid()`（v4），不加这一句就与上游产出的 id 形态不同（下游按 v7 排序的
//!    假设会静默失效）⇒ 这里显式 `Uuid::now_v7()`。
//! 3. **`details` 的四个键都是字符串**（上游 `json.Marshal(map[string]string{…})`）：
//!    `squad_id` / `task_id` / `outcome` / `reason`，其中 `reason` 允许空串（**不是** `null`）。
//! 4. **`actor_type` 恒 `'agent'`**：上游写死 `pgtype.Text{String: "agent"}`；
//!    `activity_log_actor_type_check` 允许 `member|agent|system` 三个值。
//!
//! ## 字段裁剪（登记为偏离）
//!
//! 上游 `GetAgentTaskInWorkspace` 是 `SELECT atq.*`（49 列）。本模块只投影这条 handler
//! 真正读的 5 列（`id` / `agent_id` / `issue_id` / `is_leader_task` / `squad_id`）——
//! 这是**投影收窄**，不是语义变更：谓词（`atq.id = $1 AND a.workspace_id = $2`）与
//! 上游逐字相同，`JOIN agent` 也保留（它才是租户闸门）。
//!
//! 约定与 M1/M2/M3 各 Repo 保持一致（见 `crate::issue` / `crate::pin`）：
//! - 行类型用原始 `Uuid` / `bool` 字段 + `Id` 访问器（`mc_core::Id` 没有 sqlx impl）
//! - 错误统一走 `crate::workspace::map_sqlx_err`
//! - Pg 实现 + `#[ignore]` 的 PG 集成测试（`MULTICA_TEST_DATABASE_URL`，**不允许静默跳过**）
//!
//! 硬约束：**不引入本仓自造列**；不加迁移。

use chrono::{DateTime, Utc};
use mc_core::Id;
use mc_db::Db;
use serde_json::{json, Value as JsonValue};
use sqlx::FromRow;
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::{RepoWithDb, Result};

/// 判决行写入 `activity_log.action` 的值（上游 `CreateActivityParams{Action: …}` 逐字）。
pub const ACTION_SQUAD_LEADER_EVALUATED: &str = "squad_leader_evaluated";

/// 判决行的 `actor_type`（上游写死；`activity_log` 的 CHECK 允许 `member|agent|system`）。
pub const ACTOR_TYPE_AGENT: &str = "agent";

/// `outcome` 白名单（上游 `req.Outcome != "action" && != "no_action" && != "failed"`）。
pub const OUTCOMES: [&str; 3] = ["action", "no_action", "failed"];

/// 白名单外的 `outcome` 触发的 400 文案（上游逐字，客户端按文案提示 ⇒ 不翻译）。
pub const OUTCOME_ERROR: &str = "outcome must be 'action', 'no_action', or 'failed'";

/// `details` 的四个键（上游 `map[string]string`；顺序按上游字面量顺序）。
pub const DETAIL_KEYS: [&str; 4] = ["squad_id", "task_id", "outcome", "reason"];

/// 抑制查询读的列（`server/pkg/db/queries/activity.sql:35`
/// `HasSquadLeaderNoActionEvaluationForTask`：`issue_id` + `actor_type='agent'` +
/// `actor_id = @agent_id` + `action='squad_leader_evaluated'` + `details->>'outcome'='no_action'`
/// + `details->>'task_id' = @task_id::text`）。
///
/// 它在**本仓没有消费者**（上游 service 面未移植）；这类常量留在类型旁边是为了让
/// 「`actor_id` 为什么必须是 `task.agent_id`」这条推断在代码里可核对，而不是只活在注释里。
pub const SUPPRESSION_LOOKUP_COLUMNS: [&str; 3] = ["actor_id", "action", "details->>'task_id'"];

/// `outcome` 是否在三个合法取值内（纯函数，单测钉住）。
#[must_use]
pub fn is_valid_outcome(raw: &str) -> bool {
    OUTCOMES.contains(&raw)
}

/// 判决行的 `details` 值（上游 `json.Marshal(map[string]string{...})`）。
///
/// 四个键**全部**是字符串（`reason` 允许空串），顺序与 [`DETAIL_KEYS`] 一致。
#[must_use]
pub fn evaluation_details(squad_id: Id, task_id: Id, outcome: &str, reason: &str) -> JsonValue {
    json!({
        "squad_id": squad_id.to_string(),
        "task_id": task_id.to_string(),
        "outcome": outcome,
        "reason": reason,
    })
}

/// `agent_task_queue` 上这条 handler 读的 5 列（见模块头的「字段裁剪」）。
#[derive(Debug, Clone, FromRow)]
pub struct SquadLeaderTaskRow {
    /// `agent_task_queue.id`。
    pub id: Uuid,
    /// `agent_task_queue.agent_id`（`NOT NULL` ⇒ 恒有效；上游 `task.AgentID.Valid` 恒真）。
    pub agent_id: Uuid,
    /// `agent_task_queue.issue_id`（chat / quick-create 任务为空）。
    pub issue_id: Option<Uuid>,
    /// `agent_task_queue.is_leader_task`（`090_task_is_leader` 加的列，默认 `FALSE`）。
    pub is_leader_task: bool,
    /// `agent_task_queue.squad_id`（`127_task_squad_id` 加的列，`NULL` 允许）。
    pub squad_id: Option<Uuid>,
}

impl SquadLeaderTaskRow {
    /// 主键。
    #[must_use]
    pub fn id(&self) -> Id {
        Id::from(self.id)
    }

    /// 这条任务入队给哪个 agent（= 判决行的 `actor_id`）。
    #[must_use]
    pub fn agent_id(&self) -> Id {
        Id::from(self.agent_id)
    }

    /// 任务跑在哪个 issue 上（上游 `task.IssueID.Valid` 判空）。
    #[must_use]
    pub fn issue_id(&self) -> Option<Id> {
        self.issue_id.map(Id::from)
    }

    /// 入队时的**意图**：这是一次 squad leader 回合（`090` 的列注释）。
    #[must_use]
    pub fn is_leader_task(&self) -> bool {
        self.is_leader_task
    }

    /// 入队时盖的 squad（`127`；`MUL-3730` 之前的行可能为空）。
    #[must_use]
    pub fn squad_id(&self) -> Option<Id> {
        self.squad_id.map(Id::from)
    }
}

/// `result` 为 `activity_log` 一行的投影（插入的 `RETURNING` 与响应体同形）。
#[derive(Debug, Clone, FromRow)]
pub struct SquadEvaluationActivityRow {
    /// `activity_log.id`（显式 v7；见模块头第 2 条）。
    pub id: Uuid,
    /// `activity_log.action`（恒 [`ACTION_SQUAD_LEADER_EVALUATED`]）。
    pub action: String,
    /// `activity_log.created_at`。
    pub created_at: DateTime<Utc>,
}

impl SquadEvaluationActivityRow {
    /// 主键（响应体里是**字符串**，上游 `uuidToString(activity.ID)`）。
    #[must_use]
    pub fn id(&self) -> Id {
        Id::from(self.id)
    }
}

/// squad leader 判决的仓储（`agent_task_queue` 只读 + `activity_log` 只写）。
pub struct SquadEvaluationRepo {
    db: Db,
}

impl RepoWithDb for SquadEvaluationRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

impl SquadEvaluationRepo {
    /// 构造。
    #[must_use]
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 上游 `GetAgentTaskInWorkspace`（`agent.sql:731`）+ handler 的 `task.IssueID.Valid` 判空。
    ///
    /// `None` 覆盖上游两个分支（`err != nil` 与 `!task.IssueID.Valid`）—— 两者在 handler 里
    /// 落到同一句 400 `task does not belong to issue` ⇒ 这里合并不损失语义。
    ///
    /// tenancy 一律走 `JOIN agent`（上游注释：`agent_id` 在每条任务行上都 `NOT NULL`
    /// 且 `ON DELETE CASCADE`，所以 agent 才是 workspace 作用域的那一侧）。
    ///
    /// # Errors
    ///
    /// DB 错误映射为 [`crate::RepoError`]。
    pub async fn leader_task_in_workspace(
        &self,
        task_id: Id,
        workspace_id: Id,
    ) -> Result<Option<SquadLeaderTaskRow>> {
        let row = sqlx::query_as::<_, SquadLeaderTaskRow>(
            "SELECT atq.id, atq.agent_id, atq.issue_id, atq.is_leader_task, atq.squad_id \
             FROM agent_task_queue atq \
             JOIN agent a ON a.id = atq.agent_id \
             WHERE atq.id = $1 AND a.workspace_id = $2",
        )
        .bind(task_id.0)
        .bind(workspace_id.0)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(row)
    }

    /// 上游 `CreateActivity`（`activity.sql:29`）：落一条 `activity_log` 行并回读
    /// `id, action, created_at`（handler 的 201 体就是这三列）。
    ///
    /// `action` 不可参数化 —— 这个端点只有一条判决语义，接口上没有第二个取值。
    ///
    /// # Errors
    ///
    /// DB 错误映射为 [`crate::RepoError`]（handler 转 500 `failed to record evaluation`）。
    pub async fn record_evaluation(
        &self,
        workspace_id: Id,
        issue_id: Id,
        actor_id: Id,
        details: &JsonValue,
    ) -> Result<SquadEvaluationActivityRow> {
        let row = sqlx::query_as::<_, SquadEvaluationActivityRow>(
            "INSERT INTO activity_log \
                (id, workspace_id, issue_id, actor_type, actor_id, action, details) \
             VALUES ($1, $2, $3, 'agent', $4, 'squad_leader_evaluated', $5::jsonb) \
             RETURNING id, action, created_at",
        )
        .bind(Uuid::now_v7())
        .bind(workspace_id.0)
        .bind(issue_id.0)
        .bind(actor_id.0)
        .bind(details)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_whitelist_matches_upstream_three_values() {
        for ok in ["action", "no_action", "failed"] {
            assert!(is_valid_outcome(ok), "{ok} 是上游白名单成员");
        }
        // 反例：空串（缺字段 / 显式 `""`）、大小写变形、近似值。
        for bad in ["", "Action", "action ", "no action", "success", "failed!"] {
            assert!(!is_valid_outcome(bad), "{bad} 不在白名单里");
        }
    }

    /// 四个键必须都是**字符串**，且 `reason` 空串不是 `null`（上游 `map[string]string`）。
    #[test]
    fn details_are_four_string_keys_with_quad_id_and_task_id() {
        let squad = Id::from(Uuid::nil());
        let task = Id::from(Uuid::max());
        let details = evaluation_details(squad, task, "no_action", "");

        let object = details.as_object().expect("details 是对象");
        assert_eq!(object.len(), 4, "恰好四个键：{details}");
        for key in DETAIL_KEYS {
            assert!(
                object.get(key).is_some_and(JsonValue::is_string),
                "{key} 必须是字符串：{details}"
            );
        }
        assert_eq!(object["squad_id"], json!(squad.to_string()));
        assert_eq!(object["task_id"], json!(task.to_string()));
        assert_eq!(object["outcome"], json!("no_action"));
        assert_eq!(object["reason"], json!(""), "空 reason 是空串，不是 null");
    }

    #[test]
    fn action_and_actor_type_match_upstream_literals() {
        assert_eq!(ACTION_SQUAD_LEADER_EVALUATED, "squad_leader_evaluated");
        assert_eq!(ACTOR_TYPE_AGENT, "agent");
        assert_eq!(
            OUTCOME_ERROR,
            "outcome must be 'action', 'no_action', or 'failed'"
        );
    }

    /// `actor_id` 进的是 `task.agent_id`，不是 `squad.leader_id`；抑制查询按这一列比。
    #[test]
    fn suppression_lookup_columns_name_the_actor_column() {
        assert!(SUPPRESSION_LOOKUP_COLUMNS.contains(&"actor_id"));
        assert!(SUPPRESSION_LOOKUP_COLUMNS.contains(&"action"));
        assert!(SUPPRESSION_LOOKUP_COLUMNS
            .iter()
            .any(|c| c.contains("task_id")));
    }

    #[test]
    fn row_accessors_expose_optional_links() {
        let row = SquadLeaderTaskRow {
            id: Uuid::nil(),
            agent_id: Uuid::max(),
            issue_id: None,
            is_leader_task: true,
            squad_id: None,
        };
        assert_eq!(row.id(), Id::from(Uuid::nil()));
        assert_eq!(row.agent_id(), Id::from(Uuid::max()));
        assert_eq!(row.issue_id(), None, "chat 任务的 issue_id 为空");
        assert!(row.is_leader_task());
        assert_eq!(
            row.squad_id(),
            None,
            "pre-MUL-3730 的 leader 任务无 squad_id"
        );
    }
}
