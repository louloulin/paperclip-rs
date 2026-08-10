//! End-to-end tests for `pc-issue-assignment-wakeup`.
//!
//! 包含：
//! - 纯函数 service 测试（注入 mock heartbeat）
//! - Hook 测试：BeforeQueue / AfterQueue / OnSkipped / OnSwallowed 触发
//! - Mock heartbeat 验证 payload 构造正确

use async_trait::async_trait;
use pc_issue_assignment_wakeup::{
    queue_issue_assignment_wakeup, IssueAssignmentSnapshot,
    IssueAssignmentWakeupDeps, IssueAssignmentWakeupHookEvent,
    IssueAssignmentWakeupOptions, IssueAssignmentWakeupService,
    IssueAssignmentWakeupService as _IssueAssignmentWakeupService,
    NoopIssueAssignmentWakeupHook, QueueIssueAssignmentWakeupOutcome, QueueRequest,
    RecordingIssueAssignmentWakeupHook, WakeupRequestedByActorType, WakeupSource,
    WakeupTriggerDetail,
};
use serde_json::{Map, Value};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

// ============================================================================
// Mock heartbeat
// ============================================================================

#[derive(Debug, Default)]
struct MockHeartbeat {
    calls: Mutex<Vec<(String, Map<String, Value>, Map<String, Value>)>>,
    fail_with: Mutex<Option<String>>,
}

impl MockHeartbeat {
    fn new() -> Self {
        Self::default()
    }

    fn failing(error: &str) -> Self {
        Self {
            fail_with: Mutex::new(Some(error.to_string())),
            ..Self::default()
        }
    }

