//! `pc-acpx` usage helpers — pure functions that mirror `summarizeAcpxTurnUsage`
//! from Node `acpx-engine/execute.ts`.

use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

// ============================================================================
// Public types
// ============================================================================

/// Token breakdown snapshot collected by the ACP runtime.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct AcpxTurnUsageBreakdown {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_read_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_write_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thought_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<i64>,
}

/// Cost reported by the ACP runtime, denominated in a specific currency.
#[derive(Debug, Clone, PartialEq)]
pub struct AcpxTurnUsageCost {
    pub amount: f64,
    pub currency: Option<String>,
}

/// Persisted cumulative usage snapshot.
#[derive(Debug, Default, Clone)]
pub struct AcpxRuntimeUsageView {
    pub cumulative: Option<AcpxTurnUsageBreakdown>,
    pub cost: Option<AcpxTurnUsageCost>,
}

/// Snapshot of the ACP runtime status before / after a turn.
#[derive(Debug, Default, Clone)]
pub struct AcpxRuntimeStatusView {
    pub usage: Option<AcpxRuntimeUsageView>,
}

/// Aggregated inputs to `summarizeAcpxTurnUsage`.
#[derive(Debug, Default, Clone)]
pub struct SummarizeAcpxTurnUsageInput {
    pub pre_status: Option<AcpxRuntimeStatusView>,
    pub post_status: Option<AcpxRuntimeStatusView>,
    pub event_breakdown: Option<AcpxTurnUsageBreakdown>,
    pub event_cost_usd: Option<f64>,
}

/// Output of `summarizeAcpxTurnUsage`. Fields are populated only when the
/// runtime reported them; otherwise `None` (or zero, for `costUsd`).
#[derive(Debug, Default, Clone, PartialEq)]
pub struct SummarizeAcpxTurnUsageOutput {
    pub usage: Option<UsageSummary>,
    pub usage_detail: Option<BTreeMap<String, i64>>,
    pub cost_usd: Option<f64>,
    pub cumulative_cost_usd: Option<f64>,
}

/// Tokens for one turn. Mirrors `UsageSummary` from `pc-adapter-api`.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct UsageSummary {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cached_input_tokens: i64,
}

// ============================================================================
// Main entry
// ============================================================================

