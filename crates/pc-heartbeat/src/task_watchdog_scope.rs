//! Task watchdog 变更作用域解析（原 `pc-task-watchdog-scope` 已下沉到 `pc-heartbeat`）
//!
//! 对应 Node `server/src/services/task-watchdog-scope.ts`。

use std::collections::HashSet;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ============================================================================
// Constants
// ============================================================================

/// Watchdog ancestry 最大深度（与 Node `MAX_WATCHDOG_SCOPE_ANCESTRY_DEPTH = 100` 1:1 对齐）。
pub const MAX_WATCHDOG_SCOPE_ANCESTRY_DEPTH: u32 = 100;

/// Task watchdog origin kind（与 Node `TASK_WATCHDOG_ORIGIN_KIND = "task_watchdog"` 1:1 对齐）。
pub const TASK_WATCHDOG_ORIGIN_KIND: &str = "task_watchdog";

// ============================================================================
// Types
// ============================================================================

/// Mutation scope 解析结果（与 Node `TaskWatchdogMutationScope` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskWatchdogMutationScope {
    /// 不是 agent / 没有 taskWatchdog 上下文
    None,
    /// 解析失败（run 不属于 agent、watchdog 不存在等）
    Invalid { detail: String },
    /// 解析成功
    Watchdog {
        watchdog_id: String,
        company_id: String,
        watched_issue_id: String,
        watchdog_issue_id: Option<String>,
        stop_fingerprint: Option<String>,
    },
}

/// Agent run actor 投影（与 Node `AgentRunActor` 1:1 对齐）。
#[derive(Debug, Clone, Default)]
pub struct AgentRunActor {
    pub actor_type: String,
    pub agent_id: Option<String>,
    pub company_id: Option<String>,
    pub run_id: Option<String>,
}

/// Issue scope target 投影（与 Node `IssueScopeTarget` 1:1 对齐）。
#[derive(Debug, Clone, Default)]
pub struct IssueScopeTarget {
    pub id: String,
    pub company_id: String,
    pub parent_id: Option<String>,
}

/// TaskWatchdog 上下文（从 run.contextSnapshot 中提取）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskWatchdogContext {
    pub watched_issue_id: Option<String>,
    pub stop_fingerprint: Option<String>,
}

/// Run projection（与 Node run 查询字段 1:1 对齐）。
#[derive(Debug, Clone, Default)]
pub struct RunProjection {
    pub id: String,
    pub company_id: Option<String>,
    pub agent_id: Option<String>,
    pub context_snapshot: Option<serde_json::Value>,
}

/// Watchdog projection（与 Node watchdog 查询字段 1:1 对齐）。
#[derive(Debug, Clone, Default)]
pub struct WatchdogProjection {
    pub id: String,
    pub company_id: Option<String>,
    pub issue_id: Option<String>,
    pub watchdog_agent_id: Option<String>,
    pub watchdog_issue_id: Option<String>,
    pub status: Option<String>,
}

/// Issue parent projection（与 Node issue 查询字段 1:1 对齐）。
#[derive(Debug, Clone, Default)]
pub struct IssueParentProjection {
    pub id: String,
    pub company_id: String,
    pub parent_id: Option<String>,
    pub origin_kind: Option<String>,
}

// ============================================================================
// Data source trait
// ============================================================================

/// 抽象 DB 查询（与 Node 端 heartbeat_runs / issue_watchdogs / issues 1:1 对齐）。
#[async_trait]
pub trait TaskWatchdogDataSource: Send + Sync {
    /// 按 run id 查 run
    async fn find_run(&self, run_id: &str) -> Option<RunProjection>;
    /// 按 (companyId, issueId, watchdogAgentId, status) 查 watchdog
    async fn find_watchdog(
        &self,
        company_id: &str,
        watched_issue_id: &str,
        watchdog_agent_id: &str,
    ) -> Option<WatchdogProjection>;
    /// 按 (id, companyId) 查 issue 父节点
    async fn find_issue_parent(
        &self,
        company_id: &str,
        issue_id: &str,
    ) -> Option<IssueParentProjection>;
}

// ============================================================================
// Pure helpers
// ============================================================================

/// `isPlainRecord` —— 与 Node 等价。
pub fn is_plain_record(value: &serde_json::Value) -> bool {
    value.is_object()
}

