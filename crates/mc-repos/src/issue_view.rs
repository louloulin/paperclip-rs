//! `IssueViewRepo` —— `issue_view`（保存的视图定义）+ `issue_view_preference`
//! （视图栏每用户偏好）的行访问（M2-A 尾 / LUM-1691）。
//!
//! 对应上游 `server/internal/handler/issue_view.go` + `issue_view_preference.go` 与
//! `server/pkg/db/queries/{issue_view,issue_view_preference}.sql`。两张表已在 W0-B2 的
//! 上游迁移链上（`265_issue_view` / `268_issue_view_preference`）⇒ 本片**不新增迁移**。
//!
//! ```text
//! issue_view(id PK, workspace_id, owner_id, name(1..80), scope_type IN
//!            (workspace|my|project), scope_id, scope_variant IN
//!            (assigned|created|involved|any), visibility IN (private|workspace),
//!            definition_version int default 1, query jsonb object not null,
//!            display jsonb object not null default '{}', revision int default 1,
//!            created_at, updated_at)
//!   CHECK project ⇒ scope_id NOT NULL / workspace|my ⇒ scope_id IS NULL
//!   CHECK my ⇒ scope_variant NOT NULL / 其余 ⇒ scope_variant IS NULL
//!   CHECK my ⇒ visibility = 'private'
//!   —— 三张表都**没有外键**（上游仓库策略：生命周期清理由应用事务做）
//! issue_view_preference(workspace_id, user_id, scope_type, scope_id NOT NULL,
//!                       prefs jsonb object, updated_at)  PK 四列复合
//! ```
//!
//! 关键语义（照上游）：
//! - **读权限**：所有者**或** `visibility='workspace'`（[`IssueViewRow::is_readable_by`]）。
//!   不可读与不存在都返回 404 —— 私有视图的**存在性**不得泄漏（上游
//!   `loadIssueViewForUser` 把两者合成一个分支）。
//! - **写权限**：所有者；或共享视图的 workspace `owner` / `admin`
//!   （上游 `canManageIssueView`，判定在路由层，因为它要读 `member.role`）。
//! - **乐观并发**：[`IssueViewRepo::update`] 带 `revision = $8` 闸门，未命中返回 `None`
//!   ⇒ 路由层 409（上游 `UpdateIssueView` 的 `pgx.ErrNoRows` 分支）。
//! - **删除顺手清 pin**：`issue_view` 行删掉的同一条语句里把指向它的 `pinned_item`
//!   （`item_type='view'`）一起删 —— 视图没了但 pin 还在的侧栏项在 UI 上**点不动也删不掉**
//!   （view pin 不会自动取消），上游 `DeleteIssueView` 的 CTE 就是这个理由。
//! - **preference 无记录不是错误**：`GET` 未命中返回 `Ok(None)`，路由层回 200 + `prefs={}`
//!   （上游明确 `pgx.ErrNoRows` → 空文档）。
//! - **preference 的 `scope_id` 永不为 NULL**：`workspace` → workspace id、`my` → user id、
//!   `project` → project id（上游 `resolvePreferenceScope` 的回填口径，见
//!   [`preference_scope_id`]）。复合主键因此不依赖 NULL。

use chrono::{DateTime, Utc};
use mc_core::Id;
use mc_db::Db;
use serde_json::Value as JsonValue;
use sqlx::FromRow;
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::{RepoError, RepoWithDb, Result};

/// 视图名长度上限（上游 `issueViewNameMaxLen`）。
pub const MAX_NAME_LEN: usize = 80;
/// 视图写请求体字节上限（上游 `issueViewBodyMaxBytes`）。
pub const BODY_MAX_BYTES: usize = 128 * 1024;
/// 每成员每 workspace 的视图配额（上游 `issueViewsPerOwnerMax`）。
pub const PER_OWNER_MAX: i64 = 100;
/// `list_for_user` 的硬上限（上游 `ListIssueViewsForUser` 的 `LIMIT 200`）。
pub const LIST_LIMIT: i64 = 200;

/// 合法的 `scope_type`（上游 `validIssueViewScopeTypes`）。
pub const SCOPE_TYPES: [&str; 3] = ["workspace", "my", "project"];
/// `my` 作用域必须带的变体（上游 `validIssueViewMyVariants`）。
pub const MY_VARIANTS: [&str; 4] = ["assigned", "created", "involved", "any"];
/// `workspace` / `project` 作用域可选的指派人收窄（上游 `validIssueViewWorkspaceVariants`）。
pub const WORKSPACE_VARIANTS: [&str; 2] = ["members", "agents"];
/// 合法的 `visibility`（上游 `validIssueViewVisibilities`）。
pub const VISIBILITIES: [&str; 2] = ["private", "workspace"];

