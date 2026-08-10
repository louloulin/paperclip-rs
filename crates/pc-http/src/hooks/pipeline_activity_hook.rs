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
        let company_id_res: Result<uuid::Uuid, sqlx::Error> =
            sqlx::query_scalar("SELECT company_id FROM pipelines WHERE id = $1")
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

        let company_id_res: Result<uuid::Uuid, sqlx::Error> =
            sqlx::query_scalar("SELECT company_id FROM pipelines WHERE id = $1")
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
        let company_id_res: Result<uuid::Uuid, sqlx::Error> =
            sqlx::query_scalar("SELECT company_id FROM pipelines WHERE id = $1")
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

        let company_id_res: Result<uuid::Uuid, sqlx::Error> =
            sqlx::query_scalar("SELECT company_id FROM pipelines WHERE id = $1")
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
            LiveEvent::new(
                "pipeline.transition.created",
                "pipeline_transition",
                transition_id,
            )
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
        let company_id_res: Result<uuid::Uuid, sqlx::Error> =
            sqlx::query_scalar("SELECT company_id FROM pipelines WHERE id = $1")
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
            LiveEvent::new(
                "pipeline.transition.deleted",
                "pipeline_transition",
                transition_id,
            )
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
            LiveEvent::new(
                "pipeline.case.event_recorded",
                "pipeline_case_event",
                event_id,
            )
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

    // -------- R603 v6.1: case issue link 子资源 lifecycle --------

    async fn on_case_issue_linked(
        &self,
        link: &pc_repos::pipeline::PipelineCaseIssueLinkRow,
    ) -> pc_pipelines::PipelineServiceResult<()> {
        let link_id = link.id;
        let case_id = link.case_id;
        let company_id = link.company_id;
        let issue_id = link.issue_id;
        let role = link.role.clone();

        let activity_event = ActivityEvent::new(
            ActivityKind::PipelineCaseIssueLinked,
            ActivityActor::System {
                component: "pipeline_service".into(),
            },
            "pipeline_case_issue_link",
            link_id,
        )
        .with_company(company_id)
        .with_payload(serde_json::json!({
            "link_id": link_id.to_string(),
            "case_id": case_id.to_string(),
            "company_id": company_id.to_string(),
            "issue_id": issue_id.to_string(),
            "role": role,
        }));

        if let Err(e) = self.state.activity.emit(activity_event.clone()).await {
            tracing::warn!(link_id = %link_id, error = %e, "activity emit failed");
        }

        self.state.realtime.publish(
            LiveEvent::new(
                "pipeline.case.issue_linked",
                "pipeline_case_issue_link",
                link_id,
            )
            .with_company(company_id)
            .with_actor("system"),
        );

        let plugin_event = PluginEvent {
            event_id: activity_event.id.0.to_string(),
            event_type: activity_event.kind.as_str().to_string(),
            occurred_at: activity_event.occurred_at,
            actor_id: None,
            actor_type: Some(ActorType::System),
            entity_id: Some(link_id.to_string()),
            entity_type: Some(activity_event.subject_kind.clone()),
            company_id: company_id.to_string(),
            payload: activity_event.payload.clone(),
        };
        let _ = self.state.plugin_event_bus.emit(plugin_event).await;

        Ok(())
    }

    async fn on_case_issue_unlinked(
        &self,
        link_id: Uuid,
        case_id: Uuid,
    ) -> pc_pipelines::PipelineServiceResult<()> {
        let company_id: Option<uuid::Uuid> =
            sqlx::query_scalar("SELECT company_id FROM pipeline_cases WHERE id = $1")
                .bind(case_id)
                .fetch_optional(self.state.db.pool())
                .await
                .ok()
                .flatten();
        let company_id = match company_id {
            Some(c) => c,
            None => return Ok(()),
        };

        let activity_event = ActivityEvent::new(
            ActivityKind::PipelineCaseIssueUnlinked,
            ActivityActor::System {
                component: "pipeline_service".into(),
            },
            "pipeline_case_issue_link",
            link_id,
        )
        .with_company(company_id)
        .with_payload(serde_json::json!({
            "link_id": link_id.to_string(),
            "case_id": case_id.to_string(),
            "company_id": company_id.to_string(),
        }));

        if let Err(e) = self.state.activity.emit(activity_event.clone()).await {
            tracing::warn!(link_id = %link_id, error = %e, "activity emit failed");
        }

        self.state.realtime.publish(
            LiveEvent::new(
                "pipeline.case.issue_unlinked",
                "pipeline_case_issue_link",
                link_id,
            )
            .with_company(company_id)
            .with_actor("system"),
        );

        let plugin_event = PluginEvent {
            event_id: activity_event.id.0.to_string(),
            event_type: activity_event.kind.as_str().to_string(),
            occurred_at: activity_event.occurred_at,
            actor_id: None,
            actor_type: Some(ActorType::System),
            entity_id: Some(link_id.to_string()),
            entity_type: Some(activity_event.subject_kind.clone()),
            company_id: company_id.to_string(),
            payload: activity_event.payload.clone(),
        };
        let _ = self.state.plugin_event_bus.emit(plugin_event).await;

        Ok(())
    }
    // -------- R603 v6.5: documents 子资源 lifecycle --------

    async fn on_pipeline_document_upserted(
        &self,
        pipeline_id: Uuid,
        company_id: Uuid,
        key: String,
        content: serde_json::Value,
    ) -> pc_pipelines::PipelineServiceResult<()> {
        let activity_event = ActivityEvent::new(
            ActivityKind::PipelineDocumentUpserted,
            ActivityActor::System {
                component: "pipeline_service".into(),
            },
            "pipeline_document",
            pipeline_id,
        )
        .with_company(company_id)
        .with_payload(serde_json::json!({
            "pipeline_id": pipeline_id.to_string(),
            "company_id": company_id.to_string(),
            "key": key,
            "content": content,
        }));

        if let Err(e) = self.state.activity.emit(activity_event.clone()).await {
            tracing::warn!(pipeline_id = %pipeline_id, error = %e, "activity emit failed");
        }

        self.state.realtime.publish(
            LiveEvent::new("pipeline.document_upserted", "pipeline", pipeline_id)
                .with_company(company_id)
                .with_actor("system")
                .with_data(serde_json::json!({"key": key})),
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

    async fn on_pipeline_document_revision_restored(
        &self,
        pipeline_id: Uuid,
        company_id: Uuid,
        key: String,
        revision_id: Uuid,
    ) -> pc_pipelines::PipelineServiceResult<()> {
        let activity_event = ActivityEvent::new(
            ActivityKind::PipelineDocumentRevisionRestored,
            ActivityActor::System {
                component: "pipeline_service".into(),
            },
            "pipeline_document",
            pipeline_id,
        )
        .with_company(company_id)
        .with_payload(serde_json::json!({
            "pipeline_id": pipeline_id.to_string(),
            "company_id": company_id.to_string(),
            "key": key,
            "revision_id": revision_id.to_string(),
        }));

        if let Err(e) = self.state.activity.emit(activity_event.clone()).await {
            tracing::warn!(pipeline_id = %pipeline_id, error = %e, "activity emit failed");
        }

        self.state.realtime.publish(
            LiveEvent::new(
                "pipeline.document_revision_restored",
                "pipeline",
                pipeline_id,
            )
            .with_company(company_id)
            .with_actor("system")
            .with_data(serde_json::json!({"key": key})),
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

    // -------- R603 v6.6: bulk review + automation retry --------

    async fn on_cases_bulk_reviewed(
        &self,
        company_id: Uuid,
        succeeded: i64,
        failed: i64,
        total: i64,
    ) -> pc_pipelines::PipelineServiceResult<()> {
        let activity_event = ActivityEvent::new(
            ActivityKind::PipelineCasesBulkReviewed,
            ActivityActor::System {
                component: "pipeline_service".into(),
            },
            "company",
            company_id,
        )
        .with_company(company_id)
        .with_payload(serde_json::json!({
            "company_id": company_id.to_string(),
            "succeeded": succeeded,
            "failed": failed,
            "total": total,
        }));

        if let Err(e) = self.state.activity.emit(activity_event.clone()).await {
            tracing::warn!(company_id = %company_id, error = %e, "activity emit failed");
        }

        self.state.realtime.publish(
            LiveEvent::new("pipeline.cases.bulk_reviewed", "company", company_id)
                .with_company(company_id)
                .with_actor("system")
                .with_data(serde_json::json!({
                    "succeeded": succeeded,
                    "failed": failed,
                    "total": total,
                })),
        );

        let plugin_event = PluginEvent {
            event_id: activity_event.id.0.to_string(),
            event_type: activity_event.kind.as_str().to_string(),
            occurred_at: activity_event.occurred_at,
            actor_id: None,
            actor_type: Some(ActorType::System),
            entity_id: Some(company_id.to_string()),
            entity_type: Some(activity_event.subject_kind.clone()),
            company_id: company_id.to_string(),
            payload: activity_event.payload.clone(),
        };
        let _ = self.state.plugin_event_bus.emit(plugin_event).await;

        Ok(())
    }

    async fn on_case_automation_retry_requested(
        &self,
        case_id: Uuid,
        company_id: Uuid,
        from_version: i32,
        to_version: i32,
    ) -> pc_pipelines::PipelineServiceResult<()> {
        let activity_event = ActivityEvent::new(
            ActivityKind::PipelineCaseAutomationRetryRequested,
            ActivityActor::System {
                component: "pipeline_service".into(),
            },
            "case",
            case_id,
        )
        .with_company(company_id)
        .with_payload(serde_json::json!({
            "case_id": case_id.to_string(),
            "company_id": company_id.to_string(),
            "from_version": from_version,
            "to_version": to_version,
        }));

        if let Err(e) = self.state.activity.emit(activity_event.clone()).await {
            tracing::warn!(case_id = %case_id, error = %e, "activity emit failed");
        }

        self.state.realtime.publish(
            LiveEvent::new("case.automation.retry_requested", "case", case_id)
                .with_company(company_id)
                .with_actor("system")
                .with_data(serde_json::json!({
                    "case_id": case_id.to_string(),
                    "from_version": from_version,
                    "to_version": to_version,
                })),
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

    async fn on_case_automation_specific_retry_requested(
        &self,
        case_id: Uuid,
        company_id: Uuid,
        automation_id: Uuid,
    ) -> pc_pipelines::PipelineServiceResult<()> {
        let activity_event = ActivityEvent::new(
            ActivityKind::PipelineCaseAutomationSpecificRetryRequested,
            ActivityActor::System {
                component: "pipeline_service".into(),
            },
            "case",
            case_id,
        )
        .with_company(company_id)
        .with_payload(serde_json::json!({
            "case_id": case_id.to_string(),
            "company_id": company_id.to_string(),
            "automation_id": automation_id.to_string(),
        }));

        if let Err(e) = self.state.activity.emit(activity_event.clone()).await {
            tracing::warn!(case_id = %case_id, error = %e, "activity emit failed");
        }

        self.state.realtime.publish(
            LiveEvent::new("case.automation.specific_retry", "case", case_id)
                .with_company(company_id)
                .with_actor("system")
                .with_data(serde_json::json!({
                    "case_id": case_id.to_string(),
                    "automation_id": automation_id.to_string(),
                })),
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

    async fn on_case_automation_current_stage_rerun_requested(
        &self,
        case_id: Uuid,
        company_id: Uuid,
        stage_id: Uuid,
        version: i32,
    ) -> pc_pipelines::PipelineServiceResult<()> {
        let activity_event = ActivityEvent::new(
            ActivityKind::PipelineCaseAutomationCurrentStageRerunRequested,
            ActivityActor::System {
                component: "pipeline_service".into(),
            },
            "case",
            case_id,
        )
        .with_company(company_id)
        .with_payload(serde_json::json!({
            "case_id": case_id.to_string(),
            "company_id": company_id.to_string(),
            "stage_id": stage_id.to_string(),
            "version": version,
        }));

        if let Err(e) = self.state.activity.emit(activity_event.clone()).await {
            tracing::warn!(case_id = %case_id, error = %e, "activity emit failed");
        }

        self.state.realtime.publish(
            LiveEvent::new("case.automation.current_stage_rerun", "case", case_id)
                .with_company(company_id)
                .with_actor("system")
                .with_data(serde_json::json!({
                    "case_id": case_id.to_string(),
                    "stage_id": stage_id.to_string(),
                    "version": version,
                })),
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
}