/// `readString` —— 从 unknown 读非空 trim 字符串。
pub fn read_string(value: &serde_json::Value) -> Option<String> {
    value.as_str().and_then(|s| {
        let t = s.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    })
}

/// `readString` over `Option<&Value>` —— Node 风格短路链。
pub fn read_string_opt(value: Option<&serde_json::Value>) -> Option<String> {
    value.and_then(read_string)
}

/// 从 run.contextSnapshot 读取 taskWatchdog 上下文（与 Node 1:1 对齐）。
///
/// 支持两种 context 形态：
/// 1. `{ taskWatchdog: { watchedIssueId, stopFingerprint } }`
/// 2. `{ watchedIssueId, stopFingerprint }`（顶层）
pub fn read_task_watchdog_context(
    context_snapshot: Option<&serde_json::Value>,
) -> Option<TaskWatchdogContext> {
    let context = context_snapshot.and_then(|v| if v.is_object() { Some(v) } else { None })?;
    let task_watchdog =
        context
            .get("taskWatchdog")
            .and_then(|v| if v.is_object() { Some(v) } else { None });
    if task_watchdog.is_none()
        && context.get("taskWatchdog") != Some(&serde_json::Value::Bool(true))
    {
        return None;
    }
    let empty = serde_json::Value::Object(serde_json::Map::new());
    let tw = task_watchdog.unwrap_or(&empty);
    Some(TaskWatchdogContext {
        watched_issue_id: read_string_opt(tw.get("watchedIssueId"))
            .or_else(|| read_string_opt(context.get("watchedIssueId"))),
        stop_fingerprint: read_string_opt(tw.get("stopFingerprint"))
            .or_else(|| read_string_opt(context.get("stopFingerprint"))),
    })
}

// ============================================================================
// Resolve scope
// ============================================================================

/// Resolve task watchdog mutation scope（与 Node `resolveTaskWatchdogMutationScope` 1:1 对齐）。
pub async fn resolve_task_watchdog_mutation_scope(
    data: &dyn TaskWatchdogDataSource,
    actor: &AgentRunActor,
) -> TaskWatchdogMutationScope {
    if actor.actor_type != "agent" {
        return TaskWatchdogMutationScope::None;
    }
    let agent_id = match actor.agent_id.as_deref().and_then(|s| {
        let t = s.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    }) {
        Some(s) => s,
        None => return TaskWatchdogMutationScope::None,
    };
    let run_id = match actor.run_id.as_deref().and_then(|s| {
        let t = s.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    }) {
        Some(s) => s,
        None => return TaskWatchdogMutationScope::None,
    };
    let actor_company_id = actor.company_id.clone();

    let run = match data.find_run(&run_id).await {
        Some(r) => r,
        None => return TaskWatchdogMutationScope::None,
    };
    let task_watchdog = match read_task_watchdog_context(run.context_snapshot.as_ref()) {
        Some(t) => t,
        None => return TaskWatchdogMutationScope::None,
    };
    if run.agent_id.as_deref() != Some(agent_id.as_str())
        || (actor_company_id.is_some() && run.company_id.as_deref() != actor_company_id.as_deref())
    {
        return TaskWatchdogMutationScope::Invalid {
            detail: "Task-watchdog run context does not belong to this agent.".to_string(),
        };
    }
    let company_id = match run.company_id.as_ref() {
        Some(c) => c.clone(),
        None => {
            return TaskWatchdogMutationScope::Invalid {
                detail: "Task-watchdog run is missing company id.".to_string(),
            }
        }
    };
    let watched_issue_id = match task_watchdog.watched_issue_id {
        Some(s) => s,
        None => {
            return TaskWatchdogMutationScope::Invalid {
                detail: "Task-watchdog run context is missing a persisted watched issue id."
                    .to_string(),
            }
        }
    };

    let watchdog = match data
        .find_watchdog(&company_id, &watched_issue_id, &agent_id)
        .await
    {
        Some(w) => w,
        None => {
            return TaskWatchdogMutationScope::Invalid {
                detail: "Task-watchdog run context is not backed by an active persisted watchdog."
                    .to_string(),
            }
        }
    };

    TaskWatchdogMutationScope::Watchdog {
        watchdog_id: watchdog.id,
        company_id: watchdog.company_id.unwrap_or(company_id),
        watched_issue_id: watchdog.issue_id.unwrap_or(watched_issue_id),
        watchdog_issue_id: watchdog.watchdog_issue_id,
        stop_fingerprint: task_watchdog.stop_fingerprint,
    }
}