/// `scope_variant` 与 `scope_type` 不配对（路由层 400）。
///
/// 用一个零尺寸类型而不是 `()`：`Result<_, ()>` 会被 `clippy::result_unit_err` 判为
/// 错误无法携带上下文，而这里确实只有「配 / 不配」两态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidVariant;

/// `scope_variant` 校验（上游 `validateIssueViewVariant`）。
///
/// 返回值是**有效值**（`None` = 该 scope 下应为 NULL），`Err(InvalidVariant)` = 这个配对非法
/// （路由层 400）。
/// - `my`：必须给且必须命中 [`MY_VARIANTS`]；
/// - `workspace` / `project`：缺失 / `""` / `"all"` ⇒ `Ok(None)`（无收窄 = All 页签）；
///   命中 [`WORKSPACE_VARIANTS`] ⇒ `Ok(Some(v))`；其余 ⇒ `Err(InvalidVariant)`。
pub fn validate_variant(
    scope_type: &str,
    variant: Option<&str>,
) -> std::result::Result<Option<String>, InvalidVariant> {
    match scope_type {
        "my" => match variant {
            Some(v) if MY_VARIANTS.contains(&v) => Ok(Some(v.to_string())),
            _ => Err(InvalidVariant),
        },
        _ => match variant {
            None | Some("" | "all") => Ok(None),
            Some(v) if WORKSPACE_VARIANTS.contains(&v) => Ok(Some(v.to_string())),
            _ => Err(InvalidVariant),
        },
    }
}

/// preference 的 `scope_id` 回填（上游 `resolvePreferenceScope`）。
///
/// - `workspace` → `workspace_id`；
/// - `my` → `user_id`；
/// - `project` → `project_id`（`None` 表示请求里缺 `scope_id` ⇒ 路由层 400）；
/// - 其余 `scope_type` ⇒ `None`（路由层 400 `invalid scope_type`）。
pub fn preference_scope_id(
    scope_type: &str,
    workspace_id: Id,
    user_id: Id,
    project_id: Option<Id>,
) -> Option<Id> {
    match scope_type {
        "workspace" => Some(workspace_id),
        "my" => Some(user_id),
        "project" => project_id,
        _ => None,
    }
}

/// `isJSONObject`：`query` / `display` / `prefs` 必须是 JSON 对象。
///
/// JSON `null` 反序列化后是 `Value::Null`（不是对象）⇒ 直接判否，而不是留给 DB 的 CHECK
/// 变成 500（上游注释同一句）。
pub fn is_json_object(raw: &JsonValue) -> bool {
    matches!(raw, JsonValue::Object(_))
}

/// `issue_view` 行。
#[derive(Debug, Clone, FromRow)]
pub struct IssueViewRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub owner_id: Uuid,
    pub name: String,
    /// `workspace` / `my` / `project`
    pub scope_type: String,
    /// project 作用域才有值（DB CHECK 保证）。
    pub scope_id: Option<Uuid>,
    /// 只有 `my` 作用域才有值（DB CHECK 保证）。
    pub scope_variant: Option<String>,
    /// `private` / `workspace`
    pub visibility: String,
    pub definition_version: i32,
    /// 过滤文档（不透明 JSON 对象，语义在客户端 `definition_version` 契约里）。
    pub query: JsonValue,
    /// 首次打开的展示默认值（不透明 JSON 对象）。
    pub display: JsonValue,
    /// 乐观并发版本号，每次 `update` +1。
    pub revision: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl IssueViewRow {
    /// 主键。
    pub fn id(&self) -> Id {
        Id::from(self.id)
    }

    /// 所属 workspace。
    pub fn workspace_id(&self) -> Id {
        Id::from(self.workspace_id)
    }

    /// 读权限（上游 `canReadIssueView`）：所有者**或** workspace 共享。
    ///
    /// `my` 作用域被 DB CHECK 钉死为 `private`，所以它只会命中所有者那一支。
    pub fn is_readable_by(&self, user_id: Id) -> bool {
        self.owner_id == user_id.0 || self.visibility == "workspace"
    }

    /// 是否 workspace 共享（路由层的 admin 覆写权限只对共享视图开放）。
    pub fn is_shared(&self) -> bool {
        self.visibility == "workspace"
    }
}

