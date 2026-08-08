//! R403 - integration tests for `pc_acpx::ssh` (port of Node
//! `ssh.ts` from `paperclip/packages/adapter-utils/src/`).
//!
//! These tests verify the cross-module flow that an SSH-backed
//! adapter takes: parse an `SshRemoteExecutionSpec` JSON payload,
//! classify the connection config (host / port / username /
//! remote_cwd), build the `tar --exclude` argv + env hints, and
//! recognize shell-safe env var keys + known_hosts entries.

use pc_acpx::execution_target::{
    adapter_execution_target_from_remote_execution, adapter_execution_target_is_remote,
    adapter_execution_target_to_remote_spec, parse_adapter_execution_target,
    AdapterExecutionTarget, AdapterRemoteExecutionTarget,
};
use pc_acpx::ssh::{
    build_known_hosts_entry, is_valid_shell_env_key, parse_ssh_remote_execution_spec,
    shell_quote, tar_exclude_args, tar_pattern_to_regexp, tar_spawn_env_defaults,
    KnownHostsEntryInput, SshConnectionConfig, SshRemoteExecutionSpec,
};
use serde_json::json;

// -------------------------------------------------------------------
// canonical ssh spec parsing
// -------------------------------------------------------------------

#[test]
fn ssh_spec_round_trips_via_camelcase_json() {
    let original = SshRemoteExecutionSpec {
        host: "sandbox.example.com".to_string(),
        port: 2222,
        username: "paperclip".to_string(),
        remote_workspace_path: "/home/paperclip/work".to_string(),
        private_key: Some("-----BEGIN PRIVATE KEY-----abc".to_string()),
        known_hosts: Some("h.example.com ssh-ed25519 AAAA".to_string()),
        strict_host_key_checking: true,
        remote_cwd: "/home/paperclip/work/proj".to_string(),
    };
    let j = serde_json::to_value(&original).expect("to_value");
    // Confirm camelCase wire format
    assert!(j.get("host").is_some());
    assert!(j.get("remoteCwd").is_some());
    assert!(j.get("privateKey").is_some());
    assert!(j.get("knownHosts").is_some());
    assert!(j.get("strictHostKeyChecking").is_some());
    assert!(j.get("remoteWorkspacePath").is_some());

    let back = parse_ssh_remote_execution_spec(&j).expect("parse back");
    assert_eq!(back, original);
}

#[test]
fn ssh_spec_parser_accepts_string_port() {
    let v = json!({
        "host": "h",
        "username": "u",
        "remoteCwd": "/w",
        "port": "22",
    });
    let s = parse_ssh_remote_execution_spec(&v).expect("must parse");
    assert_eq!(s.port, 22);
}

#[test]
fn ssh_spec_parser_rejects_partial_payload() {
    let cases = [
        json!(null),
        json!("str"),
        json!(42),
        json!([1, 2]),
        json!({}),
        json!({"host": "h", "port": 22}),  // missing username, remoteCwd
        json!({"host": "h", "username": "u", "remoteCwd": "/w", "port": 0}),
        json!({"host": "h", "username": "u", "remoteCwd": "/w", "port": 70_000}),
    ];
    for v in cases {
        assert!(
            parse_ssh_remote_execution_spec(&v).is_none(),
            "should reject: {v}"
        );
    }
}

#[test]
fn ssh_spec_parser_optional_fields_default_to_none() {
    let v = json!({"host": "h", "username": "u", "remoteCwd": "/w", "port": 22});
    let s = parse_ssh_remote_execution_spec(&v).expect("parse");
    assert!(s.private_key.is_none());
    assert!(s.known_hosts.is_none());
    assert!(s.strict_host_key_checking);
}

#[test]
fn ssh_spec_workspace_path_defaults_to_remote_cwd() {
    let v = json!({"host": "h", "username": "u", "remoteCwd": "/main", "port": 22});
    let s = parse_ssh_remote_execution_spec(&v).expect("parse");
    assert_eq!(s.remote_workspace_path, "/main");
    assert_eq!(s.effective_remote_workspace_path(), "/main");
}

// -------------------------------------------------------------------
// tar helpers
// -------------------------------------------------------------------

#[test]
fn tar_exclude_args_prepends_resource_fork_pattern() {
    let args = tar_exclude_args(Some(&["node_modules".into(), "target".into()]));
    // Always starts with `._*` then any caller-supplied excludes
    assert_eq!(
        args,
        vec![
            "--exclude", "._*",
            "--exclude", "node_modules",
            "--exclude", "target",
        ]
    );
}

