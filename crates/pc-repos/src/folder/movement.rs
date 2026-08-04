//! folder item movement：routines / skills 跨文件夹移动。
//!
//! 对齐 Node `moveItem`：校验目标 folder kind、bundled 只读、返回 { kind, itemId, folderId }。

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::folder::{FolderKind, FolderRepo};
use crate::{Db, RepoError, RepoResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MoveFolderItemKind {
    Routine,
    Skill,
}

impl MoveFolderItemKind {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveFolderItem {
    pub kind: MoveFolderItemKind,
    pub item_id: Uuid,
    /// None = 移到顶级（unfiled）
    pub folder_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveFolderItemResult {
    pub kind: MoveFolderItemKind,
    pub item_id: Uuid,
    pub folder_id: Option<Uuid>,
}

impl<'a> FolderRepo<'a> {
    /// 移动一个 routine 或 skill 到目标 folder。
    /// 校验：
    /// - folder_id 存在且 kind 匹配
    /// - bundled 容器只读
    /// - 当前所在 folder 是 bundled 时禁止移出
    pub async fn move_item(
        &self,
        company_id: Uuid,
        input: &MoveFolderItem,
    ) -> RepoResult<MoveFolderItemResult> {
        if let Some(target_id) = input.folder_id {
            let target = self
                .get(company_id, target_id)
                .await?
                .ok_or_else(|| RepoError::Invalid("Folder not found".into()))?;
            let expected_kind = match input.kind {
                MoveFolderItemKind::Routine => FolderKind::Routine,
                MoveFolderItemKind::Skill => FolderKind::Skill,
            };
            if target.kind != expected_kind.as_str() {
                return Err(RepoError::Invalid(
                    "Folder kind must match item kind".into(),
                ));
            }
            if self.is_bundled_folder(company_id, target.id).await? {
                return Err(RepoError::Invalid("Bundled folders are read-only".into()));
            }
        }
        match input.kind {
            MoveFolderItemKind::Routine => self.move_routine_item(company_id, input).await,
            MoveFolderItemKind::Skill => self.move_skill_item(company_id, input).await,
        }
    }

    async fn move_routine_item(
        &self,
        company_id: Uuid,
        input: &MoveFolderItem,
    ) -> RepoResult<MoveFolderItemResult> {
        let row: Option<(Uuid, Option<Uuid>)> = sqlx::query_as(
            "UPDATE routines SET folder_id=$1, updated_at=now() WHERE company_id=$2 AND id=$3 RETURNING id, folder_id",
        )
        .bind(input.folder_id)
        .bind(company_id)
        .bind(input.item_id)
        .fetch_optional(self.db.pool())
        .await?;
        let (id, folder_id) = row.ok_or_else(|| RepoError::Invalid("Routine not found".into()))?;
        Ok(MoveFolderItemResult {
            kind: MoveFolderItemKind::Routine,
            item_id: id,
            folder_id,
        })
    }

    async fn move_skill_item(
        &self,
        company_id: Uuid,
        input: &MoveFolderItem,
    ) -> RepoResult<MoveFolderItemResult> {
        // 先取出 skill 的当前 folder_id，若当前 folder 是 bundled 则禁止移出。
        let current: Option<(Option<Uuid>,)> = sqlx::query_as(
            "SELECT folder_id FROM company_skills WHERE company_id=$1 AND id=$2",
        )
        .bind(company_id)
        .bind(input.item_id)
        .fetch_optional(self.db.pool())
        .await?;
        let Some((current_folder,)) = current else {
            return Err(RepoError::Invalid("Skill not found".into()));
        };
        if let Some(current_id) = current_folder {
            if self.is_bundled_folder(company_id, current_id).await? {
                return Err(RepoError::Invalid("Bundled skills cannot be moved".into()));
            }
        }
        let row: Option<(Uuid, Option<Uuid>)> = sqlx::query_as(
            "UPDATE company_skills SET folder_id=$1, updated_at=now() WHERE company_id=$2 AND id=$3 RETURNING id, folder_id",
        )
        .bind(input.folder_id)
        .bind(company_id)
        .bind(input.item_id)
        .fetch_optional(self.db.pool())
        .await?;
        let (id, folder_id) = row.ok_or_else(|| RepoError::Invalid("Skill not found".into()))?;
        Ok(MoveFolderItemResult {
            kind: MoveFolderItemKind::Skill,
            item_id: id,
            folder_id,
        })
    }
}

#[allow(dead_code)]
pub fn repo(db: &Db) -> FolderRepo<'_> {
    FolderRepo::new(db)
}
