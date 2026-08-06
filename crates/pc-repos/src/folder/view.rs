//! 视图构建：从原始行计算 path / depth / itemCount。
//!
//! 对齐 Node `buildFolderViews`：DFS 解析 parent 链，检测环与悬空 parent。

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{FolderRow, FolderView};

/// 解析一组 folder 行，返回 id -> 视图的 map。
/// 遇到：
/// - 环（parent 链出现重复）→ 抛 `RepoError::Invalid`
/// - 悬空 parent（parent_id 指向不存在的行）→ 抛 `RepoError::Invalid`
pub fn build_folder_views(
    rows: &[FolderRow],
) -> crate::RepoResult<std::collections::HashMap<Uuid, FolderView>> {
    use std::collections::HashMap;

    let by_id: HashMap<Uuid, &FolderRow> = rows.iter().map(|row| (row.id, row)).collect();
    let mut views: HashMap<Uuid, FolderView> = HashMap::with_capacity(rows.len());
    let mut visiting = std::collections::HashSet::new();

    fn resolve(
        row: &FolderRow,
        by_id: &HashMap<Uuid, &FolderRow>,
        views: &mut HashMap<Uuid, FolderView>,
        visiting: &mut std::collections::HashSet<Uuid>,
    ) -> crate::RepoResult<FolderView> {
        if let Some(existing) = views.get(&row.id) {
            return Ok(existing.clone());
        }
        if !visiting.insert(row.id) {
            return Err(crate::RepoError::Invalid(
                "Folder hierarchy contains a cycle".into(),
            ));
        }
        let parent_view = match row.parent_id {
            Some(parent_id) => match by_id.get(&parent_id) {
                Some(parent_row) => Some(resolve(parent_row, by_id, views, visiting)?),
                None => {
                    return Err(crate::RepoError::Invalid(
                        "Folder hierarchy contains an invalid parent".into(),
                    ));
                }
            },
            None => None,
        };
        visiting.remove(&row.id);
        let path = match &parent_view {
            Some(parent) => format!("{}/{}", parent.path, row.slug),
            None => row.slug.clone(),
        };
        let depth = parent_view.as_ref().map(|p| p.depth + 1).unwrap_or(1);
        let view = FolderView {
            id: row.id,
            company_id: row.company_id,
            kind: row.kind.clone(),
            parent_id: row.parent_id,
            name: row.name.clone(),
            slug: row.slug.clone(),
            system_key: row.system_key.clone(),
            color: row.color.clone(),
            position: row.position,
            path,
            depth,
            created_at: row.created_at,
            updated_at: row.updated_at,
            item_count: 0,
        };
        views.insert(row.id, view.clone());
        Ok(view)
    }

    for row in rows {
        resolve(row, &by_id, &mut views, &mut visiting)?;
    }
    Ok(views)
}
