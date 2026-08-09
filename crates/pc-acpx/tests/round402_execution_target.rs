//! R402 - integration tests for `execution_target` (port of Node
//! `execution-target.ts` from
//! `paperclip/packages/adapter-utils/src/`).
//!
//! These tests cover the cross-module flow an adapter takes when it
//! needs to know whether a target is local / SSH / sandbox, what its
//! cwd / timeout / identity should be, and how it picks the right
//! asset directory at runtime.

use pc_acpx::execution_target::{
    adapter_execution_target_from_remote_execution, adapter_execution_target_is_remote,
    adapter_execution_target_remote_cwd, adapter_execution_target_session_identity,
    adapter_execution_target_session_matches, adapter_execution_target_to_remote_spec,
    adapter_execution_target_uses_managed_home, adapter_execution_target_uses_paperclip_bridge,
    describe_adapter_execution_target, format_adapter_execution_timeout_error_message,
    format_adapter_execution_timeout_start_log_line, is_adapter_execution_target_instance,
    is_bridge_debug_enabled_from, override_adapter_execution_target_remote_cwd,
    parse_adapter_execution_target, parse_ssh_remote_execution_spec, read_adapter_execution_target,
    resolve_adapter_execution_target_cwd, resolve_adapter_execution_target_timeout,
    resolve_adapter_execution_target_timeout_sec, resolve_host_for_url, runtime_asset_dir,
    AdapterExecutionTarget, AdapterExecutionTargetTimeoutResolution,
    AdapterExecutionTargetTimeoutSource, AdapterLocalExecutionTarget,
    AdapterLocalExecutionTargetMetadata, AdapterRemoteExecutionTarget,
    AdapterSandboxExecutionTarget, AdapterSshExecutionTarget,
    PreparedAdapterExecutionTargetRuntime,
};
use serde_json::json;
use std::collections::BTreeMap;

// -------------------------------------------------------------------
// helpers
// -------------------------------------------------------------------

fn local() -> AdapterExecutionTarget {
    AdapterExecutionTarget::Local(AdapterLocalExecutionTarget {
        kind: "local".to_string(),
        environment_id: Some("env-L".to_string()),
        lease_id: Some("lease-L".to_string()),
        workspace_realization: None,
    })
}

fn ssh() -> AdapterExecutionTarget {
    AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Ssh(
        AdapterSshExecutionTarget {
            kind: "remote".to_string(),
            transport: "ssh".to_string(),
            environment_id: None,
            lease_id: None,
            remote_cwd: "/workspace/ssh".to_string(),
            spec: pc_acpx::execution_target::SshRemoteExecutionSpec {
                host: "host.example".to_string(),
                port: 2222,
                username: "alice".to_string(),
                remote_cwd: "/workspace/ssh".to_string(),
                remote_workspace_path: "/workspace/ssh".to_string(),
                private_key: Some("fake-key".to_string()),
                known_hosts: None,
                strict_host_key_checking: true,
            },
            workspace_realization: None,
        },
    ))
}

fn sandbox() -> AdapterExecutionTarget {
    AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Sandbox(
        AdapterSandboxExecutionTarget {
            kind: "remote".to_string(),
            transport: "sandbox".to_string(),
            provider_key: Some("e2b".to_string()),
            shell_command: Some("bash".to_string()),
            environment_id: Some("env-S".to_string()),
            lease_id: Some("lease-S".to_string()),
            remote_cwd: "/workspace/sb".to_string(),
            timeout_ms: Some(60_000),
            stream_run_logs: Some(true),
            workspace_realization: None,
        },
    ))
}

fn prepared(t: AdapterExecutionTarget) -> PreparedAdapterExecutionTargetRuntime {
    PreparedAdapterExecutionTargetRuntime {
        target: t,
        workspace_remote_dir: None,
        runtime_root_dir: None,
        asset_dirs: BTreeMap::new(),
        additional_source_dirs: BTreeMap::new(),
        additional_source_failures: vec![],
        has_restore_workspace: false,
    }
}

// -------------------------------------------------------------------
// host / url / debug helpers
// -------------------------------------------------------------------

#[test]
fn url_helpers_normalize_wildcards_to_localhost() {
    assert_eq!(resolve_host_for_url("0.0.0.0"), "localhost");
    assert_eq!(resolve_host_for_url("::"), "localhost");
    assert_eq!(resolve_host_for_url(""), "localhost");
    assert_eq!(resolve_host_for_url("host.example"), "host.example");
}

