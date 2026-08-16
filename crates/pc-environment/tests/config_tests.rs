//! Pure-function tests for the Node `environment-config.ts` 1:1 parity layer.

use pc_environment::{
    parse_environment_driver_config, parse_fake_sandbox_environment_config,
    parse_plugin_environment_config, parse_plugin_sandbox_environment_config,
    parse_sandbox_environment_config, parse_ssh_environment_config,
    read_ssh_environment_private_key_secret_id, strip_sandbox_provider_envelope,
    FakeSandboxEnvironmentConfig, ParsedEnvironmentConfig, PluginEnvironmentConfig,
    PluginSandboxEnvironmentConfig, SandboxEnvironmentConfig, SecretRef, SshEnvironmentConfig,
};
use serde_json::{json, Value};
use uuid::Uuid;

// =======================================================================
// SecretRef
// =======================================================================

#[test]
fn r675_secret_ref_parses_minimal() {
    let u = Uuid::new_v4();
    let v = json!({"type": "secret_ref", "secretId": u.to_string()});
    let r = SecretRef::parse(&v).expect("ok");
    assert_eq!(r.kind, "secret_ref");
    assert_eq!(r.secret_id, u);
    assert_eq!(r.version, None);
}

#[test]
fn r675_secret_ref_parses_with_latest() {
    let u = Uuid::new_v4();
    let v = json!({"type": "secret_ref", "secretId": u.to_string(), "version": "latest"});
    let r = SecretRef::parse(&v).expect("ok");
    assert!(matches!(r.version, Some(pc_environment::SecretRefVersion::Latest)));
}

#[test]
fn r675_secret_ref_rejects_wrong_type() {
    let v = json!({"type": "blob", "secretId": Uuid::new_v4().to_string()});
    let err = SecretRef::parse(&v).expect_err("should fail");
    assert!(err.issues[0].message.contains("literal 'secret_ref'"));
}

#[test]
fn r675_secret_ref_rejects_bad_uuid() {
    let v = json!({"type": "secret_ref", "secretId": "not-a-uuid"});
    let err = SecretRef::parse(&v).expect_err("should fail");
    assert!(err.issues[0].message.contains("uuid"));
}

// =======================================================================
// SSH Environment Config
// =======================================================================

#[test]
fn r675_ssh_minimal() {
    let v = json!({
        "host": "10.0.0.1",
        "username": "root",
        "remoteWorkspacePath": "/home/pc/work"
    });
    let p = parse_ssh_environment_config(&v).expect("ok");
    assert_eq!(p.host, "10.0.0.1");
    assert_eq!(p.port, 22);
    assert_eq!(p.username, "root");
    assert_eq!(p.remote_workspace_path, "/home/pc/work");
    assert_eq!(p.private_key, None);
    assert_eq!(p.private_key_secret_ref, None);
    assert!(p.strict_host_key_checking);
}

#[test]
fn r675_ssh_full_with_secret_ref() {
    let u = Uuid::new_v4();
    let v = json!({
        "host": "10.0.0.2",
        "port": 2222,
        "username": "agent",
        "remoteWorkspacePath": "/srv/agent",
        "privateKeySecretRef": {"type":"secret_ref","secretId":u.to_string()},
        "knownHosts": "ssh-ed25519 AAAA...",
        "strictHostKeyChecking": false
    });
    let p = parse_ssh_environment_config(&v).expect("ok");
    assert_eq!(p.port, 2222);
    assert_eq!(p.private_key_secret_ref.as_ref().unwrap().secret_id, u);
    assert_eq!(p.known_hosts.as_deref(), Some("ssh-ed25519 AAAA..."));
    assert!(!p.strict_host_key_checking);
}

#[test]
fn r675_ssh_relative_path_rejected() {
    let v = json!({
        "host": "h", "username": "u", "remoteWorkspacePath": "rel/path"
    });
    let err = parse_ssh_environment_config(&v).expect_err("should fail");
    assert!(err.message.contains("absolute"));
}

#[test]
fn r675_ssh_blank_host_rejected() {
    let v = json!({"host": "  ", "username": "u", "remoteWorkspacePath": "/a"});
    let err = parse_ssh_environment_config(&v).expect_err("should fail");
    assert!(err.message.contains("host"));
}