// ============================================================================
// Subtree check
// ============================================================================

/// 检查 issue 是否在 watched issue 的子树内（与 Node 1:1 对齐）。
///
/// 行为：
/// - 从 `issue_id` 向上爬 parent，直到 `watched_issue_id` 为止
/// - 任何祖先是 `originKind = "task_watchdog"` → 拒绝（不跨 watchdog 边界）
/// - 检测循环（`seen`）→ 拒绝
/// - 深度超过 `MAX_WATCHDOG_SCOPE_ANCESTRY_DEPTH` → 拒绝
pub async fn issue_is_in_task_watchdog_subtree(
    data: &dyn TaskWatchdogDataSource,
    company_id: &str,
    issue_id: &str,
    watched_issue_id: &str,
) -> bool {
    let mut current_id: Option<String> = Some(issue_id.to_string());
    let mut seen: HashSet<String> = HashSet::new();

    for _ in 0..MAX_WATCHDOG_SCOPE_ANCESTRY_DEPTH {
        let cid = match current_id.take() {
            Some(c) => c,
            None => return false,
        };
        if seen.contains(&cid) {
            return false;
        }
        seen.insert(cid.clone());

        let parent = match data.find_issue_parent(company_id, &cid).await {
            Some(p) => p,
            None => return false,
        };
        if parent.origin_kind.as_deref() == Some(TASK_WATCHDOG_ORIGIN_KIND) {
            return false;
        }
        if cid == watched_issue_id {
            return true;
        }
        current_id = parent.parent_id;
    }
    false
}

// ============================================================================
// Check mutation allowed
// =====================================================================================

/// 检查 task watchdog mutation 是否被允许（与 Node `taskWatchdogScopeAllowsIssueMutation` 1:1 对齐）。
pub async fn task_watchdog_scope_allows_issue_mutation(
    data: &dyn TaskWatchdogDataSource,
    scope: TaskWatchdogMutationScope,
    issue: &IssueScopeTarget,
    opts: TaskWatchdogMutationOptions,
) -> TaskWatchdogMutationScope {
    let TaskWatchdogMutationScope::Watchdog {
        watchdog_id,
        company_id,
        watched_issue_id,
        watchdog_issue_id,
        stop_fingerprint,
    } = scope
    else {
        return scope;
    };

    if issue.company_id != company_id {
        return TaskWatchdogMutationScope::Invalid {
            detail: "Task-watchdog mutation target is outside the watchdog company.".to_string(),
        };
    }
    if opts.allow_watchdog_issue != Some(false) {
        if let Some(wid) = &watchdog_issue_id {
            if &issue.id == wid {
                return TaskWatchdogMutationScope::Watchdog {
                    watchdog_id,
                    company_id,
                    watched_issue_id,
                    watchdog_issue_id,
                    stop_fingerprint,
                };
            }
        }
    }
    if issue_is_in_task_watchdog_subtree(data, &company_id, &issue.id, &watched_issue_id).await {
        return TaskWatchdogMutationScope::Watchdog {
            watchdog_id,
            company_id,
            watched_issue_id,
            watchdog_issue_id,
            stop_fingerprint,
        };
    }
    TaskWatchdogMutationScope::Invalid {
        detail: "Task-watchdog runs can only mutate the watched issue subtree.".to_string(),
    }
}

/// Options for `task_watchdog_scope_allows_issue_mutation`。
#[derive(Debug, Clone, Default)]
pub struct TaskWatchdogMutationOptions {
    /// 默认 true；设为 false 禁止 mutate watchdog issue 自身
    pub allow_watchdog_issue: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    // ----- constants -----

    #[test]
    fn r718_constants_match_node() {
        assert_eq!(MAX_WATCHDOG_SCOPE_ANCESTRY_DEPTH, 100);
        assert_eq!(TASK_WATCHDOG_ORIGIN_KIND, "task_watchdog");
    }

    // ----- helpers -----

