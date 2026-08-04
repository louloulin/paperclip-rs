//! Issue assignment wakeup (1:1 port of Node `server/src/services/issue-assignment-wakeup.ts`, 57 行).
//!
//! 单一职责：当 issue 被分配 / 重新分配时，向 assignee agent 发起一次系统级 wakeup。
//!
//! - `IssueAssignmentWakeupDeps` —— 抽象心跳侧依赖，便于单测注入 mock
//! - `IssueAssignmentSnapshot` —— 触发本次 wakeup 的 issue 摘要
//! - `QueueIssueAssignmentWakeupInput` —— 完整入参（含 reason / mutation / contextSource 等）
//! - `queue_issue_assignment_wakeup(input)` —— 公开函数，提前返回 + 错误吞咽 + 可选 rethrow
//!
//! 不持有状态；仅依赖一个外部 `wakeup` trait。

use async_trait::async_trait;
use serde_json::{Map, Value};
use tracing::warn;

// ============================================================================
// Types
// ============================================================================

/// Wakeup 触发 detail（与 Node `WakeupTriggerDetail` 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeupTriggerDetail {
    Manual,
    Ping,
    Callback,
    System,
}

impl WakeupTriggerDetail {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Ping => "ping",
            Self::Callback => "callback",
            Self::System => "system",
        }
    }
}

/// Wakeup 来源（与 Node `WakeupSource` 1:1 对齐）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeupSource {
    Timer,
    Assignment,
    OnDemand,
    Automation,
}

impl WakeupSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Timer => "timer",
            Self::Assignment => "assignment",
            Self::OnDemand => "on_demand",
            Self::Automation => "automation",
        }
    }
}

/// Wakeup 发起 actor 类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeupRequestedByActorType {
    User,
    Agent,
    System,
}

impl WakeupRequestedByActorType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
            Self::System => "system",
        }
    }
}

/// 心跳侧依赖抽象（与 Node `IssueAssignmentWakeupDeps` 1:1 对齐）。
///
/// 单方法：`wakeup(agent_id, opts) -> anyhow::Result<()>` 或自定义错误。
/// 实际错误处理由 trait 实现方决定；本模块只关心成功路径与失败吞咽。
#[async_trait]
pub trait IssueAssignmentWakeupDeps: Send + Sync {
    async fn wakeup(
        &self,
        agent_id: &str,
        opts: IssueAssignmentWakeupOptions<'_>,
    ) -> Result<(), String>;
}

/// `wakeup` 入参 options（与 Node `opts` 1:1 对齐）。
#[derive(Debug, Clone, Default)]
pub struct IssueAssignmentWakeupOptions<'a> {
    pub source: Option<WakeupSource>,
    pub trigger_detail: Option<WakeupTriggerDetail>,
    pub reason: Option<&'a str>,
    pub payload: Option<Map<String, Value>>,
    pub requested_by_actor_type: Option<WakeupRequestedByActorType>,
    pub requested_by_actor_id: Option<String>,
    pub context_snapshot: Option<Map<String, Value>>,
}

/// Issue 摘要（与 Node `issue` 字段 1:1 对齐）。
#[derive(Debug, Clone, Default)]
pub struct IssueAssignmentSnapshot {
    pub id: String,
    pub assignee_agent_id: Option<String>,
    pub status: String,
}

/// `queue_issue_assignment_wakeup` 入参（与 Node `input` 1:1 对齐）。
///
/// 注：`heartbeat: &'a dyn Trait` 不能 derive `Debug` / `Clone` / `Default`，
/// 故本结构体手动构造。
pub struct QueueIssueAssignmentWakeupInput<'a> {
    pub heartbeat: &'a dyn IssueAssignmentWakeupDeps,
    pub issue: IssueAssignmentSnapshot,
    pub reason: String,
    pub mutation: String,
    pub context_source: String,
    pub requested_by_actor_type: Option<WakeupRequestedByActorType>,
    pub requested_by_actor_id: Option<String>,
    pub task_key: Option<String>,
    pub rethrow_on_error: bool,
}

