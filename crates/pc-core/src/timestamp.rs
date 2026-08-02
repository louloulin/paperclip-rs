//! 时间戳值对象。统一为 UTC with timezone。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(DateTime<Utc>);

impl Timestamp {
    pub fn now() -> Self {
        Self(Utc::now())
    }
    pub fn from_dt(dt: DateTime<Utc>) -> Self {
        Self(dt)
    }
    pub fn as_datetime(&self) -> DateTime<Utc> {
        self.0
    }
}

impl Default for Timestamp {
    fn default() -> Self {
        Self::now()
    }
}

impl std::fmt::Display for Timestamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.to_rfc3339())
    }
}

#[cfg(feature = "sqlx")]
impl sqlx::Type<sqlx::Postgres> for Timestamp {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <chrono::DateTime<chrono::Utc> as sqlx::Type<sqlx::Postgres>>::type_info()
    }
    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <chrono::DateTime<chrono::Utc> as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

#[cfg(feature = "sqlx")]
impl<'r> sqlx::Decode<'r, sqlx::Postgres> for Timestamp {
    fn decode(
        value: <sqlx::Postgres as sqlx::Database>::ValueRef<'r>,
    ) -> Result<Self, sqlx::error::BoxDynError> {
        <chrono::DateTime<chrono::Utc> as sqlx::Decode<'r, sqlx::Postgres>>::decode(value)
            .map(Timestamp)
    }
}

#[cfg(feature = "sqlx")]
impl<'q> sqlx::Encode<'q, sqlx::Postgres> for Timestamp {
    fn encode_by_ref(
        &self,
        buf: &mut <sqlx::Postgres as sqlx::Database>::ArgumentBuffer<'q>,
    ) -> Result<sqlx::encode::IsNull, sqlx::error::BoxDynError> {
        <chrono::DateTime<chrono::Utc> as sqlx::Encode<'q, sqlx::Postgres>>::encode_by_ref(
            &self.0, buf,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_is_recent() {
        let t = Timestamp::now();
        let delta = Utc::now().signed_duration_since(t.0);
        assert!(delta.num_seconds().abs() < 2);
    }

    #[test]
    fn rfc3339_format() {
        let t = Timestamp::from_dt(
            chrono::DateTime::parse_from_rfc3339("2026-08-02T10:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        );
        assert_eq!(t.to_string(), "2026-08-02T10:00:00+00:00");
    }
}
