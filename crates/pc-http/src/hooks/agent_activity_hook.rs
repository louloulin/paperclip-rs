//! `AgentActivityHook` — R594。
//!
//! 把 AgentService 的 lifecycle event (Terminated / Paused / Resumed)
//! 转换为 ActivityLog + Realtime 三路 fanout（plugin_event_bus 留给 plugin
//! 系统独立订阅，避免与 AgentLifecycleEvent 重复）。
//!
//! 设计目标：
//! - 高内聚：单一职责（agent lifecycle → 多路事件）
//! - 低耦合：不直接访问 AgentService；只接受 AppState 子集

use async_trait::async_trait;
use pc_activity::{ActivityActor, ActivityEvent, ActivityKind};
use pc_agent::AgentHook;
use pc_companies::CompanyServiceResult;
use pc_realtime::LiveEvent;
use std::sync::Arc;
use uuid::Uuid;

use crate::state::AppState;

#[derive(Clone)]
pub struct AgentActivityHook {
    state: Arc<AppState>,
}

impl std::fmt::Debug for AgentActivityHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentActivityHook").finish()
    }
}

impl AgentActivityHook {
    #[must_use]
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }
}

fn kind_for(event: &pc_agent::AgentLifecycleEvent) -> ActivityKind {
    use pc_agent::AgentLifecycleEvent::*;
    match event {
        Terminated { .. } => ActivityKind::AgentStopped,
        Paused { .. } => ActivityKind::AgentStopped,
        Resumed { .. } => ActivityKind::AgentStarted,
    }
}

fn subject_id(event: &pc_agent::AgentLifecycleEvent) -> Uuid {
    use pc_agent::AgentLifecycleEvent::*;
    match *event {
        Terminated { id, .. } | Paused { id, .. } | Resumed { id, .. } => id,
    }
}

fn company_id(event: &pc_agent::AgentLifecycleEvent) -> Option<Uuid> {
    use pc_agent::AgentLifecycleEvent::*;
    match *event {
        Terminated { company_id, .. }
        | Paused { company_id, .. }
        | Resumed { company_id, .. } => Some(company_id),
    }
}

fn live_event_for(event: &pc_agent::AgentLifecycleEvent) -> Option<(&'static str, Uuid)> {
    use pc_agent::AgentLifecycleEvent::*;
    match *event {
        Terminated { id, .. } => Some(("agent.terminated", id)),
        Paused { id, .. } => Some(("agent.paused", id)),
        Resumed { id, .. } => Some(("agent.resumed", id)),
    }
}

#[async_trait]
impl AgentHook for AgentActivityHook {
    async fn on_lifecycle(
        &self,
        event: pc_agent::AgentLifecycleEvent,
    ) -> pc_errors::Result<()> {
        let id = subject_id(&event);
        let mut activity_event = ActivityEvent::new(
            kind_for(&event),
            ActivityActor::System {
                component: "agent_supervisor".into(),
            },
            "agent",
            id,
        );
        if let Some(cid) = company_id(&event) {
            activity_event = activity_event.with_company(cid);
        }

        if let Err(e) = self.state.activity.emit(activity_event).await {
            tracing::warn!(agent_id = %id, error = %e, "activity emit failed");
        }

        if let Some((event_name, resource_id)) = live_event_for(&event) {
            let mut live = LiveEvent::new(event_name, "agent", resource_id)
                .with_actor("system");
            if let Some(cid) = company_id(&event) {
                live = live.with_company(cid);
            }
            self.state.realtime.publish(live);
        }

        // 让 CompanyServiceError 也能转换（hook trait 用 pc_errors::Result，
        // 但 CompanyServiceResult 别名 — 兼容性处理）
        let _ = CompanyServiceResult::<()>::Ok(());
        Ok(())
    }
}