    #[test]
    fn r718_is_plain_record() {
        assert!(is_plain_record(&json!({})));
        assert!(is_plain_record(&json!({"a": 1})));
        assert!(!is_plain_record(&json!("x")));
        assert!(!is_plain_record(&json!(null)));
        assert!(!is_plain_record(&json!([1, 2])));
    }

    #[test]
    fn r718_read_string() {
        assert_eq!(read_string(&json!("x")).as_deref(), Some("x"));
        assert_eq!(read_string(&json!("  x  ")).as_deref(), Some("x"));
        assert_eq!(read_string(&json!("")), None);
        assert_eq!(read_string(&json!("   ")), None);
        assert_eq!(read_string(&json!(1)), None);
        assert_eq!(read_string(&json!(null)), None);
    }

    // ----- read_task_watchdog_context -----

    #[test]
    fn r718_read_context_nested() {
        let snap = json!({
            "taskWatchdog": {
                "watchedIssueId": "i-1",
                "stopFingerprint": "fp-1"
            }
        });
        let ctx = read_task_watchdog_context(Some(&snap)).unwrap();
        assert_eq!(ctx.watched_issue_id.as_deref(), Some("i-1"));
        assert_eq!(ctx.stop_fingerprint.as_deref(), Some("fp-1"));
    }

    #[test]
    fn r718_read_context_with_task_watchdog_true_flag() {
        // taskWatchdog: true 表示空对象，可 fallback 到顶层字段
        let snap = json!({
            "taskWatchdog": true,
            "watchedIssueId": "i-2",
            "stopFingerprint": "fp-2"
        });
        let ctx = read_task_watchdog_context(Some(&snap)).unwrap();
        assert_eq!(ctx.watched_issue_id.as_deref(), Some("i-2"));
        assert_eq!(ctx.stop_fingerprint.as_deref(), Some("fp-2"));
    }

    #[test]
    fn r718_read_context_nested_overrides_top_level() {
        // nested 优先
        let snap = json!({
            "watchedIssueId": "top",
            "taskWatchdog": {
                "watchedIssueId": "nested"
            }
        });
        let ctx = read_task_watchdog_context(Some(&snap)).unwrap();
        assert_eq!(ctx.watched_issue_id.as_deref(), Some("nested"));
    }

    #[test]
    fn r718_read_context_missing_returns_none() {
        let snap = json!({"other": "x"});
        assert!(read_task_watchdog_context(Some(&snap)).is_none());
    }

    #[test]
    fn r718_read_context_empty_string_falls_through() {
        let snap = json!({
            "taskWatchdog": {
                "watchedIssueId": "",
                "stopFingerprint": "  "
            },
            "watchedIssueId": "fallback"
        });
        let ctx = read_task_watchdog_context(Some(&snap)).unwrap();
        assert_eq!(ctx.watched_issue_id.as_deref(), Some("fallback"));
        assert_eq!(ctx.stop_fingerprint, None);
    }

    // ----- resolve scope (with fake data source) -----

    #[derive(Default, Clone)]
    struct FakeData {
        runs: Arc<Mutex<HashMap<String, RunProjection>>>,
        watchdogs: Arc<Mutex<Vec<WatchdogProjection>>>,
        issues: Arc<Mutex<HashMap<String, IssueParentProjection>>>,
    }

    #[async_trait]
    impl TaskWatchdogDataSource for FakeData {
        async fn find_run(&self, run_id: &str) -> Option<RunProjection> {
            self.runs.lock().await.get(run_id).cloned()
        }
        async fn find_watchdog(
            &self,
            company_id: &str,
            watched_issue_id: &str,
            watchdog_agent_id: &str,
        ) -> Option<WatchdogProjection> {
            self.watchdogs
                .lock()
                .await
                .iter()
                .find(|w| {
                    w.company_id.as_deref() == Some(company_id)
                        && w.issue_id.as_deref() == Some(watched_issue_id)
                        && w.watchdog_agent_id.as_deref() == Some(watchdog_agent_id)
                        && w.status.as_deref() == Some("active")
                })
                .cloned()
        }
        async fn find_issue_parent(
            &self,
            company_id: &str,
            issue_id: &str,
        ) -> Option<IssueParentProjection> {
            self.issues
                .lock()
                .await
                .get(issue_id)
                .filter(|i| i.company_id == company_id)
                .cloned()
        }
    }

