//! `PropertyRepo` —— `issue_property` 自定义属性**定义目录** + `issue.properties` JSONB
//! 值的桥接（M2-E / LUM-1370）。
//!
//! 对应上游 `server/internal/handler/property.go`（定义 CRUD）+
//! `server/internal/issueproperty/value.go`（值与定义的类型契约）+
//! `server/pkg/db/queries/issue_property.sql`。
//!
//! 两段式模型（上游 `191_issue_properties.up.sql`）：
//! 1. `issue_property` —— workspace 级定义（列与上游逐字一致：`name` / `type` /
//!    `description` / `config` JSONB / `position` / `archived_at` / `icon`）；
//! 2. `issue.properties` JSONB —— issue 上的值袋，**键是定义 UUID**（定义改名不需要迁移值）。
//!
//! 关键语义（照上游）：
//! - **权限**：定义写操作 = 人类 owner/admin（agent actor 一律 403），值写 = 任何成员 / agent；
//! - **只归档不删除**：`archived_at`；归档定义拒绝新值但旧值仍可解析；
//! - **活跃定义上限 20**：`create` 与「取消归档」在同一把 workspace advisory lock
//!   （`props:<ws>`）下做 read-then-write，避免 TOCTOU（上游 F5）；
//! - **值写入与定义改动串行**：值校验 + 写入同一把 `prop:<id>` 锁（上游 F1）
//!   ——本仓值写入在 `IssueRepo::set_property`，路由在写前调用
//!   [`PropertyRepo::definition_for_value`] 取定义（本仓无独立事务包装，见
//!   `docs/59-M2-E-LABEL-PROPERTY.md` §4 的偏差登记）；
//! - **`type` 不可变**（改类型会静默作废已有值）；
//! - **删除仍被引用的选项 → 409**，消息列出 `"选项名" (N issues)`（上游
//!   `describeOptionsInUse`）；
//! - 唯一性：`idx_issue_property_ws_name` 对 `(workspace_id, LOWER(name))` 唯一 ⇒
//!   重名 `23505` → 409。

use chrono::{DateTime, Utc};
use mc_core::Id;
use mc_db::Db;
use serde_json::Value as JsonValue;
use sqlx::{FromRow, Postgres, Transaction};
use uuid::Uuid;

use crate::workspace::map_sqlx_err;
use crate::{RepoError, RepoWithDb, Result};

/// 每个 workspace 的活跃定义上限（上游 `maxActivePropertiesPerWorkspace`）。
pub const MAX_ACTIVE_PROPERTIES: usize = 20;
/// `select` / `multi_select` 的选项数上限（上游 `maxPropertySelectOptions`）。
pub const MAX_SELECT_OPTIONS: usize = 50;
/// 定义名长度上限（上游 `maxPropertyNameLen`）。
pub const MAX_NAME_LEN: usize = 32;
/// 图标 key 长度上限（上游 `maxPropertyIconLen`）。
pub const MAX_ICON_LEN: usize = 32;
/// 描述长度上限（上游 `maxPropertyDescriptionLen`）。
pub const MAX_DESCRIPTION_LEN: usize = 500;
/// text 值长度上限（上游 `maxTextValueLen`）。
pub const MAX_TEXT_VALUE_LEN: usize = 2000;
/// url 值长度上限（上游 `maxURLValueLen`，按字节）。
pub const MAX_URL_VALUE_LEN: usize = 2048;
/// `multi_actor` 值条数上限（上游 `MaxActorValues`）。
pub const MAX_ACTOR_VALUES: usize = 20;
/// 值袋大小上限（上游 `issue_properties_size_limit` = 16KB）。
pub const MAX_PROPERTIES_BAG_BYTES: usize = 16 * 1024;

/// 合法属性类型（上游 `validPropertyTypes`，顺序也一致——错误消息会打印这个列表）。
pub const PROPERTY_TYPES: [&str; 9] = [
    "text",
    "number",
    "select",
    "multi_select",
    "date",
    "checkbox",
    "url",
    "actor",
    "multi_actor",
];