#[test]
fn r675_ssh_blank_username_rejected() {
    let v = json!({"host": "h", "username": "", "remoteWorkspacePath": "/a"});
    let err = parse_ssh_environment_config(&v).expect_err("should fail");
    assert!(err.message.contains("username"));
}

#[test]
fn r675_ssh_blank_remote_path_rejected() {
    let v = json!({"host": "h", "username": "u"});
    let err = parse_ssh_environment_config(&v).expect_err("should fail");
    assert!(err.message.contains("workspace path"));
}

#[test]
fn r675_ssh_private_key_rejected_in_canonical() {
    // Canonical sshEnvironmentConfigSchema has privateKey default null,
    // so any non-null value is invalid
    let v = json!({
        "host": "h",
        "username": "u",
        "remoteWorkspacePath": "/a",
        "privateKey": "PEM-STRING"
    });
    let err = parse_ssh_environment_config(&v).expect_err("should fail");
    assert!(err.message.contains("privateKey"));
}

#[test]
fn r675_ssh_probe_allows_private_key() {
    let v = json!({
        "host": "h",
        "username": "u",
        "remoteWorkspacePath": "/a",
        "privateKey": "PEM-STRING"
    });
    let p = pc_environment::normalize_ssh_for_probe(&v).expect("ok");
    assert_eq!(p.private_key.as_deref(), Some("PEM-STRING"));
}

// =======================================================================
// Fake Sandbox
// =======================================================================

#[test]
fn r675_fake_sandbox_defaults() {
    let v = json!({});
    let p = parse_fake_sandbox_environment_config(&v).expect("ok");
    assert_eq!(p.provider, "fake");
    assert_eq!(p.image, "ubuntu:24.04");
    assert!(!p.reuse_lease);
}

#[test]
fn r675_fake_sandbox_with_image() {
    let v = json!({"provider":"fake","image":"alpine:3.19","reuseLease":true});
    let p = parse_fake_sandbox_environment_config(&v).expect("ok");
    assert_eq!(p.image, "alpine:3.19");
    assert!(p.reuse_lease);
}

// =======================================================================
// Plugin Sandbox
// =======================================================================

#[test]
fn r675_plugin_sandbox_minimal() {
    let v = json!({"provider":"docker"});
    let p = parse_plugin_sandbox_environment_config(&v).expect("ok");
    assert_eq!(p.provider, "docker");
    assert_eq!(p.timeout_ms, None);
    assert!(!p.reuse_lease);
}

#[test]
fn r675_plugin_sandbox_with_timeout_and_extra() {
    let v = json!({
        "provider":"docker",
        "timeoutMs": 300000,
        "streamRunLogs": true,
        "image":"alpine",
        "networkMode":"bridge"
    });
    let p = parse_plugin_sandbox_environment_config(&v).expect("ok");
    assert_eq!(p.timeout_ms, Some(300000));
    assert_eq!(p.stream_run_logs, Some(true));
    // catchall driverConfig fields
    assert_eq!(p.extra.get("image").and_then(|v| v.as_str()), Some("alpine"));
    assert_eq!(p.extra.get("networkMode").and_then(|v| v.as_str()), Some("bridge"));
}

#[test]
fn r675_plugin_sandbox_rejects_invalid_provider() {
    let v = json!({"provider":"-bad-start"});
    let err = parse_plugin_sandbox_environment_config(&v).expect_err("should fail");
    assert!(err.message.contains("lowercase alphanumeric"));
}

#[test]
fn r675_plugin_sandbox_rejects_timeout_out_of_range() {
    let v = json!({"provider":"docker","timeoutMs":0});
    let err = parse_plugin_sandbox_environment_config(&v).expect_err("should fail");
    assert!(err.message.contains("timeoutMs"));
}

#[test]
fn r675_plugin_sandbox_timeout_max() {
    let v = json!({"provider":"docker","timeoutMs":86400000});
    let p = parse_plugin_sandbox_environment_config(&v).expect("ok");
    assert_eq!(p.timeout_ms, Some(86_400_000));
}