/// Fold the ACP runtime's pre-/post-turn usage into a stable per-run summary.
///
/// The runtime persists the latest turn's breakdown, so the post-turn
/// snapshot is authoritative when it differs from the pre-turn snapshot. A
/// matching pre/post pair means the runtime did not advance the counter this
/// turn, so we fall back to the in-turn event breakdown.
pub fn summarize_acpx_turn_usage(
    input: &SummarizeAcpxTurnUsageInput,
) -> SummarizeAcpxTurnUsageOutput {
    let pre_breakdown = input
        .pre_status
        .as_ref()
        .and_then(|s| s.usage.as_ref())
        .and_then(|u| u.cumulative.clone());
    let post_breakdown = input
        .post_status
        .as_ref()
        .and_then(|s| s.usage.as_ref())
        .and_then(|u| u.cumulative.clone());

    let post_breakdown_is_stale = match (&pre_breakdown, &post_breakdown) {
        (Some(left), Some(right)) => usage_breakdowns_equal(left, right),
        _ => false,
    };

    let breakdown = if post_breakdown_is_stale {
        input.event_breakdown.clone()
    } else {
        post_breakdown.or_else(|| input.event_breakdown.clone())
    };

    let input_tokens = clamp_to_zero(breakdown.as_ref().and_then(|b| b.input_tokens));
    let output_tokens = clamp_to_zero(breakdown.as_ref().and_then(|b| b.output_tokens));
    let cached_read_tokens = clamp_to_zero(breakdown.as_ref().and_then(|b| b.cached_read_tokens));
    let cached_write_tokens = clamp_to_zero(breakdown.as_ref().and_then(|b| b.cached_write_tokens));

    let has_tokens =
        input_tokens > 0 || output_tokens > 0 || cached_read_tokens > 0 || cached_write_tokens > 0;

    let usage = if has_tokens {
        // Cache-write tokens are prompt tokens the provider billed to create
        // cache entries; `UsageSummary` has no dedicated field, so roll them
        // into `input_tokens`.
        Some(UsageSummary {
            input_tokens: input_tokens + cached_write_tokens,
            output_tokens,
            cached_input_tokens: cached_read_tokens,
        })
    } else {
        None
    };

    let usage_detail = breakdown.as_ref().map(|b| {
        let mut map = BTreeMap::new();
        if let Some(v) = b.input_tokens {
            map.insert("inputTokens".to_string(), v);
        }
        if let Some(v) = b.output_tokens {
            map.insert("outputTokens".to_string(), v);
        }
        if let Some(v) = b.cached_read_tokens {
            map.insert("cachedReadTokens".to_string(), v);
        }
        if let Some(v) = b.cached_write_tokens {
            map.insert("cachedWriteTokens".to_string(), v);
        }
        if let Some(v) = b.thought_tokens {
            map.insert("thoughtTokens".to_string(), v);
        }
        if let Some(v) = b.total_tokens {
            map.insert("totalTokens".to_string(), v);
        }
        map
    });

    let previous_cost_usd = usd_cost_amount(
        input
            .pre_status
            .as_ref()
            .and_then(|s| s.usage.as_ref())
            .and_then(|u| u.cost.as_ref()),
    );
    let post_cost_usd = usd_cost_amount(
        input
            .post_status
            .as_ref()
            .and_then(|s| s.usage.as_ref())
            .and_then(|u| u.cost.as_ref()),
    );
    let post_cost_is_stale = matches!(
        (input.event_cost_usd, previous_cost_usd, post_cost_usd),
        (Some(_), Some(prev), Some(post)) if post == prev
    );
    let cumulative_cost_usd = if post_cost_is_stale {
        input.event_cost_usd
    } else {
        post_cost_usd.or(input.event_cost_usd)
    };

    let cost_usd = match (previous_cost_usd, cumulative_cost_usd) {
        (Some(prev), Some(cum)) if cum >= prev => Some(cum - prev),
        (_, Some(cum)) => Some(cum),
        _ => None,
    };

    SummarizeAcpxTurnUsageOutput {
        usage,
        usage_detail,
        cost_usd,
        cumulative_cost_usd,
    }
}

// ============================================================================
// Helpers
// ============================================================================

fn usage_breakdowns_equal(left: &AcpxTurnUsageBreakdown, right: &AcpxTurnUsageBreakdown) -> bool {
    fallback_i64(left.input_tokens) == fallback_i64(right.input_tokens)
        && fallback_i64(left.output_tokens) == fallback_i64(right.output_tokens)
        && fallback_i64(left.cached_read_tokens) == fallback_i64(right.cached_read_tokens)
        && fallback_i64(left.cached_write_tokens) == fallback_i64(right.cached_write_tokens)
        && fallback_i64(left.thought_tokens) == fallback_i64(right.thought_tokens)
        && fallback_i64(left.total_tokens) == fallback_i64(right.total_tokens)
}

fn usd_cost_amount(cost: Option<&AcpxTurnUsageCost>) -> Option<f64> {
    let cost = cost?;
    if !cost.amount.is_finite() {
        return None;
    }
    match cost.currency.as_deref() {
        Some(currency) if currency.trim().to_uppercase() != "USD" => None,
        _ => Some(cost.amount),
    }
}

fn fallback_i64(value: Option<i64>) -> i64 {
    value.unwrap_or(0)
}

fn clamp_to_zero(value: Option<i64>) -> i64 {
    value.unwrap_or(0).max(0)
}