/// 合法图标 key（上游 `validPropertyIcons`；客户端把它映射成 Lucide 字形）。
pub const PROPERTY_ICONS: [&str; 36] = [
    "circle-dot",
    "signal-high",
    "user-round",
    "folder-kanban",
    "calendar-days",
    "tag",
    "milestone",
    "flag",
    "bookmark",
    "star",
    "target",
    "shield",
    "bug",
    "zap",
    "rocket",
    "sparkles",
    "lightbulb",
    "globe-2",
    "link",
    "hash",
    "list-checks",
    "circle-check",
    "clock-3",
    "briefcase-business",
    "layers-3",
    "gauge",
    "database",
    "code-2",
    "palette",
    "megaphone",
    "map-pin",
    "package",
    "wrench",
    "heart",
    "circle-alert",
    "lock-keyhole",
];

/// 保留名（上游 `reservedPropertyNames`）：与内置 issue 字段同名会被拒（按规范化形态比较）。
pub const RESERVED_NAMES: [&str; 17] = [
    "status",
    "priority",
    "assignee",
    "project",
    "parent",
    "stage",
    "label",
    "labels",
    "start_date",
    "due_date",
    "title",
    "description",
    "creator",
    "created_at",
    "updated_at",
    "metadata",
    "properties",
];

/// 唯一的 actor kind（上游 `actorKinds`；只有 member，加新 kind 时要在路由层补可见性门）。
pub const ACTOR_KINDS: [&str; 1] = ["member"];

const PROPERTY_COLUMNS: &str = "id, workspace_id, name, type, description, icon, config, \
                                position, archived_at, created_at, updated_at";

// ---------------------------------------------------------------------------
// 行 / 输入类型
// ---------------------------------------------------------------------------

/// `issue_property` 行。
#[derive(Debug, Clone, FromRow)]
pub struct PropertyRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    /// 列名 `type` 是关键字 ⇒ 字段改名。
    #[sqlx(rename = "type")]
    pub property_type: String,
    pub description: String,
    pub icon: String,
    /// 规范化后的配置（非 select 类型恒为 `{}`）。
    pub config: JsonValue,
    pub position: f64,
    pub archived_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl PropertyRow {
    /// 主键。
    pub fn id(&self) -> Id {
        Id::from(self.id)
    }

    /// 所属 workspace。
    pub fn workspace_id(&self) -> Id {
        Id::from(self.workspace_id)
    }

    /// 是否已归档（归档定义拒绝新值）。
    pub fn is_archived(&self) -> bool {
        self.archived_at.is_some()
    }

    /// 该类型的定义是否带选项。
    pub fn has_options(&self) -> bool {
        type_has_options(&self.property_type)
    }
}

/// `ListIssueProperties` 行：定义行 + `usage_count`（本 workspace 携带该 key 的 issue 数）。
#[derive(Debug, Clone, FromRow)]
pub struct PropertyListRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub name: String,
    #[sqlx(rename = "type")]
    pub property_type: String,
    pub description: String,
    pub icon: String,
    pub config: JsonValue,
    pub position: f64,
    pub archived_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub usage_count: i64,
}

/// 新建定义的输入（`config` 已由路由层规范化）。
#[derive(Debug, Clone)]
pub struct NewProperty {
    pub name: String,
    pub property_type: String,
    pub description: String,
    pub icon: String,
    pub config: JsonValue,
}

/// 定义更新补丁：`None` = 不动；`config = Some(...)` 已规范化。
#[derive(Debug, Clone, Default)]
pub struct PropertyUpdate {
    pub name: Option<String>,
    pub description: Option<String>,
    pub icon: Option<String>,
    pub config: Option<JsonValue>,
    /// 三态：`None` 不动、`Some(true)` 归档、`Some(false)` 取消归档（上游 `archived_set`）。
    pub archived: Option<bool>,
}

