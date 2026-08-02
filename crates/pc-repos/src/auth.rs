//! auth 域（users / sessions）。

use serde::{Deserialize, Serialize};
use sqlx::FromRow;

use pc_core::Timestamp;

use crate::Db;

#[derive(Debug, Clone, FromRow, Serialize, Deserialize, PartialEq, Eq)]
pub struct User {
    pub id: String,
    pub email: String,
    pub name: Option<String>,
    pub role: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

pub struct AuthRepo<'a> { pub db: &'a Db }

impl<'a> AuthRepo<'a> {
    pub fn new(db: &'a Db) -> Self { Self { db } }
    pub async fn find_by_email(&self, email: &str) -> sqlx::Result<Option<User>> {
        sqlx::query_as::<_, User>(
            "SELECT id, email, name, role, created_at, updated_at FROM \"user\" WHERE email = $1",
        ).bind(email).fetch_optional(self.db.pool()).await
    }
}
