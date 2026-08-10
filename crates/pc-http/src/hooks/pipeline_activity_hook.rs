//! `PipelineActivityHook` — R603 v1。
//!
//! 把 PipelineService 的 4 个 lifecycle event
//! (Created / Updated / Archived / Deleted) 桥接到 ActivityLog + Realtime + PluginEventBus。
//!
//! 设计目标：
//! - 高内聚：单一职责（pipeline lifecycle → 多路事件）
//! - 低耦合：不直接访问 PipelineService；只接受 hook trait + AppState
//! - 失败容忍：emit/publish 失败仅 trace warn

use async_trait::async_trait;
use pc_activity::{ActivityActor, ActivityEvent, ActivityKind};
use pc_pipelines::{PipelineHook, PipelineServiceResult};
use pc_plugin_host::plugin_event_bus::{ActorType, PluginEvent};
use pc_realtime::LiveEvent;
use std::sync::Arc;
use uuid::Uuid;

use crate::state::AppState;

#[derive(Clone)]
pub struct PipelineActivityHook {
    state: Arc<AppState>,
}

impl std::fmt::Debug for PipelineActivityHook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipelineActivityHook").finish()
    }
}

impl PipelineActivityHook {
    #[must_use]
    pub fn new(state: Arc<AppState>) -> Self {
        Self { state }
    }
}

#[async_trait]
impl pc_pipelines::PipelineHook for PipelineActivityHook {
    async fn on_created(
        &self,
        row: &pc_repos::pipeline::PipelineRow,
    ) -> pc_pipelines::PipelineServiceResult<()> {
        let company_id = row.company_id;
        let pipeline_id = row.id;

        let activity_event = ActivityEvent::new(
            ActivityKind::PipelineCreated,
            ActivityActor::System {
                component: "pipeline_service".into(),
            },
            "pipeline",
            pipeline_id,
        )
        .with_company(company_id)
        .with_payload(serde_json::json!({
            "pipeline_id": pipeline_id.to_string(),
            "company_id": company_id.to_string(),
            "key": row.key,
            "name": row.name,
        }));

        if let Err(e) = self.state.activity.emit(activity_event.clone()).await {
            tracing::warn!(pipeline_id = %pipeline_id, error = %e, "activity emit failed");
        }

        self.state.realtime.publish(
            LiveEvent::new("pipeline.created", "pipeline", pipeline_id)
                .with_company(company_id)
                .with_actor("system"),
        );

        let plugin_event = PluginEvent {
            event_id: activity_event.id.0.to_string(),
            event_type: activity_event.kind.as_str().to_string(),
            occurred_at: activity_event.occurred_at,
            actor_id: None,
            actor_type: Some(ActorType::System),
            entity_id: Some(pipeline_id.to_string()),
            entity_type: Some(activity_event.subject_kind.clone()),
            company_id: company_id.to_string(),
            payload: activity_event.payload.clone(),
        };
        let _ = self.state.plugin_event_bus.emit(plugin_event).await;

        Ok(())
    }

    async fn on_updated(
        &self,
        row: &pc_repos::pipeline::PipelineRow,
    ) -> pc_pipelines::PipelineServiceResult<()> {
        let company_id = row.company_id;
        let pipeline_id = row.id;

        let activity_event = ActivityEvent::new(
            ActivityKind::PipelineUpdated,
            ActivityActor::System {
                component: "pipeline_service".into(),
            },
            "pipeline",
            pipeline_id,
        )
        .with_company(company_id)
        .with_payload(serde_json::json!({
            "pipeline_id": pipeline_id.to_string(),
            "company_id": company_id.to_string(),
            "name": row.name,
        }));

        if let Err(e) = self.state.activity.emit(activity_event.clone()).await {
            tracing::warn!(pipeline_id = %pipeline_id, error = %e, "activity emit failed");
        }

        self.state.realtime.publish(
            LiveEvent::new("pipeline.updated", "pipeline", pipeline_id)
                .with_company(company_id)
                .with_actor("system"),
        );

        let plugin_event = PluginEvent {
            event_id: activity_event.id.0.to_string(),
            event_type: activity_event.kind.as_str().to_string(),
            occurred_at: activity_event.occurred_at,
            actor_id: None,
            actor_type: Some(ActorType::System),
            entity_id: Some(pipeline_id.to_string()),
            entity_type: Some(activity_event.subject_kind.clone()),
            company_id: company_id.to_string(),
            payload: activity_event.payload.clone(),
        };
        let _ = self.state.plugin_event_bus.emit(plugin_event).await;

        Ok(())
    }