#[test]
fn tar_exclude_args_with_empty_inputs_is_still_resource_fork() {
    let args_none = tar_exclude_args(None);
    let args_empty = tar_exclude_args(Some(&[]));
    assert_eq!(args_none, vec!["--exclude", "._*"]);
    assert_eq!(args_empty, vec!["--exclude", "._*"]);
}

#[test]
fn tar_spawn_env_disables_mac_appledouble() {
    let env = tar_spawn_env_defaults();
    // Mirrors Node's `tarSpawnEnv` which sets COPYFILE_DISABLE=1 to
    // prevent macOS bsdtar from emitting ._foo.md metadata files.
    assert_eq!(env.get("COPYFILE_DISABLE").map(String::as_str), Some("1"));
    // BTreeMap: deterministic iteration order
    let keys: Vec<&str> = env.keys().map(String::as_str).collect();
    assert_eq!(keys, vec!["COPYFILE_DISABLE"]);
}

#[test]
fn tar_pattern_to_regexp_literal_match() {
    // `node_modules` pattern matches only `node_modules` (anchored)
    let re = tar_pattern_to_regexp("node_modules").expect("re");
    assert!(re.is_match("node_modules"));
    assert!(!re.is_match("sub/node_modules"));
}

#[test]
fn tar_pattern_to_regexp_handles_glob() {
    let star = tar_pattern_to_regexp("*/target").expect("re");
    assert!(star.is_match("a/target"));
    // Glob star does NOT span `/`
    assert!(!star.is_match("a/b/target"));

    let q = tar_pattern_to_regexp("?").expect("re");
    assert!(q.is_match("a"));
    assert!(!q.is_match("ab"));
}

#[test]
fn tar_pattern_to_regexp_escapes_regex_metachars() {
    // `.` should match literal `.`, not any character
    let re = tar_pattern_to_regexp("file.txt").expect("re");
    assert!(re.is_match("file.txt"));
    assert!(!re.is_match("fileXtxt"));
}


// -------------------------------------------------------------------
// shell_quote
// -------------------------------------------------------------------

#[test]
fn shell_quote_round_trips_safe_paths() {
    let cases = [
        ("/home/user/work", "'/home/user/work'"),
        ("plain", "'plain'"),
        ("/tmp/with space/dir", "'/tmp/with space/dir'"),
    ];
    for (input, expected) in cases {
        assert_eq!(shell_quote(input), expected);
    }
}

#[test]
fn shell_quote_handles_embedded_quote() {
    // 2 outer `'`s + 3 `'`s per escape (`'"'"'`)
    // = 2 + 2*3 = 8 total `'` characters in output.
    let q = shell_quote("name'with'quote");
    assert_eq!(q.chars().filter(|c| *c == '\'').count(), 8);
}



// -------------------------------------------------------------------
// shell env keys
// -------------------------------------------------------------------

#[test]
fn shell_env_keys_are_validated() {
    for k in ["PATH", "_PRIVATE", "FOO_BAR", "X", "a1_b2_c3"] {
        assert!(is_valid_shell_env_key(k), "{k} should be valid");
    }
    for k in ["1ST", "a-b", "a.b", "a b", "", "FOO$", "a,b"] {
        assert!(!is_valid_shell_env_key(k), "{k} should be invalid");
    }
}

// -------------------------------------------------------------------
// known_hosts entry
// -------------------------------------------------------------------

#[test]
fn known_hosts_entry_uses_bracketed_host_port_form() {
    let entry = build_known_hosts_entry(KnownHostsEntryInput {
        host: "h.example".to_string(),
        port: 2222,
        public_key: "ssh-ed25519 AAAA...rest".to_string(),
    });
    assert_eq!(entry, "[h.example]:2222 ssh-ed25519 AAAA...rest");
}

#[test]
fn known_hosts_entry_trims_whitespace_in_inputs() {
    let entry = build_known_hosts_entry(KnownHostsEntryInput {
        host: "  h.example  ".to_string(),
        port: 22,
        public_key: "  ssh-ed25519 AAAA  ".to_string(),
    });
    assert_eq!(entry, "[h.example]:22 ssh-ed25519 AAAA");
}

// -------------------------------------------------------------------
// cross-module smoke: parse -> session identity -> tar helpers
// -------------------------------------------------------------------

