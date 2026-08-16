//! Routines domain hook -> realtime LiveEvent bridge.
//!
//! 与 Node 端 services/routines.ts 通过 emitter.on("routineRunSkipped", ...)
//! 推送到 realtime hub 完全等价：把 RoutineHookEvent::RunSkipped 翻译成
//! LiveEvent { event: "routine.run_skipped", resource: "routine_run",
//! resource_id: run_id, company_id }，让前端 WS 客户端能实时收到
//! routine 自动调度被抑制的事件。

use std::sync::Arc;

use async_trait::async_trait;
use pc_errors::Result;
use pc_routines::{RoutineHook, RoutineHookEvent};

use crate::LiveEvent;

/// Bridge: 持有 LiveEvent 句柄 + RoutineHook 实现。
#[derive(Clone)]
pub struct RealtimeRoutineHook {
    handle: crate::RealtimeHandle,
}

impl RealtimeRoutineHook {
    #[must_use]
    pub fn new(handle: crate::RealtimeHandle) -> Self {
        Self { handle }
    }

    #[must_use]
    pub fn into_arc(self) -> Arc<dyn RoutineHook> {
        Arc::new(self)
    }
}

#[async_trait]
impl RoutineHook for RealtimeRoutineHook {
    async fn on_routine_event(&self, event: RoutineHookEvent) -> Result<()> {
        match event {
            RoutineHookEvent::RunSkipped {
                run_id,
                routine_id,
                company_id,
                source,
                trigger_id,
                reason,
                details,
            } => {
                let mut live = LiveEvent::new("routine.run_skipped", "routine_run", run_id)
                    .with_company(company_id)
                    .with_actor(format!("routine-{source}"));
                let mut payload = serde_json::json!({
                    "runId": run_id,
                    "routineId": routine_id,
                    "companyId": company_id,
                    "triggerId": trigger_id,
                    "source": source,
                    "reason": reason,
                });
                if let Some(d) = details {
                    payload["details"] = d;
                }
                live = live.with_data(payload);
                self.handle.publish(live);
            }
            // 其他事件暂不桥接（CRUD 由 activity_log 覆盖，dispatch/finalize 由 RunSkipped 路径覆盖）。
            RoutineHookEvent::Created { .. }
            | RoutineHookEvent::Updated { .. }
            | RoutineHookEvent::Archived { .. }
            | RoutineHookEvent::TriggerCreated { .. }
            | RoutineHookEvent::TriggerUpdated { .. }
            | RoutineHookEvent::TriggerDeleted { .. }
            | RoutineHookEvent::RunDispatched { .. }
            | RoutineHookEvent::RunFinalized { .. } => {}
        }
        Ok(())
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use pc_routines::RoutineHookEvent;
    use std::sync::Arc;
    use uuid::Uuid;

    fn new_handle() -> crate::RealtimeHandle {
        crate::RealtimeHandle::start_with_replay(16, 16)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn run_skipped_publishes_live_event_with_company_id() {
        let handle = new_handle();
        let mut rx = handle.subscribe();
        let hook = RealtimeRoutineHook::new(handle.clone()).into_arc();

        let run_id = Uuid::new_v4();
        let routine_id = Uuid::new_v4();
        let company_id = Uuid::new_v4();
        let trigger_id = Uuid::new_v4();
        let details = serde_json::json!({"reason": "paused"});

        hook.on_routine_event(RoutineHookEvent::RunSkipped {
            run_id,
            routine_id,
            company_id,
            source: "schedule".into(),
            trigger_id,
            reason: "paused".into(),
            details: Some(details.clone()),
        })
        .await
        .expect("hook");

        let ev = rx.recv().await.expect("event");
        let ev = Arc::try_unwrap(ev).unwrap_or_else(|arc| (*arc).clone());
        assert_eq!(ev.event, "routine.run_skipped");
        assert_eq!(ev.resource, "routine_run");
        assert_eq!(ev.resource_id, run_id);
        assert_eq!(ev.company_id, Some(company_id));
        let data = ev.data.expect("data");
        assert_eq!(data["runId"], run_id.to_string());
        assert_eq!(data["reason"], "paused");
        assert_eq!(data["details"], details);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn run_dispatched_is_silently_dropped() {
        let handle = new_handle();
        let mut rx = handle.subscribe();
        let hook = RealtimeRoutineHook::new(handle.clone()).into_arc();

        hook.on_routine_event(RoutineHookEvent::RunDispatched {
            run_id: Uuid::new_v4(),
            routine_id: Uuid::new_v4(),
            company_id: Uuid::new_v4(),
            source: "schedule".into(),
            status: "running".into(),
        })
        .await
        .expect("hook");

        // No event published for RunDispatched
        let result = rx.try_recv();
        assert!(result.is_err(), "RunDispatched should not publish LiveEvent");
    }

    #[test]
    fn hook_constructs_via_arc() {
        let handle = new_handle();
        let arc_hook: Arc<dyn RoutineHook> = RealtimeRoutineHook::new(handle).into_arc();
        let _ = arc_hook; // type assertion
    }
}