#[test]
fn r675_plugin_sandbox_timeout_above_max() {
    let v = json!({"provider":"docker","timeoutMs":86400001});
    let err = parse_plugin_sandbox_environment_config(&v).expect_err("should fail");
    assert!(err.message.contains("timeoutMs"));
}

// =======================================================================
// Plugin Environment (driver, not sandbox)
// =======================================================================

#[test]
fn r675_plugin_env_minimal() {
    let v = json!({"pluginKey":"my-plugin","driverKey":"slack"});
    let p = parse_plugin_environment_config(&v).expect("ok");
    assert_eq!(p.plugin_key, "my-plugin");
    assert_eq!(p.driver_key, "slack");
    assert!(p.driver_config.is_empty());
}

#[test]
fn r675_plugin_env_with_driver_config() {
    let v = json!({
        "pluginKey":"my-plugin",
        "driverKey":"slack",
        "driverConfig":{"workspace":"general","prefix":"pc-"}
    });
    let p = parse_plugin_environment_config(&v).expect("ok");
    assert_eq!(p.driver_config.get("workspace").and_then(|v| v.as_str()), Some("general"));
}

#[test]
fn r675_plugin_env_rejects_invalid_driver_key() {
    let v = json!({"pluginKey":"p","driverKey":"-bad"});
    let err = parse_plugin_environment_config(&v).expect_err("should fail");
    assert!(err.message.contains("driver key"));
}

#[test]
fn r675_plugin_env_rejects_blank_plugin_key() {
    let v = json!({"pluginKey":"","driverKey":"x"});
    let err = parse_plugin_environment_config(&v).expect_err("should fail");
    assert!(err.message.contains("pluginKey"));
}

// =======================================================================
// Sandbox dispatch
// =======================================================================

#[test]
fn r675_sandbox_dispatch_fake() {
    let v = json!({"image":"alpine:3.19"});
    let p = parse_sandbox_environment_config(&v).expect("ok");
    match p {
        SandboxEnvironmentConfig::Fake(f) => {
            assert_eq!(f.image, "alpine:3.19");
        }
        _ => panic!("expected fake"),
    }
}

#[test]
fn r675_sandbox_dispatch_plugin() {
    let v = json!({"provider":"docker"});
    let p = parse_sandbox_environment_config(&v).expect("ok");
    match p {
        SandboxEnvironmentConfig::Plugin(_) => {}
        _ => panic!("expected plugin"),
    }
}

#[test]
fn r675_sandbox_provider_default_fake() {
    let v = json!({});
    let p = pc_environment::get_sandbox_provider(&v);
    assert_eq!(p, "fake");
}

#[test]
fn r675_sandbox_provider_trimmed() {
    let v = json!({"provider":"  docker  "});
    let p = pc_environment::get_sandbox_provider(&v);
    assert_eq!(p, "docker");
}

// =======================================================================
// stripSandboxProviderEnvelope
// =======================================================================

#[test]
fn r675_strip_sandbox_provider_envelope() {
    let v = json!({"provider":"fake","image":"alpine","reuseLease":true});
    let out = strip_sandbox_provider_envelope(&v);
    assert!(!out.contains_key("provider"));
    assert_eq!(out.get("image").and_then(|v| v.as_str()), Some("alpine"));
    assert_eq!(out.get("reuseLease").and_then(|v| v.as_bool()), Some(true));
}

// =======================================================================
// parseEnvironmentDriverConfig dispatch
// =======================================================================

#[test]
fn r675_parse_env_driver_local() {
    let cfg = json!({"anything":"goes"});
    let p = parse_environment_driver_config("local", &cfg).expect("ok");
    assert!(matches!(p, ParsedEnvironmentConfig::Local));
}

#[test]
fn r675_parse_env_driver_ssh() {
    let cfg = json!({"host":"h","username":"u","remoteWorkspacePath":"/a"});
    let p = parse_environment_driver_config("ssh", &cfg).expect("ok");
    match p {
        ParsedEnvironmentConfig::Ssh(s) => assert_eq!(s.host, "h"),
        _ => panic!("expected ssh"),
    }
}