#[test]
fn bridge_debug_flag_detects_truthy_values() {
    for v in ["1", "true", "YES", "Yes", "TrUe"] {
        assert!(
            is_bridge_debug_enabled_from(Some(v)),
            "expected truthy for {v}"
        );
    }
    for v in ["0", "false", "no", "off", "garbage", ""] {
        assert!(
            !is_bridge_debug_enabled_from(Some(v)),
            "expected falsy for {v}"
        );
    }
    assert!(!is_bridge_debug_enabled_from(None));
}

// -------------------------------------------------------------------
// type predicate
// -------------------------------------------------------------------

#[test]
fn is_instance_accepts_all_three_target_shapes() {
    assert!(is_adapter_execution_target_instance(
        &json!({"kind": "local"})
    ));
    assert!(is_adapter_execution_target_instance(&json!({
        "kind": "remote",
        "transport": "ssh",
        "spec": {"host": "h", "username": "u", "remoteCwd": "/w", "port": 22},
    })));
    assert!(is_adapter_execution_target_instance(
        &json!({"kind": "remote", "transport": "sandbox", "remoteCwd": "/w"})
    ));
}

#[test]
fn is_instance_rejects_alien_shapes() {
    assert!(!is_adapter_execution_target_instance(&json!({})));
    assert!(!is_adapter_execution_target_instance(
        &json!({"kind": "alien"})
    ));
    assert!(!is_adapter_execution_target_instance(
        &json!({"kind": "remote", "transport": "ssh"})
    ));
    assert!(!is_adapter_execution_target_instance(
        &json!({"kind": "remote", "transport": "sandbox"})
    ));
}

// -------------------------------------------------------------------
// target routing
// -------------------------------------------------------------------

#[test]
fn is_remote_classifies_three_variants() {
    assert!(!adapter_execution_target_is_remote(Some(&local())));
    assert!(adapter_execution_target_is_remote(Some(&ssh())));
    assert!(adapter_execution_target_is_remote(Some(&sandbox())));
    assert!(!adapter_execution_target_is_remote(None));
}

#[test]
fn uses_managed_home_only_sandbox() {
    assert!(!adapter_execution_target_uses_managed_home(Some(&local())));
    assert!(!adapter_execution_target_uses_managed_home(Some(&ssh())));
    assert!(adapter_execution_target_uses_managed_home(Some(&sandbox())));
}

#[test]
fn uses_paperclip_bridge_alias_of_is_remote() {
    assert!(!adapter_execution_target_uses_paperclip_bridge(Some(
        &local()
    )));
    assert!(adapter_execution_target_uses_paperclip_bridge(Some(&ssh())));
    assert!(adapter_execution_target_uses_paperclip_bridge(Some(
        &sandbox()
    )));
}

#[test]
fn remote_cwd_resolves_correctly() {
    assert_eq!(
        adapter_execution_target_remote_cwd(Some(&local()), "/fallback"),
        "/fallback"
    );
    assert_eq!(
        adapter_execution_target_remote_cwd(Some(&ssh()), "/fallback"),
        "/workspace/ssh"
    );
    assert_eq!(
        adapter_execution_target_remote_cwd(Some(&sandbox()), "/fallback"),
        "/workspace/sb"
    );
}

// -------------------------------------------------------------------
// describe
// -------------------------------------------------------------------

#[test]
fn describe_each_target_variant_human_readably() {
    assert_eq!(describe_adapter_execution_target(None), "local environment");
    assert_eq!(
        describe_adapter_execution_target(Some(&local())),
        "local environment"
    );
    assert_eq!(
        describe_adapter_execution_target(Some(&ssh())),
        "SSH environment alice@host.example:2222"
    );
    assert_eq!(
        describe_adapter_execution_target(Some(&sandbox())),
        "sandbox environment (e2b)"
    );
}

// -------------------------------------------------------------------
// override cwd
// -------------------------------------------------------------------

#[test]
fn override_cwd_updates_ssh_target_and_spec_in_lockstep() {
    let t = override_adapter_execution_target_remote_cwd(ssh(), Some("/new/path"));
    assert_eq!(
        adapter_execution_target_remote_cwd(Some(&t), "/fallback"),
        "/new/path"
    );
    let spec = adapter_execution_target_to_remote_spec(Some(&t)).expect("ssh spec");
    assert_eq!(spec.remote_cwd, "/new/path");
}

#[test]
fn override_cwd_noop_when_target_already_matches() {
    let t = ssh();
    let out = override_adapter_execution_target_remote_cwd(t.clone(), Some("/workspace/ssh"));
    assert_eq!(
        adapter_execution_target_remote_cwd(Some(&out), "/fb"),
        adapter_execution_target_remote_cwd(Some(&t), "/fb"),
    );
}