    fn calls(&self) -> Vec<(String, Map<String, Value>, Map<String, Value>)> {
        self.calls.lock().unwrap().clone()
    }

    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

#[async_trait]
impl IssueAssignmentWakeupDeps for MockHeartbeat {
    async fn wakeup(
        &self,
        agent_id: &str,
        opts: IssueAssignmentWakeupOptions<'_>,
    ) -> Result<(), String> {
        if let Some(err) = self.fail_with.lock().unwrap().clone() {
            return Err(err);
        }
        let payload = opts.payload.unwrap_or_default();
        let ctx = opts.context_snapshot.unwrap_or_default();
        self.calls
            .lock()
            .unwrap()
            .push((agent_id.to_string(), payload, ctx));
        Ok(())
    }
}

// ============================================================================
// 基本行为测试（与 Node 1:1 对齐）
// ============================================================================

#[tokio::test]
async fn r662_skips_when_no_assignee() {
    let hb = Arc::new(MockHeartbeat::new());
    let issue = IssueAssignmentSnapshot {
        id: Uuid::new_v4().to_string(),
        assignee_agent_id: None,
        status: "todo".to_string(),
    };
    let req = QueueRequest {
        issue: issue.clone(),
        reason: "assignee updated".to_string(),
        mutation: "assigned".to_string(),
        context_source: "test".to_string(),
        ..Default::default()
    };

    let outcome = queue_issue_assignment_wakeup(hb.as_ref(), req).await;
    assert_eq!(outcome, QueueIssueAssignmentWakeupOutcome::Skipped);
    assert_eq!(hb.call_count(), 0, "no wakeup call expected");
}

#[tokio::test]
async fn r662_skips_when_status_is_backlog() {
    let hb = Arc::new(MockHeartbeat::new());
    let issue = IssueAssignmentSnapshot {
        id: Uuid::new_v4().to_string(),
        assignee_agent_id: Some(Uuid::new_v4().to_string()),
        status: "backlog".to_string(),
    };
    let req = QueueRequest {
        issue,
        reason: "reassign".to_string(),
        mutation: "assigned".to_string(),
        context_source: "test".to_string(),
        ..Default::default()
    };

    let outcome = queue_issue_assignment_wakeup(hb.as_ref(), req).await;
    assert_eq!(outcome, QueueIssueAssignmentWakeupOutcome::Skipped);
    assert_eq!(hb.call_count(), 0, "no wakeup call for backlog status");
}

#[tokio::test]
async fn r662_calls_wakeup_when_assignee_present() {
    let hb = Arc::new(MockHeartbeat::new());
    let agent_id = Uuid::new_v4().to_string();
    let issue_id = Uuid::new_v4().to_string();
    let issue = IssueAssignmentSnapshot {
        id: issue_id.clone(),
        assignee_agent_id: Some(agent_id.clone()),
        status: "todo".to_string(),
    };
    let req = QueueRequest {
        issue,
        reason: "test reason".to_string(),
        mutation: "assigned".to_string(),
        context_source: "test-source".to_string(),
        ..Default::default()
    };

    let outcome = queue_issue_assignment_wakeup(hb.as_ref(), req).await;
    assert_eq!(outcome, QueueIssueAssignmentWakeupOutcome::Succeeded);
    assert_eq!(hb.call_count(), 1);
    let calls = hb.calls();
    assert_eq!(calls[0].0, agent_id);
    assert_eq!(
        calls[0].1.get("issueId"),
        Some(&Value::String(issue_id.clone()))
    );
    assert_eq!(
        calls[0].1.get("mutation"),
        Some(&Value::String("assigned".to_string()))
    );
}

#[tokio::test]
async fn r662_payload_includes_task_key_when_provided() {
    let hb = Arc::new(MockHeartbeat::new());
    let agent_id = Uuid::new_v4().to_string();
    let task_key = "task-123".to_string();
    let issue = IssueAssignmentSnapshot {
        id: Uuid::new_v4().to_string(),
        assignee_agent_id: Some(agent_id.clone()),
        status: "todo".to_string(),
    };
    let req = QueueRequest {
        issue,
        reason: "test".to_string(),
        mutation: "assigned".to_string(),
        context_source: "ctx-source".to_string(),
        task_key: Some(task_key.clone()),
        ..Default::default()
    };

    queue_issue_assignment_wakeup(hb.as_ref(), req).await;
    let calls = hb.calls();
    assert_eq!(calls[0].1.get("taskKey"), Some(&Value::String(task_key.clone())));
    assert_eq!(calls[0].2.get("taskKey"), Some(&Value::String(task_key)));
}

#[tokio::test]
async fn r662_payload_omits_task_key_when_none() {
    let hb = Arc::new(MockHeartbeat::new());
    let issue = IssueAssignmentSnapshot {
        id: Uuid::new_v4().to_string(),
        assignee_agent_id: Some(Uuid::new_v4().to_string()),
        status: "todo".to_string(),
    };
    let req = QueueRequest {
        issue,
        reason: "test".to_string(),
        mutation: "assigned".to_string(),
        context_source: "ctx-source".to_string(),
        task_key: None,
        ..Default::default()
    };

    queue_issue_assignment_wakeup(hb.as_ref(), req).await;
    let calls = hb.calls();
    assert!(!calls[0].1.contains_key("taskKey"), "taskKey should be absent");
}

#[tokio::test]
async fn r662_context_snapshot_includes_source() {
    let hb = Arc::new(MockHeartbeat::new());
    let issue = IssueAssignmentSnapshot {
        id: Uuid::new_v4().to_string(),
        assignee_agent_id: Some(Uuid::new_v4().to_string()),
        status: "todo".to_string(),
    };
    let req = QueueRequest {
        issue,
        reason: "test".to_string(),
        mutation: "assigned".to_string(),
        context_source: "my-source".to_string(),
        ..Default::default()
    };

    queue_issue_assignment_wakeup(hb.as_ref(), req).await;
    let calls = hb.calls();
    assert_eq!(
        calls[0].2.get("source"),
        Some(&Value::String("my-source".to_string()))
    );
}

// ============================================================================
// 错误处理
// ============================================================================

#[tokio::test]
async fn r662_swallowed_default_no_rethrow_returns_success() {
    // pc-repos 行为：当 rethrow_on_error=false（默认），
    // heartbeat 失败被静默吞咽，返回 Succeeded（与 Node .catch → return null 1:1）
    let hb = Arc::new(MockHeartbeat::failing("network error"));
    let issue = IssueAssignmentSnapshot {
        id: Uuid::new_v4().to_string(),
        assignee_agent_id: Some(Uuid::new_v4().to_string()),
        status: "todo".to_string(),
    };
    let req = QueueRequest {
        issue,
        reason: "test".to_string(),
        mutation: "assigned".to_string(),
        context_source: "ctx".to_string(),
        // rethrow_on_error 默认 false
        ..Default::default()
    };

    let outcome = queue_issue_assignment_wakeup(hb.as_ref(), req).await;
    assert_eq!(outcome, QueueIssueAssignmentWakeupOutcome::Succeeded);
}

#[tokio::test]
async fn r662_rethrow_on_error_returns_swallowed_with_message() {
    // pc-repos 行为：当 rethrow_on_error=true，
    // heartbeat 失败时返回 Swallowed(err) 信号（与 Node rethrow 对齐）
    let hb = Arc::new(MockHeartbeat::failing("network error"));
    let issue = IssueAssignmentSnapshot {
        id: Uuid::new_v4().to_string(),
        assignee_agent_id: Some(Uuid::new_v4().to_string()),
        status: "todo".to_string(),
    };
    let req = QueueRequest {
        issue,
        reason: "test".to_string(),
        mutation: "assigned".to_string(),
        context_source: "ctx".to_string(),
        rethrow_on_error: true,
        ..Default::default()
    };

    let outcome = queue_issue_assignment_wakeup(hb.as_ref(), req).await;
    match outcome {
        QueueIssueAssignmentWakeupOutcome::Swallowed(err) => {
            assert_eq!(err, "network error");
        }
        _ => panic!("expected Swallowed outcome, got {:?}", outcome),
    }
}

// ============================================================================
// Hook 测试
// ============================================================================

#[tokio::test]
async fn r662_hook_before_and_after_queue() {
    let hb = Arc::new(MockHeartbeat::new());
    let hook = Arc::new(RecordingIssueAssignmentWakeupHook::new());
    let svc = IssueAssignmentWakeupService::with_hook(hook.clone());

    let agent_id = Uuid::new_v4().to_string();
    let issue_id = Uuid::new_v4().to_string();
    let issue = IssueAssignmentSnapshot {
        id: issue_id.clone(),
        assignee_agent_id: Some(agent_id.clone()),
        status: "todo".to_string(),
    };
    let req = QueueRequest {
        issue,
        reason: "test".to_string(),
        mutation: "assigned".to_string(),
        context_source: "ctx".to_string(),
        ..Default::default()
    };

    svc.queue(hb.as_ref(), req).await;

    let events = hook.events();
    assert_eq!(events.len(), 2);
    assert!(matches!(
        events[0],
        IssueAssignmentWakeupHookEvent::BeforeQueue { .. }
    ));
    assert!(matches!(
        events[1],
        IssueAssignmentWakeupHookEvent::AfterQueue { .. }
    ));
}

#[tokio::test]
async fn r662_hook_on_skipped() {
    let hb = Arc::new(MockHeartbeat::new());
    let hook = Arc::new(RecordingIssueAssignmentWakeupHook::new());
    let svc = IssueAssignmentWakeupService::with_hook(hook.clone());

    let issue = IssueAssignmentSnapshot {
        id: Uuid::new_v4().to_string(),
        assignee_agent_id: None,
        status: "todo".to_string(),
    };
    let req = QueueRequest {
        issue,
        reason: "test".to_string(),
        mutation: "assigned".to_string(),
        context_source: "ctx".to_string(),
        ..Default::default()
    };

    svc.queue(hb.as_ref(), req).await;

    let events = hook.events();
    assert_eq!(events.len(), 2, "BeforeQueue + OnSkipped");
    assert!(matches!(
        events[1],
        IssueAssignmentWakeupHookEvent::OnSkipped { .. }
    ));
}

#[tokio::test]
async fn r662_hook_on_skipped_backlog() {
    let hb = Arc::new(MockHeartbeat::new());
    let hook = Arc::new(RecordingIssueAssignmentWakeupHook::new());
    let svc = IssueAssignmentWakeupService::with_hook(hook.clone());

    let issue = IssueAssignmentSnapshot {
        id: Uuid::new_v4().to_string(),
        assignee_agent_id: Some(Uuid::new_v4().to_string()),
        status: "backlog".to_string(),
    };
    let req = QueueRequest {
        issue,
        reason: "test".to_string(),
        mutation: "assigned".to_string(),
        context_source: "ctx".to_string(),
        ..Default::default()
    };

    svc.queue(hb.as_ref(), req).await;

    let events = hook.events();
    assert!(matches!(
        events[1],
        IssueAssignmentWakeupHookEvent::OnSkipped { ref status, .. } if status == "backlog"
    ));
}

#[tokio::test]
async fn r662_hook_on_swallowed() {
    let hb = Arc::new(MockHeartbeat::failing("oops"));
    let hook = Arc::new(RecordingIssueAssignmentWakeupHook::new());
    let svc = IssueAssignmentWakeupService::with_hook(hook.clone());

    let issue = IssueAssignmentSnapshot {
        id: Uuid::new_v4().to_string(),
        assignee_agent_id: Some(Uuid::new_v4().to_string()),
        status: "todo".to_string(),
    };
    let req = QueueRequest {
        issue,
        reason: "test".to_string(),
        mutation: "assigned".to_string(),
        context_source: "ctx".to_string(),
        rethrow_on_error: true, // 必须为 true 才会触发 OnSwallowed hook
        ..Default::default()
    };

    svc.queue(hb.as_ref(), req).await;

    let events = hook.events();
    assert_eq!(events.len(), 2, "BeforeQueue + OnSwallowed");
    assert!(matches!(
        events[1],
        IssueAssignmentWakeupHookEvent::OnSwallowed { .. }
    ));
}

#[tokio::test]
async fn r662_hook_clear() {
    let hb = Arc::new(MockHeartbeat::new());
    let hook = Arc::new(RecordingIssueAssignmentWakeupHook::new());
    let svc = IssueAssignmentWakeupService::with_hook(hook.clone());

    let issue = IssueAssignmentSnapshot {
        id: Uuid::new_v4().to_string(),
        assignee_agent_id: Some(Uuid::new_v4().to_string()),
        status: "todo".to_string(),
    };
    let req = QueueRequest {
        issue,
        reason: "test".to_string(),
        mutation: "assigned".to_string(),
        context_source: "ctx".to_string(),
        ..Default::default()
    };

    svc.queue(hb.as_ref(), req).await;
    assert_eq!(hook.len(), 2);
    hook.clear();
    assert!(hook.is_empty());
}

#[test]
fn r662_default_service_uses_noop_hook() {
    let svc = IssueAssignmentWakeupService::new();
    let hook = svc.hook();
    // Just exercise — no panic = pass
    let issue = IssueAssignmentSnapshot::default();
    hook.before_queue(&issue);
    hook.after_queue("i", "a");
    hook.on_skipped("i", "backlog");
    hook.on_swallowed("i", "err");
}

// ============================================================================
// 枚举字符串转换
// ============================================================================

#[test]
fn r662_wakeup_source_as_str() {
    assert_eq!(WakeupSource::Timer.as_str(), "timer");
    assert_eq!(WakeupSource::Assignment.as_str(), "assignment");
    assert_eq!(WakeupSource::OnDemand.as_str(), "on_demand");
    assert_eq!(WakeupSource::Automation.as_str(), "automation");
}

#[test]
fn r662_wakeup_trigger_detail_as_str() {
    assert_eq!(WakeupTriggerDetail::Manual.as_str(), "manual");
    assert_eq!(WakeupTriggerDetail::Ping.as_str(), "ping");
    assert_eq!(WakeupTriggerDetail::Callback.as_str(), "callback");
    assert_eq!(WakeupTriggerDetail::System.as_str(), "system");
}

#[test]
fn r662_actor_type_as_str() {
    assert_eq!(WakeupRequestedByActorType::User.as_str(), "user");
    assert_eq!(WakeupRequestedByActorType::Agent.as_str(), "agent");
    assert_eq!(WakeupRequestedByActorType::System.as_str(), "system");
}