    #[tokio::test]
    async fn r718_resolve_non_agent_returns_none() {
        let data = FakeData::default();
        let actor = AgentRunActor {
            actor_type: "user".into(),
            ..Default::default()
        };
        let scope = resolve_task_watchdog_mutation_scope(&data, &actor).await;
        assert_eq!(scope, TaskWatchdogMutationScope::None);
    }

    #[tokio::test]
    async fn r718_resolve_missing_run_id_returns_none() {
        let data = FakeData::default();
        let actor = AgentRunActor {
            actor_type: "agent".into(),
            agent_id: Some("a-1".into()),
            run_id: None,
            ..Default::default()
        };
        let scope = resolve_task_watchdog_mutation_scope(&data, &actor).await;
        assert_eq!(scope, TaskWatchdogMutationScope::None);
    }

    #[tokio::test]
    async fn r718_resolve_agent_mismatch_invalid() {
        let mut data = FakeData::default();
        data.runs.lock().await.insert(
            "r-1".into(),
            RunProjection {
                id: "r-1".into(),
                company_id: Some("co-1".into()),
                agent_id: Some("different-agent".into()),
                context_snapshot: Some(json!({
                    "taskWatchdog": {"watchedIssueId": "i-1"}
                })),
            },
        );
        let actor = AgentRunActor {
            actor_type: "agent".into(),
            agent_id: Some("a-1".into()),
            run_id: Some("r-1".into()),
            company_id: Some("co-1".into()),
        };
        let scope = resolve_task_watchdog_mutation_scope(&data, &actor).await;
        match scope {
            TaskWatchdogMutationScope::Invalid { detail } => {
                assert!(detail.contains("does not belong"));
            }
            _ => panic!("expected invalid"),
        }
    }

    #[tokio::test]
    async fn r718_resolve_missing_watched_issue_invalid() {
        let mut data = FakeData::default();
        data.runs.lock().await.insert(
            "r-1".into(),
            RunProjection {
                id: "r-1".into(),
                company_id: Some("co-1".into()),
                agent_id: Some("a-1".into()),
                context_snapshot: Some(json!({"taskWatchdog": {}})),
            },
        );
        let actor = AgentRunActor {
            actor_type: "agent".into(),
            agent_id: Some("a-1".into()),
            run_id: Some("r-1".into()),
            company_id: Some("co-1".into()),
        };
        let scope = resolve_task_watchdog_mutation_scope(&data, &actor).await;
        match scope {
            TaskWatchdogMutationScope::Invalid { detail } => {
                assert!(detail.contains("missing a persisted"));
            }
            _ => panic!("expected invalid"),
        }
    }

    #[tokio::test]
    async fn r718_resolve_watchdog_not_found_invalid() {
        let mut data = FakeData::default();
        data.runs.lock().await.insert(
            "r-1".into(),
            RunProjection {
                id: "r-1".into(),
                company_id: Some("co-1".into()),
                agent_id: Some("a-1".into()),
                context_snapshot: Some(json!({"taskWatchdog": {"watchedIssueId": "i-1"}})),
            },
        );
        // no watchdog registered
        let actor = AgentRunActor {
            actor_type: "agent".into(),
            agent_id: Some("a-1".into()),
            run_id: Some("r-1".into()),
            company_id: Some("co-1".into()),
        };
        let scope = resolve_task_watchdog_mutation_scope(&data, &actor).await;
        match scope {
            TaskWatchdogMutationScope::Invalid { detail } => {
                assert!(detail.contains("not backed by an active"));
            }
            _ => panic!("expected invalid"),
        }
    }