#[test]
fn override_cwd_local_target_unchanged() {
    let t = local();
    let out = override_adapter_execution_target_remote_cwd(t.clone(), Some("/new"));
    assert!(matches!(out, AdapterExecutionTarget::Local(_)));
}

// -------------------------------------------------------------------
// resolve_cwd
// -------------------------------------------------------------------

#[test]
fn resolve_cwd_prefers_configured_otherwise_target_otherwise_local() {
    assert_eq!(
        resolve_adapter_execution_target_cwd(Some(&ssh()), Some("/cfg"), "/fb"),
        "/cfg"
    );
    assert_eq!(
        resolve_adapter_execution_target_cwd(Some(&ssh()), None, "/fb"),
        "/workspace/ssh"
    );
    assert_eq!(
        resolve_adapter_execution_target_cwd(Some(&local()), None, "/fb"),
        "/fb"
    );
    assert_eq!(
        resolve_adapter_execution_target_cwd(Some(&local()), Some("   "), "/fb"),
        "/fb"
    );
}

// -------------------------------------------------------------------
// timeout resolution
// -------------------------------------------------------------------

#[test]
fn resolve_timeout_positive_configured_passes_through() {
    let r = resolve_adapter_execution_target_timeout(Some(&sandbox()), Some(45.0));
    assert_eq!(r.source, AdapterExecutionTargetTimeoutSource::Configured);
    assert_eq!(r.timeout_sec, 45.0);
}

#[test]
fn resolve_timeout_negative_disabled() {
    let r = resolve_adapter_execution_target_timeout(Some(&local()), Some(-5.0));
    assert_eq!(r.source, AdapterExecutionTargetTimeoutSource::Configured);
    assert_eq!(r.timeout_sec, 0.0);
    assert!(r.is_disabled());
}

#[test]
fn resolve_timeout_zero_sandbox_falls_to_default() {
    let r = resolve_adapter_execution_target_timeout(Some(&sandbox()), Some(0.0));
    assert_eq!(
        r.source,
        AdapterExecutionTargetTimeoutSource::SandboxDefault
    );
    assert_eq!(
        r.timeout_sec as u64,
        pc_acpx::execution_target::DEFAULT_REMOTE_SANDBOX_ADAPTER_TIMEOUT_SEC
    );
}

#[test]
fn resolve_timeout_zero_local_is_unlimited() {
    let r = resolve_adapter_execution_target_timeout(Some(&local()), Some(0.0));
    assert_eq!(r.source, AdapterExecutionTargetTimeoutSource::Unlimited);
    assert!(r.is_disabled());
}

#[test]
fn resolve_timeout_none_sandbox_picks_default() {
    let r = resolve_adapter_execution_target_timeout(Some(&sandbox()), None);
    assert_eq!(
        r.source,
        AdapterExecutionTargetTimeoutSource::SandboxDefault
    );
}

#[test]
fn resolve_timeout_sec_returns_just_seconds_value() {
    assert_eq!(
        resolve_adapter_execution_target_timeout_sec(Some(&sandbox()), None) as u64,
        pc_acpx::execution_target::DEFAULT_REMOTE_SANDBOX_ADAPTER_TIMEOUT_SEC,
    );
}

#[test]
fn timeout_error_message_includes_value_and_source() {
    let r = AdapterExecutionTargetTimeoutResolution {
        timeout_sec: 60.0,
        source: AdapterExecutionTargetTimeoutSource::Configured,
    };
    let msg = format_adapter_execution_timeout_error_message(&r);
    assert!(msg.contains("timeoutSec=60"));
    assert!(msg.contains("configured"));
}

#[test]
fn timeout_start_log_line_when_disabled_omits_numeric_value() {
    let r = AdapterExecutionTargetTimeoutResolution {
        timeout_sec: 0.0,
        source: AdapterExecutionTargetTimeoutSource::Configured,
    };
    let line = format_adapter_execution_timeout_start_log_line(&r);
    assert!(line.contains("none"));
    assert!(line.contains("explicitly disabled"));
}

#[test]
fn timeout_start_log_line_when_enabled_lists_knob() {
    let r = AdapterExecutionTargetTimeoutResolution {
        timeout_sec: 60.0,
        source: AdapterExecutionTargetTimeoutSource::Configured,
    };
    let line = format_adapter_execution_timeout_start_log_line(&r);
    assert!(line.contains("timeoutSec=60"));
    assert!(line.contains("adapterConfig.timeoutSec"));
}

// -------------------------------------------------------------------
// parseAdapterExecutionTarget / fromRemoteExecution / read
// -------------------------------------------------------------------

