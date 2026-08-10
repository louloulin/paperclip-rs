//! `DecisionActivityHook` — R599。
//!
//! 把 DecisionService 的 lifecycle event (created/decided/dismissed/cancelled)
//! 转换为 ActivityLog + Realtime。
//!
//! 设计目标：
//! - 高内聚：单一职责（decision lifecycle → 多路事件）
//! - 低耦合：不直接访问 DecisionService；只接受 hook trait

use async_trait::async_trait;
use pc_activity::{ActivityActor, ActivityEvent, ActivityKind};
use pc_decisions::DecisionHook;
use pc_realtime::LiveEvent;
use std::sync::Arc;
use uuid::Uuid;

use crate::state::AppState;

#[derive(Clone)]
pub struct DecisionActivityHook {
    state: Arc<AppState>,
}

impl std::fmt::Debug for DecisionActivityHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecisionActivityHook").finish()
    }
}

impl DecisionActivityHook {
    #[must_use]
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl DecisionHook for DecisionActivityHook {
    async fn on_created(
        &self,
        row: &pc_repos::decision::DecisionRow,
    ) -> pc_decisions::DecisionServiceResult<()> {
        let company_id = row.company_id;
        let decision_id: Uuid = row.id;
        let kind = ActivityKind::DecisionProposed;
        let event_type = "decision.created";

        let activity_event = ActivityEvent::new(
            kind,
            ActivityActor::System {
                component: "decision_service".into(),
            },
            "decision",
            decision_id,
        )
        .with_company(company_id)
        .with_payload(serde_json::json!({
            "decision_id": decision_id.to_string(),
            "company_id": company_id.to_string(),
            "title": row.title,
        }));

        if let Err(e) = self.state.activity.emit(activity_event).await {
            tracing::warn!(decision_id = %decision_id, error = %e, "activity emit failed");
        }

        self.state.realtime.publish(
            LiveEvent::new(event_type, "decision", decision_id)
                .with_company(company_id)
                .with_actor("system"),
        );
        Ok(())
    }

    async fn on_decided(
        &self,
        row: &pc_repos::decision::DecisionRow,
        chosen_option_id: &str,
    ) -> pc_decisions::DecisionServiceResult<()> {
        let company_id = row.company_id;
        let decision_id: Uuid = row.id;
        let kind = ActivityKind::DecisionApproved;
        let event_type = "decision.decided";

        let activity_event = ActivityEvent::new(
            kind,
            ActivityActor::System {
                component: "decision_service".into(),
            },
            "decision",
            decision_id,
        )
        .with_company(company_id)
        .with_payload(serde_json::json!({
            "decision_id": decision_id.to_string(),
            "chosen_option_id": chosen_option_id,
        }));

        if let Err(e) = self.state.activity.emit(activity_event).await {
            tracing::warn!(decision_id = %decision_id, error = %e, "activity emit failed");
        }

        self.state.realtime.publish(
            LiveEvent::new(event_type, "decision", decision_id)
                .with_company(company_id)
                .with_actor("system"),
        );
        Ok(())
    }

    async fn on_dismissed(
        &self,
        row: &pc_repos::decision::DecisionRow,
    ) -> pc_decisions::DecisionServiceResult<()> {
        let company_id = row.company_id;
        let decision_id: Uuid = row.id;
        let kind = ActivityKind::DecisionDismissed;
        let event_type = "decision.dismissed";

        let activity_event = ActivityEvent::new(
            kind,
            ActivityActor::System {
                component: "decision_service".into(),
            },
            "decision",
            decision_id,
        )
        .with_company(company_id);

        if let Err(e) = self.state.activity.emit(activity_event).await {
            tracing::warn!(decision_id = %decision_id, error = %e, "activity emit failed");
        }

        self.state.realtime.publish(
            LiveEvent::new(event_type, "decision", decision_id)
                .with_company(company_id)
                .with_actor("system"),
        );
        Ok(())
    }

    async fn on_cancelled(
        &self,
        row: &pc_repos::decision::DecisionRow,
    ) -> pc_decisions::DecisionServiceResult<()> {
        let company_id = row.company_id;
        let decision_id: Uuid = row.id;
        let kind = ActivityKind::DecisionCancelled;
        let event_type = "decision.cancelled";

        let activity_event = ActivityEvent::new(
            kind,
            ActivityActor::System {
                component: "decision_service".into(),
            },
            "decision",
            decision_id,
        )
        .with_company(company_id);

        if let Err(e) = self.state.activity.emit(activity_event).await {
            tracing::warn!(decision_id = %decision_id, error = %e, "activity emit failed");
        }

        self.state.realtime.publish(
            LiveEvent::new(event_type, "decision", decision_id)
                .with_company(company_id)
                .with_actor("system"),
        );
        Ok(())
    }
}
