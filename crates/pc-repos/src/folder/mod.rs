//! `folders` 域 — 嵌套文件夹容器，支持 routine / skill 两种分类。
//!
//! 设计：
//! - 三层唯一约束：(company, kind, NULL parent, slug) / (company, kind, parent, slug) /
//!   (company, kind, system_key)
//! - 重组树：移动到新父（update parent_id）、批量 re-position（drag/drop）
//! - 软删除通过 `archived_at`（外键 ON DELETE RESTRICT，需要先 archive）
//!
//! 目录结构（按 docs/08-RUST-MODULAR-ARCHITECTURE.md 拆分职责）：
//! - `slug` — slug 规范化等纯函数
//! - `view` — 视图构建（path / depth）
//! - `crud` — create / patch / delete / get / list
//! - `hierarchy` — 循环检测、descendants、reorder、parent 校验
//! - `counts` — list_with_counts + item counts
//! - `personal` — bundled / my / projects 容器与 ensureMyFolder
//! - `movement` — move_item（routines / skills 跨文件夹移动）
//! - `tests` — 纯规则单测

use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use pc_core::Timestamp;

use crate::Db;

mod counts;
mod crud;
mod hierarchy;
mod movement;
mod personal;
pub mod slug;
mod view;

#[cfg(test)]
mod tests;

pub use counts::{CountsQuery, FolderItemCount, FolderListResult};
pub use movement::{MoveFolderItem, MoveFolderItemKind, MoveFolderItemResult};
// FolderView is defined above as a top-level pub struct

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderKind {
    Routine,
    Skill,
}

impl FolderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Routine => "routine",
            Self::Skill => "skill",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "routine" => Some(Self::Routine),
            "skill" => Some(Self::Skill),
            _ => None,
        }
    }
}

pub(crate) const COLS: &str = "id, company_id, kind, parent_id, name, slug, system_key, color,      position, created_at, updated_at";

#[derive(Debug, Clone, FromRow, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderRow {
    pub id: Uuid,
    pub company_id: Uuid,
    pub kind: String,
    pub parent_id: Option<Uuid>,
    pub name: String,
    pub slug: String,
    pub system_key: Option<String>,
    pub color: Option<String>,
    pub position: i32,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewFolder {
    pub company_id: Uuid,
    pub kind: FolderKind,
    pub parent_id: Option<Uuid>,
    pub name: String,
    pub slug: String,
    pub system_key: Option<String>,
    pub color: Option<String>,
    pub position: i32,
}

/// folder 仓储。所有方法都接受 `&'a Db` 引用，避免持有连接。
pub struct FolderRepo<'a> {
    pub db: &'a Db,
}

/// 用于 list API 输出的视图，包含 path / depth / itemCount。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderView {
    pub id: Uuid,
    pub company_id: Uuid,
    pub kind: String,
    pub parent_id: Option<Uuid>,
    pub name: String,
    pub slug: String,
    pub system_key: Option<String>,
    pub color: Option<String>,
    pub position: i32,
    pub path: String,
    pub depth: i32,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub item_count: i64,
}

#[derive(Debug, Clone, Default)]
pub struct FolderPatch {
    pub name: Option<String>,
    pub slug: Option<String>,
    pub color: Option<String>,
    pub position: Option<i32>,
    pub parent_id: Option<Option<Uuid>>, // double Option: None = 不改, Some(None) = 设顶级, Some(Some(x)) = 设 x
}
