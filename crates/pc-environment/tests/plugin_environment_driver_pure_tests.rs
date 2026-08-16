// SPDX-License-Identifier: MIT
//
// R679 parity tests for `plugin-environment-driver.ts` pure functions.

use pc_environment::{
    plugin_driver_provider_key, resolve_plugin_execute_rpc_timeout_ms,
    PluginEnvironmentDriverKey, DEFAULT_READY_PLUGIN_WORKER_RECOVERY_TIMEOUT_MS,
    RPC_OVERHEAD_BUFFER_MS,
};
use serde_json::json;

#[test]
fn r679_plugin_driver_provider_key_basic() {
    let k = PluginEnvironmentDriverKey {
        plugin_key: "paperclip".to_string(),
        driver_key: "aws".to_string(),
    };
    assert_eq!(plugin_driver_provider_key(&k), "paperclip:aws");
}

#[test]
fn r679_plugin_driver_provider_key_uuid_like() {
    let k = PluginEnvironmentDriverKey {
        plugin_key: "p1".to_string(),
        driver_key: "d2".to_string(),
    };
    assert_eq!(plugin_driver_provider_key(&k), "p1:d2");
}

#[test]
fn r679_plugin_driver_provider_key_empty_parts() {
    let k = PluginEnvironmentDriverKey {
        plugin_key: String::new(),
        driver_key: String::new(),
    };
    assert_eq!(plugin_driver_provider_key(&k), ":");
}

#[test]
fn r679_plugin_driver_provider_key_with_colon_in_plugin() {
    let k = PluginEnvironmentDriverKey {
        plugin_key: "a:b".to_string(),
        driver_key: "c".to_string(),
    };
    assert_eq!(plugin_driver_provider_key(&k), "a:b:c");
}

#[test]
fn r679_rpc_timeout_constants_match_node() {
    assert_eq!(RPC_OVERHEAD_BUFFER_MS, 30_000);
    assert_eq!(DEFAULT_READY_PLUGIN_WORKER_RECOVERY_TIMEOUT_MS, 2_000);
}

#[test]
fn r679_rpc_timeout_from_requested_only() {
    let cfg = json!({});
    let r = resolve_plugin_execute_rpc_timeout_ms(Some(5_000.0), &cfg);
    assert_eq!(r, Some(5_000 + 30_000));
}

#[test]
fn r679_rpc_timeout_from_config_only_when_requested_zero() {
    let cfg = json!({ "timeoutMs": 10_000 });
    let r = resolve_plugin_execute_rpc_timeout_ms(Some(0.0), &cfg);
    assert_eq!(r, Some(10_000 + 30_000));
}

#[test]
fn r679_rpc_timeout_from_config_only_when_requested_negative() {
    let cfg = json!({ "timeoutMs": 10_000 });
    let r = resolve_plugin_execute_rpc_timeout_ms(Some(-1.0), &cfg);
    assert_eq!(r, Some(10_000 + 30_000));
}

#[test]
fn r679_rpc_timeout_from_config_only_when_requested_none() {
    let cfg = json!({ "timeoutMs": 10_000 });
    let r = resolve_plugin_execute_rpc_timeout_ms(None, &cfg);
    assert_eq!(r, Some(10_000 + 30_000));
}

#[test]
fn r679_rpc_timeout_requested_wins_over_config() {
    let cfg = json!({ "timeoutMs": 1_000 });
    let r = resolve_plugin_execute_rpc_timeout_ms(Some(20_000.0), &cfg);
    assert_eq!(r, Some(20_000 + 30_000));
}

#[test]
fn r679_rpc_timeout_no_requested_no_config_returns_none() {
    let cfg = json!({});
    let r = resolve_plugin_execute_rpc_timeout_ms(None, &cfg);
    assert_eq!(r, None);
}

#[test]
fn r679_rpc_timeout_config_zero_returns_none() {
    let cfg = json!({ "timeoutMs": 0 });
    let r = resolve_plugin_execute_rpc_timeout_ms(None, &cfg);
    assert_eq!(r, None);
}

#[test]
fn r679_rpc_timeout_config_negative_returns_none() {
    let cfg = json!({ "timeoutMs": -5 });
    let r = resolve_plugin_execute_rpc_timeout_ms(None, &cfg);
    assert_eq!(r, None);
}

#[test]
fn r679_rpc_timeout_config_non_numeric_returns_none() {
    let cfg = json!({ "timeoutMs": "10000" });
    let r = resolve_plugin_execute_rpc_timeout_ms(None, &cfg);
    assert_eq!(r, None);
}

#[test]
fn r679_rpc_timeout_config_null_returns_none() {
    let cfg = json!({ "timeoutMs": null });
    let r = resolve_plugin_execute_rpc_timeout_ms(None, &cfg);
    assert_eq!(r, None);
}

#[test]
fn r679_rpc_timeout_config_float_truncates_down() {
    let cfg = json!({ "timeoutMs": 12_345.9 });
    let r = resolve_plugin_execute_rpc_timeout_ms(None, &cfg);
    assert_eq!(r, Some(12_345 + 30_000));
}

#[test]
fn r679_rpc_timeout_config_integer_u64_passthrough() {
    let cfg = json!({ "timeoutMs": 60_000u64 });
    let r = resolve_plugin_execute_rpc_timeout_ms(None, &cfg);
    assert_eq!(r, Some(60_000 + 30_000));
}

#[test]
fn r679_rpc_timeout_requested_float_truncates_down() {
    let cfg = json!({});
    let r = resolve_plugin_execute_rpc_timeout_ms(Some(7_777.7), &cfg);
    assert_eq!(r, Some(7_777 + 30_000));
}

#[test]
fn r679_rpc_timeout_requested_nan_falls_through_to_config() {
    let cfg = json!({ "timeoutMs": 4_000 });
    let r = resolve_plugin_execute_rpc_timeout_ms(Some(f64::NAN), &cfg);
    assert_eq!(r, Some(4_000 + 30_000));
}

#[test]
fn r679_rpc_timeout_requested_infinity_falls_through_to_config() {
    let cfg = json!({ "timeoutMs": 4_000 });
    let r = resolve_plugin_execute_rpc_timeout_ms(Some(f64::INFINITY), &cfg);
    assert_eq!(r, Some(4_000 + 30_000));
}

#[test]
fn r679_rpc_timeout_requested_neg_infinity_falls_through_to_config() {
    let cfg = json!({ "timeoutMs": 4_000 });
    let r = resolve_plugin_execute_rpc_timeout_ms(Some(f64::NEG_INFINITY), &cfg);
    assert_eq!(r, Some(4_000 + 30_000));
}

#[test]
fn r679_rpc_timeout_requested_just_above_zero_accepted() {
    let cfg = json!({});
    let r = resolve_plugin_execute_rpc_timeout_ms(Some(0.0001), &cfg);
    assert_eq!(r, Some(30_000));
}

#[test]
fn r679_rpc_timeout_huge_value_saturates_no_overflow() {
    let cfg = json!({});
    let r = resolve_plugin_execute_rpc_timeout_ms(Some(u64::MAX as f64), &cfg);
    assert_eq!(r, Some(u64::MAX));
}

#[test]
fn r679_rpc_timeout_extra_config_keys_ignored() {
    let cfg = json!({ "timeoutMs": 5_000, "foo": "bar", "nested": { "x": 1 } });
    let r = resolve_plugin_execute_rpc_timeout_ms(None, &cfg);
    assert_eq!(r, Some(5_000 + 30_000));
}