/// property 面的领域错误。路由层**逐条**映射到 HTTP 状态码（上游同位置的状态码写在注释里）。
#[derive(Debug, thiserror::Error)]
pub enum PropertyError {
    /// 传输层错误（`NotFound` → 404、`Conflict` → 409、其它 → 500）。
    #[error(transparent)]
    Repo(#[from] RepoError),
    /// 活跃定义数到顶 → 400（上游 `CreateProperty` / 取消归档）。
    #[error("a workspace cannot have more than {0} active properties; archive unused ones first")]
    ActiveCap(usize),
    /// 归档定义不接受新值 → 400（上游 `SetIssueProperty`）。
    #[error("property {0:?} is archived and cannot receive new values")]
    Archived(String),
    /// 值/参数校验失败 → 400（消息逐字对齐上游 `issueproperty.ValidateValue`）。
    #[error("{0}")]
    Invalid(String),
    /// 删除仍被引用的选项 → 409。
    #[error("{0}")]
    OptionsInUse(String),
}

// ---------------------------------------------------------------------------
// 纯校验（定义侧）：搬到同目录的 `validation.rs`（R7 单文件 800 行上限），
// 这里按原路径重新导出，路由层 / 测试的引用路径不变。
// ---------------------------------------------------------------------------

mod validation;

pub use self::validation::*;

// ---------------------------------------------------------------------------
// Repo
// ---------------------------------------------------------------------------

/// `PropertyRepo`。
#[derive(Clone)]
pub struct PropertyRepo {
    db: Db,
}

impl PropertyRepo {
    /// 构造。
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// 目录列表（`ORDER BY position, LOWER(name)`，带 `usage_count`）。
    pub async fn list(
        &self,
        workspace_id: Id,
        include_archived: bool,
    ) -> Result<Vec<PropertyListRow>> {
        let sql = "SELECT p.id, p.workspace_id, p.name, p.type, p.description, p.icon, p.config, \
                          p.position, p.archived_at, p.created_at, p.updated_at, \
                          (SELECT COUNT(*) FROM issue i \
                            WHERE i.workspace_id = p.workspace_id \
                              AND jsonb_exists(i.properties, p.id::text))::bigint AS usage_count \
                   FROM issue_property p \
                   WHERE p.workspace_id = $1 AND ($2::bool OR p.archived_at IS NULL) \
                   ORDER BY p.position ASC, LOWER(p.name) ASC";
        sqlx::query_as::<_, PropertyListRow>(sql)
            .bind(workspace_id.0)
            .bind(include_archived)
            .fetch_all(self.db.pool())
            .await
            .map_err(map_sqlx_err)
    }

    /// 单条定义（未命中 → [`RepoError::NotFound`] ⇒ 路由 404 `"property not found"`）。
    pub async fn get(&self, workspace_id: Id, property_id: Id) -> Result<PropertyRow> {
        sqlx::query_as::<_, PropertyRow>(&format!(
            "SELECT {PROPERTY_COLUMNS} FROM issue_property WHERE id = $1 AND workspace_id = $2"
        ))
        .bind(property_id.0)
        .bind(workspace_id.0)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 活跃定义数。
    pub async fn active_count(&self, workspace_id: Id) -> Result<i64> {
        sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM issue_property \
             WHERE workspace_id = $1 AND archived_at IS NULL",
        )
        .bind(workspace_id.0)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)
    }

