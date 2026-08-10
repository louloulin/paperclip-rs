//! `CompanyActivityHook` — 把 `CompanyService` 的 lifecycle event 转换为
//! ActivityLog + Realtime + PluginEventBus 三路 fanout。
//!
//! 设计目标：
//! - 高内聚：单一职责（lifecycle → 多路事件）
//! - 低耦合：不直接访问 CompanyService / CompanyRepo；只接受 AppState 子集
//! - 失败容忍：每路失败仅记 warn，不影响主流程

use async_trait::async_trait;
use pc_activity::{ActivityActor, ActivityEvent, ActivityKind};
use pc_companies::{CompanyActor, CompanyHook, CompanyLifecycleEvent, CompanyServiceResult};
use pc_plugin_host::plugin_event_bus::{ActorType, PluginEvent};
use pc_realtime::LiveEvent;
use std::sync::Arc;

use crate::state::AppState;

/// 把 lifecycle event 转成三路事件的 hook。
///
/// 字段都是 `Arc` 共享，clone 便宜。
#[derive(Clone)]
pub struct CompanyActivityHook {
    state: Arc<AppState>,
}

impl std::fmt::Debug for CompanyActivityHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompanyActivityHook").finish()
    }
}

impl CompanyActivityHook {
    #[must_use]
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }
}

fn actor_for(actor: &CompanyActor) -> ActivityActor {
    // CompanyActor 现在只有 system / user_id 等简单字段
    // 复杂 user/agent 信息保留 payload 即可
    if actor.actor_type == "agent" {
        ActivityActor::System {
            component: "agent".into(),
        }
    } else if actor.actor_type == "user" {
        ActivityActor::System {
            component: "user".into(),
        }
    } else {
        ActivityActor::System {
            component: "system".into(),
        }
    }
}

fn kind_for(event: &CompanyLifecycleEvent) -> ActivityKind {
    match event {
        CompanyLifecycleEvent::Created { .. } => ActivityKind::CompanyCreated,
        CompanyLifecycleEvent::Updated { .. } => ActivityKind::CompanyUpdated,
        CompanyLifecycleEvent::Archived { .. } => ActivityKind::CompanyArchived,
        CompanyLifecycleEvent::Removed { .. } => ActivityKind::CompanyRemoved,
    }
}

fn subject_kind_for(event: &CompanyLifecycleEvent) -> &'static str {
    match event {
        CompanyLifecycleEvent::Created { .. }
        | CompanyLifecycleEvent::Updated { .. }
        | CompanyLifecycleEvent::Archived { .. }
        | CompanyLifecycleEvent::Removed { .. } => "company",
    }
}

fn live_event_for(event: &CompanyLifecycleEvent) -> Option<(&'static str, uuid::Uuid)> {
    match *event {
        CompanyLifecycleEvent::Created { id, .. } => Some(("company.created", id)),
        CompanyLifecycleEvent::Updated { id, .. } => Some(("company.updated", id)),
        CompanyLifecycleEvent::Archived { id, .. } => Some(("company.archived", id)),
        CompanyLifecycleEvent::Removed { id, .. } => Some(("company.removed", id)),
    }
}

#[async_trait]
impl CompanyHook for CompanyActivityHook {
    async fn on_lifecycle(
        &self,
        event: CompanyLifecycleEvent,
    ) -> CompanyServiceResult<()> {
        let id = match event {
            CompanyLifecycleEvent::Created { id, .. }
            | CompanyLifecycleEvent::Updated { id, .. }
            | CompanyLifecycleEvent::Archived { id, .. }
            | CompanyLifecycleEvent::Removed { id, .. } => id,
        };

        // 1. ActivityLog（in-memory 默认；可换 DB sink）
        let mut activity_event = ActivityEvent::new(
            kind_for(&event),
            actor_for(event_actor(&event)),
            subject_kind_for(&event),
            id,
        )
        .with_company(id);
        if let Some(payload) = payload_for(&event) {
            activity_event = activity_event.with_payload(payload);
        }
        if let Err(e) = self.state.activity.emit(activity_event.clone()).await {
            tracing::warn!(company_id = %id, error = %e, "activity emit failed");
        }

        // 2. PluginEventBus
        let plugin_event = PluginEvent {
            event_id: activity_event.id.0.to_string(),
            event_type: activity_event.kind.as_str().to_string(),
            occurred_at: activity_event.occurred_at,
            actor_id: None,
            actor_type: Some(ActorType::System),
            entity_id: Some(id.to_string()),
            entity_type: Some(activity_event.subject_kind.clone()),
            company_id: activity_event
                .company_id
                .map(|c| c.to_string())
                .unwrap_or_default(),
            payload: activity_event.payload.clone(),
        };
        let _ = self.state.plugin_event_bus.emit(plugin_event).await;

        // 3. Realtime（UI 实时更新）
        if let Some((event_name, resource_id)) = live_event_for(&event) {
            self.state.realtime.publish(
                LiveEvent::new(event_name, "company", resource_id)
                    .with_company(resource_id)
                    .with_actor("system"),
            );
        }

        Ok(())
    }
}

fn event_actor(event: &CompanyLifecycleEvent) -> &CompanyActor {
    match event {
        CompanyLifecycleEvent::Created { actor, .. }
        | CompanyLifecycleEvent::Updated { actor, .. }
        | CompanyLifecycleEvent::Archived { actor, .. }
        | CompanyLifecycleEvent::Removed { actor, .. } => actor,
    }
}

fn payload_for(event: &CompanyLifecycleEvent) -> Option<serde_json::Value> {
    match event {
        CompanyLifecycleEvent::Created { owner_principal_id, .. } => {
            Some(serde_json::json!({ "owner_principal_id": owner_principal_id }))
        }
        CompanyLifecycleEvent::Updated { patch, .. } => Some(serde_json::to_value(patch).unwrap_or_default()),
        CompanyLifecycleEvent::Archived { .. } | CompanyLifecycleEvent::Removed { .. } => None,
    }
}