    #[tokio::test]
    async fn r718_resolve_success() {
        let mut data = FakeData::default();
        data.runs.lock().await.insert(
            "r-1".into(),
            RunProjection {
                id: "r-1".into(),
                company_id: Some("co-1".into()),
                agent_id: Some("a-1".into()),
                context_snapshot: Some(json!({
                    "taskWatchdog": {
                        "watchedIssueId": "i-1",
                        "stopFingerprint": "fp-1"
                    }
                })),
            },
        );
        data.watchdogs.lock().await.push(WatchdogProjection {
            id: "w-1".into(),
            company_id: Some("co-1".into()),
            issue_id: Some("i-1".into()),
            watchdog_agent_id: Some("a-1".into()),
            watchdog_issue_id: Some("i-2".into()),
            status: Some("active".into()),
        });
        let actor = AgentRunActor {
            actor_type: "agent".into(),
            agent_id: Some("a-1".into()),
            run_id: Some("r-1".into()),
            company_id: Some("co-1".into()),
        };
        let scope = resolve_task_watchdog_mutation_scope(&data, &actor).await;
        match scope {
            TaskWatchdogMutationScope::Watchdog {
                watchdog_id,
                watched_issue_id,
                watchdog_issue_id,
                stop_fingerprint,
                ..
            } => {
                assert_eq!(watchdog_id, "w-1");
                assert_eq!(watched_issue_id, "i-1");
                assert_eq!(watchdog_issue_id.as_deref(), Some("i-2"));
                assert_eq!(stop_fingerprint.as_deref(), Some("fp-1"));
            }
            _ => panic!("expected watchdog"),
        }
    }

    // ----- issueIsInTaskWatchdogSubtree -----

    #[tokio::test]
    async fn r718_subtree_self_is_in() {
        // i-1 是顶级 issue（parent=None），从 i-1 自己开始爬：
        // 1. seen.insert("i-1")
        // 2. find_issue_parent("co-1", "i-1") → Some(IssueParentProjection { id, company_id, parent_id: None, origin_kind: None })
        // 3. currentId == watchedIssueId → return true
        let mut data = FakeData::default();
        data.issues.lock().await.insert(
            "i-1".into(),
            IssueParentProjection {
                id: "i-1".into(),
                company_id: "co-1".into(),
                parent_id: None,
                origin_kind: None,
            },
        );
        let r = issue_is_in_task_watchdog_subtree(&data, "co-1", "i-1", "i-1").await;
        assert!(r);
    }

    #[tokio::test]
    async fn r718_subtree_descendant_is_in() {
        let mut data = FakeData::default();
        data.issues.lock().await.insert(
            "i-1".into(),
            IssueParentProjection {
                id: "i-1".into(),
                company_id: "co-1".into(),
                parent_id: None,
                origin_kind: None,
            },
        );
        data.issues.lock().await.insert(
            "i-2".into(),
            IssueParentProjection {
                id: "i-2".into(),
                company_id: "co-1".into(),
                parent_id: Some("i-1".into()),
                origin_kind: None,
            },
        );
        let r = issue_is_in_task_watchdog_subtree(&data, "co-1", "i-2", "i-1").await;
        assert!(r);
    }

    #[tokio::test]
    async fn r718_subtree_outside_returns_false() {
        let mut data = FakeData::default();
        data.issues.lock().await.insert(
            "i-other".into(),
            IssueParentProjection {
                id: "i-other".into(),
                company_id: "co-1".into(),
                parent_id: Some("i-other-parent".into()),
                origin_kind: None,
            },
        );
        // 没有 i-1
        let r = issue_is_in_task_watchdog_subtree(&data, "co-1", "i-other", "i-1").await;
        assert!(!r);
    }

    #[tokio::test]
    async fn r718_subtree_origin_task_watchdog_rejected() {
        let mut data = FakeData::default();
        data.issues.lock().await.insert(
            "i-1".into(),
            IssueParentProjection {
                id: "i-1".into(),
                company_id: "co-1".into(),
                parent_id: None,
                origin_kind: None,
            },
        );
        data.issues.lock().await.insert(
            "i-mid".into(),
            IssueParentProjection {
                id: "i-mid".into(),
                company_id: "co-1".into(),
                parent_id: Some("i-1".into()),
                origin_kind: Some(TASK_WATCHDOG_ORIGIN_KIND.into()),
            },
        );
        let r = issue_is_in_task_watchdog_subtree(&data, "co-1", "i-mid", "i-1").await;
        assert!(!r);
    }

    // ----- taskWatchdogScopeAllowsIssueMutation -----

