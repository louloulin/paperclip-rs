//! 主体（执行操作的实体）。与原 server `Actor = User | Agent` 等价。

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 主体标识：用户或代理。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Actor {
    User { id: String },
    Agent { id: Uuid },
    System,
}

impl Actor {
    pub fn system() -> Self {
        Actor::System
    }
    pub fn is_system(&self) -> bool {
        matches!(self, Actor::System)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_serializes_with_kind() {
        let a = Actor::User { id: "u1".into() };
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["kind"], "user");
        assert_eq!(v["id"], "u1");
    }

    #[test]
    fn system_is_recognized() {
        assert!(Actor::system().is_system());
    }
}