    async fn on_archived(
        &self,
        row: &pc_repos::pipeline::PipelineRow,
    ) -> pc_pipelines::PipelineServiceResult<()> {
        let company_id = row.company_id;
        let pipeline_id = row.id;

        let activity_event = ActivityEvent::new(
            ActivityKind::PipelineArchived,
            ActivityActor::System {
                component: "pipeline_service".into(),
            },
            "pipeline",
            pipeline_id,
        )
        .with_company(company_id)
        .with_payload(serde_json::json!({
            "pipeline_id": pipeline_id.to_string(),
            "company_id": company_id.to_string(),
        }));

        if let Err(e) = self.state.activity.emit(activity_event.clone()).await {
            tracing::warn!(pipeline_id = %pipeline_id, error = %e, "activity emit failed");
        }

        self.state.realtime.publish(
            LiveEvent::new("pipeline.archived", "pipeline", pipeline_id)
                .with_company(company_id)
                .with_actor("system"),
        );

        let plugin_event = PluginEvent {
            event_id: activity_event.id.0.to_string(),
            event_type: activity_event.kind.as_str().to_string(),
            occurred_at: activity_event.occurred_at,
            actor_id: None,
            actor_type: Some(ActorType::System),
            entity_id: Some(pipeline_id.to_string()),
            entity_type: Some(activity_event.subject_kind.clone()),
            company_id: company_id.to_string(),
            payload: activity_event.payload.clone(),
        };
        let _ = self.state.plugin_event_bus.emit(plugin_event).await;

        Ok(())
    }

    async fn on_deleted(
        &self,
        id: Uuid,
        company_id: Uuid,
    ) -> pc_pipelines::PipelineServiceResult<()> {
        let activity_event = ActivityEvent::new(
            ActivityKind::PipelineRemoved,
            ActivityActor::System {
                component: "pipeline_service".into(),
            },
            "pipeline",
            id,
        )
        .with_company(company_id)
        .with_payload(serde_json::json!({
            "pipeline_id": id.to_string(),
            "company_id": company_id.to_string(),
        }));

        if let Err(e) = self.state.activity.emit(activity_event.clone()).await {
            tracing::warn!(pipeline_id = %id, error = %e, "activity emit failed");
        }

        self.state.realtime.publish(
            LiveEvent::new("pipeline.deleted", "pipeline", id)
                .with_company(company_id)
                .with_actor("system"),
        );

        let plugin_event = PluginEvent {
            event_id: activity_event.id.0.to_string(),
            event_type: activity_event.kind.as_str().to_string(),
            occurred_at: activity_event.occurred_at,
            actor_id: None,
            actor_type: Some(ActorType::System),
            entity_id: Some(id.to_string()),
            entity_type: Some(activity_event.subject_kind.clone()),
            company_id: company_id.to_string(),
            payload: activity_event.payload.clone(),
        };
        let _ = self.state.plugin_event_bus.emit(plugin_event).await;

        Ok(())
    }

    // -------- R603 v2: stage lifecycle hooks --------

    async fn on_stage_created(
        &self,
        pipeline_id: Uuid,
        stage: &pc_repos::pipeline::PipelineStageRow,
    ) -> pc_pipelines::PipelineServiceResult<()> {
        // 通过 stage_id 找 company_id（stage row 不存 company_id）。
        // 这里依赖 hook 与 service 在同一进程内 — service.create_stage 已经校验过 company。
        // 直接走 db 查 pipeline.company_id 以避免让 service 传冗余字段。
        let company_id_res: Result<uuid::Uuid, sqlx::Error> = sqlx::query_scalar(
            "SELECT company_id FROM pipelines WHERE id = $1",
        )
        .bind(pipeline_id)
        .fetch_one(self.state.db.pool())
        .await;
        let company_id = match company_id_res {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(pipeline_id = %pipeline_id, error = %e, "stage hook: company lookup failed");
                return Ok(());
            }
        };
        let stage_id = stage.id;

