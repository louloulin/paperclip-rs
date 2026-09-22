//! `IssueStatusRepo` —— `issue_status` 表（workspace 自定义 status 目录，M2-A / LUM-1348）。
//!
//! 对应上游 `server/internal/handler/issue_status.go` + `server/internal/issuestatus`
//! 包。内置 7 个 canonical key 在 `mc_core::status::CANONICAL_KEYS`；自定义 status
//! 按 workspace 存储，`category`（open/closed）决定它是否算终态。
//!
//! 与上游的**有意偏离**（详见 `docs/11-M2-ISSUE.md` §5）：
//! - 本仓 `0001_init.up.sql` 的 `issue_status` 没有 `description` / `color` /
//!   `archived_at` / `is_system` 列（也没有 `0005` 迁移可加）。因此：
//!   * `description` / `color` 在请求里接受但**不落库**（handler 显式忽略并记 TODO）
//!   * `DELETE` 是**硬删**而不是归档；内置 key 拒绝删除（403），仍被 issue 使用的
//!     key 拒绝删除（409 `issue_status_in_use`，与上游一致的语义保护）
//! - `list` 不返回 `archived` 概念，`include_archived` 参数被接受但无效果
//!
//! 其余约定与 `crate::issue` 一致：原始类型 Row + `Id` 访问器、`map_sqlx_err`、
//! 运行时 sqlx builder、`#[ignore]` + `MULTICA_TEST_DATABASE_URL` 集成测试。

use mc_core::status::{StatusCategory, CANONICAL_KEYS};
use mc_core::Id;

use mc_db::Db;

use crate::issue::IssueStatusRow;
use crate::workspace::map_sqlx_err;
use crate::{RepoError, RepoWithDb, Result};

/// 内置 status 的默认目录（key / 展示名 / category / position）。
pub const DEFAULT_STATUSES: [(&str, &str, &str, f64); 7] = [
    ("backlog", "Backlog", "open", 0.0),
    ("todo", "Todo", "open", 1.0),
    ("in_progress", "In Progress", "open", 2.0),
    ("in_review", "In Review", "open", 3.0),
    ("done", "Done", "closed", 4.0),
    ("cancelled", "Cancelled", "closed", 5.0),
    ("triage", "Triage", "open", 6.0),
];

/// 自定义 key 长度上限（上游 64）。
pub const KEY_MAX_LEN: usize = 64;
/// 派生 key 撞车时的最大后缀尝试次数。
const DERIVE_SUFFIX_MAX: u32 = 20;

/// `IssueStatusRepo`。
#[derive(Clone)]
pub struct IssueStatusRepo {
    db: Db,
}

impl IssueStatusRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

impl RepoWithDb for IssueStatusRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

/// 新建自定义 status 的输入。
#[derive(Debug, Clone)]
pub struct NewIssueStatus {
    /// 展示名（1..=64 字符；handler 校验）
    pub name: String,
    /// 显式 key；`None` 时从 name 派生
    pub key: Option<String>,
    /// `open` / `closed`
    pub category: StatusCategory,
    /// 图标名（可空）
    pub icon: Option<String>,
    /// 位置；`None` 时排到队尾
    pub position: Option<f64>,
}

/// status 更新补丁（全部可选；`position` 由 reorder 专用接口批量设置）。
#[derive(Debug, Clone, Default)]
pub struct IssueStatusUpdate {
    pub name: Option<String>,
    pub category: Option<StatusCategory>,
    pub icon: Option<Option<String>>,
    pub position: Option<f64>,
}

/// `category` 字符串 → 枚举（`Open` / `Closed` / `open` / `closed`）。
pub fn parse_category(raw: &str) -> Option<StatusCategory> {
    match raw.to_ascii_lowercase().as_str() {
        "open" => Some(StatusCategory::Open),
        "closed" => Some(StatusCategory::Closed),
        _ => None,
    }
}

/// `category` 枚举 → wire 字符串。
pub fn category_str(category: StatusCategory) -> &'static str {
    match category {
        StatusCategory::Open => "open",
        StatusCategory::Closed => "closed",
    }
}