    #[tokio::test]
    async fn r718_scope_allows_watchdog_issue() {
        let data = FakeData::default();
        let scope = TaskWatchdogMutationScope::Watchdog {
            watchdog_id: "w-1".into(),
            company_id: "co-1".into(),
            watched_issue_id: "i-1".into(),
            watchdog_issue_id: Some("i-2".into()),
            stop_fingerprint: None,
        };
        let issue = IssueScopeTarget {
            id: "i-2".into(),
            company_id: "co-1".into(),
            parent_id: None,
        };
        let r = task_watchdog_scope_allows_issue_mutation(
            &data,
            scope,
            &issue,
            TaskWatchdogMutationOptions::default(),
        )
        .await;
        match r {
            TaskWatchdogMutationScope::Watchdog { .. } => {}
            _ => panic!("expected watchdog scope"),
        }
    }

    #[tokio::test]
    async fn r718_scope_rejects_company_mismatch() {
        let data = FakeData::default();
        let scope = TaskWatchdogMutationScope::Watchdog {
            watchdog_id: "w-1".into(),
            company_id: "co-1".into(),
            watched_issue_id: "i-1".into(),
            watchdog_issue_id: Some("i-2".into()),
            stop_fingerprint: None,
        };
        let issue = IssueScopeTarget {
            id: "i-2".into(),
            company_id: "co-2".into(),
            parent_id: None,
        };
        let r = task_watchdog_scope_allows_issue_mutation(
            &data,
            scope,
            &issue,
            TaskWatchdogMutationOptions::default(),
        )
        .await;
        match r {
            TaskWatchdogMutationScope::Invalid { detail } => {
                assert!(detail.contains("outside the watchdog company"));
            }
            _ => panic!("expected invalid"),
        }
    }

    #[tokio::test]
    async fn r718_scope_rejects_outside_subtree() {
        let data = FakeData::default();
        let scope = TaskWatchdogMutationScope::Watchdog {
            watchdog_id: "w-1".into(),
            company_id: "co-1".into(),
            watched_issue_id: "i-1".into(),
            watchdog_issue_id: None,
            stop_fingerprint: None,
        };
        let issue = IssueScopeTarget {
            id: "i-other".into(),
            company_id: "co-1".into(),
            parent_id: None,
        };
        let r = task_watchdog_scope_allows_issue_mutation(
            &data,
            scope,
            &issue,
            TaskWatchdogMutationOptions::default(),
        )
        .await;
        match r {
            TaskWatchdogMutationScope::Invalid { detail } => {
                assert!(detail.contains("only mutate the watched issue subtree"));
            }
            _ => panic!("expected invalid"),
        }
    }

    #[tokio::test]
    async fn r718_scope_disallow_watchdog_issue() {
        let data = FakeData::default();
        let scope = TaskWatchdogMutationScope::Watchdog {
            watchdog_id: "w-1".into(),
            company_id: "co-1".into(),
            watched_issue_id: "i-1".into(),
            watchdog_issue_id: Some("i-2".into()),
            stop_fingerprint: None,
        };
        let issue = IssueScopeTarget {
            id: "i-2".into(),
            company_id: "co-1".into(),
            parent_id: None,
        };
        let r = task_watchdog_scope_allows_issue_mutation(
            &data,
            scope,
            &issue,
            TaskWatchdogMutationOptions {
                allow_watchdog_issue: Some(false),
            },
        )
        .await;
        match r {
            TaskWatchdogMutationScope::Invalid { .. } => {}
            _ => panic!("expected invalid when allow_watchdog_issue=false"),
        }
    }

    #[tokio::test]
    async fn r718_scope_non_watchdog_passthrough() {
        let data = FakeData::default();
        let scope = TaskWatchdogMutationScope::None;
        let issue = IssueScopeTarget {
            id: "i-x".into(),
            company_id: "co-1".into(),
            parent_id: None,
        };
        let r = task_watchdog_scope_allows_issue_mutation(
            &data,
            scope.clone(),
            &issue,
            TaskWatchdogMutationOptions::default(),
        )
        .await;
        assert_eq!(r, scope);
    }

    // ----- send/sync -----

    #[test]
    fn r718_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<TaskWatchdogMutationScope>();
        assert_send_sync::<AgentRunActor>();
        assert_send_sync::<TaskWatchdogDataSourceBox>();
    }
}

// Dummy type to allow Send + Sync assertion in tests
pub type TaskWatchdogDataSourceBox = Box<dyn TaskWatchdogDataSource>;
