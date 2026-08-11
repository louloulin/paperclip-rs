//! "Routable blocked" 过渡闸门 + agent 通知（原 `pc-routable-blocked` 已下沉）。
//!
//! 对应 Node `server/src/services/routable-blocked.ts`（54 行）。
//!
//! 设计目标：1:1 复刻
//! - `ROUTABLE_BLOCKED_ROLLOUT_AT_MS` / `rollout_at()` rollout 时间
//! - `isProspectiveBlockedTransition` 判定
//! - `deliverAgentUnblockNotification` 注入式唤醒 + 幂等

use std::sync::Arc;

use chrono::{DateTime, Utc};

/// rollout 时间戳（毫秒）—— 与 Node `ROUTABLE_BLOCKED_ROLLOUT_AT` 1:1。
pub const ROUTABLE_BLOCKED_ROLLOUT_AT_MS: i64 = 1_784_830_383_000;

/// rollout 时间 —— 与 Node `ROUTABLE_BLOCKED_ROLLOUT_AT` 1:1 的 `DateTime<Utc>` 形式。
pub fn rollout_at() -> DateTime<Utc> {
    DateTime::<Utc>::from_timestamp_millis(ROUTABLE_BLOCKED_ROLLOUT_AT_MS)
        .expect("hard-coded rollout timestamp millis is always valid")
}

/// unblock descriptor 的 owner —— 与 Node `IssueUnblockDescriptor.owner` 1:1。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UnblockOwner {
    Board,
    Agent { agent_id: String },
}

/// unblock descriptor —— 描述"issue 解除阻塞"所需的 action / owner。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IssueUnblockDescriptor {
    pub owner: UnblockOwner,
    pub action: String,
}

/// 描述一个 routable-blocked issue 的最小形状。
#[derive(Debug, Clone, Default)]
pub struct RoutableBlockedIssue {
    pub id: String,
    pub status: String,
    pub unblock_descriptor: Option<IssueUnblockDescriptor>,
    pub blocked_transition_at: Option<DateTime<Utc>>,
    pub blocked_owner_notified_at: Option<DateTime<Utc>>,
}

/// 判定是否为"routable blocked"过渡（status=blocked 且 blockedTransitionAt >= rollout）。
pub fn is_prospective_blocked_transition(issue: &RoutableBlockedIssue) -> bool {
    if issue.status != "blocked" {
        return false;
    }
    match issue.blocked_transition_at {
        Some(t) => t >= rollout_at(),
        None => false,
    }
}

/// 唤醒参数 payload 子结构。
#[derive(Debug, Clone)]
pub struct AgentUnblockWakePayload {
    pub issue_id: String,
    pub action: String,
}

/// 唤醒 context snapshot。
#[derive(Debug, Clone)]
pub struct AgentUnblockWakeContextSnapshot {
    pub wake_reason: &'static str,
    pub issue_id: String,
    pub task_id: String,
}

/// 唤醒参数。
#[derive(Debug, Clone)]
pub struct AgentUnblockWakeOptions {
    pub source: &'static str,
    pub trigger_detail: &'static str,
    pub reason: &'static str,
    pub idempotency_key: String,
    pub payload: AgentUnblockWakePayload,
    pub context_snapshot: AgentUnblockWakeContextSnapshot,
}

/// `deliverAgentUnblockNotification` 输入。
///
/// 用 `Arc<dyn Fn>` 持有 wakeup / mark_notified 函数。返回 boxed future。
pub struct DeliverAgentUnblockInput {
    pub issue: RoutableBlockedIssue,
    pub wakeup: Arc<dyn WakeupFn>,
    pub mark_notified: Arc<dyn MarkNotifiedFn>,
    pub now: Option<DateTime<Utc>>,
}

impl std::fmt::Debug for DeliverAgentUnblockInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeliverAgentUnblockInput")
            .field("issue", &self.issue)
            .field("now", &self.now)
            .field("wakeup", &"<fn>")
            .field("mark_notified", &"<fn>")
            .finish()
    }
}

