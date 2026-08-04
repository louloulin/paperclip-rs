//! Routable blocked 通知（对齐 Node `server/src/services/routable-blocked.ts`，54 行）。
//!
//! 单一职责：判断某个 issue 是否进入「routable blocked」状态，
//! 并在首次进入时调用注入的 `wakeup` 函数通知 owner agent（同时调用 `markNotified` 标记）。
//!
//! 不依赖任何 IO：所有副作用（wakeup / markNotified）通过 trait 注入，
//! 便于单测用 `mockall` 或手工实现 fake 替换。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Routable blocked rollout 起始时间（与 Node `routable_blocked_rollout_at()` 1:1 对齐）。
///
/// 注：Node 端用 `new Date(...)` 在模块加载时构造；
/// Rust 端因 chrono 没有 const DateTime 构造器，改用 `OnceLock` 懒初始化。
pub fn routable_blocked_rollout_at() -> DateTime<Utc> {
    static CACHE: std::sync::OnceLock<DateTime<Utc>> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        DateTime::parse_from_rfc3339("2026-07-23T18:13:03.000Z")
            .expect("routable_blocked_rollout_at() must be a valid RFC3339 timestamp")
            .with_timezone(&Utc)
    })
}

/// Issue unblock owner 判别式（与 Node `IssueUnblockOwner` 1:1 对齐）。
///
/// 三种 owner：`Agent { agent_id }` / `User { user_id }` / `Board`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum IssueUnblockOwner {
    Agent {
        #[serde(rename = "agentId")]
        agent_id: String,
    },
    User {
        #[serde(rename = "userId")]
        user_id: String,
    },
    Board,
}

impl IssueUnblockOwner {
    pub fn is_agent(&self) -> bool {
        matches!(self, Self::Agent { .. })
    }
}

/// Issue unblock descriptor（与 Node `IssueUnblockDescriptor` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueUnblockDescriptor {
    pub owner: IssueUnblockOwner,
    pub action: String,
}

/// Wakeup 请求中的 payload（与 Node `payload: { issueId, action }` 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueUnblockPayload {
    #[serde(rename = "issueId")]
    pub issue_id: String,
    pub action: String,
}

/// Wakeup 请求中的 contextSnapshot（与 Node 1:1 对齐）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueUnblockContextSnapshot {
    #[serde(rename = "wakeReason")]
    pub wake_reason: &'static str,
    #[serde(rename = "issueId")]
    pub issue_id: String,
    #[serde(rename = "taskId")]
    pub task_id: String,
}

/// Agent wakeup 请求体（与 Node `wakeup(agentId, options)` 的 options 1:1 对齐）。
///
/// 当前模块只构造 `issue_unblock_requested` 这一种 reason，
/// 但保留 `source` / `trigger_detail` / `reason` 字段以保持与 Node `wakeup` 上游契约一致。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentWakeupRequest {
    pub source: &'static str,
    #[serde(rename = "triggerDetail")]
    pub trigger_detail: &'static str,
    pub reason: &'static str,
    #[serde(rename = "idempotencyKey")]
    pub idempotency_key: String,
    pub payload: IssueUnblockPayload,
    #[serde(rename = "contextSnapshot")]
    pub context_snapshot: IssueUnblockContextSnapshot,
}

/// 通知 owner agent 的副作用 trait（与 Node `wakeup` 注入函数 1:1 对齐）。
///
/// 实现方：HTTP 层可包装 `AgentWakeupRepo::request_wakeup`；
/// 单测可用 `Arc<Mutex<Vec<...>>>` fake。
#[async_trait::async_trait]
pub trait WakeupNotifier: Send + Sync {
    async fn wakeup(&self, agent_id: &str, request: AgentWakeupRequest) -> anyhow::Result<()>;
}

/// 标记 owner notified 副作用 trait（与 Node `markNotified` 注入函数 1:1 对齐）。
#[async_trait::async_trait]
pub trait NotifiedMarker: Send + Sync {
    async fn mark_notified(&self, notified_at: DateTime<Utc>) -> anyhow::Result<()>;
}

/// `deliverAgentUnblockNotification` 的输入参数（与 Node 函数签名 1:1 对齐）。
///
/// `now` 为可注入时钟（默认 `Utc::now`）；用于单测固定时间。
pub struct DeliverAgentUnblockNotificationInput<'a, W: WakeupNotifier, M: NotifiedMarker> {
    pub issue: &'a RoutableBlockedIssue,
    pub wakeup: &'a W,
    pub marker: &'a M,
    pub now: Option<Box<dyn Fn() -> DateTime<Utc> + Send + Sync + 'a>>,
}