    /// 新建定义：活跃数上限 + `position = MAX(position) + 1` 在同一把
    /// `props:<ws>` advisory lock 下完成（上游 F5；两个并发 create 不会都通过 cap）。
    pub async fn create(
        &self,
        workspace_id: Id,
        input: &NewProperty,
    ) -> std::result::Result<PropertyRow, PropertyError> {
        let mut tx = self.db.pool().begin().await.map_err(map_sqlx_err)?;
        lock_workspace(&mut tx, workspace_id).await?;
        let active: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)::bigint FROM issue_property \
             WHERE workspace_id = $1 AND archived_at IS NULL",
        )
        .bind(workspace_id.0)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;
        if active >= i64::try_from(MAX_ACTIVE_PROPERTIES).unwrap_or(i64::MAX) {
            return Err(PropertyError::ActiveCap(MAX_ACTIVE_PROPERTIES));
        }
        let row = sqlx::query_as::<_, PropertyRow>(
            "INSERT INTO issue_property (workspace_id, name, type, description, icon, config, position) \
             SELECT $1::uuid, $2::text, $3::text, $4::text, $5::text, $6::jsonb, \
                    COALESCE((SELECT MAX(position) FROM issue_property WHERE workspace_id = $1::uuid), 0) + 1 \
             RETURNING id, workspace_id, name, type, description, icon, config, position, \
                       archived_at, created_at, updated_at",
        )
        .bind(workspace_id.0)
        .bind(&input.name)
        .bind(&input.property_type)
        .bind(&input.description)
        .bind(&input.icon)
        .bind(&input.config)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;
        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(row)
    }

    /// 更新定义：读 → 校验 → 选项使用普查 → 写，全部在 `props:<ws>` + `prop:<id>` 锁下
    /// （上游 F1/F5）。类型不可变；归档三态；选项被删且仍被引用 → 409。
    pub async fn update(
        &self,
        workspace_id: Id,
        property_id: Id,
        patch: &PropertyUpdate,
    ) -> std::result::Result<PropertyRow, PropertyError> {
        let mut tx = self.db.pool().begin().await.map_err(map_sqlx_err)?;
        lock_workspace(&mut tx, workspace_id).await?;
        lock_property(&mut tx, property_id).await?;

        let existing = sqlx::query_as::<_, PropertyRow>(&format!(
            "SELECT {PROPERTY_COLUMNS} FROM issue_property WHERE id = $1 AND workspace_id = $2"
        ))
        .bind(property_id.0)
        .bind(workspace_id.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_sqlx_err)?
        .ok_or(RepoError::NotFound)?;

        if let Some(config) = &patch.config {
            let removed = removed_option_ids(&existing.config, config);
            if !removed.is_empty() {
                let rows = count_issues_using_options(&mut tx, workspace_id, property_id, &removed)
                    .await?;
                if !rows.is_empty() {
                    return Err(PropertyError::OptionsInUse(describe_options_in_use(
                        &existing.config,
                        &rows,
                    )));
                }
            }
        }
        if patch.archived == Some(false) && existing.is_archived() {
            let active: i64 = sqlx::query_scalar(
                "SELECT COUNT(*)::bigint FROM issue_property \
                 WHERE workspace_id = $1 AND archived_at IS NULL",
            )
            .bind(workspace_id.0)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_sqlx_err)?;
            if active >= i64::try_from(MAX_ACTIVE_PROPERTIES).unwrap_or(i64::MAX) {
                return Err(PropertyError::ActiveCap(MAX_ACTIVE_PROPERTIES));
            }
        }

        let archived_flag = patch.archived;
        let archived_at: Option<DateTime<Utc>> = match archived_flag {
            Some(true) => Some(Utc::now()),
            Some(false) => None,
            None => existing.archived_at,
        };
        let row = sqlx::query_as::<_, PropertyRow>(
            "UPDATE issue_property SET \
                 name = COALESCE($3, name), \
                 description = COALESCE($4, description), \
                 icon = COALESCE($5, icon), \
                 config = COALESCE($6::jsonb, config), \
                 archived_at = CASE WHEN $7::bool THEN $8::timestamptz ELSE archived_at END, \
                 updated_at = now() \
             WHERE id = $1 AND workspace_id = $2 \
             RETURNING id, workspace_id, name, type, description, icon, config, position, \
                       archived_at, created_at, updated_at",
        )
        .bind(property_id.0)
        .bind(workspace_id.0)
        .bind(patch.name.as_deref())
        .bind(patch.description.as_deref())
        .bind(patch.icon.as_deref())
        .bind(patch.config.as_ref())
        .bind(patch.archived.is_some())
        .bind(archived_at)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_sqlx_err)?;
        tx.commit().await.map_err(map_sqlx_err)?;
        Ok(row)
    }

    /// 值面桥接：取定义 + 归档判定（上游 `SetIssueProperty` 的 `GetIssueProperty` +
    /// `ArchivedAt.Valid`）。
    ///
    /// - 未命中 → `PropertyError::Repo(NotFound)` ⇒ 路由 404 `"property not found"`；
    /// - 已归档 → [`PropertyError::Archived`] ⇒ 路由 400。
    pub async fn definition_for_value(
        &self,
        workspace_id: Id,
        property_id: Id,
    ) -> std::result::Result<PropertyRow, PropertyError> {
        let row = self.get(workspace_id, property_id).await?;
        if row.is_archived() {
            return Err(PropertyError::Archived(row.name));
        }
        Ok(row)
    }

    /// actor 值解析：每个引用必须指向本 workspace 的成员（上游 `resolveActorRefs`）。
    /// 引用方是「本仓唯一的 member kind」，故不需要可见性门（上游注明加新 kind 时要补）。
    pub async fn resolve_actor_refs(
        &self,
        workspace_id: Id,
        refs: &[String],
    ) -> std::result::Result<(), PropertyError> {
        for reference in refs {
            let Some((_, id)) = reference.split_once(':') else {
                return Err(PropertyError::Invalid(format!(
                    "actor id in {reference:?} must be a UUID"
                )));
            };
            let Ok(user_id) = Uuid::parse_str(id) else {
                return Err(PropertyError::Invalid(format!(
                    "actor id in {reference:?} must be a UUID"
                )));
            };
            let found: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM member WHERE workspace_id = $1 AND user_id = $2)",
            )
            .bind(workspace_id.0)
            .bind(user_id)
            .fetch_one(self.db.pool())
            .await
            .map_err(map_sqlx_err)?;
            if !found {
                return Err(PropertyError::Invalid(format!(
                    "{reference:?} does not refer to a member of this workspace"
                )));
            }
        }
        Ok(())
    }

    /// 定义是否存在（值面 DELETE 用：归档与否都允许清值，但必须同 workspace）。
    pub async fn definition_exists(&self, workspace_id: Id, property_id: Id) -> Result<bool> {
        let found: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM issue_property WHERE id = $1 AND workspace_id = $2)",
        )
        .bind(property_id.0)
        .bind(workspace_id.0)
        .fetch_one(self.db.pool())
        .await
        .map_err(map_sqlx_err)?;
        Ok(found)
    }
}