#[test]
fn parse_round_trip_local_target() {
    let v = json!({
        "kind": "local",
        "environmentId": "env-1",
        "leaseId": "lease-1",
    });
    let t = parse_adapter_execution_target(&v).expect("must parse");
    assert!(matches!(t, AdapterExecutionTarget::Local(_)));
}

#[test]
fn parse_round_trip_ssh_target() {
    let v = json!({
        "kind": "remote",
        "transport": "ssh",
        "remoteCwd": "/workspace",
        "spec": {"host": "h", "username": "u", "remoteCwd": "/workspace", "port": 22},
    });
    let t = parse_adapter_execution_target(&v).expect("must parse");
    assert!(matches!(
        t,
        AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Ssh(_))
    ));
}

#[test]
fn parse_round_trip_sandbox_target() {
    let v = json!({
        "kind": "remote",
        "transport": "sandbox",
        "remoteCwd": "/w",
        "providerKey": "e2b",
        "timeoutMs": 30_000,
        "streamRunLogs": true,
    });
    let t = parse_adapter_execution_target(&v).expect("must parse");
    assert!(matches!(
        t,
        AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Sandbox(_))
    ));
}

#[test]
fn parse_rejects_missing_required_fields() {
    assert!(parse_adapter_execution_target(&json!({})).is_none());
    assert!(parse_adapter_execution_target(&json!({"kind": "remote"})).is_none());
    assert!(
        parse_adapter_execution_target(&json!({"kind": "remote", "transport": "sandbox"}))
            .is_none()
    );
}

#[test]
fn parse_ssh_remote_execution_spec_then_build_target() {
    let v = json!({
        "host": "h",
        "username": "u",
        "remoteCwd": "/w",
        "port": 2222,
    });
    let spec = parse_ssh_remote_execution_spec(&v).expect("must parse");
    assert_eq!(spec.port, 2222);
    let t = adapter_execution_target_from_remote_execution(
        &v,
        Some(AdapterLocalExecutionTargetMetadata {
            environment_id: Some("env-L".to_string()),
            lease_id: None,
        }),
    )
    .expect("must build");
    assert!(matches!(
        t,
        AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Ssh(_))
    ));
}

#[test]
fn read_prefers_typed_target_over_legacy() {
    let typed = json!({"kind": "local"});
    let legacy = json!({"host": "h", "username": "u", "remoteCwd": "/w", "port": 22});
    let t = read_adapter_execution_target(Some(&typed), Some(&legacy)).expect("typed wins");
    assert!(matches!(t, AdapterExecutionTarget::Local(_)));
}

#[test]
fn read_falls_back_to_legacy_when_typed_is_invalid() {
    let typed = json!({"kind": "alien"});
    let legacy = json!({"host": "h", "username": "u", "remoteCwd": "/w", "port": 22});
    let t = read_adapter_execution_target(Some(&typed), Some(&legacy)).expect("legacy");
    assert!(matches!(
        t,
        AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Ssh(_))
    ));
}

// -------------------------------------------------------------------
// session identity
// -------------------------------------------------------------------

#[test]
fn session_identity_none_for_local_target() {
    let id = adapter_execution_target_session_identity(Some(&local()));
    assert!(id.is_none());
}

#[test]
fn session_identity_for_ssh_carries_4tuple() {
    let id = adapter_execution_target_session_identity(Some(&ssh())).expect("ssh id");
    match id {
        pc_acpx::execution_target::AdapterExecutionTargetSessionIdentity::Ssh(s) => {
            assert_eq!(s.host, "host.example");
            assert_eq!(s.username, "alice");
            assert_eq!(s.port, 2222);
            assert_eq!(s.transport, "ssh");
        }
        _ => panic!("expected ssh variant"),
    }
}

#[test]
fn session_identity_for_sandbox_carries_5tuple() {
    let id = adapter_execution_target_session_identity(Some(&sandbox())).expect("sb id");
    match id {
        pc_acpx::execution_target::AdapterExecutionTargetSessionIdentity::Sandbox(s) => {
            assert_eq!(s.transport, "sandbox");
            assert_eq!(s.provider_key.as_deref(), Some("e2b"));
            assert_eq!(s.environment_id.as_deref(), Some("env-S"));
            assert_eq!(s.lease_id.as_deref(), Some("lease-S"));
            assert_eq!(s.remote_cwd, "/workspace/sb");
        }
        _ => panic!("expected sandbox variant"),
    }
}

