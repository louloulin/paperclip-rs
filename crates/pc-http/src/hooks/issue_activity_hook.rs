//! `IssueActivityHook` — R602。
//!
//! 把 IssueService 的 `on_created` 事件桥接到 ActivityLog + Realtime + PluginEventBus。
//!
//! 设计目标：
//! - 高内聚：单一职责（issue.created → 多路事件）
//! - 低耦合：不直接访问 IssueService；只接受 hook trait + AppState
//! - 失败容忍：emit/publish 失败仅 trace warn

use async_trait::async_trait;
use pc_activity::{ActivityActor, ActivityEvent, ActivityKind};
use pc_issues::IssueHook;
use pc_plugin_host::plugin_event_bus::{ActorType, PluginEvent};
use pc_realtime::LiveEvent;
use std::sync::Arc;

use crate::state::AppState;

#[derive(Clone)]
pub struct IssueActivityHook {
    state: Arc<AppState>,
}

impl std::fmt::Debug for IssueActivityHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssueActivityHook").finish()
    }
}

impl IssueActivityHook {
    #[must_use]
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl IssueHook for IssueActivityHook {
    async fn on_created(
        &self,
        row: &pc_repos::issue::IssueRow,
    ) -> pc_issues::IssueServiceResult<()> {
        let company_id = row.company_id;
        let issue_id = row.id;
        let event_type = "issue.created";

        let activity_event = ActivityEvent::new(
            ActivityKind::IssueCreated,
            ActivityActor::System {
                component: "issue_service".into(),
            },
            "issue",
            issue_id,
        )
        .with_company(company_id)
        .with_payload(serde_json::json!({
            "issue_id": issue_id.to_string(),
            "company_id": company_id.to_string(),
            "title": row.title,
            "status": row.status,
            "priority": row.priority,
        }));

        if let Err(e) = self.state.activity.emit(activity_event.clone()).await {
            tracing::warn!(issue_id = %issue_id, error = %e, "activity emit failed");
        }

        self.state.realtime.publish(
            LiveEvent::new(event_type, "issue", issue_id)
                .with_company(company_id)
                .with_actor("system"),
        );

        // 推 plugin event bus（host → plugin 订阅链）
        let plugin_event = PluginEvent {
            event_id: activity_event.id.0.to_string(),
            event_type: activity_event.kind.as_str().to_string(),
            occurred_at: activity_event.occurred_at,
            actor_id: None,
            actor_type: Some(ActorType::System),
            entity_id: Some(issue_id.to_string()),
            entity_type: Some(activity_event.subject_kind.clone()),
            company_id: company_id.to_string(),
            payload: activity_event.payload.clone(),
        };
        let _ = self.state.plugin_event_bus.emit(plugin_event).await;

        Ok(())
    }
}