impl RepoWithDb for PropertyRepo {
    fn db(&self) -> &Db {
        &self.db
    }
}

/// 定义改动与值写入共用同一把锁键（`hashtextextended`；锁随事务释放）。
async fn lock_workspace(tx: &mut Transaction<'_, Postgres>, workspace_id: Id) -> Result<()> {
    advisory_lock(tx, &format!("props:{}", workspace_id.0)).await
}

/// 单个定义的锁键 `prop:<uuid>`（值写入与配置普查串行）。
async fn lock_property(tx: &mut Transaction<'_, Postgres>, property_id: Id) -> Result<()> {
    advisory_lock(tx, &format!("prop:{}", property_id.0)).await
}

async fn advisory_lock(tx: &mut Transaction<'_, Postgres>, key: &str) -> Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(key)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlx_err)?;
    Ok(())
}

/// 被移除的选项 id（上游 `removedOptionIDs`）。
fn removed_option_ids(existing: &JsonValue, next: &JsonValue) -> Vec<String> {
    let next = parse_config(next);
    parse_config(existing)
        .options
        .iter()
        .filter(|opt| !next.options.iter().any(|kept| kept.id == opt.id))
        .map(|opt| opt.id.clone())
        .collect()
}

/// 选项使用普查（上游 `CountIssuesUsingPropertyOptions`；`?` 换成等价的 `jsonb_exists`）。
async fn count_issues_using_options(
    tx: &mut Transaction<'_, Postgres>,
    workspace_id: Id,
    property_id: Id,
    option_ids: &[String],
) -> Result<Vec<(String, i64)>> {
    sqlx::query_as::<_, (String, i64)>(
        "SELECT opt::text AS option_id, COUNT(i.id)::bigint AS usage_count \
         FROM unnest($1::text[]) AS opt \
         LEFT JOIN issue i ON i.workspace_id = $2::uuid \
              AND jsonb_exists(i.properties -> $3::text, opt) \
         GROUP BY opt HAVING COUNT(i.id) > 0",
    )
    .bind(option_ids)
    .bind(workspace_id.0)
    .bind(property_id.0.to_string())
    .fetch_all(&mut **tx)
    .await
    .map_err(map_sqlx_err)
}

/// 409 消息（上游 `describeOptionsInUse`）。
fn describe_options_in_use(existing: &JsonValue, rows: &[(String, i64)]) -> String {
    let config = parse_config(existing);
    let mut parts: Vec<String> = rows
        .iter()
        .map(|(option_id, usage)| {
            let name = config
                .options
                .iter()
                .find(|opt| &opt.id == option_id)
                .map_or_else(|| option_id.clone(), |opt| opt.name.clone());
            format!("{name:?} ({usage} issues)")
        })
        .collect();
    parts.sort();
    format!(
        "cannot remove options still in use: {}; clear or change those values first",
        parts.join(", ")
    )
}

#[cfg(test)]
mod db_tests;
#[cfg(test)]
mod tests;