/// 新建视图的输入（字段已由路由层校验 / 归一）。
#[derive(Debug, Clone)]
pub struct NewIssueView {
    pub workspace_id: Id,
    pub owner_id: Id,
    pub name: String,
    pub scope_type: String,
    pub scope_id: Option<Id>,
    pub scope_variant: Option<String>,
    pub visibility: String,
    pub definition_version: i32,
    pub query: JsonValue,
    pub display: JsonValue,
}

/// 更新补丁（路由层已按「缺失 = 不动」把缺省从当前行补齐）。
///
/// `scope_variant` 是三层：`None` = 不动、`Some(None)` = 清空、`Some(Some(v))` = 写入。
#[derive(Debug, Clone)]
pub struct IssueViewPatch {
    pub name: String,
    pub visibility: String,
    pub scope_variant: Option<String>,
    pub query: JsonValue,
    pub display: JsonValue,
    /// 调用者看到的版本号（乐观并发闸门）。
    pub expected_revision: i32,
}

/// `issue_view_preference` 行。
#[derive(Debug, Clone, FromRow)]
pub struct IssueViewPreferenceRow {
    pub workspace_id: Uuid,
    pub user_id: Uuid,
    pub scope_type: String,
    /// 回填后的容器 id（永不为 NULL）。
    pub scope_id: Uuid,
    /// 不透明偏好文档（`{"hidden":[…],"order":[…]}`）。
    pub prefs: JsonValue,
    pub updated_at: DateTime<Utc>,
}

impl IssueViewPreferenceRow {
    /// 容器 id。
    pub fn scope_id(&self) -> Id {
        Id::from(self.scope_id)
    }
}

const VIEW_COLUMNS: &str =
    "id, workspace_id, owner_id, name, scope_type, scope_id, scope_variant, \
                            visibility, definition_version, query, display, revision, \
                            created_at, updated_at";
const PREFERENCE_COLUMNS: &str = "workspace_id, user_id, scope_type, scope_id, prefs, updated_at";

/// `issue_view` + `issue_view_preference` 仓储。
pub struct IssueViewRepo {
    db: Db,
}