#[test]
fn cross_module_smoke_ssh_target_full_flow() {
    // 1. Parse a JSON SSH config into a real spec
    let payload = json!({
        "host": "sandbox.example.com",
        "username": "paperclip",
        "remoteCwd": "/home/paperclip/work/proj",
        "remoteWorkspacePath": "/home/paperclip/work",
        "port": 2222,
        "privateKey": "-----BEGIN PRIVATE KEY-----abc",
        "strictHostKeyChecking": true,
    });
    let spec = parse_ssh_remote_execution_spec(&payload).expect("must parse");
    assert_eq!(spec.host, "sandbox.example.com");
    assert_eq!(spec.port, 2222);
    assert_eq!(spec.username, "paperclip");
    assert_eq!(spec.remote_cwd, "/home/paperclip/work/proj");
    assert_eq!(spec.remote_workspace_path, "/home/paperclip/work");

    // 2. Verify we can build an execution target from the legacy
    //    shape used by execution_target's parser.
    let target = adapter_execution_target_from_remote_execution(
        &payload,
        Some(pc_acpx::execution_target::AdapterLocalExecutionTargetMetadata {
            environment_id: Some("env-1".to_string()),
            lease_id: Some("lease-1".to_string()),
        }),
    )
    .expect("target");
    assert!(matches!(
        target,
        AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Ssh(_))
    ));
    assert!(adapter_execution_target_is_remote(Some(&target)));

    // The execution_target's to_remote_spec emits a reference to
    // its own SshRemoteExecutionSpec (re-exported from ssh.rs). We
    // can read the host + port through that accessor.
    let remote_spec = adapter_execution_target_to_remote_spec(Some(&target)).expect("ssh spec");
    assert_eq!(remote_spec.host, "sandbox.example.com");
    assert_eq!(remote_spec.port, 2222);

    // 3. Build the tar exclude argv used by SSH tar transfers.
    let args = tar_exclude_args(Some(&["node_modules".into(), ".git".into()]));
    assert_eq!(
        args,
        vec![
            "--exclude", "._*",
            "--exclude", "node_modules",
            "--exclude", ".git",
        ]
    );

    // 4. The tar spawn env defaults (used by ssh tar streams).
    let env = tar_spawn_env_defaults();
    assert_eq!(env.get("COPYFILE_DISABLE").map(String::as_str), Some("1"));

    // 5. Sanity-check the known_hosts entry construction (used by
    //    the SSH env-lab fixture that Rust async half defers).
    let entry = build_known_hosts_entry(KnownHostsEntryInput {
        host: spec.host.clone(),
        port: spec.port,
        public_key: "ssh-ed25519 AAAA".to_string(),
    });
    assert_eq!(entry, "[sandbox.example.com]:2222 ssh-ed25519 AAAA");
}

#[test]
fn connection_config_round_trip_with_spec() {
    let cfg = SshConnectionConfig {
        host: "h".to_string(),
        port: 22,
        username: "u".to_string(),
        remote_workspace_path: "/w".to_string(),
        private_key: Some("pk".to_string()),
        known_hosts: None,
        strict_host_key_checking: true,
    };
    let spec = SshRemoteExecutionSpec::from_parts(cfg.clone(), "/w/cwd".to_string());
    let back = spec.as_connection_config();
    assert_eq!(back, cfg);

    // The spec carries the cwd; the connection config does not.
    assert_eq!(spec.remote_cwd, "/w/cwd");
    assert_eq!(back.remote_workspace_path, "/w");
}

// -------------------------------------------------------------------
// integration with execution_target::parseAdapterExecutionTarget
// -------------------------------------------------------------------

#[test]
fn execution_target_parser_supports_ssh_via_ssh_module() {
    let payload = json!({
        "kind": "remote",
        "transport": "ssh",
        "remoteCwd": "/w",
        "environmentId": "env-1",
        "leaseId": "lease-1",
        "spec": {
            "host": "h",
            "username": "u",
            "remoteCwd": "/w",
            "port": 22,
            "privateKey": "pk",
        },
    });
    let t = parse_adapter_execution_target(&payload).expect("must parse");
    let AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Ssh(s)) = t else {
        panic!("expected ssh target, got {t:?}");
    };
    let pc_acpx::execution_target::SshRemoteExecutionSpec {
        host,
        port,
        username,
        ..
    } = s.spec
    else {
        unreachable!()
    };
    assert_eq!(host, "h");
    assert_eq!(port, 22);
    assert_eq!(username, "u");
}