// ============================================================================
// Free-form YAML/JSON helpers for callers that want to build the input from
// raw `serde_json::Value` blobs (e.g. heartbeat_runs.result_json).
// ============================================================================

/// Build a `SummarizeAcpxTurnUsageInput` from a free-form `serde_json::Value`
/// that mirrors the runtime's wire payload (e.g. `AcpRuntimeStatus`).
pub fn summarize_from_value(
    pre_status: Option<&Value>,
    post_status: Option<&Value>,
    event_breakdown: Option<&Value>,
    event_cost_usd: Option<f64>,
) -> SummarizeAcpxTurnUsageOutput {
    let pre = parse_status(pre_status);
    let post = parse_status(post_status);
    let event = event_breakdown.and_then(parse_breakdown);
    summarize_acpx_turn_usage(&SummarizeAcpxTurnUsageInput {
        pre_status: pre,
        post_status: post,
        event_breakdown: event,
        event_cost_usd,
    })
}

fn parse_status(value: Option<&Value>) -> Option<AcpxRuntimeStatusView> {
    let value = value?;
    let obj = value.as_object()?;
    let usage = obj.get("usage").and_then(|v| parse_usage(Some(v)));
    Some(AcpxRuntimeStatusView { usage })
}

fn parse_usage(value: Option<&Value>) -> Option<AcpxRuntimeUsageView> {
    let value = value?;
    let obj = value.as_object()?;
    let cumulative = obj.get("cumulative").and_then(parse_breakdown);
    let cost = obj.get("cost").and_then(parse_cost);
    Some(AcpxRuntimeUsageView { cumulative, cost })
}

fn parse_breakdown(value: &Value) -> Option<AcpxTurnUsageBreakdown> {
    let obj = value.as_object()?;
    Some(AcpxTurnUsageBreakdown {
        input_tokens: parse_i64(obj.get("inputTokens")),
        output_tokens: parse_i64(obj.get("outputTokens")),
        cached_read_tokens: parse_i64(obj.get("cachedReadTokens")),
        cached_write_tokens: parse_i64(obj.get("cachedWriteTokens")),
        thought_tokens: parse_i64(obj.get("thoughtTokens")),
        total_tokens: parse_i64(obj.get("totalTokens")),
    })
}

fn parse_cost(value: &Value) -> Option<AcpxTurnUsageCost> {
    let obj = value.as_object()?;
    let amount = obj.get("amount")?.as_f64()?;
    let currency = obj
        .get("currency")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    Some(AcpxTurnUsageCost { amount, currency })
}