impl IssueViewRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 该成员在该 workspace 已拥有的视图数（配额判定，上游 `CountIssueViewsByOwner`）。
    pub async fn count_by_owner(&self, workspace_id: Id, owner_id: Id) -> Result<i64> {
        let (count,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*)::bigint FROM issue_view WHERE workspace_id = $1 AND owner_id = $2",
        )
        .bind(workspace_id.0)
        .bind(owner_id.0)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(count)
    }

    /// 新建（上游 `CreateIssueView`）。
    ///
    /// 非法 `scope`/`visibility`/`scope_variant` 组合会撞 DB 的 CHECK ⇒ 这里不预检，
    /// 让路由层做词汇校验（上游同款分工）。
    pub async fn create(&self, new: &NewIssueView) -> Result<IssueViewRow> {
        let sql = format!(
            "INSERT INTO issue_view (workspace_id, owner_id, name, scope_type, scope_id, \
                 scope_variant, visibility, definition_version, query, display) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING {VIEW_COLUMNS}"
        );
        sqlx::query_as::<_, IssueViewRow>(&sql)
            .bind(new.workspace_id.0)
            .bind(new.owner_id.0)
            .bind(&new.name)
            .bind(&new.scope_type)
            .bind(new.scope_id.map(|id| id.0))
            .bind(new.scope_variant.as_deref())
            .bind(&new.visibility)
            .bind(new.definition_version)
            .bind(&new.query)
            .bind(&new.display)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 一个 `(scope_type, scope_id)` 容器下该用户能看到的视图（上游 `ListIssueViewsForUser`）。
    ///
    /// `scope_id IS NOT DISTINCT FROM $4` —— workspace / my 作用域的 `scope_id` 是 NULL，
    /// 必须用 NULL-安全比较，不能用 `= NULL`。共享视图对任何成员可见。
    pub async fn list_for_user(
        &self,
        workspace_id: Id,
        scope_type: &str,
        owner_id: Id,
        scope_id: Option<Id>,
    ) -> Result<Vec<IssueViewRow>> {
        let sql = format!(
            "SELECT {VIEW_COLUMNS} FROM issue_view \
             WHERE workspace_id = $1 \
               AND scope_type = $2 \
               AND scope_id IS NOT DISTINCT FROM $4::uuid \
               AND (owner_id = $3 OR visibility = 'workspace') \
             ORDER BY created_at ASC \
             LIMIT {LIST_LIMIT}"
        );
        sqlx::query_as::<_, IssueViewRow>(&sql)
            .bind(workspace_id.0)
            .bind(scope_type)
            .bind(owner_id.0)
            .bind(scope_id.map(|id| id.0))
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 单条读取，workspace 收窄（**不含**读权限判定 —— 权限在路由层，两者都 404）。
    pub async fn get(&self, workspace_id: Id, view_id: Id) -> Result<IssueViewRow> {
        let sql =
            format!("SELECT {VIEW_COLUMNS} FROM issue_view WHERE id = $1 AND workspace_id = $2");
        sqlx::query_as::<_, IssueViewRow>(&sql)
            .bind(view_id.0)
            .bind(workspace_id.0)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)?
            .ok_or(RepoError::NotFound)
    }

    /// 乐观更新（上游 `UpdateIssueView`）：`WHERE … AND revision = $8`。
    ///
    /// `Ok(None)` = 0 行命中（版本被别处推进）—— 路由层据此回 409；行本身在调用前已
    /// 读过，所以「行消失」不会走到这里。
    pub async fn update(
        &self,
        workspace_id: Id,
        view_id: Id,
        patch: &IssueViewPatch,
    ) -> Result<Option<IssueViewRow>> {
        let sql = format!(
            "UPDATE issue_view SET \
                 name = $3, visibility = $4, scope_variant = $5, query = $6, display = $7, \
                 revision = revision + 1, updated_at = now() \
             WHERE id = $1 AND workspace_id = $2 AND revision = $8 \
             RETURNING {VIEW_COLUMNS}"
        );
        sqlx::query_as::<_, IssueViewRow>(&sql)
            .bind(view_id.0)
            .bind(workspace_id.0)
            .bind(&patch.name)
            .bind(&patch.visibility)
            .bind(patch.scope_variant.as_deref())
            .bind(&patch.query)
            .bind(&patch.display)
            .bind(patch.expected_revision)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 删除视图，并在**同一条语句**里清扫指向它的 `view` pin（上游 `DeleteIssueView`）。
    ///
    /// 未命中 ⇒ [`RepoError::NotFound`]。清扫是数据修改型 CTE —— PostgreSQL 保证
    /// WITH 里的数据修改语句「恰好执行一次且跑到底」，与主查询是否读它的输出无关。
    pub async fn delete(&self, workspace_id: Id, view_id: Id) -> Result<()> {
        let deleted: Option<(Uuid,)> = sqlx::query_as(
            "WITH deleted AS ( \
                 DELETE FROM issue_view \
                 WHERE issue_view.id = $1 AND issue_view.workspace_id = $2 \
                 RETURNING issue_view.id \
             ), swept_pins AS ( \
                 DELETE FROM pinned_item \
                 WHERE pinned_item.item_type = 'view' \
                   AND pinned_item.workspace_id = $2 \
                   AND pinned_item.item_id IN (SELECT deleted.id FROM deleted) \
             ) \
             SELECT deleted.id FROM deleted",
        )
        .bind(view_id.0)
        .bind(workspace_id.0)
        .fetch_optional(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        deleted.map(|_| ()).ok_or(RepoError::NotFound)
    }

    /// 读偏好：**未命中返回 `Ok(None)`**（上游把它当空文档，不是 404）。
    pub async fn get_preference(
        &self,
        workspace_id: Id,
        user_id: Id,
        scope_type: &str,
        scope_id: Id,
    ) -> Result<Option<IssueViewPreferenceRow>> {
        let sql = format!(
            "SELECT {PREFERENCE_COLUMNS} FROM issue_view_preference \
             WHERE workspace_id = $1 AND user_id = $2 AND scope_type = $3 AND scope_id = $4"
        );
        sqlx::query_as::<_, IssueViewPreferenceRow>(&sql)
            .bind(workspace_id.0)
            .bind(user_id.0)
            .bind(scope_type)
            .bind(scope_id.0)
            .fetch_optional(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 整文档覆盖写（上游 `UpsertIssueViewPreference`）：单用户数据，last-write-wins。
    pub async fn upsert_preference(
        &self,
        workspace_id: Id,
        user_id: Id,
        scope_type: &str,
        scope_id: Id,
        prefs: &JsonValue,
    ) -> Result<IssueViewPreferenceRow> {
        let sql = format!(
            "INSERT INTO issue_view_preference (workspace_id, user_id, scope_type, scope_id, prefs) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (workspace_id, user_id, scope_type, scope_id) \
             DO UPDATE SET prefs = EXCLUDED.prefs, updated_at = now() \
             RETURNING {PREFERENCE_COLUMNS}"
        );
        sqlx::query_as::<_, IssueViewPreferenceRow>(&sql)
            .bind(workspace_id.0)
            .bind(user_id.0)
            .bind(scope_type)
            .bind(scope_id.0)
            .bind(prefs)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }
}

impl RepoWithDb for IssueViewRepo {
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
    use serde_json::json;

    fn id(n: u128) -> Id {
        Id(Uuid::from_u128(n))
    }

    #[test]
    fn my_scope_requires_a_known_variant() {
        assert_eq!(
            validate_variant("my", Some("assigned")),
            Ok(Some("assigned".into()))
        );
        assert_eq!(validate_variant("my", Some("any")), Ok(Some("any".into())));
        // 缺失 / 未知 / 空 ⇒ 非法（与 workspace 作用域的语义相反）。
        assert_eq!(validate_variant("my", None), Err(InvalidVariant));
        assert_eq!(validate_variant("my", Some("")), Err(InvalidVariant));
        assert_eq!(validate_variant("my", Some("all")), Err(InvalidVariant));
        assert_eq!(validate_variant("my", Some("members")), Err(InvalidVariant));
    }

    #[test]
    fn workspace_and_project_scopes_accept_optional_narrowing() {
        for scope in ["workspace", "project"] {
            // 缺省 / "" / "all" 都是「无收窄 ⇒ NULL」。
            assert_eq!(validate_variant(scope, None), Ok(None));
            assert_eq!(validate_variant(scope, Some("")), Ok(None));
            assert_eq!(validate_variant(scope, Some("all")), Ok(None));
            assert_eq!(
                validate_variant(scope, Some("members")),
                Ok(Some("members".into()))
            );
            assert_eq!(
                validate_variant(scope, Some("agents")),
                Ok(Some("agents".into()))
            );
            // my 专属变体不得漏进来。
            assert_eq!(
                validate_variant(scope, Some("assigned")),
                Err(InvalidVariant)
            );
        }
    }

    #[test]
    fn preference_scope_backfills_so_the_key_never_carries_null() {
        let ws = id(1);
        let user = id(2);
        let project = id(3);
        assert_eq!(preference_scope_id("workspace", ws, user, None), Some(ws));
        assert_eq!(preference_scope_id("my", ws, user, None), Some(user));
        assert_eq!(
            preference_scope_id("project", ws, user, Some(project)),
            Some(project)
        );
        // project 缺 scope_id ⇒ 交给路由层 400（这里只回 None）。
        assert_eq!(preference_scope_id("project", ws, user, None), None);
        assert_eq!(preference_scope_id("label", ws, user, Some(project)), None);
    }

    #[test]
    fn only_json_objects_pass_the_definition_blob_gate() {
        assert!(is_json_object(&json!({})));
        assert!(is_json_object(&json!({"filters": []})));
        assert!(!is_json_object(&JsonValue::Null));
        assert!(!is_json_object(&json!([])));
        assert!(!is_json_object(&json!("x")));
        assert!(!is_json_object(&json!(3)));
    }

    #[test]
    fn read_rule_is_owner_or_workspace_shared() {
        let owner = id(10);
        let other = id(11);
        let row = |owner_id: Uuid, visibility: &str| IssueViewRow {
            id: id(20).0,
            workspace_id: id(1).0,
            owner_id,
            name: "v".into(),
            scope_type: "workspace".into(),
            scope_id: None,
            scope_variant: None,
            visibility: visibility.into(),
            definition_version: 1,
            query: json!({}),
            display: json!({}),
            revision: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        assert!(row(owner.0, "private").is_readable_by(owner));
        assert!(!row(owner.0, "private").is_readable_by(other));
        assert!(row(owner.0, "workspace").is_readable_by(other));
        assert!(!row(owner.0, "private").is_shared());
        assert!(row(owner.0, "workspace").is_shared());
    }
}
