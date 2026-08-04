//! folder counts：list_with_counts（含 item counts per folder + unfiled）。
//!
//! 对齐 Node `list`：返回 `{ kind, folders, allCount, unfiledCount }`，
//! 其中每个 folder 携带 item_count。

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::folder::view::build_folder_views;
use crate::folder::{FolderKind, FolderRow, FolderView};
use crate::{Db, RepoError, RepoResult};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderItemCount {
    pub folder_id: Option<Uuid>,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderListResult {
    pub kind: FolderKind,
    pub folders: Vec<FolderView>,
    pub all_count: i64,
    pub unfiled_count: i64,
}

pub struct CountsQuery<'a> {
    pub db: &'a Db,
}

impl<'a> CountsQuery<'a> {
    pub fn new(db: &'a Db) -> Self {
        Self { db }
    }

    /// 对齐 Node `list`：返回指定 kind 的 folders + counts。
    pub async fn list_with_counts(
        &self,
        company_id: Uuid,
        kind: FolderKind,
    ) -> RepoResult<FolderListResult> {
        let rows = self.fetch_rows(company_id, kind).await?;
        let counts = self.fetch_counts(company_id, kind).await?;
        let views = build_folder_views(&rows)?;
        let mut count_by_folder: std::collections::HashMap<Option<Uuid>, i64> =
            std::collections::HashMap::new();
        for c in counts {
            count_by_folder.insert(c.folder_id, c.count);
        }
        let mut total: i64 = 0;
        let mut folders: Vec<FolderView> = Vec::with_capacity(rows.len());
        for row in &rows {
            let mut view = views
                .get(&row.id)
                .cloned()
                .ok_or_else(|| RepoError::Invalid("missing view for row".into()))?;
            let count = *count_by_folder.get(&Some(row.id)).unwrap_or(&0);
            view.item_count = count;
            total += count;
            folders.push(view);
        }
        let unfiled = *count_by_folder.get(&None).unwrap_or(&0);
        Ok(FolderListResult {
            kind,
            folders,
            all_count: total + unfiled,
            unfiled_count: unfiled,
        })
    }

    async fn fetch_rows(&self, company_id: Uuid, kind: FolderKind) -> RepoResult<Vec<FolderRow>> {
        crate::folder::FolderRepo::new(self.db)
            .list_by_kind(company_id, kind)
            .await
    }

    async fn fetch_counts(
        &self,
        company_id: Uuid,
        kind: FolderKind,
    ) -> RepoResult<Vec<FolderItemCount>> {
        let (table, folder_col) = match kind {
            FolderKind::Routine => ("routines", "folder_id"),
            FolderKind::Skill => ("company_skills", "folder_id"),
        };
        let sql = format!(
            "SELECT {folder_col} AS folder_id, COUNT(*)::bigint AS count FROM {table} WHERE company_id=$1 GROUP BY {folder_col}"
        );
        let rows: Vec<(Option<Uuid>, i64)> = sqlx::query_as(&sql)
            .bind(company_id)
            .fetch_all(self.db.pool())
            .await?;
        Ok(rows
            .into_iter()
            .map(|(folder_id, count)| FolderItemCount { folder_id, count })
            .collect())
    }
}
