//! 业务服务层 — 封装 pc-repos `issue_visibility` 谓词 + 纯函数 classifier，提供：
//!
//! - `classify`：单 issue 分类（带 hook）
//! - `classify_batch`：批量分类（带 hook）
//! - `filter_visible`：过滤可见 issue
//! - `filter_with_config`：按 filter config 过滤
//! - `stats`：批量统计
//! - SQL 谓词生成（`visibility_condition` / `visibility_sql` / `and_visible` / `or_visible`）

use std::sync::Arc;

use pc_repos::issue::IssueRow;

use crate::classifier;
use crate::hook::IssueVisibilityHook;
use crate::types::{
    IssueVisibilityClassification, IssueVisibilityError, IssueVisibilityResult,
    VisibilityFilterConfig, VisibilityStats,
};

/// 业务 service。
#[derive(Clone)]
pub struct IssueVisibilityService {
    hook: Arc<dyn IssueVisibilityHook>,
}

impl std::fmt::Debug for IssueVisibilityService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssueVisibilityService").finish()
    }
}

impl IssueVisibilityService {
    pub fn new() -> Self {
        Self {
            hook: Arc::new(crate::hook::NoopIssueVisibilityHook),
        }
    }

    pub fn with_hook(mut self, hook: Arc<dyn IssueVisibilityHook>) -> Self {
        self.hook = hook;
        self
    }

    pub fn hook(&self) -> Arc<dyn IssueVisibilityHook> {
        Arc::clone(&self.hook)
    }

    /// 分类单个 issue（带 hook）。
    pub async fn classify(
        &self,
        row: &IssueRow,
    ) -> IssueVisibilityResult<IssueVisibilityClassification> {
        self.hook
            .before_classify(row)
            .await
            .map_err(IssueVisibilityError::Validation)?;
        let c = classifier::classify(row);
        self.hook.after_classify(row, &c).await;
        Ok(c)
    }

    /// 批量分类（每个 issue 都触发 hook）。
    pub async fn classify_batch(
        &self,
        rows: &[&IssueRow],
    ) -> IssueVisibilityResult<Vec<IssueVisibilityClassification>> {
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let c = self.classify(*row).await?;
            out.push(c);
        }
        Ok(out)
    }

    /// 过滤可见 issue（不触发 hook，与 Node `visibleIssueCondition` 1:1 对齐）。
    pub fn filter_visible<'a>(&self, rows: &'a [IssueRow]) -> Vec<&'a IssueRow> {
        classifier::filter_visible(rows)
    }

    /// 按 config 过滤（带 hook）。
    pub async fn filter_with_config<'a>(
        &self,
        rows: &'a [IssueRow],
        config: &VisibilityFilterConfig,
    ) -> IssueVisibilityResult<Vec<&'a IssueRow>> {
        self.hook
            .before_filter(config)
            .await
            .map_err(IssueVisibilityError::Validation)?;
        let accepted = classifier::filter_with_config(rows, config);
        let rejected = rows.len() - accepted.len();
        self.hook.after_filter(config, accepted.len(), rejected).await;
        Ok(accepted)
    }

    /// 计算 visibility 统计。
    pub fn stats(&self, rows: &[IssueRow]) -> VisibilityStats {
        classifier::stats(rows)
    }

    /// 同步分类（不触发 hook） — 适用于内部循环场景。
    pub fn classify_sync(&self, row: &IssueRow) -> IssueVisibilityClassification {
        classifier::classify(row)
    }
}

impl Default for IssueVisibilityService {
    fn default() -> Self {
        Self::new()
    }
}