        let activity_event = ActivityEvent::new(
            ActivityKind::PipelineStageCreated,
            ActivityActor::System {
                component: "pipeline_service".into(),
            },
            "pipeline_stage",
            stage_id,
        )
        .with_company(company_id)
        .with_payload(serde_json::json!({
            "pipeline_id": pipeline_id.to_string(),
            "stage_id": stage_id.to_string(),
            "company_id": company_id.to_string(),
            "key": stage.key,
            "name": stage.name,
            "kind": stage.kind,
            "position": stage.position,
        }));

        if let Err(e) = self.state.activity.emit(activity_event.clone()).await {
            tracing::warn!(stage_id = %stage_id, error = %e, "activity emit failed");
        }

        self.state.realtime.publish(
            LiveEvent::new("pipeline.stage.created", "pipeline_stage", stage_id)
                .with_company(company_id)
                .with_actor("system"),
        );

        let plugin_event = PluginEvent {
            event_id: activity_event.id.0.to_string(),
            event_type: activity_event.kind.as_str().to_string(),
            occurred_at: activity_event.occurred_at,
            actor_id: None,
            actor_type: Some(ActorType::System),
            entity_id: Some(stage_id.to_string()),
            entity_type: Some(activity_event.subject_kind.clone()),
            company_id: company_id.to_string(),
            payload: activity_event.payload.clone(),
        };
        let _ = self.state.plugin_event_bus.emit(plugin_event).await;

