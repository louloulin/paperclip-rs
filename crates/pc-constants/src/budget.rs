//! Budget / Finance 域常量。

/// Budget scope type（per-company / per-agent / per-project）。
pub const BUDGET_SCOPE_TYPES: &[&str] = &["company", "agent", "project"];

/// Budget metric（目前仅 billed_cents）。
pub const BUDGET_METRICS: &[&str] = &["billed_cents"];

/// Budget window kind（calendar_month_utc / lifetime）。
pub const BUDGET_WINDOW_KINDS: &[&str] = &["calendar_month_utc", "lifetime"];

/// Budget threshold type（soft / hard）。
pub const BUDGET_THRESHOLD_TYPES: &[&str] = &["soft", "hard"];

/// Budget incident 状态。
pub const BUDGET_INCIDENT_STATUSES: &[&str] = &["open", "resolved", "dismissed"];

/// Budget incident resolution action。
pub const BUDGET_INCIDENT_RESOLUTION_ACTIONS: &[&str] = &["throttle", "pause", "notify", "ignore"];

/// Billing type。
pub const BILLING_TYPES: &[&str] = &["subscription", "usage", "hybrid"];

/// Cost 状态（reported / unpriced）。
pub const COST_STATUSES: &[&str] = &["reported", "unpriced"];

/// Finance event kind。
pub const FINANCE_EVENT_KINDS: &[&str] = &["charge", "refund", "adjustment", "credit", "debit"];

/// Finance direction。
pub const FINANCE_DIRECTIONS: &[&str] = &["debit", "credit"];

/// Finance unit（货币 / token / 秒）。
pub const FINANCE_UNITS: &[&str] = &["usd_cents", "tokens", "seconds"];

/// Storage provider。
pub const STORAGE_PROVIDERS: &[&str] = &["local_disk", "s3"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_types_match_node() {
        assert_eq!(BUDGET_SCOPE_TYPES, &["company", "agent", "project"]);
    }

    #[test]
    fn metrics_only_billed_cents() {
        assert_eq!(BUDGET_METRICS, &["billed_cents"]);
    }

    #[test]
    fn window_kinds_match_node() {
        assert_eq!(BUDGET_WINDOW_KINDS, &["calendar_month_utc", "lifetime"]);
    }

    #[test]
    fn threshold_types_soft_hard() {
        assert_eq!(BUDGET_THRESHOLD_TYPES, &["soft", "hard"]);
    }

    #[test]
    fn storage_providers_local_and_s3() {
        assert!(STORAGE_PROVIDERS.contains(&"local_disk"));
        assert!(STORAGE_PROVIDERS.contains(&"s3"));
    }

    #[test]
    fn finance_directions_match() {
        assert_eq!(FINANCE_DIRECTIONS, &["debit", "credit"]);
    }
}