/// 唤醒函数 trait —— 用 trait object 实现（dyn-compatible）。
pub trait WakeupFn: Send + Sync + 'static {
    fn call(
        &self,
        agent_id: String,
        opts: AgentUnblockWakeOptions,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>;
}

/// 标记已通知函数 trait。
pub trait MarkNotifiedFn: Send + Sync + 'static {
    fn call(
        &self,
        notified_at: DateTime<Utc>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>;
}

/// 创建一个 boxed wakeup fn。
pub fn make_wakeup_fn<F, Fut>(f: F) -> Arc<dyn WakeupFn>
where
    F: Fn(String, AgentUnblockWakeOptions) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<(), String>> + Send + 'static,
{
    struct Wrap<F>(F);
    impl<F, Fut> WakeupFn for Wrap<F>
    where
        F: Fn(String, AgentUnblockWakeOptions) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), String>> + Send + 'static,
    {
        fn call(
            &self,
            agent_id: String,
            opts: AgentUnblockWakeOptions,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>
        {
            Box::pin((self.0)(agent_id, opts))
        }
    }
    Arc::new(Wrap(f))
}

/// 创建一个 boxed mark notified fn。
pub fn make_mark_notified_fn<F, Fut>(f: F) -> Arc<dyn MarkNotifiedFn>
where
    F: Fn(DateTime<Utc>) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<(), String>> + Send + 'static,
{
    struct Wrap<F>(F);
    impl<F, Fut> MarkNotifiedFn for Wrap<F>
    where
        F: Fn(DateTime<Utc>) -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = Result<(), String>> + Send + 'static,
    {
        fn call(
            &self,
            notified_at: DateTime<Utc>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>
        {
            Box::pin((self.0)(notified_at))
        }
    }
    Arc::new(Wrap(f))
}

/// `deliverAgentUnblockNotification` 顶层函数。
pub async fn deliver_agent_unblock_notification(input: DeliverAgentUnblockInput) -> bool {
    if !is_prospective_blocked_transition(&input.issue) {
        return false;
    }
    let Some(descriptor) = &input.issue.unblock_descriptor else {
        return false;
    };
    if input.issue.blocked_owner_notified_at.is_some() {
        return false;
    }
    let UnblockOwner::Agent { agent_id } = &descriptor.owner else {
        return false;
    };

    let blocked_transition_at = input
        .issue
        .blocked_transition_at
        .expect("checked by is_prospective_blocked_transition");

    let opts = AgentUnblockWakeOptions {
        source: "automation",
        trigger_detail: "system",
        reason: "issue_unblock_requested",
        idempotency_key: format!(
            "issue-unblock:{}:{}",
            input.issue.id,
            blocked_transition_at.to_rfc3339()
        ),
        payload: AgentUnblockWakePayload {
            issue_id: input.issue.id.clone(),
            action: descriptor.action.clone(),
        },
        context_snapshot: AgentUnblockWakeContextSnapshot {
            wake_reason: "issue_unblock_requested",
            issue_id: input.issue.id.clone(),
            task_id: input.issue.id.clone(),
        },
    };

    if let Err(e) = input.wakeup.call(agent_id.clone(), opts).await {
        tracing::error!(error = %e, issue_id = %input.issue.id, "wakeup failed");
        return false;
    }

    let now = input.now.unwrap_or_else(Utc::now);
    if let Err(e) = input.mark_notified.call(now).await {
        tracing::error!(error = %e, issue_id = %input.issue.id, "mark notified failed");
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    #[test]
    fn r697_rollout_constant_is_correct() {
        let expected: DateTime<Utc> = DateTime::parse_from_rfc3339("2026-07-23T18:13:03.000+00:00")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(rollout_at(), expected);
        assert_eq!(ROUTABLE_BLOCKED_ROLLOUT_AT_MS, expected.timestamp_millis());
    }

    #[test]
    fn r697_is_prospective_blocked_requires_status_blocked() {
        let issue = RoutableBlockedIssue {
            id: "i1".into(),
            status: "in_progress".into(),
            blocked_transition_at: Some(rollout_at()),
            ..Default::default()
        };
        assert!(!is_prospective_blocked_transition(&issue));
    }

    #[test]
    fn r697_is_prospective_blocked_requires_transition_after_rollout() {
        let issue = RoutableBlockedIssue {
            id: "i1".into(),
            status: "blocked".into(),
            blocked_transition_at: Some(rollout_at() - chrono::Duration::seconds(1)),
            ..Default::default()
        };
        assert!(!is_prospective_blocked_transition(&issue));
    }

    #[test]
    fn r697_is_prospective_blocked_boundary_at_rollout() {
        let issue = RoutableBlockedIssue {
            id: "i1".into(),
            status: "blocked".into(),
            blocked_transition_at: Some(rollout_at()),
            ..Default::default()
        };
        assert!(is_prospective_blocked_transition(&issue));
    }

    #[test]
    fn r697_is_prospective_blocked_requires_transition_present() {
        let issue = RoutableBlockedIssue {
            id: "i1".into(),
            status: "blocked".into(),
            blocked_transition_at: None,
            ..Default::default()
        };
        assert!(!is_prospective_blocked_transition(&issue));
    }

    #[test]
    fn r697_is_prospective_blocked_full_match() {
        let issue = RoutableBlockedIssue {
            id: "i1".into(),
            status: "blocked".into(),
            blocked_transition_at: Some(rollout_at() + chrono::Duration::hours(1)),
            ..Default::default()
        };
        assert!(is_prospective_blocked_transition(&issue));
    }

    fn test_issue_with_agent_owner() -> RoutableBlockedIssue {
        RoutableBlockedIssue {
            id: "issue-1".into(),
            status: "blocked".into(),
            unblock_descriptor: Some(IssueUnblockDescriptor {
                owner: UnblockOwner::Agent {
                    agent_id: "a-7".into(),
                },
                action: "review-and-resume".into(),
            }),
            blocked_transition_at: Some(rollout_at() + chrono::Duration::minutes(10)),
            blocked_owner_notified_at: None,
        }
    }

    fn empty_wakeup() -> Arc<dyn WakeupFn> {
        make_wakeup_fn(|_: String, _: AgentUnblockWakeOptions| async { Ok(()) })
    }

    fn empty_mark() -> Arc<dyn MarkNotifiedFn> {
        make_mark_notified_fn(|_: DateTime<Utc>| async { Ok(()) })
    }

    #[tokio::test]
    async fn r697_deliver_skips_when_not_prospective() {
        let issue = RoutableBlockedIssue {
            status: "in_progress".into(),
            ..test_issue_with_agent_owner()
        };
        let wake_called = Arc::new(AtomicBool::new(false));
        let wake_inner = wake_called.clone();
        let wake = make_wakeup_fn(move |_: String, _: AgentUnblockWakeOptions| {
            let w = wake_inner.clone();
            async move {
                w.store(true, Ordering::SeqCst);
                Ok(())
            }
        });
        let mark_called = Arc::new(AtomicBool::new(false));
        let mark_inner = mark_called.clone();
        let mark = make_mark_notified_fn(move |_: DateTime<Utc>| {
            let m = mark_inner.clone();
            async move {
                m.store(true, Ordering::SeqCst);
                Ok(())
            }
        });

        let r = deliver_agent_unblock_notification(DeliverAgentUnblockInput {
            issue,
            wakeup: wake,
            mark_notified: mark,
            now: None,
        })
        .await;
        assert!(!r);
        assert!(!wake_called.load(Ordering::SeqCst));
        assert!(!mark_called.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn r697_deliver_skips_when_no_unblock_descriptor() {
        let mut issue = test_issue_with_agent_owner();
        issue.unblock_descriptor = None;
        let r = deliver_agent_unblock_notification(DeliverAgentUnblockInput {
            issue,
            wakeup: empty_wakeup(),
            mark_notified: empty_mark(),
            now: None,
        })
        .await;
        assert!(!r);
    }

    #[tokio::test]
    async fn r697_deliver_skips_when_already_notified() {
        let mut issue = test_issue_with_agent_owner();
        issue.blocked_owner_notified_at = Some(Utc::now());
        let r = deliver_agent_unblock_notification(DeliverAgentUnblockInput {
            issue,
            wakeup: empty_wakeup(),
            mark_notified: empty_mark(),
            now: None,
        })
        .await;
        assert!(!r);
    }

    #[tokio::test]
    async fn r697_deliver_skips_when_owner_is_board() {
        let mut issue = test_issue_with_agent_owner();
        issue.unblock_descriptor = Some(IssueUnblockDescriptor {
            owner: UnblockOwner::Board,
            action: "manual-review".into(),
        });
        let r = deliver_agent_unblock_notification(DeliverAgentUnblockInput {
            issue,
            wakeup: empty_wakeup(),
            mark_notified: empty_mark(),
            now: None,
        })
        .await;
        assert!(!r);
    }

    #[tokio::test]
    async fn r697_deliver_calls_wakeup_and_mark_notified() {
        let issue = test_issue_with_agent_owner();
        let captured_agent_id = Arc::new(Mutex::new(None::<String>));
        let captured_idem = Arc::new(Mutex::new(None::<String>));
        let captured_payload = Arc::new(Mutex::new(None::<AgentUnblockWakePayload>));
        let captured_mark = Arc::new(Mutex::new(None::<DateTime<Utc>>));

        let agent_inner = captured_agent_id.clone();
        let idem_inner = captured_idem.clone();
        let payload_inner = captured_payload.clone();
        let wake = make_wakeup_fn(move |agent_id: String, opts: AgentUnblockWakeOptions| {
            let agent_inner = agent_inner.clone();
            let idem_inner = idem_inner.clone();
            let payload_inner = payload_inner.clone();
            async move {
                *agent_inner.lock().unwrap() = Some(agent_id);
                *idem_inner.lock().unwrap() = Some(opts.idempotency_key);
                *payload_inner.lock().unwrap() = Some(opts.payload);
                Ok(())
            }
        });
        let mark_inner = captured_mark.clone();
        let mark = make_mark_notified_fn(move |notified_at: DateTime<Utc>| {
            let mark_inner = mark_inner.clone();
            async move {
                *mark_inner.lock().unwrap() = Some(notified_at);
                Ok(())
            }
        });

        let fixed_now = Utc::now();
        let r = deliver_agent_unblock_notification(DeliverAgentUnblockInput {
            issue: issue.clone(),
            wakeup: wake,
            mark_notified: mark,
            now: Some(fixed_now),
        })
        .await;
        assert!(r);
        assert_eq!(*captured_agent_id.lock().unwrap(), Some("a-7".to_string()));
        let idem = captured_idem.lock().unwrap().clone().unwrap();
        assert!(idem.starts_with(&format!("issue-unblock:{}:", issue.id)));
        let payload = captured_payload.lock().unwrap().clone().unwrap();
        assert_eq!(payload.issue_id, issue.id);
        assert_eq!(payload.action, "review-and-resume");
        assert_eq!(*captured_mark.lock().unwrap(), Some(fixed_now));
    }

    #[tokio::test]
    async fn r697_deliver_propagates_wakeup_error() {
        let issue = test_issue_with_agent_owner();
        let wake =
            make_wakeup_fn(|_: String, _: AgentUnblockWakeOptions| async { Err("boom".into()) });
        let mark_called = Arc::new(AtomicBool::new(false));
        let mark_inner = mark_called.clone();
        let mark = make_mark_notified_fn(move |_: DateTime<Utc>| {
            let m = mark_inner.clone();
            async move {
                m.store(true, Ordering::SeqCst);
                Ok(())
            }
        });
        let r = deliver_agent_unblock_notification(DeliverAgentUnblockInput {
            issue,
            wakeup: wake,
            mark_notified: mark,
            now: None,
        })
        .await;
        assert!(!r);
        assert!(!mark_called.load(Ordering::SeqCst));
    }
}