#[test]
fn r675_parse_env_driver_sandbox() {
    let cfg = json!({"provider":"docker"});
    let p = parse_environment_driver_config("sandbox", &cfg).expect("ok");
    assert!(matches!(p, ParsedEnvironmentConfig::Sandbox(_)));
}

#[test]
fn r675_parse_env_driver_plugin() {
    let cfg = json!({"pluginKey":"p","driverKey":"d"});
    let p = parse_environment_driver_config("plugin", &cfg).expect("ok");
    assert!(matches!(p, ParsedEnvironmentConfig::Plugin(_)));
}

#[test]
fn r675_parse_env_driver_unsupported() {
    let cfg = json!({});
    let err = parse_environment_driver_config("zzz", &cfg).expect_err("should fail");
    assert!(err.message.contains("Unsupported"));
}

// =======================================================================
// normalize
// =======================================================================

#[test]
fn r675_normalize_local() {
    let p = pc_environment::normalize_environment_config("local", Some(&json!({"a":1}))).expect("ok");
    match p {
        pc_environment::NormalizedEnvironmentConfig::Local(m) => {
            assert_eq!(m.get("a").and_then(|v| v.as_i64()), Some(1));
        }
        _ => panic!("expected local"),
    }
}

#[test]
fn r675_normalize_local_null_passes_through() {
    let p = pc_environment::normalize_environment_config("local", None).expect("ok");
    match p {
        pc_environment::NormalizedEnvironmentConfig::Local(m) => assert!(m.is_empty()),
        _ => panic!("expected local"),
    }
}

#[test]
fn r675_normalize_ssh() {
    let p = pc_environment::normalize_environment_config("ssh", Some(&json!({"host":"h","username":"u","remoteWorkspacePath":"/a"}))).expect("ok");
    match p {
        pc_environment::NormalizedEnvironmentConfig::Ssh(s) => assert_eq!(s.host, "h"),
        _ => panic!("expected ssh"),
    }
}

#[test]
fn r675_normalize_sandbox() {
    let p = pc_environment::normalize_environment_config("sandbox", Some(&json!({"provider":"docker"}))).expect("ok");
    assert!(matches!(
        p,
        pc_environment::NormalizedEnvironmentConfig::Sandbox(_)
    ));
}

#[test]
fn r675_normalize_plugin() {
    let p = pc_environment::normalize_environment_config(
        "plugin",
        Some(&json!({"pluginKey":"p","driverKey":"d"})),
    )
    .expect("ok");
    assert!(matches!(
        p,
        pc_environment::NormalizedEnvironmentConfig::Plugin(_)
    ));
}

#[test]
fn r675_normalize_unsupported() {
    let err = pc_environment::normalize_environment_config("zzz", None).expect_err("should fail");
    assert!(err.message.contains("Unsupported"));
}

#[test]
fn r675_normalize_ssh_propagates_issue() {
    let err = pc_environment::normalize_environment_config(
        "ssh",
        Some(&json!({"host":"","username":"u","remoteWorkspacePath":"/a"})),
    )
    .expect_err("should fail");
    assert!(err.issues.len() >= 1);
    assert!(err.message.contains("host"));
}

// =======================================================================
// readSshEnvironmentPrivateKeySecretId
// =======================================================================

#[test]
fn r675_read_ssh_secret_id_present() {
    let u = Uuid::new_v4();
    let v = json!({
        "host":"h","username":"u","remoteWorkspacePath":"/a",
        "privateKeySecretRef":{"type":"secret_ref","secretId":u.to_string()}
    });
    let got = read_ssh_environment_private_key_secret_id(&v);
    assert_eq!(got, Some(u.to_string()));
}

#[test]
fn r675_read_ssh_secret_id_absent() {
    let v = json!({"host":"h","username":"u","remoteWorkspacePath":"/a"});
    let got = read_ssh_environment_private_key_secret_id(&v);
    assert_eq!(got, None);
}

#[test]
fn r675_read_ssh_secret_id_invalid_input() {
    // Garbage payload that's not even valid SSH should not panic — return None.
    let got = read_ssh_environment_private_key_secret_id(&json!({}));
    assert_eq!(got, None);
}
