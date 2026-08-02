//! 会话/Cookie 解析、CSRF 与 API key 校验（占位骨架）。

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthContext {
    pub user_id: Option<String>,
    pub agent_id: Option<Uuid>,
    pub actor_kind: ActorKind,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ActorKind {
    User,
    Agent,
    System,
    Anonymous,
}

impl AuthContext {
    pub fn system() -> Self {
        Self {
            user_id: None,
            agent_id: None,
            actor_kind: ActorKind::System,
        }
    }
}

#[axum::async_trait]
impl FromRequestParts<AppState> for AuthContext {
    type Rejection = crate::ApiError;
    async fn from_request_parts(
        _parts: &mut Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        // 真实实现：从 cookie / API key / session 表中解析身份。
        // 当前为骨架：所有请求视为匿名，与 better-auth 行为对齐需要在 Phase C 接入。
        Ok(Self {
            user_id: None,
            agent_id: None,
            actor_kind: ActorKind::Anonymous,
        })
    }
}
