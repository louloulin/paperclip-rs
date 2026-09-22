//! 时间戳抽象。

use chrono::{DateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};

/// Multica 时间戳。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(DateTime<Utc>);

impl Timestamp {
    pub fn now() -> Self {
        Self(Utc::now())
    }

    pub fn from_unix(secs: i64) -> Self {
        Self(Utc.timestamp_opt(secs, 0).single().unwrap_or_else(Utc::now))
    }

    pub fn as_datetime(self) -> DateTime<Utc> {
        self.0
    }

    pub fn as_unix(self) -> i64 {
        self.0.timestamp()
    }

    pub fn as_iso(self) -> String {
        self.0.to_rfc3339()
    }

    pub fn elapsed(self) -> chrono::Duration {
        Utc::now() - self.0
    }
}

impl Default for Timestamp {
    fn default() -> Self {
        Self::now()
    }
}

impl From<DateTime<Utc>> for Timestamp {
    fn from(dt: DateTime<Utc>) -> Self {
        Self(dt)
    }
}

impl From<Timestamp> for DateTime<Utc> {
    fn from(t: Timestamp) -> Self {
        t.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_round_trip() {
        let now = Timestamp::now();
        let unix = now.as_unix();
        let back = Timestamp::from_unix(unix);
        assert_eq!(back.as_unix(), unix);
    }

    #[test]
    fn ordering() {
        let a = Timestamp::from_unix(100);
        let b = Timestamp::from_unix(200);
        assert!(a < b);
    }
}