/// 显式 key 校验：小写字母开头，只含 `[a-z0-9_]`，≤64；内置 key 保留 → `None`。
pub fn validate_key(raw: &str) -> Option<String> {
    let key = raw.trim().to_ascii_lowercase();
    if key.is_empty() || key.len() > KEY_MAX_LEN {
        return None;
    }
    let mut chars = key.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_lowercase()) {
        return None;
    }
    if !key
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return None;
    }
    if CANONICAL_KEYS.contains(&key.as_str()) {
        return None;
    }
    Some(key)
}

/// 从展示名派生 key：小写、非字母数字 → `_`、折叠重复、去首尾 `_`、截断 64。
pub fn derive_key_from_name(name: &str) -> String {
    let mut out = String::new();
    let mut prev_underscore = false;
    for c in name.to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_underscore = false;
        } else if !prev_underscore && !out.is_empty() {
            out.push('_');
            prev_underscore = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    out.truncate(KEY_MAX_LEN);
    if out.is_empty() {
        "status".to_string()
    } else {
        out
    }
}

impl IssueStatusRepo {
    /// 幂等确保内置目录存在（上游 `issuestatus.Ensure`：读路径 self-heal）。
    pub async fn ensure_defaults(&self, workspace_id: Id) -> Result<u64> {
        let mut inserted = 0_u64;
        for (key, name, category, position) in DEFAULT_STATUSES {
            let affected = sqlx::query(
                "INSERT INTO issue_status (workspace_id, name, key, category, position) \
                 VALUES ($1, $2, $3, $4, $5) \
                 ON CONFLICT (workspace_id, key) DO NOTHING",
            )
            .bind(workspace_id.0)
            .bind(name)
            .bind(key)
            .bind(category)
            .bind(position)
            .execute(self.db.pool())
            .await
            .map_err(map_sqlx_err)?
            .rows_affected();
            inserted += affected;
        }
        Ok(inserted)
    }

