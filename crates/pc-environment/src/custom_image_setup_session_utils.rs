//! Pure helpers for custom-image setup session lifecycle.
//!
//! Mirrors Node `server/src/services/environment-custom-image-setup-session-utils.ts`
//! (32 lines) 1:1. Pure functions, no IO / no DB.

use chrono::{DateTime, Utc};

/// `readCustomImageSetupSessionCompanyId` — pull companyId from setup session
/// metadata, normalize "instance" → null and trim empty → null.
pub fn read_custom_image_setup_session_company_id(
    metadata: Option<&serde_json::Map<String, serde_json::Value>>,
) -> Option<String> {
    let value = metadata.and_then(|m| m.get("setupRpcCompanyId"))?;
    let s = value.as_str()?;
    let trimmed = s.trim();
    if trimmed.is_empty() || trimmed == "instance" {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// `readNullableDate` — coerce `Date | string | null | undefined` → `Option<DateTime<Utc>>`.
/// Invalid date strings → `None`.
pub fn read_nullable_date(value: Option<&serde_json::Value>) -> Option<DateTime<Utc>> {
    let v = value?;
    if v.is_null() {
        return None;
    }
    if let Some(s) = v.as_str() {
        if s.is_empty() {
            return None;
        }
        return DateTime::parse_from_rfc3339(s)
            .map(|dt| dt.with_timezone(&Utc))
            .ok();
    }
    None
}

/// `readFutureDate` — like `readNullableDate` but additionally requires
/// `date > now`. Past or invalid → `None`.
pub fn read_future_date(
    value: Option<&serde_json::Value>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    let date = read_nullable_date(value)?;
    if date > now {
        Some(date)
    } else {
        None
    }
}

#[derive(Debug, thiserror::Error)]
#[error("Environment customImage setup session has expired.")]
pub struct SetupSessionExpiredError;

/// `requireFutureCustomImageSetupExpiry` — throw-style helper that returns
/// the parsed expiry or returns an error.
pub fn require_future_custom_image_setup_expiry(
    expires_at: Option<&serde_json::Value>,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>, SetupSessionExpiredError> {
    read_future_date(expires_at, now).ok_or(SetupSessionExpiredError)
}
