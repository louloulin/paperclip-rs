//! `LabelRepo` —— `issue_label` 标签目录 + `issue_to_label` 关联（M2-E / LUM-1370）。
//!
//! 对应上游 `server/internal/handler/label.go` + `server/pkg/db/queries/issue_label.sql`。
//! 表结构与上游**逐字一致**（上游 `001_init.up.sql:75/82` + `059_label_timestamps` +
//! `162_resource_labels`；W0-B2 schema 切换后本地运行库同构 ⇒ 本片**不新增迁移**，
//! 见 `docs/59-M2-E-LABEL-PROPERTY.md` §2）：
//!
//! ```text
//! issue_label(id, workspace_id FK→workspace ON DELETE CASCADE, name, color,
//!             created_at, updated_at, resource_type, description)
//! issue_to_label(issue_id, label_id)  -- 复合主键，两侧 ON DELETE CASCADE
//! ```
//!
//! 关键语义（照上游）：
//! - **唯一性**：`issue_label_workspace_type_name_lower_idx` 对
//!   `(workspace_id, resource_type, LOWER(name))` 唯一 ⇒ 同 workspace 同类型下
//!   大小写不敏感重名 → SQLSTATE `23505` → [`RepoError::Conflict`]（路由层 409）。
//! - **workspace 隔离**：每个查询都带 `workspace_id` 谓词；跨 workspace 的 label id
//!   一律 `NotFound`（路由层 404），不泄漏「存在但不在你的 workspace」。
//! - **删除**：资源侧关联表（`issue_to_label` / `agent_to_label` / `skill_to_label`）
//!   **故意没有外键**（上游注释：避免未经评审的级联锁与审计行为）⇒ 目录行删除必须
//!   在**同一事务**里显式清关联，见 [`LabelRepo::delete`]。删除用
//!   `DELETE ... RETURNING id` 区分 404 与基础设施错误（无 TOCTOU 预检）。
//! - **attach 幂等**：`ON CONFLICT DO NOTHING`，重复 attach 不报错；`changed` 区分
//!   本次调用是否真的插入了行（上游 `AttachLabelToIssue.changed`）。
//! - `resource_type` 默认 `issue`，合法值 `issue | agent | skill`（上游
//!   `parseLabelResourceType`）。

use chrono::{DateTime, Utc};
use mc_core::Id;
use mc_db::Db;
use sqlx::FromRow;
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::{RepoError, RepoWithDb, Result};

/// 标签名长度上限（上游 `maxLabelNameLen`）。
pub const MAX_LABEL_NAME_LEN: usize = 32;
/// 合法的 `resource_type` 取值（上游 `parseLabelResourceType`）。
pub const RESOURCE_TYPES: [&str; 3] = ["issue", "agent", "skill"];
/// `resource_type` 缺省值。
pub const DEFAULT_RESOURCE_TYPE: &str = "issue";

const LABEL_COLUMNS: &str = "id, workspace_id, resource_type, name, description, color, \
                             created_at, updated_at";

/// `issue_label` 行。
#[derive(Debug, Clone, FromRow)]
pub struct LabelRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    /// `issue` / `agent` / `skill`
    pub resource_type: String,
    pub name: String,
    pub description: String,
    /// 规范化后的 `#rrggbb`
    pub color: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl LabelRow {
    /// 主键。
    pub fn id(&self) -> Id {
        Id::from(self.id)
    }

    /// 所属 workspace。
    pub fn workspace_id(&self) -> Id {
        Id::from(self.workspace_id)
    }

    /// 是否可用于 issue（上游 `AttachLabel` 的 `label.ResourceType != "issue"` 判定）。
    pub fn is_issue_label(&self) -> bool {
        self.resource_type == "issue"
    }
}

/// `ListLabels` 行：目录行 + `usage_count`（各资源侧关联表计数）。
#[derive(Debug, Clone, FromRow)]
pub struct LabelListRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub resource_type: String,
    pub name: String,
    pub description: String,
    pub color: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// 该标签在当前 `resource_type` 侧的关联条数。
    pub usage_count: i64,
}

impl LabelListRow {
    /// 主键。
    pub fn id(&self) -> Id {
        Id::from(self.id)
    }
}