/// Routable blocked issue 最小形状（与 Node `RoutableBlockedIssue` 1:1 对齐）。
///
/// 字段顺序：`id / status / unblockDescriptor / blockedTransitionAt / blockedOwnerNotifiedAt`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutableBlockedIssue {
    pub id: String,
    pub status: String,
    #[serde(rename = "unblockDescriptor", skip_serializing_if = "Option::is_none")]
    pub unblock_descriptor: Option<IssueUnblockDescriptor>,
    #[serde(
        rename = "blockedTransitionAt",
        skip_serializing_if = "Option::is_none"
    )]
    pub blocked_transition_at: Option<DateTime<Utc>>,
    #[serde(
        rename = "blockedOwnerNotifiedAt",
        skip_serializing_if = "Option::is_none"
    )]
    pub blocked_owner_notified_at: Option<DateTime<Utc>>,
}

impl RoutableBlockedIssue {
    /// 是否进入 prospective blocked transition（与 Node `isProspectiveBlockedTransition` 1:1 对齐）。
    ///
    /// 三条 ALL 条件：
    /// - `status == "blocked"`
    /// - `blockedTransitionAt` 非空
    /// - `blockedTransitionAt >= routable_blocked_rollout_at()`
    pub fn is_prospective_blocked_transition(&self) -> bool {
        self.status == "blocked"
            && self
                .blocked_transition_at
                .map(|t| t >= routable_blocked_rollout_at())
                .unwrap_or(false)
    }
}