fn parse_i64(value: Option<&Value>) -> Option<i64> {
    let value = value?;
    if let Some(int) = value.as_i64() {
        return Some(int);
    }
    if let Some(uint) = value.as_u64() {
        return Some(uint as i64);
    }
    if let Some(float) = value.as_f64() {
        return Some(float as i64);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_breakdown(
        input: i64,
        output: i64,
        cached_read: i64,
        cached_write: i64,
        thought: i64,
        total: i64,
    ) -> AcpxTurnUsageBreakdown {
        AcpxTurnUsageBreakdown {
            input_tokens: Some(input),
            output_tokens: Some(output),
            cached_read_tokens: Some(cached_read),
            cached_write_tokens: Some(cached_write),
            thought_tokens: Some(thought),
            total_tokens: Some(total),
        }
    }

    #[test]
    fn uses_post_turn_amount_when_cumulative_resets() {
        let pre = AcpxRuntimeStatusView {
            usage: Some(AcpxRuntimeUsageView {
                cumulative: None,
                cost: Some(AcpxTurnUsageCost {
                    amount: 2.5,
                    currency: Some("USD".into()),
                }),
            }),
        };
        let post = AcpxRuntimeStatusView {
            usage: Some(AcpxRuntimeUsageView {
                cumulative: Some(make_breakdown(10, 20, 0, 0, 0, 30)),
                cost: Some(AcpxTurnUsageCost {
                    amount: 0.3,
                    currency: Some("USD".into()),
                }),
            }),
        };
        let out = summarize_acpx_turn_usage(&SummarizeAcpxTurnUsageInput {
            pre_status: Some(pre),
            post_status: Some(post),
            event_breakdown: None,
            event_cost_usd: None,
        });
        assert!((out.cost_usd.unwrap() - 0.3).abs() < 1e-9);
        assert!((out.cumulative_cost_usd.unwrap() - 0.3).abs() < 1e-9);
    }

    #[test]
    fn ignores_non_usd_cost_amounts() {
        let post = AcpxRuntimeStatusView {
            usage: Some(AcpxRuntimeUsageView {
                cumulative: None,
                cost: Some(AcpxTurnUsageCost {
                    amount: 4.0,
                    currency: Some("EUR".into()),
                }),
            }),
        };
        let out = summarize_acpx_turn_usage(&SummarizeAcpxTurnUsageInput {
            pre_status: None,
            post_status: Some(post),
            event_breakdown: None,
            event_cost_usd: None,
        });
        assert_eq!(out.cost_usd, None);
        assert_eq!(out.cumulative_cost_usd, None);
    }

    #[test]
    fn returns_no_usage_when_nothing_reported() {
        let out = summarize_acpx_turn_usage(&SummarizeAcpxTurnUsageInput::default());
        assert_eq!(out.usage, None);
        assert_eq!(out.cost_usd, None);
    }

    #[test]
    fn suppresses_stale_breakdown_when_post_is_unchanged() {
        let stale = make_breakdown(10, 500, 30, 0, 0, 540);
        let pre = AcpxRuntimeStatusView {
            usage: Some(AcpxRuntimeUsageView {
                cumulative: Some(stale.clone()),
                cost: Some(AcpxTurnUsageCost {
                    amount: 0.5,
                    currency: Some("USD".into()),
                }),
            }),
        };
        let post = AcpxRuntimeStatusView {
            usage: Some(AcpxRuntimeUsageView {
                cumulative: Some(stale),
                cost: Some(AcpxTurnUsageCost {
                    amount: 0.5,
                    currency: Some("USD".into()),
                }),
            }),
        };
        let out = summarize_acpx_turn_usage(&SummarizeAcpxTurnUsageInput {
            pre_status: Some(pre),
            post_status: Some(post),
            event_breakdown: None,
            event_cost_usd: None,
        });
        assert_eq!(out.usage, None);
        assert_eq!(out.usage_detail, None);
        assert!(out.cost_usd.unwrap().abs() < 1e-9);
    }

    #[test]
    fn prefers_event_breakdown_when_persisted_is_stale() {
        let stale = make_breakdown(10, 500, 30, 0, 0, 540);
        let current = make_breakdown(25, 75, 5, 0, 0, 105);
        let pre = AcpxRuntimeStatusView {
            usage: Some(AcpxRuntimeUsageView {
                cumulative: Some(stale.clone()),
                cost: None,
            }),
        };
        let post = AcpxRuntimeStatusView {
            usage: Some(AcpxRuntimeUsageView {
                cumulative: Some(stale),
                cost: None,
            }),
        };
        let out = summarize_acpx_turn_usage(&SummarizeAcpxTurnUsageInput {
            pre_status: Some(pre),
            post_status: Some(post),
            event_breakdown: Some(current),
            event_cost_usd: None,
        });
        assert_eq!(
            out.usage,
            Some(UsageSummary {
                input_tokens: 25,
                output_tokens: 75,
                cached_input_tokens: 5,
            })
        );
    }

    #[test]
    fn treats_omitted_zero_fields_as_stale_breakdown() {
        let current = make_breakdown(25, 75, 5, 0, 0, 105);
        let pre = AcpxRuntimeStatusView {
            usage: Some(AcpxRuntimeUsageView {
                cumulative: Some(make_breakdown(10, 500, 0, 0, 0, 0)),
                cost: None,
            }),
        };
        let post = AcpxRuntimeStatusView {
            usage: Some(AcpxRuntimeUsageView {
                cumulative: Some(make_breakdown(10, 500, 0, 0, 0, 0)),
                cost: None,
            }),
        };
        let out = summarize_acpx_turn_usage(&SummarizeAcpxTurnUsageInput {
            pre_status: Some(pre),
            post_status: Some(post),
            event_breakdown: Some(current),
            event_cost_usd: None,
        });
        assert_eq!(
            out.usage,
            Some(UsageSummary {
                input_tokens: 25,
                output_tokens: 75,
                cached_input_tokens: 5,
            })
        );
    }

    #[test]
    fn does_not_reuse_stale_tokens_when_only_cost_reported() {
        let stale = make_breakdown(10, 500, 30, 0, 0, 540);
        let pre = AcpxRuntimeStatusView {
            usage: Some(AcpxRuntimeUsageView {
                cumulative: Some(stale.clone()),
                cost: Some(AcpxTurnUsageCost {
                    amount: 0.5,
                    currency: Some("USD".into()),
                }),
            }),
        };
        let post = AcpxRuntimeStatusView {
            usage: Some(AcpxRuntimeUsageView {
                cumulative: Some(stale),
                cost: Some(AcpxTurnUsageCost {
                    amount: 0.5,
                    currency: Some("USD".into()),
                }),
            }),
        };
        let out = summarize_acpx_turn_usage(&SummarizeAcpxTurnUsageInput {
            pre_status: Some(pre),
            post_status: Some(post),
            event_breakdown: None,
            event_cost_usd: Some(0.75),
        });
        assert_eq!(out.usage, None);
        assert!((out.cost_usd.unwrap() - 0.25).abs() < 1e-9);
        assert!((out.cumulative_cost_usd.unwrap() - 0.75).abs() < 1e-9);
    }

    #[test]
    fn summarize_from_value_uses_event_breakdown_when_needed() {
        let pre = serde_json::json!({
            "usage": {
                "cumulative": { "inputTokens": 10, "outputTokens": 500, "cachedReadTokens": 30 },
                "cost": { "amount": 0.5, "currency": "USD" }
            }
        });
        let post = serde_json::json!({
            "usage": {
                "cumulative": { "inputTokens": 10, "outputTokens": 500, "cachedReadTokens": 30 },
                "cost": { "amount": 0.5, "currency": "USD" }
            }
        });
        let event = serde_json::json!({
            "inputTokens": 25,
            "outputTokens": 75,
            "cachedReadTokens": 5
        });
        let out = summarize_from_value(Some(&pre), Some(&post), Some(&event), None);
        assert_eq!(
            out.usage,
            Some(UsageSummary {
                input_tokens: 25,
                output_tokens: 75,
                cached_input_tokens: 5,
            })
        );
    }

    #[test]
    fn usage_detail_drops_non_numeric_fields() {
        let breakdown = make_breakdown(1, 2, 3, 4, 5, 6);
        let map = summarize_acpx_turn_usage(&SummarizeAcpxTurnUsageInput {
            post_status: Some(AcpxRuntimeStatusView {
                usage: Some(AcpxRuntimeUsageView {
                    cumulative: Some(breakdown),
                    cost: None,
                }),
            }),
            ..Default::default()
        })
        .usage_detail
        .unwrap();
        assert_eq!(map.get("inputTokens"), Some(&1));
        assert_eq!(map.get("outputTokens"), Some(&2));
        assert_eq!(map.get("cachedReadTokens"), Some(&3));
        assert_eq!(map.get("cachedWriteTokens"), Some(&4));
        assert_eq!(map.get("thoughtTokens"), Some(&5));
        assert_eq!(map.get("totalTokens"), Some(&6));
    }
}
