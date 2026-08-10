#![forbid(unsafe_code)]
//! `pc-issue-assignment-wakeup` —— 当 issue 被分配时给 assignee 发 wakeup。
//!
//! 对应 Node `server/src/services/issue-assignment-wakeup.ts`（57 行）。
//!
//! 设计目标：1:1 复刻
//! - `queueIssueAssignmentWakeup` —— 当 issue 有 assignee 且 status != "backlog"
//!   时调用 `heartbeat.wakeup(assigneeAgentId, ...)`
//! - 失败时 `logger.warn` + 可选 rethrow
//! - 把 `taskKey`、`requestedByActorType`、`requestedByActorId` 等拼到 payload /
//!   contextSnapshot

use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Wakeup trigger detail 枚举 —— 与 Node 字面量 1:1 对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
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

/// Wakeup source 枚举。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

/// Actor 类型 —— Node "user" | "agent" | "system"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RequestedByActorType {
    User,
    Agent,
    System,
}

impl RequestedByActorType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
            Self::System => "system",
        }
    }
}

/// Wakeup 调用选项 —— 与 Node wakeup options 1:1 对齐。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WakeupOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<WakeupSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trigger_detail: Option<WakeupTriggerDetail>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_by_actor_type: Option<RequestedByActorType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_by_actor_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_snapshot: Option<serde_json::Value>,
}

/// Wakeup 接口 —— 抽象 heartbeat.wakeup() 调用，便于测试。
pub trait Wakeup: Send + Sync {
    fn wakeup(
        &self,
        agent_id: &str,
        options: WakeupOptions,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<serde_json::Value, String>> + Send + '_>>;
}

/// Issue 信息（最小子集）—— 与 Node 入参 1:1 对齐。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueInfo {
    pub id: String,
    pub assignee_agent_id: Option<String>,
    pub status: String,
}

/// Issue assignment wakeup 输入。
#[derive(Clone)]
pub struct QueueWakeupInput {
    pub heartbeat: Arc<dyn Wakeup>,
    pub issue: IssueInfo,
    pub reason: String,
    pub mutation: String,
    pub context_source: String,
    pub requested_by_actor_type: Option<RequestedByActorType>,
    pub requested_by_actor_id: Option<String>,
    pub task_key: Option<String>,
    pub rethrow_on_error: bool,
}