/// 新建标签的输入（字段已由 handler 校验 / 规范化）。
#[derive(Debug, Clone)]
pub struct NewLabel {
    pub resource_type: String,
    pub name: String,
    pub description: String,
    pub color: String,
}

/// 标签更新补丁：`None` = 不动该字段（上游 `UpdateLabel` 的 `COALESCE(narg, col)`）。
#[derive(Debug, Clone, Default)]
pub struct LabelUpdate {
    pub name: Option<String>,
    pub description: Option<String>,
    pub color: Option<String>,
}

/// `attach` / `detach` 的结果：本次调用是否改了行 + 更新后的 issue revision。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssignmentOutcome {
    /// 是否真的插入了 / 删除了关联行（幂等调用为 `false`）。
    pub changed: bool,
    /// issue revision（`changed == false` 时为上游语义的 `0`）。
    pub issue_revision: i64,
}

/// `resource_type` 解析：空 → 默认 `issue`；非法 → `None`（路由层 400）。
pub fn parse_resource_type(raw: &str) -> Option<String> {
    let value = raw.trim();
    if value.is_empty() {
        return Some(DEFAULT_RESOURCE_TYPE.to_string());
    }
    if RESOURCE_TYPES.contains(&value) {
        return Some(value.to_string());
    }
    None
}

/// 标签名校验（上游 `validateLabelName`）：控制字符拒绝 → 去空白 → 非空 → ≤32 字符。
///
/// 返回 `Ok(trimmed)` 或 `Err(消息)`（消息逐字对齐上游，路由层直接作为 400 body）。
pub fn validate_name(raw: &str) -> std::result::Result<String, String> {
    if raw.chars().any(char::is_control) {
        return Err("name cannot contain tabs, newlines, or control characters".into());
    }
    let name = raw.trim();
    if name.is_empty() {
        return Err("name is required".into());
    }
    if name.chars().count() > MAX_LABEL_NAME_LEN {
        return Err(format!(
            "name must be {MAX_LABEL_NAME_LEN} characters or fewer"
        ));
    }
    Ok(name.to_string())
}

/// 颜色规范化（上游 `normalizeColor`）：`#rrggbb` 或 `rrggbb` → 小写 `#rrggbb`。
///
/// **不可放宽**：客户端直接把它当 `backgroundColor` 用（LabelChip），放宽成任意 CSS
/// 即等于开一个注入面（上游注释 load-bearing invariant）。
pub fn normalize_color(raw: &str) -> std::result::Result<String, String> {
    let value = raw.trim();
    let hex = value.strip_prefix('#').unwrap_or(value);
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("color must be a 6-digit hex value like #3b82f6".into());
    }
    Ok(format!("#{}", hex.to_ascii_lowercase()))
}

/// `LabelRepo`。
#[derive(Clone)]
pub struct LabelRepo {
    db: Db,
}