    /// 目录列表（按 position 升序）。
    pub async fn list(&self, workspace_id: Id) -> Result<Vec<IssueStatusRow>> {
        sqlx::query_as::<_, IssueStatusRow>(
            "SELECT id, workspace_id, name, key, category, NULLIF(icon, '') AS icon, position, created_at, updated_at \
             FROM issue_status WHERE workspace_id = $1 \
             ORDER BY position ASC, key ASC",
        )
        .bind(workspace_id.0)
        .fetch_all(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 按 id 取。
    pub async fn get(&self, workspace_id: Id, id: Id) -> Result<IssueStatusRow> {
        sqlx::query_as::<_, IssueStatusRow>(
            "SELECT id, workspace_id, name, key, category, NULLIF(icon, '') AS icon, position, created_at, updated_at \
             FROM issue_status WHERE workspace_id = $1 AND id = $2",
        )
        .bind(workspace_id.0)
        .bind(id.0)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)
    }

    /// 按 key 取（自定义 key 的 category 解析入口）。
    pub async fn find_by_key(&self, workspace_id: Id, key: &str) -> Result<Option<IssueStatusRow>> {
        sqlx::query_as::<_, IssueStatusRow>(
            "SELECT id, workspace_id, name, key, category, NULLIF(icon, '') AS icon, position, created_at, updated_at \
             FROM issue_status WHERE workspace_id = $1 AND key = $2",
        )
        .bind(workspace_id.0)
        .bind(key)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 解析某个 status key 的生命周期分类：内置优先，其次查目录。
    pub async fn resolve_category(
        &self,
        workspace_id: Id,
        key: &str,
    ) -> Result<Option<StatusCategory>> {
        if let Some(builtin) = mc_core::status::IssueStatus::from_key(key) {
            return Ok(Some(builtin.category()));
        }
        Ok(self
            .find_by_key(workspace_id, key)
            .await?
            .and_then(|row| parse_category(&row.category)))
    }

    /// 某 key 是否仍是终态（`resolve_category == Closed`）。
    pub async fn is_closed_key(&self, workspace_id: Id, key: &str) -> Result<bool> {
        Ok(matches!(
            self.resolve_category(workspace_id, key).await?,
            Some(StatusCategory::Closed)
        ))
    }

    /// 新建自定义 status。
    ///
    /// - 显式 key：`validate_key` 不通过 → `RepoError::Conflict`
    ///   （handler 在调用前已经用 `validate_key` 做过 400 映射）
    /// - 派生 key：与已有 key 撞车时追加 `_2`、`_3`…
    pub async fn create(&self, workspace_id: Id, input: &NewIssueStatus) -> Result<IssueStatusRow> {
        let key = match input.key.as_deref() {
            Some(raw) => validate_key(raw).ok_or(RepoError::Conflict)?,
            None => self.derive_available_key(workspace_id, &input.name).await?,
        };
        let position = match input.position {
            Some(p) => p,
            None => self.next_position(workspace_id).await?,
        };
        sqlx::query_as::<_, IssueStatusRow>(
            "INSERT INTO issue_status (workspace_id, name, key, category, icon, position) \
             VALUES ($1, $2, $3, $4, COALESCE($5::text, ''), $6) \
             RETURNING id, workspace_id, name, key, category, NULLIF(icon, '') AS icon, position, created_at, updated_at",
        )
        .bind(workspace_id.0)
        .bind(&input.name)
        .bind(&key)
        .bind(category_str(input.category))
        .bind(input.icon.as_deref())
        .bind(position)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 更新自定义 status（内置 key 也可改展示名 / 图标，但 category 由调用方决定是否允许）。
    pub async fn update(
        &self,
        workspace_id: Id,
        id: Id,
        patch: &IssueStatusUpdate,
    ) -> Result<IssueStatusRow> {
        let row = sqlx::query_as::<_, IssueStatusRow>(
            "UPDATE issue_status SET \
                 name = CASE WHEN $3::boolean THEN $4::text ELSE name END, \
                 category = CASE WHEN $5::boolean THEN $6::text ELSE category END, \
                 icon = CASE WHEN $7::boolean THEN COALESCE($8::text, '') ELSE icon END, \
                 position = CASE WHEN $9::boolean THEN $10::double precision ELSE position END, \
                 updated_at = now() \
             WHERE workspace_id = $1 AND id = $2 \
             RETURNING id, workspace_id, name, key, category, NULLIF(icon, '') AS icon, position, created_at, updated_at",
        )
        .bind(workspace_id.0)
        .bind(id.0)
        .bind(patch.name.is_some())
        .bind(patch.name.as_deref())
        .bind(patch.category.is_some())
        .bind(patch.category.map(category_str))
        .bind(patch.icon.is_some())
        .bind(patch.icon.clone().flatten())
        .bind(patch.position.is_some())
        .bind(patch.position)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        row.ok_or(RepoError::NotFound)
    }

    /// 批量重排（`PATCH /api/issue-statuses/reorder`）：`(id, position)` 列表，事务内逐条写。
    pub async fn reorder(&self, workspace_id: Id, order: &[(Id, f64)]) -> Result<()> {
        let mut tx = self.db.pool().begin().await.map_err(map_sqlx_err)?;
        for (id, position) in order {
            let affected = sqlx::query(
                "UPDATE issue_status SET position = $3, updated_at = now() \
                 WHERE workspace_id = $1 AND id = $2",
            )
            .bind(workspace_id.0)
            .bind(id.0)
            .bind(position)
            .execute(&mut *tx)
            .await
            .map_err(map_sqlx_err)?
            .rows_affected();
            if affected == 0 {
                return Err(RepoError::NotFound);
            }
        }
        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(())
    }

    /// 仍在使用该 key 的 issue 数量（删除前的保护）。
    pub async fn count_issues_using_key(&self, workspace_id: Id, key: &str) -> Result<i64> {
        sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM issue WHERE workspace_id = $1 AND status = $2",
        )
        .bind(workspace_id.0)
        .bind(key)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 删除自定义 status。
    ///
    /// - 内置 key → `Conflict`（handler 映射 403，上游用 `is_system` 判定）
    /// - 仍被 issue 使用 → `Conflict`（handler 映射 409 `issue_status_in_use`）
    /// - 不存在 → `NotFound`
    pub async fn delete(&self, workspace_id: Id, id: Id) -> Result<()> {
        let row = self.get(workspace_id, id).await?;
        if row.is_builtin() {
            return Err(RepoError::Conflict);
        }
        if self.count_issues_using_key(workspace_id, &row.key).await? > 0 {
            return Err(RepoError::Conflict);
        }
        self.db_delete(workspace_id, id).await
    }

    async fn db_delete(&self, workspace_id: Id, id: Id) -> Result<()> {
        let affected = sqlx::query("DELETE FROM issue_status WHERE workspace_id = $1 AND id = $2")
            .bind(workspace_id.0)
            .bind(id.0)
            .execute(self.db.pool())
            .await
            .map_err(map_sqlx_err)?
            .rows_affected();
        if affected == 0 {
            return Err(RepoError::NotFound);
        }
        Ok(())
    }

    async fn next_position(&self, workspace_id: Id) -> Result<f64> {
        let position: Option<f64> = sqlx::query_scalar(
            "SELECT MAX(position) + 1 FROM issue_status WHERE workspace_id = $1",
        )
        .bind(workspace_id.0)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(position.unwrap_or(0.0))
    }

    async fn derive_available_key(&self, workspace_id: Id, name: &str) -> Result<String> {
        let base = derive_key_from_name(name);
        let mut candidate = base.clone();
        for suffix in 2..=DERIVE_SUFFIX_MAX {
            if self.find_by_key(workspace_id, &candidate).await?.is_none()
                && !CANONICAL_KEYS.contains(&candidate.as_str())
            {
                return Ok(candidate);
            }
            candidate = format!("{base}_{suffix}");
        }
        Err(RepoError::Conflict)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn validate_key_accepts_snake_case_and_rejects_reserved() {
        assert_eq!(
            validate_key("waiting_review"),
            Some("waiting_review".into())
        );
        assert_eq!(validate_key("Blocked_2"), Some("blocked_2".into()));
        // 空格 / 连字符不在 key 字符集内（显式 key 必须自归一）
        assert_eq!(validate_key("Waiting Review"), None);
        assert_eq!(validate_key("blocked"), Some("blocked".into()));
        // 内置 key 保留
        assert_eq!(validate_key("todo"), None);
        assert_eq!(validate_key("done"), None);
        // 非法
        assert_eq!(validate_key("9lives"), None);
        assert_eq!(validate_key("with-dash"), None);
        assert_eq!(validate_key(""), None);
        assert_eq!(validate_key(&"a".repeat(65)), None);
    }

    #[test]
    fn derive_key_from_name_normalizes() {
        assert_eq!(
            derive_key_from_name("Waiting for Review"),
            "waiting_for_review"
        );
        assert_eq!(derive_key_from_name("  Blocked!!  "), "blocked");
        assert_eq!(derive_key_from_name("A   B"), "a_b");
        assert_eq!(derive_key_from_name("!!!"), "status");
        assert_eq!(derive_key_from_name(""), "status");
        assert!(derive_key_from_name(&"x".repeat(100)).len() <= KEY_MAX_LEN);
    }

    #[test]
    fn derived_key_never_collides_with_builtin() {
        // derive 出来的 key 若命中内置 key，create 会走 `_2` 后缀分支（见 derive_available_key）
        assert_eq!(derive_key_from_name("Todo"), "todo");
        assert!(CANONICAL_KEYS.contains(&derive_key_from_name("Todo").as_str()));
        assert!(validate_key("todo").is_none());
    }

    #[test]
    fn category_round_trip() {
        assert_eq!(parse_category("Open"), Some(StatusCategory::Open));
        assert_eq!(parse_category("closed"), Some(StatusCategory::Closed));
        assert_eq!(parse_category("archived"), None);
        assert_eq!(category_str(StatusCategory::Closed), "closed");
    }

    #[test]
    fn default_catalog_matches_canonical_keys() {
        assert_eq!(DEFAULT_STATUSES.len(), CANONICAL_KEYS.len());
        for (key, _, category, _) in DEFAULT_STATUSES {
            assert!(
                CANONICAL_KEYS.contains(&key),
                "{key} must be a canonical key"
            );
            assert!(matches!(category, "open" | "closed"));
        }
        // 终态只有 done / cancelled
        let closed: Vec<&str> = DEFAULT_STATUSES
            .iter()
            .filter(|(_, _, category, _)| *category == "closed")
            .map(|(key, _, _, _)| *key)
            .collect();
        assert_eq!(closed, vec!["done", "cancelled"]);
    }
}

// ---------------------------------------------------------------------------
// PG 集成测试（需要真库）
// ---------------------------------------------------------------------------
#[cfg(test)]
mod db_tests {
    use super::*;
    use uuid::Uuid;

    async fn setup() -> Option<(Db, Id)> {
        let url = std::env::var("MULTICA_TEST_DATABASE_URL").ok()?;
        let db = Db::connect(&url, 4, 1).await.ok()?;
        let workspace_id: Uuid = sqlx::query_scalar(
            "INSERT INTO workspace(name, slug) VALUES ('itest-m2a-status', $1) RETURNING id",
        )
        .bind(format!("itest-m2a-s-{}", Uuid::new_v4()))
        .fetch_one(db.pool())
        .await
        .ok()?;
        Some((db, Id::from(workspace_id)))
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
        let _ = sqlx::query("DELETE FROM workspace WHERE id = $1")
            .bind(workspace_id.0)
            .execute(db.pool())
            .await;
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_ensure_defaults_is_idempotent() {
        let (db, ws) = fixture!();
        let repo = IssueStatusRepo::new(db.clone());

        assert_eq!(repo.ensure_defaults(ws).await.expect("seed"), 7);
        assert_eq!(repo.ensure_defaults(ws).await.expect("seed again"), 0);

        let rows = repo.list(ws).await.expect("list");
        assert_eq!(rows.len(), 7);
        let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys.first(), Some(&"backlog"));
        assert_eq!(keys.last(), Some(&"triage"));
        assert!(rows.iter().all(IssueStatusRow::is_builtin));
        assert_eq!(rows.iter().filter(|r| r.is_closed()).count(), 2);
        // position 单调递增
        assert!(rows.windows(2).all(|w| w[0].position < w[1].position));

        assert_eq!(
            repo.resolve_category(ws, "done").await.expect("cat"),
            Some(StatusCategory::Closed)
        );
        assert_eq!(
            repo.resolve_category(ws, "todo").await.expect("cat"),
            Some(StatusCategory::Open)
        );
        assert_eq!(repo.resolve_category(ws, "nope").await.expect("cat"), None);
        assert!(repo.is_closed_key(ws, "cancelled").await.expect("closed"));

        teardown(&db, ws).await;
        db.close().await;
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_create_custom_status_derives_and_suffixes_key() {
        let (db, ws) = fixture!();
        let repo = IssueStatusRepo::new(db.clone());
        repo.ensure_defaults(ws).await.expect("seed");

        let custom = repo
            .create(
                ws,
                &NewIssueStatus {
                    name: "Waiting Review".into(),
                    key: None,
                    category: StatusCategory::Open,
                    icon: Some("hourglass".into()),
                    position: None,
                },
            )
            .await
            .expect("create custom");
        assert_eq!(custom.key, "waiting_review");
        assert!(!custom.is_builtin());
        assert!(custom.position > 6.0, "custom status goes to the tail");

        // 派生的 key 与内置 key 撞车 → 加后缀。
        // 展示名取 `"Todo!"`：它派生出同一个 key `todo`（非字母数字折成 `_` 再去尾），
        // 但上游 `idx_issue_status_workspace_name_active` 对 `lower(name)` 唯一，
        // 与内置 `Todo` 同名的展示名会被该唯一索引直接拒（409），走不到派生逻辑。
        let collides = repo
            .create(
                ws,
                &NewIssueStatus {
                    name: "Todo!".into(),
                    key: None,
                    category: StatusCategory::Open,
                    icon: None,
                    position: None,
                },
            )
            .await
            .expect("create colliding");
        assert_eq!(collides.key, "todo_2");

        // 显式 key：大写归一化；非法 key 被拒（handler 会先做 400 校验）
        let explicit = repo
            .create(
                ws,
                &NewIssueStatus {
                    name: "Blocked".into(),
                    key: Some("Blocked".into()),
                    category: StatusCategory::Closed,
                    icon: None,
                    position: None,
                },
            )
            .await
            .expect("create explicit");
        assert_eq!(explicit.key, "blocked");
        assert_eq!(
            repo.resolve_category(ws, "blocked").await.expect("cat"),
            Some(StatusCategory::Closed)
        );
        assert!(matches!(
            repo.create(
                ws,
                &NewIssueStatus {
                    name: "Bad".into(),
                    key: Some("nope-key!".into()),
                    category: StatusCategory::Open,
                    icon: None,
                    position: None,
                }
            )
            .await,
            Err(RepoError::Conflict)
        ));

        teardown(&db, ws).await;
        db.close().await;
    }

    #[tokio::test]
    #[ignore = "requires MULTICA_TEST_DATABASE_URL"]
    async fn db_update_reorder_and_delete_guards() {
        let (db, ws) = fixture!();
        let repo = IssueStatusRepo::new(db.clone());
        repo.ensure_defaults(ws).await.expect("seed");
        let user_id: Uuid = sqlx::query_scalar(
            r#"INSERT INTO "user"(name, email) VALUES ('itest-m2a-status', $1) RETURNING id"#,
        )
        .bind(format!("itest-m2a-st-{}@example.com", Uuid::new_v4()))
        .fetch_one(db.pool())
        .await
        .expect("user");

        let a = repo
            .create(
                ws,
                &NewIssueStatus {
                    name: "Waiting".into(),
                    key: None,
                    category: StatusCategory::Open,
                    icon: None,
                    position: None,
                },
            )
            .await
            .expect("a");
        let b = repo
            .create(
                ws,
                &NewIssueStatus {
                    name: "Paused".into(),
                    key: None,
                    category: StatusCategory::Open,
                    icon: None,
                    position: None,
                },
            )
            .await
            .expect("b");

        // 改名 / 分类 / 图标（显式清空）
        let updated = repo
            .update(
                ws,
                a.id(),
                &IssueStatusUpdate {
                    name: Some("Waiting on client".into()),
                    category: Some(StatusCategory::Closed),
                    icon: Some(None),
                    position: None,
                },
            )
            .await
            .expect("update");
        assert_eq!(updated.name, "Waiting on client");
        assert_eq!(updated.category, "closed");
        assert_eq!(updated.icon, None);
        assert!(repo.is_closed_key(ws, &a.key).await.expect("closed"));
        assert!(matches!(
            repo.update(ws, Id::new(), &IssueStatusUpdate::default())
                .await,
            Err(RepoError::NotFound)
        ));

        // 重排：把 b 放到最前面
        repo.reorder(ws, &[(b.id(), -1.0), (a.id(), 0.5)])
            .await
            .expect("reorder");
        let rows = repo.list(ws).await.expect("list");
        assert_eq!(rows[0].id, b.id);

        // issue 正在使用该 key → 删除被拒（Conflict，handler 映射 409）
        let issue_id: Uuid = sqlx::query_scalar(
            "INSERT INTO issue (workspace_id, number, identifier, title, status, creator_type, creator_id) \
             VALUES ($1, 1, 'ITESTM2AS-1', 'uses custom status', $2, 'user', $3::uuid) RETURNING id",
        )
        .bind(ws.0)
        .bind(&a.key)
        .bind(user_id.to_string())
        .fetch_one(db.pool())
        .await
        .expect("issue");
        assert_eq!(
            repo.count_issues_using_key(ws, &a.key)
                .await
                .expect("count"),
            1
        );
        assert!(matches!(
            repo.delete(ws, a.id()).await,
            Err(RepoError::Conflict)
        ));

        // 内置 key 永远不能删除
        assert!(matches!(
            repo.delete(ws, rows[1].id()).await,
            Err(RepoError::Conflict)
        ));

        // 清掉占用后可以删
        let _ = sqlx::query("DELETE FROM issue WHERE id = $1")
            .bind(issue_id)
            .execute(db.pool())
            .await;
        repo.delete(ws, a.id()).await.expect("delete custom");
        assert!(matches!(
            repo.get(ws, a.id()).await,
            Err(RepoError::NotFound)
        ));

        teardown(&db, ws).await;
        db.close().await;
    }
}