        Ok(())
    }

    async fn on_stage_updated(
        &self,
        stage: &pc_repos::pipeline::PipelineStageRow,
    ) -> pc_pipelines::PipelineServiceResult<()> {
        let stage_id = stage.id;
        let pipeline_id = stage.pipeline_id;

        let company_id_res: Result<uuid::Uuid, sqlx::Error> = sqlx::query_scalar(
            "SELECT company_id FROM pipelines WHERE id = $1",
        )
        .bind(pipeline_id)
        .fetch_one(self.state.db.pool())
        .await;
        let company_id = match company_id_res {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(pipeline_id = %pipeline_id, error = %e, "stage hook: company lookup failed");
                return Ok(());
            }
        };

        let activity_event = ActivityEvent::new(
            ActivityKind::PipelineStageUpdated,
            ActivityActor::System {
                component: "pipeline_service".into(),
            },
            "pipeline_stage",
            stage_id,
        )
        .with_company(company_id)
        .with_payload(serde_json::json!({
            "pipeline_id": pipeline_id.to_string(),
            "stage_id": stage_id.to_string(),
            "company_id": company_id.to_string(),
            "name": stage.name,
            "kind": stage.kind,
            "position": stage.position,
        }));

        if let Err(e) = self.state.activity.emit(activity_event.clone()).await {
            tracing::warn!(stage_id = %stage_id, error = %e, "activity emit failed");
        }

        self.state.realtime.publish(
            LiveEvent::new("pipeline.stage.updated", "pipeline_stage", stage_id)
                .with_company(company_id)
                .with_actor("system"),
        );

        let plugin_event = PluginEvent {
            event_id: activity_event.id.0.to_string(),
            event_type: activity_event.kind.as_str().to_string(),
            occurred_at: activity_event.occurred_at,
            actor_id: None,
            actor_type: Some(ActorType::System),
            entity_id: Some(stage_id.to_string()),
            entity_type: Some(activity_event.subject_kind.clone()),
            company_id: company_id.to_string(),
            payload: activity_event.payload.clone(),
        };
        let _ = self.state.plugin_event_bus.emit(plugin_event).await;

        Ok(())
    }

    async fn on_stage_deleted(
        &self,
        stage_id: Uuid,
        pipeline_id: Uuid,
    ) -> pc_pipelines::PipelineServiceResult<()> {
        let company_id_res: Result<uuid::Uuid, sqlx::Error> = sqlx::query_scalar(
            "SELECT company_id FROM pipelines WHERE id = $1",
        )
        .bind(pipeline_id)
        .fetch_one(self.state.db.pool())
        .await;
        let company_id = match company_id_res {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(pipeline_id = %pipeline_id, error = %e, "stage hook: company lookup failed");
                return Ok(());
            }
        };

        let activity_event = ActivityEvent::new(
            ActivityKind::PipelineStageRemoved,
            ActivityActor::System {
                component: "pipeline_service".into(),
            },
            "pipeline_stage",
            stage_id,
        )
        .with_company(company_id)
        .with_payload(serde_json::json!({
            "pipeline_id": pipeline_id.to_string(),
            "stage_id": stage_id.to_string(),
            "company_id": company_id.to_string(),
        }));

        if let Err(e) = self.state.activity.emit(activity_event.clone()).await {
            tracing::warn!(stage_id = %stage_id, error = %e, "activity emit failed");
        }

        self.state.realtime.publish(
            LiveEvent::new("pipeline.stage.deleted", "pipeline_stage", stage_id)
                .with_company(company_id)
                .with_actor("system"),
        );

        let plugin_event = PluginEvent {
            event_id: activity_event.id.0.to_string(),
            event_type: activity_event.kind.as_str().to_string(),
            occurred_at: activity_event.occurred_at,
            actor_id: None,
            actor_type: Some(ActorType::System),
            entity_id: Some(stage_id.to_string()),
            entity_type: Some(activity_event.subject_kind.clone()),
            company_id: company_id.to_string(),
            payload: activity_event.payload.clone(),
        };
        let _ = self.state.plugin_event_bus.emit(plugin_event).await;

        Ok(())
    }

    // -------- R603 v3: transition lifecycle hooks --------

    async fn on_transition_created(
        &self,
        transition: &pc_repos::pipeline::PipelineTransitionRow,
    ) -> pc_pipelines::PipelineServiceResult<()> {
        let transition_id = transition.id;
        let pipeline_id = transition.pipeline_id;

        let company_id_res: Result<uuid::Uuid, sqlx::Error> = sqlx::query_scalar(
            "SELECT company_id FROM pipelines WHERE id = $1",
        )
        .bind(pipeline_id)
        .fetch_one(self.state.db.pool())
        .await;
        let company_id = match company_id_res {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(pipeline_id = %pipeline_id, error = %e, "transition hook: company lookup failed");
                return Ok(());
            }
        };

        let activity_event = ActivityEvent::new(
            ActivityKind::PipelineTransitionCreated,
            ActivityActor::System {
                component: "pipeline_service".into(),
            },
            "pipeline_transition",
            transition_id,
        )
        .with_company(company_id)
        .with_payload(serde_json::json!({
            "transition_id": transition_id.to_string(),
            "pipeline_id": pipeline_id.to_string(),
            "company_id": company_id.to_string(),
            "from_stage_id": transition.from_stage_id.to_string(),
            "to_stage_id": transition.to_stage_id.to_string(),
            "label": transition.label,
        }));

        if let Err(e) = self.state.activity.emit(activity_event.clone()).await {
            tracing::warn!(transition_id = %transition_id, error = %e, "activity emit failed");
        }

        self.state.realtime.publish(
            LiveEvent::new("pipeline.transition.created", "pipeline_transition", transition_id)
                .with_company(company_id)
                .with_actor("system"),
        );

        let plugin_event = PluginEvent {
            event_id: activity_event.id.0.to_string(),
            event_type: activity_event.kind.as_str().to_string(),
            occurred_at: activity_event.occurred_at,
            actor_id: None,
            actor_type: Some(ActorType::System),
            entity_id: Some(transition_id.to_string()),
            entity_type: Some(activity_event.subject_kind.clone()),
            company_id: company_id.to_string(),
            payload: activity_event.payload.clone(),
        };
        let _ = self.state.plugin_event_bus.emit(plugin_event).await;

        Ok(())
    }

    async fn on_transition_deleted(
        &self,
        transition_id: Uuid,
        pipeline_id: Uuid,
    ) -> pc_pipelines::PipelineServiceResult<()> {
        let company_id_res: Result<uuid::Uuid, sqlx::Error> = sqlx::query_scalar(
            "SELECT company_id FROM pipelines WHERE id = $1",
        )
        .bind(pipeline_id)
        .fetch_one(self.state.db.pool())
        .await;
        let company_id = match company_id_res {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(pipeline_id = %pipeline_id, error = %e, "transition hook: company lookup failed");
                return Ok(());
            }
        };

        let activity_event = ActivityEvent::new(
            ActivityKind::PipelineTransitionRemoved,
            ActivityActor::System {
                component: "pipeline_service".into(),
            },
            "pipeline_transition",
            transition_id,
        )
        .with_company(company_id)
        .with_payload(serde_json::json!({
            "transition_id": transition_id.to_string(),
            "pipeline_id": pipeline_id.to_string(),
            "company_id": company_id.to_string(),
        }));

        if let Err(e) = self.state.activity.emit(activity_event.clone()).await {
            tracing::warn!(transition_id = %transition_id, error = %e, "activity emit failed");
        }

        self.state.realtime.publish(
            LiveEvent::new("pipeline.transition.deleted", "pipeline_transition", transition_id)
                .with_company(company_id)
                .with_actor("system"),
        );

        let plugin_event = PluginEvent {
            event_id: activity_event.id.0.to_string(),
            event_type: activity_event.kind.as_str().to_string(),
            occurred_at: activity_event.occurred_at,
            actor_id: None,
            actor_type: Some(ActorType::System),
            entity_id: Some(transition_id.to_string()),
            entity_type: Some(activity_event.subject_kind.clone()),
            company_id: company_id.to_string(),
            payload: activity_event.payload.clone(),
        };
        let _ = self.state.plugin_event_bus.emit(plugin_event).await;

        Ok(())
    }

    // -------- R603 v4: case lifecycle hooks --------

    async fn on_case_created(
        &self,
        case: &pc_repos::pipeline::PipelineCaseRow,
    ) -> pc_pipelines::PipelineServiceResult<()> {
        let case_id = case.id;
        let company_id = case.company_id;
        let pipeline_id = case.pipeline_id;

        let activity_event = ActivityEvent::new(
            ActivityKind::PipelineCaseCreated,
            ActivityActor::System {
                component: "pipeline_service".into(),
            },
            "pipeline_case",
            case_id,
        )
        .with_company(company_id)
        .with_payload(serde_json::json!({
            "case_id": case_id.to_string(),
            "pipeline_id": pipeline_id.to_string(),
            "company_id": company_id.to_string(),
            "stage_id": case.stage_id.to_string(),
            "case_key": case.case_key,
            "title": case.title,
        }));

        if let Err(e) = self.state.activity.emit(activity_event.clone()).await {
            tracing::warn!(case_id = %case_id, error = %e, "activity emit failed");
        }

        self.state.realtime.publish(
            LiveEvent::new("pipeline.case.created", "pipeline_case", case_id)
                .with_company(company_id)
                .with_actor("system"),
        );

        let plugin_event = PluginEvent {
            event_id: activity_event.id.0.to_string(),
            event_type: activity_event.kind.as_str().to_string(),
            occurred_at: activity_event.occurred_at,
            actor_id: None,
            actor_type: Some(ActorType::System),
            entity_id: Some(case_id.to_string()),
            entity_type: Some(activity_event.subject_kind.clone()),
            company_id: company_id.to_string(),
            payload: activity_event.payload.clone(),
        };
        let _ = self.state.plugin_event_bus.emit(plugin_event).await;

        Ok(())
    }

    async fn on_case_stage_transitioned(
        &self,
        case: &pc_repos::pipeline::PipelineCaseRow,
        from_stage_id: Uuid,
        to_stage_id: Uuid,
    ) -> pc_pipelines::PipelineServiceResult<()> {
        let case_id = case.id;
        let company_id = case.company_id;

        let activity_event = ActivityEvent::new(
            ActivityKind::PipelineCaseStageTransitioned,
            ActivityActor::System {
                component: "pipeline_service".into(),
            },
            "pipeline_case",
            case_id,
        )
        .with_company(company_id)
        .with_payload(serde_json::json!({
            "case_id": case_id.to_string(),
            "pipeline_id": case.pipeline_id.to_string(),
            "company_id": company_id.to_string(),
            "from_stage_id": from_stage_id.to_string(),
            "to_stage_id": to_stage_id.to_string(),
            "version": case.version,
            "terminal_kind": case.terminal_kind,
        }));

        if let Err(e) = self.state.activity.emit(activity_event.clone()).await {
            tracing::warn!(case_id = %case_id, error = %e, "activity emit failed");
        }

        self.state.realtime.publish(
            LiveEvent::new("pipeline.case.stage_transitioned", "pipeline_case", case_id)
                .with_company(company_id)
                .with_actor("system"),
        );

        let plugin_event = PluginEvent {
            event_id: activity_event.id.0.to_string(),
            event_type: activity_event.kind.as_str().to_string(),
            occurred_at: activity_event.occurred_at,
            actor_id: None,
            actor_type: Some(ActorType::System),
            entity_id: Some(case_id.to_string()),
            entity_type: Some(activity_event.subject_kind.clone()),
            company_id: company_id.to_string(),
            payload: activity_event.payload.clone(),
        };
        let _ = self.state.plugin_event_bus.emit(plugin_event).await;

        Ok(())
    }

    async fn on_case_deleted(
        &self,
        case_id: Uuid,
        company_id: Uuid,
    ) -> pc_pipelines::PipelineServiceResult<()> {
        let activity_event = ActivityEvent::new(
            ActivityKind::PipelineCaseRemoved,
            ActivityActor::System {
                component: "pipeline_service".into(),
            },
            "pipeline_case",
            case_id,
        )
        .with_company(company_id)
        .with_payload(serde_json::json!({
            "case_id": case_id.to_string(),
            "company_id": company_id.to_string(),
        }));

        if let Err(e) = self.state.activity.emit(activity_event.clone()).await {
            tracing::warn!(case_id = %case_id, error = %e, "activity emit failed");
        }

        self.state.realtime.publish(
            LiveEvent::new("pipeline.case.deleted", "pipeline_case", case_id)
                .with_company(company_id)
                .with_actor("system"),
        );

        let plugin_event = PluginEvent {
            event_id: activity_event.id.0.to_string(),
            event_type: activity_event.kind.as_str().to_string(),
            occurred_at: activity_event.occurred_at,
            actor_id: None,
            actor_type: Some(ActorType::System),
            entity_id: Some(case_id.to_string()),
            entity_type: Some(activity_event.subject_kind.clone()),
            company_id: company_id.to_string(),
            payload: activity_event.payload.clone(),
        };
        let _ = self.state.plugin_event_bus.emit(plugin_event).await;

        Ok(())
    }

    async fn on_case_event_recorded(
        &self,
        case: &pc_repos::pipeline::PipelineCaseRow,
        event: &pc_repos::pipeline::PipelineCaseEventRow,
    ) -> pc_pipelines::PipelineServiceResult<()> {
        let case_id = case.id;
        let company_id = case.company_id;
        let event_id = event.id;

        let activity_event = ActivityEvent::new(
            ActivityKind::PipelineCaseEventRecorded,
            ActivityActor::System {
                component: "pipeline_service".into(),
            },
            "pipeline_case_event",
            event_id,
        )
        .with_company(company_id)
        .with_payload(serde_json::json!({
            "event_id": event_id.to_string(),
            "case_id": case_id.to_string(),
            "company_id": company_id.to_string(),
            "event_type": event.r#type,
            "actor_type": event.actor_type,
            "from_stage_id": event.from_stage_id.map(|u| u.to_string()),
            "to_stage_id": event.to_stage_id.map(|u| u.to_string()),
        }));

        if let Err(e) = self.state.activity.emit(activity_event.clone()).await {
            tracing::warn!(event_id = %event_id, error = %e, "activity emit failed");
        }

        self.state.realtime.publish(
            LiveEvent::new("pipeline.case.event_recorded", "pipeline_case_event", event_id)
                .with_company(company_id)
                .with_actor("system"),
        );

        let plugin_event = PluginEvent {
            event_id: activity_event.id.0.to_string(),
            event_type: activity_event.kind.as_str().to_string(),
            occurred_at: activity_event.occurred_at,
            actor_id: None,
            actor_type: Some(ActorType::System),
            entity_id: Some(event_id.to_string()),
            entity_type: Some(activity_event.subject_kind.clone()),
            company_id: company_id.to_string(),
            payload: activity_event.payload.clone(),
        };
        let _ = self.state.plugin_event_bus.emit(plugin_event).await;

        Ok(())
    }
}