impl LabelRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 目录列表：按 `LOWER(name)` 升序，带 `usage_count`（上游 `ListLabels`）。
    pub async fn list(&self, workspace_id: Id, resource_type: &str) -> Result<Vec<LabelListRow>> {
        let sql = "SELECT l.id, l.workspace_id, l.resource_type, l.name, l.description, l.color, \
                    l.created_at, l.updated_at, \
                    CASE l.resource_type \
                        WHEN 'issue' THEN (SELECT COUNT(*) FROM issue_to_label x WHERE x.label_id = l.id) \
                        WHEN 'agent' THEN (SELECT COUNT(*) FROM agent_to_label x WHERE x.label_id = l.id) \
                        WHEN 'skill' THEN (SELECT COUNT(*) FROM skill_to_label x WHERE x.label_id = l.id) \
                        ELSE 0 \
                    END::bigint AS usage_count \
             FROM issue_label l \
             WHERE l.workspace_id = $1 AND l.resource_type = $2 \
             ORDER BY LOWER(l.name) ASC";
        sqlx::query_as::<_, LabelListRow>(sql)
            .bind(workspace_id.0)
            .bind(resource_type)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 单条读取（workspace 收窄；未命中 → [`RepoError::NotFound`]）。
    pub async fn get(&self, workspace_id: Id, label_id: Id) -> Result<LabelRow> {
        let sql =
            format!("SELECT {LABEL_COLUMNS} FROM issue_label WHERE id = $1 AND workspace_id = $2");
        sqlx::query_as::<_, LabelRow>(&sql)
            .bind(label_id.0)
            .bind(workspace_id.0)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 新建：重名（同 workspace + `resource_type` + `LOWER`(name)）→ [`RepoError::Conflict`]。
    pub async fn create(&self, workspace_id: Id, input: &NewLabel) -> Result<LabelRow> {
        let sql = format!(
            "INSERT INTO issue_label (workspace_id, resource_type, name, description, color) \
             VALUES ($1, $2, $3, $4, $5) RETURNING {LABEL_COLUMNS}"
        );
        sqlx::query_as::<_, LabelRow>(&sql)
            .bind(workspace_id.0)
            .bind(&input.resource_type)
            .bind(&input.name)
            .bind(&input.description)
            .bind(&input.color)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 更新：`WHERE (id, workspace_id)` 直接决定 404（无 TOCTOU 预检，照上游）。
    ///
    /// 空白补丁（全 `None`）仍是合法更新：只刷新 `updated_at`——上游行为一致。
    pub async fn update(
        &self,
        workspace_id: Id,
        label_id: Id,
        patch: &LabelUpdate,
    ) -> Result<LabelRow> {
        let sql = format!(
            "UPDATE issue_label SET \
                 name = COALESCE($3, name), \
                 description = COALESCE($4, description), \
                 color = COALESCE($5, color), \
                 updated_at = now() \
             WHERE id = $1 AND workspace_id = $2 RETURNING {LABEL_COLUMNS}"
        );
        sqlx::query_as::<_, LabelRow>(&sql)
            .bind(label_id.0)
            .bind(workspace_id.0)
            .bind(patch.name.as_deref())
            .bind(patch.description.as_deref())
            .bind(patch.color.as_deref())
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 删除目录行 + 三个资源侧的关联（**同一事务**）。
    ///
    /// 顺序与上游一致：先清关联、再删目录（`DELETE ... RETURNING id` ⇒ 未命中 =
    /// `RowNotFound` ⇒ [`RepoError::NotFound`]）。关联表故意无外键，所以这里必须显式清；
    /// 放在一个事务里保证「删了目录但关联残留」不会发生。
    pub async fn delete(&self, workspace_id: Id, label_id: Id) -> Result<()> {
        let mut tx = self.db.pool().begin().await.map_err(map_sqlx_err)?;
        for table in ["issue_to_label", "agent_to_label", "skill_to_label"] {
            sqlx::query(&format!("DELETE FROM {table} WHERE label_id = $1"))
                .bind(label_id.0)
                .execute(&mut *tx)
                .await
                .map_err(map_sqlx_err)?;
        }
        let deleted: Option<(Uuid,)> = sqlx::query_as(
            "DELETE FROM issue_label WHERE id = $1 AND workspace_id = $2 RETURNING id",
        )
        .bind(label_id.0)
        .bind(workspace_id.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;
        if deleted.is_none() {
            return Err(RepoError::NotFound);
        }
        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(())
    }

    /// issue 已挂标签列表（`resource_type = 'issue'`，按 `LOWER(name)` 升序）。
    ///
    /// 上游 `ListLabelsByIssue`：**SQL 层**收窄 workspace ⇒ 传错 workspace 得到空列表
    /// 而不是泄漏别的 workspace 的标签。
    pub async fn list_for_issue(&self, workspace_id: Id, issue_id: Id) -> Result<Vec<LabelRow>> {
        let sql = "SELECT l.id, l.workspace_id, l.resource_type, l.name, l.description, l.color, \
                    l.created_at, l.updated_at \
             FROM issue_label l JOIN issue_to_label il ON il.label_id = l.id \
             WHERE il.issue_id = $1 AND l.workspace_id = $2 AND l.resource_type = 'issue' \
             ORDER BY LOWER(l.name) ASC";
        sqlx::query_as::<_, LabelRow>(sql)
            .bind(issue_id.0)
            .bind(workspace_id.0)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 把标签挂到 issue 上（幂等）。
    ///
    /// 移植上游 `AttachLabelToIssue` 的 CTE：`inserted` 只在 issue 与 label 都同 workspace
    /// 且 label 是 `issue` 类型时插入；`bumped` 对**新插入**的行做 `revision + 1` 并推进
    /// `last_activity_at`。`changed == false` ⇒ revision 报 `0`（上游 `COALESCE(...,0)`）。
    pub async fn attach_to_issue(
        &self,
        workspace_id: Id,
        issue_id: Id,
        label_id: Id,
    ) -> Result<AssignmentOutcome> {
        let row: (bool, i64) = sqlx::query_as(
            "WITH inserted AS ( \
                 INSERT INTO issue_to_label (issue_id, label_id) \
                 SELECT $1::uuid, $2::uuid \
                 WHERE EXISTS (SELECT 1 FROM issue i WHERE i.id = $1::uuid AND i.workspace_id = $3::uuid) \
                   AND EXISTS (SELECT 1 FROM issue_label l WHERE l.id = $2::uuid \
                               AND l.workspace_id = $3::uuid AND l.resource_type = 'issue') \
                 ON CONFLICT DO NOTHING \
                 RETURNING issue_id \
             ), bumped AS ( \
                 UPDATE issue SET revision = revision + 1, \
                    last_activity_at = GREATEST(COALESCE(last_activity_at, updated_at), now()) \
                 WHERE id IN (SELECT issue_id FROM inserted) RETURNING revision \
             ) \
             SELECT EXISTS(SELECT 1 FROM inserted) AS changed, \
                    COALESCE((SELECT revision FROM bumped), 0)::bigint AS issue_revision",
        )
        .bind(issue_id.0)
        .bind(label_id.0)
        .bind(workspace_id.0)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(AssignmentOutcome {
            changed: row.0,
            issue_revision: row.1,
        })
    }

    /// 从 issue 上摘标签（幂等；未挂过 ⇒ `changed = false` 且 200，照上游）。
    pub async fn detach_from_issue(
        &self,
        workspace_id: Id,
        issue_id: Id,
        label_id: Id,
    ) -> Result<AssignmentOutcome> {
        let row: (bool, i64) = sqlx::query_as(
            "WITH deleted AS ( \
                 DELETE FROM issue_to_label \
                 WHERE issue_id = $1::uuid AND label_id = $2::uuid \
                   AND EXISTS (SELECT 1 FROM issue i WHERE i.id = $1::uuid AND i.workspace_id = $3::uuid) \
                 RETURNING issue_id \
             ), bumped AS ( \
                 UPDATE issue SET revision = revision + 1, \
                    last_activity_at = GREATEST(COALESCE(last_activity_at, updated_at), now()) \
                 WHERE id IN (SELECT issue_id FROM deleted) RETURNING revision \
             ) \
             SELECT EXISTS(SELECT 1 FROM deleted) AS changed, \
                    COALESCE((SELECT revision FROM bumped), 0)::bigint AS issue_revision",
        )
        .bind(issue_id.0)
        .bind(label_id.0)
        .bind(workspace_id.0)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(AssignmentOutcome {
            changed: row.0,
            issue_revision: row.1,
        })
    }
}

impl RepoWithDb for LabelRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

// ---------------------------------------------------------------------------
// 纯单测（无需 DB）
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_type_defaults_and_rejects() {
        assert_eq!(parse_resource_type("").as_deref(), Some("issue"));
        assert_eq!(parse_resource_type("  ").as_deref(), Some("issue"));
        assert_eq!(parse_resource_type(" issue ").as_deref(), Some("issue"));
        assert_eq!(parse_resource_type("agent").as_deref(), Some("agent"));
        assert_eq!(parse_resource_type("skill").as_deref(), Some("skill"));
        assert_eq!(parse_resource_type("ISSUE"), None, "上游是小写字面量比较");
        assert_eq!(parse_resource_type("epic"), None);
    }

    #[test]
    fn label_name_validation_matches_upstream() {
        assert_eq!(validate_name("  bug  ").as_deref(), Ok("bug"));
        assert_eq!(validate_name("").unwrap_err(), "name is required");
        assert_eq!(
            validate_name("has\ttab").unwrap_err(),
            "name cannot contain tabs, newlines, or control characters"
        );
        assert_eq!(
            validate_name("line\nbreak").unwrap_err(),
            "name cannot contain tabs, newlines, or control characters"
        );
        // 32 个字符（按 rune 计）可用，33 个拒绝。
        let ok32: String = "a".repeat(MAX_LABEL_NAME_LEN);
        assert_eq!(validate_name(&ok32).unwrap().chars().count(), 32);
        assert_eq!(
            validate_name(&"a".repeat(33)).unwrap_err(),
            "name must be 32 characters or fewer"
        );
        // 多字节字符按 rune 计：16 个中文 = 16 字符。
        assert_eq!(validate_name(&"标".repeat(16)).unwrap().chars().count(), 16);
    }

    #[test]
    fn color_requires_six_hex_digits() {
        assert_eq!(normalize_color("#3B82F6").as_deref(), Ok("#3b82f6"));
        assert_eq!(normalize_color("3b82f6").as_deref(), Ok("#3b82f6"));
        assert_eq!(normalize_color(" #abcdef ").as_deref(), Ok("#abcdef"));
        assert_eq!(
            normalize_color("red").unwrap_err(),
            "color must be a 6-digit hex value like #3b82f6"
        );
        assert_eq!(
            normalize_color("#12345").unwrap_err(),
            "color must be a 6-digit hex value like #3b82f6"
        );
        // 明确拒绝 CSS 注入面（上游 load-bearing invariant）。
        assert!(normalize_color("url(x)").is_err());
        assert!(normalize_color("#1234567").is_err());
    }

    #[test]
    fn issue_label_predicate() {
        let row = LabelRow {
            id: Uuid::nil(),
            workspace_id: Uuid::nil(),
            resource_type: "issue".into(),
            name: "bug".into(),
            description: String::new(),
            color: "#000000".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        assert!(row.is_issue_label());
        let agent_row = LabelRow {
            resource_type: "agent".into(),
            ..row
        };
        assert!(!agent_row.is_issue_label());
    }
}

// ---------------------------------------------------------------------------
// PG 集成测试（需要真库；`MULTICA_TEST_DATABASE_URL`）
// ---------------------------------------------------------------------------
#[cfg(test)]
mod db_tests {
    use super::*;

    async fn setup() -> Option<(Db, Id, Id)> {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        let db = Db::connect(&url, 4, 1).await.ok()?;
        let workspace_id: Uuid = sqlx::query_scalar(
            "INSERT INTO workspace(name, slug) VALUES ('itest-m2e-label', $1) RETURNING id",
        )
        .bind(format!("itest-m2e-l-{}", Uuid::new_v4()))
        .fetch_one(db.pool())
        .await
        .ok()?;
        let issue_id: Uuid = sqlx::query_scalar(
            "INSERT INTO issue (workspace_id, number, title, status) \
             VALUES ($1, 1, 'itest label issue', 'todo') RETURNING id",
        )
        .bind(workspace_id)
        .fetch_one(db.pool())
        .await
        .ok()?;
        Some((db, Id::from(workspace_id), Id::from(issue_id)))
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

    async fn teardown(db: &Db, workspace_id: Id) {
        let _ = sqlx::query(
            "DELETE FROM issue_to_label WHERE issue_id IN \
                             (SELECT id FROM issue WHERE workspace_id = $1)",
        )
        .bind(workspace_id.0)
        .execute(db.pool())
        .await;
        let _ = sqlx::query("DELETE FROM issue WHERE workspace_id = $1")
            .bind(workspace_id.0)
            .execute(db.pool())
            .await;
        let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
            .bind(workspace_id.0)
            .execute(db.pool())
            .await;
    }

    fn new_label(name: &str) -> NewLabel {
        NewLabel {
            resource_type: "issue".into(),
            name: name.into(),
            description: String::new(),
            color: "#3b82f6".into(),
        }
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_label_crud_round_trip_and_unique_conflict() {
        let (db, ws, _issue) = fixture!();
        let repo = LabelRepo::new(db.clone());

        let bug = repo.create(ws, &new_label("Bug")).await.expect("create");
        assert_eq!(bug.name, "Bug");
        assert_eq!(bug.color, "#3b82f6");
        assert!(bug.is_issue_label());

        // 大小写不敏感重名 → Conflict（23505）。
        assert!(matches!(
            repo.create(ws, &new_label("bug")).await,
            Err(RepoError::Conflict)
        ));

        let fetched = repo.get(ws, bug.id()).await.expect("get");
        assert_eq!(fetched.id, bug.id);

        let updated = repo
            .update(
                ws,
                bug.id(),
                &LabelUpdate {
                    color: Some("#ef4444".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("update");
        assert_eq!(updated.color, "#ef4444");
        assert_eq!(updated.name, "Bug", "未给 name ⇒ COALESCE 保持原值");

        let rows = repo.list(ws, "issue").await.expect("list");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].usage_count, 0);

        repo.delete(ws, bug.id()).await.expect("delete");
        assert!(matches!(
            repo.get(ws, bug.id()).await,
            Err(RepoError::NotFound)
        ));
        assert!(matches!(
            repo.delete(ws, bug.id()).await,
            Err(RepoError::NotFound)
        ));

        teardown(&db, ws).await;
        db.close().await;
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_attach_detach_is_idempotent_and_workspace_scoped() {
        let (db, ws, issue) = fixture!();
        let repo = LabelRepo::new(db.clone());
        let label = repo.create(ws, &new_label("Attach")).await.expect("create");

        let first = repo
            .attach_to_issue(ws, issue, label.id())
            .await
            .expect("attach");
        assert!(first.changed, "首次 attach 必须真的插入");
        assert!(first.issue_revision > 0);

        let second = repo
            .attach_to_issue(ws, issue, label.id())
            .await
            .expect("attach again");
        assert!(!second.changed, "重复 attach 幂等");
        assert_eq!(second.issue_revision, 0);

        let attached = repo
            .list_for_issue(ws, issue)
            .await
            .expect("list_for_issue");
        assert_eq!(attached.len(), 1);
        assert_eq!(attached[0].id(), label.id());

        // usage_count 随关联变化。
        let rows = repo.list(ws, "issue").await.expect("list");
        assert_eq!(rows[0].usage_count, 1);

        // 别人的 workspace ⇒ 看不到（隔离）。
        let other: Uuid = sqlx::query_scalar(
            "INSERT INTO workspace(name, slug) VALUES ('itest-m2e-other', $1) RETURNING id",
        )
        .bind(format!("itest-m2e-o-{}", Uuid::new_v4()))
        .fetch_one(db.pool())
        .await
        .expect("other ws");
        let other_ws = Id::from(other);
        assert!(repo
            .list_for_issue(other_ws, issue)
            .await
            .expect("iso")
            .is_empty());
        assert!(matches!(
            repo.get(other_ws, label.id()).await,
            Err(RepoError::NotFound)
        ));
        // 跨 workspace attach 静默 no-op（SQL 层 workspace 守卫）。
        let cross = repo
            .attach_to_issue(other_ws, issue, label.id())
            .await
            .expect("cross attach");
        assert!(!cross.changed);

        let detached = repo
            .detach_from_issue(ws, issue, label.id())
            .await
            .expect("detach");
        assert!(detached.changed);
        let again = repo
            .detach_from_issue(ws, issue, label.id())
            .await
            .expect("detach again");
        assert!(!again.changed, "重复 detach 幂等");

        // 删除目录行必须一并清掉关联（关联表无外键）。
        repo.attach_to_issue(ws, issue, label.id())
            .await
            .expect("re-attach");
        repo.delete(ws, label.id()).await.expect("delete");
        let left: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM issue_to_label WHERE label_id = $1")
                .bind(label.id().0)
                .fetch_one(db.pool())
                .await
                .expect("count");
        assert_eq!(left, 0, "删目录行后关联必须清空");

        let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
            .bind(other)
            .execute(db.pool())
            .await;
        teardown(&db, ws).await;
        db.close().await;
    }
}
