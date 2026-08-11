//! Decision continuations → heartbeat runtime wakeup（原 `pc-decision-wakeup` 已下沉）。
//!
//! 对应 Node `server/src/services/decision-wakeup.ts`（32 行）。
//!
//! 设计目标：1:1 复刻
//! - `createDecisionWakeOriginAgent(wakeup)` —— 当 wakeup 不为 null 时返回
//!   一个 closure；否则返回 `async () => null`（disabled scheduler）
//! - closure 用 `source = "automation"`, `triggerDetail = "system"`,
//!   `reason = "decision_${outcome}"`, `payload = { issueId, decisionId, outcome }`

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Wakeup 调用签名 —— 1:1 对应 Node `(agentId, options) => Promise<unknown>`。
pub type WakeupFn = Arc<
    dyn for<'a> Fn(
            &'a str,
            &'a str,
            &'a str,
            String,
            serde_json::Value,
        ) -> Pin<Box<dyn Future<Output = serde_json::Value> + Send + 'a>>
        + Send
        + Sync,
>;

/// Wake origin agent 入参 —— 与 Node `WakeOriginAgent` 输入 1:1。
#[derive(Debug, Clone)]
pub struct WakeOriginAgentInput {
    pub agent_id: String,
    pub issue_id: String,
    pub decision_id: String,
    pub outcome: String,
}

/// Wake origin agent 闭包类型。
pub type WakeOriginAgent = Arc<
    dyn Fn(WakeOriginAgentInput) -> Pin<Box<dyn Future<Output = Option<serde_json::Value>> + Send>>
        + Send
        + Sync,
>;

/// 创建 wake origin agent。
pub fn create_decision_wake_origin_agent(wakeup: Option<WakeupFn>) -> WakeOriginAgent {
    if let Some(wakeup) = wakeup {
        Arc::new(move |input: WakeOriginAgentInput| {
            let reason = format!("decision_{}", input.outcome);
            let payload = serde_json::json!({
                "issueId": input.issue_id,
                "decisionId": input.decision_id,
                "outcome": input.outcome,
            });
            let wakeup = wakeup.clone();
            Box::pin(async move {
                Some(wakeup(&input.agent_id, "automation", "system", reason, payload).await)
            })
        })
    } else {
        Arc::new(|_input| Box::pin(async { None }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recording {
        calls: Mutex<Vec<(String, String, String, String, serde_json::Value)>>,
    }

    fn make_recording_wakeup() -> (WakeupFn, Arc<Recording>) {
        let recorded: Arc<Recording> = Arc::new(Recording::default());
        let r = recorded.clone();
        let wakeup: WakeupFn = Arc::new(
            move |agent_id: &str,
                  source: &str,
                  trigger: &str,
                  reason: String,
                  payload: serde_json::Value| {
                let r = r.clone();
                let agent_id = agent_id.to_string();
                let source = source.to_string();
                let trigger = trigger.to_string();
                Box::pin(async move {
                    r.calls
                        .lock()
                        .unwrap()
                        .push((agent_id, source, trigger, reason, payload));
                    serde_json::json!({"ok": true})
                })
            },
        );
        (wakeup, recorded)
    }

    #[tokio::test]
    async fn r705_null_wakeup_returns_null() {
        let f = create_decision_wake_origin_agent(None);
        let r = f(WakeOriginAgentInput {
            agent_id: "a1".into(),
            issue_id: "i1".into(),
            decision_id: "d1".into(),
            outcome: "approved".into(),
        })
        .await;
        assert!(r.is_none());
    }

    #[tokio::test]
    async fn r705_invokes_wakeup() {
        let (wakeup, _r) = make_recording_wakeup();
        let f = create_decision_wake_origin_agent(Some(wakeup));
        let r = f(WakeOriginAgentInput {
            agent_id: "a1".into(),
            issue_id: "i1".into(),
            decision_id: "d1".into(),
            outcome: "approved".into(),
        })
        .await;
        assert_eq!(r.unwrap()["ok"], true);
    }

    #[tokio::test]
    async fn r705_payload_contains_issue_decision_outcome() {
        let (wakeup, recorded) = make_recording_wakeup();
        let f = create_decision_wake_origin_agent(Some(wakeup));
        let _ = f(WakeOriginAgentInput {
            agent_id: "a1".into(),
            issue_id: "i-123".into(),
            decision_id: "d-456".into(),
            outcome: "rejected".into(),
        })
        .await;

        let calls = recorded.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        let (agent_id, source, trigger, reason, payload) = &calls[0];
        assert_eq!(agent_id, "a1");
        assert_eq!(source, "automation");
        assert_eq!(trigger, "system");
        assert_eq!(reason, "decision_rejected");
        assert_eq!(payload["issueId"], "i-123");
        assert_eq!(payload["decisionId"], "d-456");
        assert_eq!(payload["outcome"], "rejected");
    }

    #[tokio::test]
    async fn r705_outcome_in_reason() {
        let (wakeup, recorded) = make_recording_wakeup();
        let f = create_decision_wake_origin_agent(Some(wakeup));
        let _ = f(WakeOriginAgentInput {
            agent_id: "a".into(),
            issue_id: "i".into(),
            decision_id: "d".into(),
            outcome: "needs_revision".into(),
        })
        .await;

        let reason = &recorded.calls.lock().unwrap()[0].3;
        assert_eq!(reason, "decision_needs_revision");
    }
}