/// 投递 agent unblock 通知（与 Node `deliverAgentUnblockNotification` 1:1 对齐）。
///
/// 短路条件（任一满足 → 返回 `false`）：
/// 1. 不是 prospective blocked transition
/// 2. 没有 `unblockDescriptor`
/// 3. 已经 notified 过（`blockedOwnerNotifiedAt` 非空）
/// 4. owner 是 `board` 或非 `Agent` 类型
///
/// 副作用：
/// 1. `wakeup(agent_id, request)`，`request.idempotencyKey = "issue-unblock:{id}:{transitionAt ISO}"`
/// 2. `mark_notified(now())`
///
/// 返回 `true` 表示本次实际投递；`false` 表示短路（不投递）。
pub async fn deliver_agent_unblock_notification<W: WakeupNotifier, M: NotifiedMarker>(
    input: DeliverAgentUnblockNotificationInput<'_, W, M>,
) -> bool {
    let issue = input.issue;
    if !issue.is_prospective_blocked_transition()
        || issue.unblock_descriptor.is_none()
        || issue.blocked_owner_notified_at.is_some()
    {
        return false;
    }

    let descriptor = issue.unblock_descriptor.as_ref().expect("checked above");
    let owner = &descriptor.owner;
    if matches!(owner, IssueUnblockOwner::Board) || !owner.is_agent() {
        return false;
    }
    let agent_id = match owner {
        IssueUnblockOwner::Agent { agent_id } => agent_id.clone(),
        _ => return false,
    };

    let transition_at = issue
        .blocked_transition_at
        .expect("checked by is_prospective_blocked_transition");
    let idempotency_key = format!(
        "issue-unblock:{}:{}",
        issue.id,
        transition_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    );

    let request = AgentWakeupRequest {
        source: "automation",
        trigger_detail: "system",
        reason: "issue_unblock_requested",
        idempotency_key: idempotency_key.clone(),
        payload: IssueUnblockPayload {
            issue_id: issue.id.clone(),
            action: descriptor.action.clone(),
        },
        context_snapshot: IssueUnblockContextSnapshot {
            wake_reason: "issue_unblock_requested",
            issue_id: issue.id.clone(),
            task_id: issue.id.clone(),
        },
    };

    if input.wakeup.wakeup(&agent_id, request).await.is_err() {
        return false;
    }

    let now_fn: Box<dyn Fn() -> DateTime<Utc> + Send + Sync> = match input.now {
        Some(f) => f,
        None => Box::new(Utc::now),
    };
    let notified_at = now_fn();
    if input.marker.mark_notified(notified_at).await.is_err() {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    /// Fake wakeup：记录所有调用参数到 `calls`。
    #[derive(Default)]
    struct FakeWakeup {
        calls: Arc<Mutex<Vec<(String, AgentWakeupRequest)>>>,
    }

    #[async_trait::async_trait]
    impl WakeupNotifier for FakeWakeup {
        async fn wakeup(&self, agent_id: &str, request: AgentWakeupRequest) -> anyhow::Result<()> {
            self.calls
                .lock()
                .await
                .push((agent_id.to_string(), request));
            Ok(())
        }
    }

    /// Fake marker：记录所有 notifiedAt 调用到 `marks`。
    #[derive(Default)]
    struct FakeMarker {
        marks: Arc<Mutex<Vec<DateTime<Utc>>>>,
    }

    #[async_trait::async_trait]
    impl NotifiedMarker for FakeMarker {
        async fn mark_notified(&self, notified_at: DateTime<Utc>) -> anyhow::Result<()> {
            self.marks.lock().await.push(notified_at);
            Ok(())
        }
    }

    const AGENT_ID: &str = "00000000-0000-4000-8000-000000000001";
    const ISSUE_ID: &str = "00000000-0000-4000-8000-000000000002";

    fn blocked_issue(
        transition_at: Option<DateTime<Utc>>,
        notified_at: Option<DateTime<Utc>>,
    ) -> RoutableBlockedIssue {
        RoutableBlockedIssue {
            id: ISSUE_ID.to_string(),
            status: "blocked".to_string(),
            unblock_descriptor: Some(IssueUnblockDescriptor {
                owner: IssueUnblockOwner::Agent {
                    agent_id: AGENT_ID.to_string(),
                },
                action: "Review the finding".to_string(),
            }),
            blocked_transition_at: transition_at,
            blocked_owner_notified_at: notified_at,
        }
    }

    #[test]
    fn rollout_at_constant_is_parseable_and_correct() {
        let expected = DateTime::parse_from_rfc3339("2026-07-23T18:13:03.000Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(routable_blocked_rollout_at(), expected);
    }

    #[test]
    fn is_prospective_requires_status_blocked_and_post_rollout_transition() {
        let mut issue = blocked_issue(None, None);
        // status != "blocked"
        issue.status = "todo".to_string();
        assert!(!issue.is_prospective_blocked_transition());

        // status == "blocked" but transitionAt is None
        issue.status = "blocked".to_string();
        assert!(!issue.is_prospective_blocked_transition());

        // status == "blocked" + transitionAt pre-rollout
        let pre = routable_blocked_rollout_at() - chrono::Duration::seconds(1);
        issue.blocked_transition_at = Some(pre);
        assert!(!issue.is_prospective_blocked_transition());

        // exactly at rollout → prospective (>= rollout)
        issue.blocked_transition_at = Some(routable_blocked_rollout_at());
        assert!(issue.is_prospective_blocked_transition());

        // post-rollout → prospective
        let post = routable_blocked_rollout_at() + chrono::Duration::seconds(1);
        issue.blocked_transition_at = Some(post);
        assert!(issue.is_prospective_blocked_transition());
    }

    #[tokio::test]
    async fn wakes_agent_and_records_delivery_on_prospective_transition() {
        let issue = blocked_issue(
            Some(routable_blocked_rollout_at() + chrono::Duration::seconds(1)),
            None,
        );
        let wakeup = FakeWakeup::default();
        let marker = FakeMarker::default();
        let now: DateTime<Utc> = DateTime::parse_from_rfc3339("2026-07-23T18:30:00.000Z")
            .unwrap()
            .with_timezone(&Utc);

        let delivered = deliver_agent_unblock_notification(DeliverAgentUnblockNotificationInput {
            issue: &issue,
            wakeup: &wakeup,
            marker: &marker,
            now: Some(Box::new(move || now)),
        })
        .await;
        assert!(delivered);

        let calls = wakeup.calls.lock().await.clone();
        assert_eq!(calls.len(), 1);
        let (called_agent_id, request) = &calls[0];
        assert_eq!(called_agent_id, AGENT_ID);
        assert_eq!(request.source, "automation");
        assert_eq!(request.trigger_detail, "system");
        assert_eq!(request.reason, "issue_unblock_requested");
        assert_eq!(
            request.idempotency_key,
            format!(
                "issue-unblock:{}:{}",
                ISSUE_ID,
                issue
                    .blocked_transition_at
                    .unwrap()
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            )
        );
        assert_eq!(request.payload.issue_id, ISSUE_ID);
        assert_eq!(request.payload.action, "Review the finding");
        assert_eq!(
            request.context_snapshot.wake_reason,
            "issue_unblock_requested"
        );
        assert_eq!(request.context_snapshot.issue_id, ISSUE_ID);
        assert_eq!(request.context_snapshot.task_id, ISSUE_ID);

        let marks = marker.marks.lock().await.clone();
        assert_eq!(marks, vec![now]);
    }

    #[tokio::test]
    async fn leaves_pre_rollout_blocked_issues_untouched() {
        let issue = blocked_issue(
            Some(routable_blocked_rollout_at() - chrono::Duration::seconds(1)),
            None,
        );
        let wakeup = FakeWakeup::default();
        let marker = FakeMarker::default();

        let delivered = deliver_agent_unblock_notification(DeliverAgentUnblockNotificationInput {
            issue: &issue,
            wakeup: &wakeup,
            marker: &marker,
            now: None,
        })
        .await;
        assert!(!delivered);
        assert!(wakeup.calls.lock().await.is_empty());
        assert!(marker.marks.lock().await.is_empty());
    }

    #[tokio::test]
    async fn deduplicates_first_transition_and_notifies_after_flap() {
        let first = routable_blocked_rollout_at() + chrono::Duration::seconds(1);
        let second = routable_blocked_rollout_at() + chrono::Duration::seconds(2);

        let wakeup = FakeWakeup::default();
        let marker = FakeMarker::default();

        // First call: already notified → noop
        let issue_first = blocked_issue(Some(first), Some(Utc::now()));
        let delivered_first =
            deliver_agent_unblock_notification(DeliverAgentUnblockNotificationInput {
                issue: &issue_first,
                wakeup: &wakeup,
                marker: &marker,
                now: None,
            })
            .await;
        assert!(!delivered_first);

        // Second call: new transitionAt → wakeup
        let issue_second = blocked_issue(Some(second), None);
        let delivered_second =
            deliver_agent_unblock_notification(DeliverAgentUnblockNotificationInput {
                issue: &issue_second,
                wakeup: &wakeup,
                marker: &marker,
                now: None,
            })
            .await;
        assert!(delivered_second);

        let calls = wakeup.calls.lock().await.clone();
        assert_eq!(
            calls.len(),
            1,
            "wakeup should fire exactly once across the flap"
        );
        let (_called_agent_id, request) = &calls[0];
        assert!(request
            .idempotency_key
            .contains(&second.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)));
    }

    #[tokio::test]
    async fn skips_board_owner() {
        let mut issue = blocked_issue(
            Some(routable_blocked_rollout_at() + chrono::Duration::seconds(1)),
            None,
        );
        issue.unblock_descriptor = Some(IssueUnblockDescriptor {
            owner: IssueUnblockOwner::Board,
            action: "Review".to_string(),
        });
        let wakeup = FakeWakeup::default();
        let marker = FakeMarker::default();

        let delivered = deliver_agent_unblock_notification(DeliverAgentUnblockNotificationInput {
            issue: &issue,
            wakeup: &wakeup,
            marker: &marker,
            now: None,
        })
        .await;
        assert!(!delivered);
        assert!(wakeup.calls.lock().await.is_empty());
    }

    #[tokio::test]
    async fn skips_user_owner() {
        let mut issue = blocked_issue(
            Some(routable_blocked_rollout_at() + chrono::Duration::seconds(1)),
            None,
        );
        issue.unblock_descriptor = Some(IssueUnblockDescriptor {
            owner: IssueUnblockOwner::User {
                user_id: "user-1".to_string(),
            },
            action: "Review".to_string(),
        });
        let wakeup = FakeWakeup::default();
        let marker = FakeMarker::default();

        let delivered = deliver_agent_unblock_notification(DeliverAgentUnblockNotificationInput {
            issue: &issue,
            wakeup: &wakeup,
            marker: &marker,
            now: None,
        })
        .await;
        assert!(!delivered);
        assert!(wakeup.calls.lock().await.is_empty());
    }

    #[tokio::test]
    async fn skips_when_unblock_descriptor_missing() {
        let mut issue = blocked_issue(
            Some(routable_blocked_rollout_at() + chrono::Duration::seconds(1)),
            None,
        );
        issue.unblock_descriptor = None;
        let wakeup = FakeWakeup::default();
        let marker = FakeMarker::default();

        let delivered = deliver_agent_unblock_notification(DeliverAgentUnblockNotificationInput {
            issue: &issue,
            wakeup: &wakeup,
            marker: &marker,
            now: None,
        })
        .await;
        assert!(!delivered);
        assert!(wakeup.calls.lock().await.is_empty());
    }

    #[tokio::test]
    async fn uses_utc_now_when_now_not_provided() {
        let issue = blocked_issue(
            Some(routable_blocked_rollout_at() + chrono::Duration::seconds(1)),
            None,
        );
        let wakeup = FakeWakeup::default();
        let marker = FakeMarker::default();

        let delivered = deliver_agent_unblock_notification(DeliverAgentUnblockNotificationInput {
            issue: &issue,
            wakeup: &wakeup,
            marker: &marker,
            now: None,
        })
        .await;
        assert!(delivered);
        let marks = marker.marks.lock().await.clone();
        assert_eq!(marks.len(), 1);
        // 距当前时间不超过 5s
        let delta = (Utc::now() - marks[0]).num_seconds().abs();
        assert!(
            delta < 5,
            "mark_notified should use Utc::now, delta={delta}s"
        );
    }

    #[test]
    fn owner_is_agent_helper() {
        assert!(IssueUnblockOwner::Agent {
            agent_id: "a".into()
        }
        .is_agent());
        assert!(!IssueUnblockOwner::User {
            user_id: "u".into()
        }
        .is_agent());
        assert!(!IssueUnblockOwner::Board.is_agent());
    }
}
