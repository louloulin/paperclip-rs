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

    async fn on_status_changed(
        &self,
        row: &pc_repos::issue::IssueRow,
        _old_status: &str,
        new_status: &str,
    ) -> pc_issues::IssueServiceResult<()> {
        let company_id = row.company_id;
        let issue_id = row.id;

        // 终态 → IssueClosed；其他 → IssueUpdated。
        // 统一走 IssueUpdated 便于 UI 通用 handling；终态额外加 IssueClosed 标记。
        let primary_kind = ActivityKind::IssueUpdated;
        let activity_event = ActivityEvent::new(
            primary_kind,
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
            "from": _old_status,
            "to": new_status,
            "is_terminal": matches!(new_status, "done" | "cancelled"),
        }));

        if let Err(e) = self.state.activity.emit(activity_event.clone()).await {
            tracing::warn!(issue_id = %issue_id, error = %e, "activity emit failed");
        }

        let event_type = "issue.status_changed";
        self.state.realtime.publish(
            LiveEvent::new(event_type, "issue", issue_id)
                .with_company(company_id)
                .with_actor("system"),
        );

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

    async fn on_assigned(
        &self,
        row: &pc_repos::issue::IssueRow,
        kind: pc_issues::AssignKind,
    ) -> pc_issues::IssueServiceResult<()> {
        let company_id = row.company_id;
        let issue_id = row.id;

        // 序列化 AssignKind → payload 友好的 JSON
        let (kind_str, payload_extra) = match &kind {
            pc_issues::AssignKind::Agent(id) => (
                "agent",
                serde_json::json!({ "assignee_agent_id": id.to_string() }),
            ),
            pc_issues::AssignKind::User(name) => (
                "user",
                serde_json::json!({ "assignee_user_id": name }),
            ),
            pc_issues::AssignKind::Unassign => (
                "unassign",
                serde_json::json!({}),
            ),
        };

        let mut payload_map = serde_json::Map::new();
        payload_map.insert("issue_id".into(), serde_json::json!(issue_id.to_string()));
        payload_map.insert("company_id".into(), serde_json::json!(company_id.to_string()));
        payload_map.insert("title".into(), serde_json::json!(row.title));
        payload_map.insert("kind".into(), serde_json::json!(kind_str));
        if let Some(obj) = payload_extra.as_object() {
            for (k, v) in obj {
                payload_map.insert(k.clone(), v.clone());
            }
        }
        let payload = serde_json::Value::Object(payload_map);

        let activity_event = ActivityEvent::new(
            ActivityKind::IssueAssigned,
            ActivityActor::System {
                component: "issue_service".into(),
            },
            "issue",
            issue_id,
        )
        .with_company(company_id)
        .with_payload(payload.clone());

        if let Err(e) = self.state.activity.emit(activity_event.clone()).await {
            tracing::warn!(issue_id = %issue_id, error = %e, "activity emit failed");
        }

        self.state.realtime.publish(
            LiveEvent::new("issue.assigned", "issue", issue_id)
                .with_company(company_id)
                .with_actor("system"),
        );

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

    async fn on_commented(
        &self,
        parent_issue: &pc_repos::issue::IssueRow,
        comment: &pc_repos::issue::IssueCommentRow,
    ) -> pc_issues::IssueServiceResult<()> {
        let company_id = parent_issue.company_id;
        let issue_id = parent_issue.id;
        let comment_id = comment.id;

        // 推断 author_kind
        let (author_kind, author_id) = if let Some(agent_id) = comment.author_agent_id {
            ("agent", serde_json::json!(agent_id.to_string()))
        } else if let Some(ref user_id) = comment.author_user_id {
            ("user", serde_json::json!(user_id))
        } else {
            ("system", serde_json::json!(null))
        };

        let activity_event = ActivityEvent::new(
            ActivityKind::IssueCommented,
            ActivityActor::System {
                component: "issue_service".into(),
            },
            "issue_comment",
            comment_id,
        )
        .with_company(company_id)
        .with_payload(serde_json::json!({
            "issue_id": issue_id.to_string(),
            "company_id": company_id.to_string(),
            "comment_id": comment_id.to_string(),
            "author_kind": author_kind,
            "author_id": author_id,
            "body_preview": comment.body.chars().take(200).collect::<String>(),
        }));

        if let Err(e) = self.state.activity.emit(activity_event.clone()).await {
            tracing::warn!(comment_id = %comment_id, error = %e, "activity emit failed");
        }

        self.state.realtime.publish(
            LiveEvent::new("issue.commented", "issue", issue_id)
                .with_company(company_id)
                .with_actor(author_kind),
        );

        let plugin_event = PluginEvent {
            event_id: activity_event.id.0.to_string(),
            event_type: activity_event.kind.as_str().to_string(),
            occurred_at: activity_event.occurred_at,
            actor_id: None,
            actor_type: Some(ActorType::System),
            entity_id: Some(comment_id.to_string()),
            entity_type: Some(activity_event.subject_kind.clone()),
            company_id: company_id.to_string(),
            payload: activity_event.payload.clone(),
        };
        let _ = self.state.plugin_event_bus.emit(plugin_event).await;

        Ok(())
    }
}