impl<'a> QueueIssueAssignmentWakeupInput<'a> {
    pub fn new(heartbeat: &'a dyn IssueAssignmentWakeupDeps, reason: impl Into<String>) -> Self {
        Self {
            heartbeat,
            issue: IssueAssignmentSnapshot::default(),
            reason: reason.into(),
            mutation: String::new(),
            context_source: String::new(),
            requested_by_actor_type: None,
            requested_by_actor_id: None,
            task_key: None,
            rethrow_on_error: false,
        }
    }
}

/// `queue_issue_assignment_wakeup` 返回（与 Node `Promise<unknown>` 1:1 对齐）。
///
/// - `Ok(None)` —— 提前返回（no assignee 或 status == "backlog"）
/// - `Ok(Some(()))` —— wakeup 成功
/// - `Err(String)` —— wakeup 失败且 `rethrow_on_error = true`
/// - `Ok(Some(()))` + 已 warn 日志 —— wakeup 失败且 `rethrow_on_error = false`（错误吞咽）
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueIssueAssignmentWakeupOutcome {
    Skipped,
    Succeeded,
    Swallowed(String),
}

// ============================================================================
// Public API
// ============================================================================

/// 在 issue 被分配 / 重新分配时，向 assignee 发起一次系统级 wakeup。
///
/// 行为（与 Node `queueIssueAssignmentWakeup` 1:1 对齐）：
/// 1. **提前返回**：`assignee_agent_id` 缺失或 `status == "backlog"` → `Ok(Skipped)`
/// 2. **构造 wakeup opts**：
///    - `source = "assignment"`
///    - `triggerDetail = "system"`
///    - `payload = { issueId, mutation, ...(taskKey ? { taskKey } : {}) }`
///    - `requestedByActorId = requestedByActorId ?? null`
///    - `contextSnapshot = { issueId, source: contextSource, ...(taskKey ? { taskKey } : {}) }`
/// 3. **错误处理**：catch 错误 → warn 日志；`rethrowOnError = true` 时 throw；否则吞咽
#[must_use]
pub async fn queue_issue_assignment_wakeup(
    input: QueueIssueAssignmentWakeupInput<'_>,
) -> QueueIssueAssignmentWakeupOutcome {
    // 提前返回
    if input.issue.assignee_agent_id.is_none() || input.issue.status == "backlog" {
        return QueueIssueAssignmentWakeupOutcome::Skipped;
    }

    let assignee = input
        .issue
        .assignee_agent_id
        .as_deref()
        .expect("guarded by is_none check above");

    // 构造 payload
    let mut payload = Map::new();
    payload.insert("issueId".into(), Value::String(input.issue.id.clone()));
    payload.insert("mutation".into(), Value::String(input.mutation.clone()));
    if let Some(ref tk) = input.task_key {
        payload.insert("taskKey".into(), Value::String(tk.clone()));
    }

    // 构造 contextSnapshot
    let mut context_snapshot = Map::new();
    context_snapshot.insert("issueId".into(), Value::String(input.issue.id.clone()));
    context_snapshot.insert(
        "source".into(),
        Value::String(input.context_source.clone()),
    );
    if let Some(ref tk) = input.task_key {
        context_snapshot.insert("taskKey".into(), Value::String(tk.clone()));
    }

    let opts = IssueAssignmentWakeupOptions {
        source: Some(WakeupSource::Assignment),
        trigger_detail: Some(WakeupTriggerDetail::System),
        reason: Some(&input.reason),
        payload: Some(payload),
        requested_by_actor_type: input.requested_by_actor_type,
        requested_by_actor_id: Some(input.requested_by_actor_id.clone().unwrap_or_default()),
        context_snapshot: Some(context_snapshot),
    };

    // 调用 wakeup
    match input.heartbeat.wakeup(assignee, opts).await {
        Ok(()) => QueueIssueAssignmentWakeupOutcome::Succeeded,
        Err(err) => {
            warn!(
                err = %err,
                issue_id = %input.issue.id,
                "failed to wake assignee on issue assignment"
            );
            if input.rethrow_on_error {
                QueueIssueAssignmentWakeupOutcome::Swallowed(err)
            } else {
                QueueIssueAssignmentWakeupOutcome::Succeeded
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    // ---- 简单 Mock ----

    #[derive(Default)]
    struct MockWakeup {
        calls: Mutex<Vec<(String, IssueAssignmentWakeupOptions<'static>)>>,
        call_count: AtomicUsize,
        return_error: Option<String>,
    }

    // 实际存储用 'static 生命周期
    #[derive(Default)]
    struct MockWakeupStatic {
        calls: Mutex<Vec<(String, IssueAssignmentWakeupOptions<'static>)>>,
        call_count: AtomicUsize,
        return_error: Option<String>,
    }

    // 由于 IssueAssignmentWakeupOptions 含 Option<&'a str>，需要 'static，
    // 我们用一个内部 buffer 把 reason 转成 'static
    impl MockWakeup {
        fn record(&self, agent_id: &str, opts: IssueAssignmentWakeupOptions<'_>) {
            // 把 &str reason 转成 'static String
            let static_opts = IssueAssignmentWakeupOptions {
                source: opts.source,
                trigger_detail: opts.trigger_detail,
                reason: opts.reason.map(|s| s.to_string().leak() as &'static str),
                payload: opts.payload.clone(),
                requested_by_actor_type: opts.requested_by_actor_type,
                requested_by_actor_id: opts.requested_by_actor_id.clone(),
                context_snapshot: opts.context_snapshot.clone(),
            };
            self.calls
                .lock()
                .unwrap()
                .push((agent_id.to_string(), static_opts));
            self.call_count.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl IssueAssignmentWakeupDeps for MockWakeup {
        async fn wakeup(
            &self,
            agent_id: &str,
            opts: IssueAssignmentWakeupOptions<'_>,
        ) -> Result<(), String> {
            self.record(agent_id, opts);
            match &self.return_error {
                Some(e) => Err(e.clone()),
                None => Ok(()),
            }
        }
    }

    // ---- as_str ----

    #[test]
    fn wakeup_source_as_str_matches_node() {
        assert_eq!(WakeupSource::Timer.as_str(), "timer");
        assert_eq!(WakeupSource::Assignment.as_str(), "assignment");
        assert_eq!(WakeupSource::OnDemand.as_str(), "on_demand");
        assert_eq!(WakeupSource::Automation.as_str(), "automation");
    }

    #[test]
    fn wakeup_trigger_detail_as_str_matches_node() {
        assert_eq!(WakeupTriggerDetail::Manual.as_str(), "manual");
        assert_eq!(WakeupTriggerDetail::Ping.as_str(), "ping");
        assert_eq!(WakeupTriggerDetail::Callback.as_str(), "callback");
        assert_eq!(WakeupTriggerDetail::System.as_str(), "system");
    }

    #[test]
    fn requested_by_actor_type_as_str_matches_node() {
        assert_eq!(WakeupRequestedByActorType::User.as_str(), "user");
        assert_eq!(WakeupRequestedByActorType::Agent.as_str(), "agent");
        assert_eq!(WakeupRequestedByActorType::System.as_str(), "system");
    }

    // ---- 提前返回 ----

    #[tokio::test]
    async fn skips_when_no_assignee() {
        let mock = MockWakeup::default();
        let out = queue_issue_assignment_wakeup(QueueIssueAssignmentWakeupInput {
            heartbeat: &mock,
            issue: IssueAssignmentSnapshot {
                id: "i1".into(),
                assignee_agent_id: None,
                status: "todo".into(),
            },
            reason: "r".into(),
            mutation: "m".into(),
            context_source: "src".into(),
            requested_by_actor_type: None,
            requested_by_actor_id: None,
            task_key: None,
            rethrow_on_error: false,
        })
        .await;
        assert_eq!(out, QueueIssueAssignmentWakeupOutcome::Skipped);
        assert_eq!(mock.call_count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn skips_when_status_is_backlog() {
        let mock = MockWakeup::default();
        let out = queue_issue_assignment_wakeup(QueueIssueAssignmentWakeupInput {
            heartbeat: &mock,
            issue: IssueAssignmentSnapshot {
                id: "i1".into(),
                assignee_agent_id: Some("a1".into()),
                status: "backlog".into(),
            },
            reason: "r".into(),
            mutation: "m".into(),
            context_source: "src".into(),
            requested_by_actor_type: None,
            requested_by_actor_id: None,
            task_key: None,
            rethrow_on_error: false,
        })
        .await;
        assert_eq!(out, QueueIssueAssignmentWakeupOutcome::Skipped);
        assert_eq!(mock.call_count.load(Ordering::SeqCst), 0);
    }

    // ---- 成功路径 ----

    #[tokio::test]
    async fn calls_wakeup_on_success() {
        let mock = MockWakeup::default();
        let out = queue_issue_assignment_wakeup(QueueIssueAssignmentWakeupInput {
            heartbeat: &mock,
            issue: IssueAssignmentSnapshot {
                id: "i1".into(),
                assignee_agent_id: Some("a1".into()),
                status: "todo".into(),
            },
            reason: "issue assigned".into(),
            mutation: "assignee_changed".into(),
            context_source: "issue.update".into(),
            requested_by_actor_type: Some(WakeupRequestedByActorType::System),
            requested_by_actor_id: Some("user-1".into()),
            task_key: Some("tk1".into()),
            rethrow_on_error: false,
        })
        .await;
        assert_eq!(out, QueueIssueAssignmentWakeupOutcome::Succeeded);
        assert_eq!(mock.call_count.load(Ordering::SeqCst), 1);

        let calls = mock.calls.lock().unwrap();
        let (agent_id, opts) = &calls[0];
        assert_eq!(agent_id, "a1");
        assert_eq!(opts.source, Some(WakeupSource::Assignment));
        assert_eq!(opts.trigger_detail, Some(WakeupTriggerDetail::System));
        assert_eq!(opts.reason, Some("issue assigned"));
        assert_eq!(
            opts.requested_by_actor_type,
            Some(WakeupRequestedByActorType::System)
        );
        assert_eq!(opts.requested_by_actor_id, Some("user-1".into()));
    }

    #[tokio::test]
    async fn payload_contains_issue_id_mutation_and_optional_task_key() {
        let mock = MockWakeup::default();
        let _ = queue_issue_assignment_wakeup(QueueIssueAssignmentWakeupInput {
            heartbeat: &mock,
            issue: IssueAssignmentSnapshot {
                id: "i-xyz".into(),
                assignee_agent_id: Some("a1".into()),
                status: "todo".into(),
            },
            reason: "r".into(),
            mutation: "assignee_changed".into(),
            context_source: "src".into(),
            requested_by_actor_type: None,
            requested_by_actor_id: None,
            task_key: Some("tk-special".into()),
            rethrow_on_error: false,
        })
        .await;

        let calls = mock.calls.lock().unwrap();
        let (_, opts) = &calls[0];
        let payload = opts.payload.as_ref().unwrap();
        assert_eq!(payload.get("issueId"), Some(&Value::String("i-xyz".into())));
        assert_eq!(
            payload.get("mutation"),
            Some(&Value::String("assignee_changed".into()))
        );
        assert_eq!(
            payload.get("taskKey"),
            Some(&Value::String("tk-special".into()))
        );
    }

    #[tokio::test]
    async fn payload_omits_task_key_when_none() {
        let mock = MockWakeup::default();
        let _ = queue_issue_assignment_wakeup(QueueIssueAssignmentWakeupInput {
            heartbeat: &mock,
            issue: IssueAssignmentSnapshot {
                id: "i1".into(),
                assignee_agent_id: Some("a1".into()),
                status: "todo".into(),
            },
            reason: "r".into(),
            mutation: "m".into(),
            context_source: "src".into(),
            requested_by_actor_type: None,
            requested_by_actor_id: None,
            task_key: None,
            rethrow_on_error: false,
        })
        .await;

        let calls = mock.calls.lock().unwrap();
        let (_, opts) = &calls[0];
        let payload = opts.payload.as_ref().unwrap();
        assert!(!payload.contains_key("taskKey"));
    }

    #[tokio::test]
    async fn context_snapshot_includes_issue_id_and_source() {
        let mock = MockWakeup::default();
        let _ = queue_issue_assignment_wakeup(QueueIssueAssignmentWakeupInput {
            heartbeat: &mock,
            issue: IssueAssignmentSnapshot {
                id: "i1".into(),
                assignee_agent_id: Some("a1".into()),
                status: "todo".into(),
            },
            reason: "r".into(),
            mutation: "m".into(),
            context_source: "issue.update.assignee".into(),
            requested_by_actor_type: None,
            requested_by_actor_id: None,
            task_key: None,
            rethrow_on_error: false,
        })
        .await;

        let calls = mock.calls.lock().unwrap();
        let (_, opts) = &calls[0];
        let snap = opts.context_snapshot.as_ref().unwrap();
        assert_eq!(snap.get("issueId"), Some(&Value::String("i1".into())));
        assert_eq!(
            snap.get("source"),
            Some(&Value::String("issue.update.assignee".into()))
        );
    }

    #[tokio::test]
    async fn requested_by_actor_id_defaults_to_null() {
        let mock = MockWakeup::default();
        let _ = queue_issue_assignment_wakeup(QueueIssueAssignmentWakeupInput {
            heartbeat: &mock,
            issue: IssueAssignmentSnapshot {
                id: "i1".into(),
                assignee_agent_id: Some("a1".into()),
                status: "todo".into(),
            },
            reason: "r".into(),
            mutation: "m".into(),
            context_source: "src".into(),
            requested_by_actor_type: None,
            requested_by_actor_id: None,
            task_key: None,
            rethrow_on_error: false,
        })
        .await;

        let calls = mock.calls.lock().unwrap();
        let (_, opts) = &calls[0];
        // requestedByActorId ?? null → 空字符串（与 Node 端 `?? null` 语义对应为 None/empty）
        assert_eq!(opts.requested_by_actor_id, Some(String::new()));
    }

    // ---- 错误处理 ----

    #[tokio::test]
    async fn error_is_swallowed_when_rethrow_false() {
        let mock = MockWakeup {
            return_error: Some("wakeup failed".into()),
            ..Default::default()
        };
        let out = queue_issue_assignment_wakeup(QueueIssueAssignmentWakeupInput {
            heartbeat: &mock,
            issue: IssueAssignmentSnapshot {
                id: "i1".into(),
                assignee_agent_id: Some("a1".into()),
                status: "todo".into(),
            },
            reason: "r".into(),
            mutation: "m".into(),
            context_source: "src".into(),
            requested_by_actor_type: None,
            requested_by_actor_id: None,
            task_key: None,
            rethrow_on_error: false,
        })
        .await;
        assert_eq!(out, QueueIssueAssignmentWakeupOutcome::Succeeded);
        assert_eq!(mock.call_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn error_is_returned_when_rethrow_true() {
        let mock = MockWakeup {
            return_error: Some("wakeup failed".into()),
            ..Default::default()
        };
        let out = queue_issue_assignment_wakeup(QueueIssueAssignmentWakeupInput {
            heartbeat: &mock,
            issue: IssueAssignmentSnapshot {
                id: "i1".into(),
                assignee_agent_id: Some("a1".into()),
                status: "todo".into(),
            },
            reason: "r".into(),
            mutation: "m".into(),
            context_source: "src".into(),
            requested_by_actor_type: None,
            requested_by_actor_id: None,
            task_key: None,
            rethrow_on_error: true,
        })
        .await;
        match out {
            QueueIssueAssignmentWakeupOutcome::Swallowed(err) => {
                assert_eq!(err, "wakeup failed");
            }
            _ => panic!("expected Swallowed outcome, got {:?}", out),
        }
    }
}
