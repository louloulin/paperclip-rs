//! Activity log types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::kinds::ActivityKind;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActivityActor {
    User { id: Uuid, name: String },
    Agent { id: Uuid, name: String },
    System { component: String },
    Plugin { plugin_id: Uuid, plugin_key: String },
    Anonymous,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(transparent)]
pub struct ActivityId(pub Uuid);

impl ActivityId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for ActivityId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for ActivityId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityEvent {
    pub id: ActivityId,
    pub kind: ActivityKind,
    pub actor: ActivityActor,
    pub company_id: Option<Uuid>,
    pub subject_kind: String,
    pub subject_id: Uuid,
    pub payload: serde_json::Value,
    pub occurred_at: DateTime<Utc>,
}

impl ActivityEvent {
    #[must_use]
    pub fn new(
        kind: ActivityKind,
        actor: ActivityActor,
        subject_kind: impl Into<String>,
        subject_id: Uuid,
    ) -> Self {
        Self {
            id: ActivityId::new(),
            kind,
            actor,
            company_id: None,
            subject_kind: subject_kind.into(),
            subject_id,
            payload: serde_json::Value::Null,
            occurred_at: Utc::now(),
        }
    }

    #[must_use]
    pub fn with_company(mut self, company_id: Uuid) -> Self {
        self.company_id = Some(company_id);
        self
    }

    #[must_use]
    pub fn with_payload(mut self, payload: serde_json::Value) -> Self {
        self.payload = payload;
        self
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActivityFilter {
    pub company_id: Option<Uuid>,
    pub kind: Option<ActivityKind>,
    pub actor_id: Option<Uuid>,
    pub subject_kind: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActivityQuery {
    pub filter: ActivityFilter,
    pub cursor: Option<ActivityId>,
}