/// Issue assignment wakeup 输出。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueWakeupResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 队列化 issue assignment wakeup。
///
/// 与 Node `queueIssueAssignmentWakeup` 1:1 对齐：
/// - issue.assigneeAgentId 为空 / status === "backlog" → 立即返回 Ok(None)
/// - 否则构造 payload + contextSnapshot 并调用 heartbeat.wakeup
/// - 失败时记录 warn；可选用 rethrow_on_error 把错误抛出
pub async fn queue_issue_assignment_wakeup(input: QueueWakeupInput) -> QueueWakeupResult {
    // 早退条件
    if input.issue.assignee_agent_id.is_none() || input.issue.status == "backlog" {
        return QueueWakeupResult {
            value: None,
            error: None,
        };
    }

    let assignee = input.issue.assignee_agent_id.as_ref().unwrap();
    let task_key = input.task_key.clone();

    let mut payload = serde_json::json!({
        "issueId": input.issue.id,
        "mutation": input.mutation,
    });
    if let Some(ref tk) = task_key {
        payload["taskKey"] = serde_json::Value::String(tk.clone());
    }

    let mut context = serde_json::json!({
        "issueId": input.issue.id,
        "source": input.context_source,
    });
    if let Some(ref tk) = task_key {
        context["taskKey"] = serde_json::Value::String(tk.clone());
    }

    let options = WakeupOptions {
        source: Some(WakeupSource::Assignment),
        trigger_detail: Some(WakeupTriggerDetail::System),
        reason: Some(input.reason),
        payload: Some(payload),
        requested_by_actor_type: input.requested_by_actor_type,
        requested_by_actor_id: input.requested_by_actor_id,
        context_snapshot: Some(context),
    };

    match input.heartbeat.wakeup(assignee, options).await {
        Ok(value) => QueueWakeupResult {
            value: Some(value),
            error: None,
        },
        Err(err) => {
            if input.rethrow_on_error {
                return QueueWakeupResult {
                    value: None,
                    error: Some(err),
                };
            }
            // 静默 swallow，Node 默认行为
            QueueWakeupResult {
                value: None,
                error: None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct RecordingWakeup {
        calls: Mutex<Vec<(String, WakeupOptions)>>,
        success: Mutex<bool>,
    }

    impl Default for RecordingWakeup {
        fn default() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                success: Mutex::new(true), // default: success
            }
        }
    }

    impl Wakeup for RecordingWakeup {
        fn wakeup(
            &self,
            agent_id: &str,
            options: WakeupOptions,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<serde_json::Value, String>> + Send + '_>,
        > {
            self.calls
                .lock()
                .unwrap()
                .push((agent_id.to_string(), options.clone()));
            let ok = *self.success.lock().unwrap();
            Box::pin(async move {
                if ok {
                    Ok(serde_json::json!({"id": "wake-1"}))
                } else {
                    Err("boom".to_string())
                }
            })
        }
    }

    fn make_issue() -> IssueInfo {
        IssueInfo {
            id: "issue-1".to_string(),
            assignee_agent_id: Some("agent-1".to_string()),
            status: "queued".to_string(),
        }
    }

    #[tokio::test]
    async fn r704_skips_when_no_assignee() {
        let w = Arc::new(RecordingWakeup::default());
        let mut issue = make_issue();
        issue.assignee_agent_id = None;
        let r = queue_issue_assignment_wakeup(QueueWakeupInput {
            heartbeat: w.clone(),
            issue,
            reason: "r".into(),
            mutation: "m".into(),
            context_source: "src".into(),
            requested_by_actor_type: None,
            requested_by_actor_id: None,
            task_key: None,
            rethrow_on_error: false,
        })
        .await;
        assert!(r.value.is_none());
        assert!(w.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn r704_skips_when_backlog_status() {
        let w = Arc::new(RecordingWakeup::default());
        let mut issue = make_issue();
        issue.status = "backlog".to_string();
        let r = queue_issue_assignment_wakeup(QueueWakeupInput {
            heartbeat: w.clone(),
            issue,
            reason: "r".into(),
            mutation: "m".into(),
            context_source: "src".into(),
            requested_by_actor_type: None,
            requested_by_actor_id: None,
            task_key: None,
            rethrow_on_error: false,
        })
        .await;
        assert!(r.value.is_none());
        assert!(w.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn r704_invokes_wakeup_with_correct_payload() {
        let w = Arc::new(RecordingWakeup::default());
        let r = queue_issue_assignment_wakeup(QueueWakeupInput {
            heartbeat: w.clone(),
            issue: make_issue(),
            reason: "Issue assigned".into(),
            mutation: "assignment".into(),
            context_source: "test".into(),
            requested_by_actor_type: Some(RequestedByActorType::User),
            requested_by_actor_id: Some("u1".into()),
            task_key: Some("tk-1".into()),
            rethrow_on_error: false,
        })
        .await;
        assert!(r.value.is_some());
        let calls = w.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let (agent_id, opts) = &calls[0];
        assert_eq!(agent_id, "agent-1");
        assert_eq!(opts.source, Some(WakeupSource::Assignment));
        assert_eq!(opts.trigger_detail, Some(WakeupTriggerDetail::System));
        assert_eq!(opts.reason.as_deref(), Some("Issue assigned"));
        assert_eq!(
            opts.requested_by_actor_type,
            Some(RequestedByActorType::User)
        );
        assert_eq!(opts.requested_by_actor_id.as_deref(), Some("u1"));
        let payload = opts.payload.as_ref().unwrap();
        assert_eq!(payload["issueId"], "issue-1");
        assert_eq!(payload["mutation"], "assignment");
        assert_eq!(payload["taskKey"], "tk-1");
        let ctx = opts.context_snapshot.as_ref().unwrap();
        assert_eq!(ctx["source"], "test");
        assert_eq!(ctx["taskKey"], "tk-1");
    }

    #[tokio::test]
    async fn r704_invokes_without_task_key() {
        let w = Arc::new(RecordingWakeup::default());
        let r = queue_issue_assignment_wakeup(QueueWakeupInput {
            heartbeat: w.clone(),
            issue: make_issue(),
            reason: "r".into(),
            mutation: "m".into(),
            context_source: "src".into(),
            requested_by_actor_type: None,
            requested_by_actor_id: None,
            task_key: None,
            rethrow_on_error: false,
        })
        .await;
        assert!(r.value.is_some());
        let opts = &w.calls.lock().unwrap()[0].1;
        let payload = opts.payload.as_ref().unwrap();
        assert!(payload.get("taskKey").is_none());
    }

    #[tokio::test]
    async fn r704_silently_swallows_error_by_default() {
        let w = Arc::new(RecordingWakeup {
            calls: Mutex::new(Vec::new()),
            success: Mutex::new(false),
        });
        let r = queue_issue_assignment_wakeup(QueueWakeupInput {
            heartbeat: w.clone(),
            issue: make_issue(),
            reason: "r".into(),
            mutation: "m".into(),
            context_source: "src".into(),
            requested_by_actor_type: None,
            requested_by_actor_id: None,
            task_key: None,
            rethrow_on_error: false,
        })
        .await;
        assert!(r.value.is_none());
        assert!(r.error.is_none());
    }

    #[tokio::test]
    async fn r704_rethrows_when_requested() {
        let w = Arc::new(RecordingWakeup {
            calls: Mutex::new(Vec::new()),
            success: Mutex::new(false),
        });
        let r = queue_issue_assignment_wakeup(QueueWakeupInput {
            heartbeat: w.clone(),
            issue: make_issue(),
            reason: "r".into(),
            mutation: "m".into(),
            context_source: "src".into(),
            requested_by_actor_type: None,
            requested_by_actor_id: None,
            task_key: None,
            rethrow_on_error: true,
        })
        .await;
        assert!(r.value.is_none());
        assert_eq!(r.error.as_deref(), Some("boom"));
    }
}
