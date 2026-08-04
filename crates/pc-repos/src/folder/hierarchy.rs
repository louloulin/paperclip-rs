//! folder 层级规则：环检测、descendants 集合、reorder、parent 校验。
//!
//! 与 view.rs 不同：这里全部是「在原始行上」操作的纯函数或仓库方法，
//! 不构建 path / depth。

use std::collections::{HashMap, HashSet, VecDeque};

use uuid::Uuid;

use crate::folder::slug::RESERVED_CHILD_ROOT_SYSTEM_KEYS;
use crate::folder::{FolderKind, FolderRepo, FolderRow};
use crate::{RepoError, RepoResult};

/// 给定一组 folder 行 + 根 id，返回所有后代 id（含自身）。
/// 抛出 `Invalid` 当：
/// - root 不在 rows 中
/// - 行中存在环
pub fn descendant_ids_from_rows(rows: &[FolderRow], folder_id: Uuid) -> RepoResult<HashSet<Uuid>> {
    if !rows.iter().any(|row| row.id == folder_id) {
        return Err(RepoError::Invalid("folder not found in rows".into()));
    }
    let mut children: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for row in rows {
        if let Some(parent) = row.parent_id {
            children.entry(parent).or_default().push(row.id);
        }
    }
    let mut result = HashSet::new();
    let mut queue = VecDeque::new();
    result.insert(folder_id);
    queue.push_back(folder_id);
    while let Some(current) = queue.pop_front() {
        for child in children.get(&current).cloned().unwrap_or_default() {
            if result.contains(&child) {
                return Err(RepoError::Invalid(
                    "Folder hierarchy contains a cycle".into(),
                ));
            }
            result.insert(child);
            queue.push_back(child);
        }
    }
    Ok(result)
}

/// 验证 parent 是否允许作为 child 的新父。
/// 返回值：
/// - Ok(None) — parent_id 是 None
/// - Ok(Some(row)) — parent 存在且合法
/// - Err(Invalid) — parent 不存在 / kind 不匹配 / bundled 只读 / 保留 root 下
pub async fn validate_parent<'a>(
    repo: &FolderRepo<'a>,
    company_id: Uuid,
    kind: FolderKind,
    parent_id: Option<Uuid>,
) -> RepoResult<Option<FolderRow>> {
    let Some(parent_id) = parent_id else {
        return Ok(None);
    };
    let Some(parent) = repo.get(company_id, parent_id).await? else {
        return Err(RepoError::Invalid("Parent folder not found".into()));
    };
    if parent.kind != kind.as_str() {
        return Err(RepoError::Invalid(
            "Parent folder kind must match child kind".into(),
        ));
    }
    if parent.system_key.as_deref() == Some("bundled") {
        return Err(RepoError::Invalid("Bundled folders are read-only".into()));
    }
    if parent.parent_id.is_none() && parent.kind == FolderKind::Skill.as_str() {
        let sys = parent.system_key.as_deref().unwrap_or("");
        if RESERVED_CHILD_ROOT_SYSTEM_KEYS.contains(&sys) || RESERVED_CHILD_ROOT_SYSTEM_KEYS.contains(&parent.slug.as_str()) {
            return Err(RepoError::Invalid(
                "Reserved skill folders are system-managed".into(),
            ));
        }
    }
    Ok(Some(parent))
}

impl<'a> FolderRepo<'a> {
    /// 递归检查 ancestor = 后代 = id：把 id 设为 new_parent 的子是否成环。
    pub(super) async fn would_create_cycle(&self, id: Uuid, new_parent: Uuid) -> RepoResult<bool> {
        let mut cur: Option<Uuid> = Some(new_parent);
        for _ in 0..512 {
            match cur {
                None => return Ok(false),
                Some(p) if p == id => return Ok(true),
                Some(p) => {
                    let n: Option<Uuid> =
                        sqlx::query_scalar("SELECT parent_id FROM folders WHERE id=$1")
                            .bind(p)
                            .fetch_optional(self.db.pool())
                            .await?;
                    cur = n;
                }
            }
        }
        Ok(true) // 太深视为不安全
    }

    /// 批量 re-position（drag/drop）。
    pub async fn reorder_siblings(
        &self,
        company_id: Uuid,
        kind: FolderKind,
        parent_id: Option<Uuid>,
        ordered_ids: &[Uuid],
    ) -> RepoResult<()> {
        let mut tx = self.db.pool().begin().await?;
        for (i, fid) in ordered_ids.iter().enumerate() {
            sqlx::query(
                "UPDATE folders SET position=$1, parent_id=$2, updated_at=now() WHERE company_id=$3 AND kind=$4 AND id=$5",
            )
            .bind(i as i32)
            .bind(parent_id)
            .bind(company_id)
            .bind(kind.as_str())
            .bind(fid)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// 给定 (company, kind, parent) 的下一个可用 position。
    pub async fn next_position(
        &self,
        company_id: Uuid,
        kind: FolderKind,
        parent_id: Option<Uuid>,
    ) -> RepoResult<i32> {
        let max_pos: Option<i32> = sqlx::query_scalar(
            "SELECT COALESCE(MAX(position), -1) FROM folders WHERE company_id=$1 AND kind=$2 AND ($3::uuid IS NULL AND parent_id IS NULL OR parent_id = $3)",
        )
        .bind(company_id)
        .bind(kind.as_str())
        .bind(parent_id)
        .fetch_one(self.db.pool())
        .await?;
        Ok(max_pos.unwrap_or(-1) + 1)
    }

    /// 沿 parent 链向上回溯，检测是否到达 systemKey="bundled" 的祖先。
    pub async fn is_bundled_folder(&self, company_id: Uuid, folder_id: Uuid) -> RepoResult<bool> {
        let mut current = match self.get(company_id, folder_id).await? {
            Some(row) => row,
            None => return Ok(false),
        };
        let mut visited = HashSet::new();
        loop {
            if current.system_key.as_deref() == Some("bundled") {
                return Ok(true);
            }
            let Some(parent_id) = current.parent_id else {
                return Ok(false);
            };
            if !visited.insert(parent_id) {
                return Ok(false); // 防御性：环保护
            }
            match self.get(company_id, parent_id).await? {
                Some(parent) => current = parent,
                None => return Ok(false),
            }
        }
    }

    /// 检查 (company, kind, parent) 下是否存在同名 slug（exclude 自身）。
    pub async fn assert_no_slug_conflict(
        &self,
        company_id: Uuid,
        kind: FolderKind,
        parent_id: Option<Uuid>,
        slug: &str,
        exclude_folder_id: Option<Uuid>,
    ) -> RepoResult<()> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM folders WHERE company_id=$1 AND kind=$2 AND slug=$3 AND ($4::uuid IS NULL AND parent_id IS NULL OR parent_id = $4) LIMIT 1",
        )
        .bind(company_id)
        .bind(kind.as_str())
        .bind(slug)
        .bind(parent_id)
        .fetch_optional(self.db.pool())
        .await?;
        if let Some((id,)) = row {
            if Some(id) != exclude_folder_id {
                return Err(RepoError::Invalid(
                    "Folder slug already exists under this parent".into(),
                ));
            }
        }
        Ok(())
    }
}