#[test]
fn session_match_sandbox_round_trip_ignores_extra() {
    let t = sandbox();
    let saved = json!({
        "transport": "sandbox",
        "providerKey": "e2b",
        "environmentId": "env-S",
        "leaseId": "lease-S",
        "remoteCwd": "/workspace/sb",
        "extraIgnored": "junk",
    });
    assert!(adapter_execution_target_session_matches(&saved, Some(&t)));
}

#[test]
fn session_match_local_empty_saved() {
    assert!(adapter_execution_target_session_matches(
        &json!({}),
        Some(&local())
    ));
    assert!(!adapter_execution_target_session_matches(
        &json!({"x": 1}),
        Some(&local())
    ));
}

// -------------------------------------------------------------------
// runtime_asset_dir
// -------------------------------------------------------------------

#[test]
fn runtime_asset_dir_picks_map_value_when_present() {
    let mut dirs = BTreeMap::new();
    dirs.insert(
        "skill-1".to_string(),
        "/sandbox/runtime/skill-1".to_string(),
    );
    let mut p = prepared(sandbox());
    p.asset_dirs = dirs;
    assert_eq!(
        runtime_asset_dir(&p, "skill-1", "/fallback"),
        "/sandbox/runtime/skill-1"
    );
}

#[test]
fn runtime_asset_dir_falls_back_to_well_known_path() {
    let p = prepared(local());
    assert_eq!(
        runtime_asset_dir(&p, "skill-1", "/workspace"),
        "/workspace/.paperclip-runtime/skill-1"
    );
}

#[test]
fn runtime_asset_dir_trims_trailing_slash() {
    let p = prepared(local());
    assert_eq!(
        runtime_asset_dir(&p, "skill-1", "/workspace/"),
        "/workspace/.paperclip-runtime/skill-1"
    );
}

// -------------------------------------------------------------------
// Cross-module smoke: full execution-target resolution pipeline.
// -------------------------------------------------------------------

#[test]
fn cross_module_smoke_router_picks_correct_lane_per_target() {
    // Parse each target variant and verify the router picks the
    // right lane (local = no ssh runner, ssh = ssh-spec, sandbox
    // = sandbox-default).
    let local_v = json!({"kind": "local"});
    let ssh_v = json!({
        "kind": "remote", "transport": "ssh", "remoteCwd": "/w",
        "spec": {"host": "h", "username": "u", "remoteCwd": "/w", "port": 22},
    });
    let sandbox_v = json!({"kind": "remote", "transport": "sandbox", "remoteCwd": "/w"});

    let lt = parse_adapter_execution_target(&local_v).expect("local");
    let st = parse_adapter_execution_target(&ssh_v).expect("ssh");
    let bt = parse_adapter_execution_target(&sandbox_v).expect("sandbox");

    // Classification
    assert!(!adapter_execution_target_is_remote(Some(&lt)));
    assert!(adapter_execution_target_is_remote(Some(&st)));
    assert!(adapter_execution_target_is_remote(Some(&bt)));

    // Managed-home flag (sandbox only)
    assert!(!adapter_execution_target_uses_managed_home(Some(&lt)));
    assert!(!adapter_execution_target_uses_managed_home(Some(&st)));
    assert!(adapter_execution_target_uses_managed_home(Some(&bt)));

    // CWD resolution: configured wins over target
    assert_eq!(
        resolve_adapter_execution_target_cwd(Some(&st), Some("/cfg"), "/fb"),
        "/cfg"
    );
    assert_eq!(
        resolve_adapter_execution_target_cwd(Some(&st), None, "/fb"),
        "/w"
    );

    // Timeout: positive configured for local; sandbox-default for unset sandbox; unlimited for ssh+none
    assert_eq!(
        resolve_adapter_execution_target_timeout(Some(&lt), Some(120.0)).timeout_sec,
        120.0
    );
    assert_eq!(
        resolve_adapter_execution_target_timeout(Some(&bt), None).source,
        AdapterExecutionTargetTimeoutSource::SandboxDefault
    );
    assert_eq!(
        resolve_adapter_execution_target_timeout(Some(&st), None).source,
        AdapterExecutionTargetTimeoutSource::Unlimited
    );

    // Session identity: SSH variant persists host; Sandbox variant persists providerKey/remoteCwd
    let ssh_id = adapter_execution_target_session_identity(Some(&st)).expect("ssh id");
    let sandbox_id = adapter_execution_target_session_identity(Some(&bt)).expect("sb id");
    let _ = (ssh_id, sandbox_id);

    // round-trip Parse the JSON representation
    let bt_json = serde_json::to_value(&bt).unwrap();
    let bt_back = parse_adapter_execution_target(&bt_json).expect("round-trip parse");
    assert!(matches!(
        bt_back,
        AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Sandbox(_))
    ));
}
