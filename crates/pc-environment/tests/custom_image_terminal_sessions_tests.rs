//! Unit tests for `pc_environment::custom_image_terminal_sessions` +
//! `pc_environment::custom_image_setup_session_utils`.

use std::sync::Arc;

use chrono::{DateTime, Utc, TimeZone, Duration};
use pc_environment::{
    parse_custom_image_setup_ssh_command, validate_custom_image_setup_ssh_payload,
    CreateTerminalSessionInput, EnvironmentCustomImageTerminalConnectionRegistry,
    EnvironmentCustomImageTerminalConnectionClose,
    EnvironmentCustomImageTerminalPayloadValidationFailureCode,
    EnvironmentCustomImageTerminalSessionStore,
        EnvironmentCustomImageTerminalPayloadValidationResult,
    ParsedCustomImageSetupSshCommand,
    read_custom_image_setup_session_company_id, read_future_date, read_nullable_date,
    require_future_custom_image_setup_expiry,
    DEFAULT_TERMINAL_SESSION_TOKEN_TTL_MS, TERMINAL_SESSION_TOKEN_BYTES,
};
use serde_json::{json, Value};

fn ts(year: i32, month: u32, day: u32, hour: u32, min: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, day, hour, min, 0).unwrap()
}

// =======================================================================
// SSH command parser
// =======================================================================

#[test]
fn r678_parse_ssh_default_port() {
    let cmd = parse_custom_image_setup_ssh_command("ssh root@example.com").unwrap();
    assert_eq!(cmd.username, "root");
    assert_eq!(cmd.host, "example.com");
    assert_eq!(cmd.port, 22);
}

#[test]
fn r678_parse_ssh_p_before_destination() {
    let cmd = parse_custom_image_setup_ssh_command("ssh -p 2222 user@host").unwrap();
    assert_eq!(cmd.port, 2222);
}

#[test]
fn r678_parse_ssh_p_after_destination() {
    let cmd = parse_custom_image_setup_ssh_command("ssh user@host -p 5022").unwrap();
    assert_eq!(cmd.port, 5022);
}

#[test]
fn r678_parse_ssh_invalid() {
    assert!(parse_custom_image_setup_ssh_command("").is_none());
    assert!(parse_custom_image_setup_ssh_command("scp user@host:file").is_none());
    assert!(parse_custom_image_setup_ssh_command("ssh").is_none());
    assert!(parse_custom_image_setup_ssh_command("ssh user host extra").is_none());
    assert!(parse_custom_image_setup_ssh_command("ssh -p abc user@host").is_none());
    assert!(parse_custom_image_setup_ssh_command("ssh -p 70000 user@host").is_none());
    assert!(parse_custom_image_setup_ssh_command("ssh -p 0 user@host").is_none());
    assert!(parse_custom_image_setup_ssh_command("ssh us/er@host").is_none());
    assert!(parse_custom_image_setup_ssh_command("ssh user@ho:st").is_none());
    assert!(parse_custom_image_setup_ssh_command("ssh -bad-flag user@host").is_none());
    assert!(parse_custom_image_setup_ssh_command("ssh user@").is_none());
    assert!(parse_custom_image_setup_ssh_command("ssh @host").is_none());
}

#[test]
fn r678_parse_ssh_trims() {
    let cmd = parse_custom_image_setup_ssh_command("   ssh   root@example.com   ").unwrap();
    assert_eq!(cmd.username, "root");
    assert_eq!(cmd.host, "example.com");
}

// =======================================================================
// Payload validation
// =======================================================================

#[test]
fn r678_validate_ssh_payload_ok() {
    let payload = json!({"type":"ssh","command":"ssh root@example.com"});
    let now = ts(2026, 8, 16, 0, 0);
    let r = validate_custom_image_setup_ssh_payload(&payload, now);
    assert!(r.is_ok());
    match r {
        EnvironmentCustomImageTerminalPayloadValidationResult::True { ssh, connection_expires_at } => {
            assert_eq!(ssh.username, "root");
            assert_eq!(ssh.host, "example.com");
            assert!(connection_expires_at.is_none());
        }
        _ => unreachable!(),
    }
}

#[test]
fn r678_validate_rejects_non_ssh() {
    let payload = json!({"type":"tcp","command":"ssh root@h"});
    let r = validate_custom_image_setup_ssh_payload(&payload, ts(2026, 8, 16, 0, 0));
    match r {
        EnvironmentCustomImageTerminalPayloadValidationResult::False { status, code, .. } => {
            assert_eq!(status, 422);
            assert_eq!(code, EnvironmentCustomImageTerminalPayloadValidationFailureCode::UnsupportedPayload);
        }
        _ => panic!("expected false"),
    }
}

#[test]
fn r678_validate_rejects_non_object() {
    let r = validate_custom_image_setup_ssh_payload(&json!(null), ts(2026, 8, 16, 0, 0));
    assert!(!r.is_ok());
    let r = validate_custom_image_setup_ssh_payload(&json!("string"), ts(2026, 8, 16, 0, 0));
    assert!(!r.is_ok());
    let r = validate_custom_image_setup_ssh_payload(&json!([1, 2, 3]), ts(2026, 8, 16, 0, 0));
    assert!(!r.is_ok());
}

#[test]
fn r678_validate_missing_command() {
    let payload = json!({"type":"ssh"});
    let r = validate_custom_image_setup_ssh_payload(&payload, ts(2026, 8, 16, 0, 0));
    match r {
        EnvironmentCustomImageTerminalPayloadValidationResult::False { code, .. } =>
            assert_eq!(code, EnvironmentCustomImageTerminalPayloadValidationFailureCode::MissingCommand),
        _ => panic!(),
    }
}

#[test]
fn r678_validate_unsupported_command() {
    let payload = json!({"type":"ssh","command":"wget http://x"});
    let r = validate_custom_image_setup_ssh_payload(&payload, ts(2026, 8, 16, 0, 0));
    match r {
        EnvironmentCustomImageTerminalPayloadValidationResult::False { code, .. } =>
            assert_eq!(code, EnvironmentCustomImageTerminalPayloadValidationFailureCode::UnsupportedCommand),
        _ => panic!(),
    }
}

#[test]
fn r678_validate_invalid_expiry() {
    let payload = json!({"type":"ssh","command":"ssh u@h","expiresAt":"not-a-date"});
    let r = validate_custom_image_setup_ssh_payload(&payload, ts(2026, 8, 16, 0, 0));
    match r {
        EnvironmentCustomImageTerminalPayloadValidationResult::False { code, .. } =>
            assert_eq!(code, EnvironmentCustomImageTerminalPayloadValidationFailureCode::InvalidExpiry),
        _ => panic!(),
    }
}

#[test]
fn r678_validate_expired_payload() {
    let now = ts(2026, 8, 16, 12, 0);
    let payload = json!({"type":"ssh","command":"ssh u@h","expiresAt":"2026-08-16T11:00:00Z"});
    let r = validate_custom_image_setup_ssh_payload(&payload, now);
    match r {
        EnvironmentCustomImageTerminalPayloadValidationResult::False { status, code, .. } => {
            assert_eq!(status, 409);
            assert_eq!(code, EnvironmentCustomImageTerminalPayloadValidationFailureCode::ExpiredPayload);
        }
        _ => panic!(),
    }
}

#[test]
fn r678_validate_future_expiry_ok() {
    let now = ts(2026, 8, 16, 12, 0);
    let payload = json!({"type":"ssh","command":"ssh u@h","expiresAt":"2026-08-16T13:00:00Z"});
    let r = validate_custom_image_setup_ssh_payload(&payload, now);
    assert!(r.is_ok());
}

// =======================================================================
// Setup session utils
// =======================================================================

#[test]
fn r678_read_setup_session_company_id_present() {
    let metadata = json!({"setupRpcCompanyId":"  company-123  "}).as_object().cloned();
    let id = read_custom_image_setup_session_company_id(metadata.as_ref()).unwrap();
    assert_eq!(id, "company-123");
}

#[test]
fn r678_read_setup_session_company_id_instance_returns_none() {
    let metadata = json!({"setupRpcCompanyId":"instance"}).as_object().cloned();
    assert!(read_custom_image_setup_session_company_id(metadata.as_ref()).is_none());
}

#[test]
fn r678_read_setup_session_company_id_empty_returns_none() {
    let metadata = json!({"setupRpcCompanyId":""}).as_object().cloned();
    assert!(read_custom_image_setup_session_company_id(metadata.as_ref()).is_none());
    let metadata = json!({"setupRpcCompanyId":"   "}).as_object().cloned();
    assert!(read_custom_image_setup_session_company_id(metadata.as_ref()).is_none());
}

#[test]
fn r678_read_nullable_date_iso() {
    let v = json!("2026-08-16T00:00:00Z");
    let d = read_nullable_date(Some(&v)).unwrap();
    assert_eq!(d.to_rfc3339(), "2026-08-16T00:00:00+00:00");
}

#[test]
fn r678_read_nullable_date_invalid_returns_none() {
    assert!(read_nullable_date(Some(&json!("not-a-date"))).is_none());
    assert!(read_nullable_date(Some(&json!(""))).is_none());
    assert!(read_nullable_date(Some(&json!(null))).is_none());
    assert!(read_nullable_date(None).is_none());
    assert!(read_nullable_date(Some(&json!(123))).is_none());
}

#[test]
fn r678_read_future_date_past_returns_none() {
    let now = ts(2026, 8, 16, 12, 0);
    let v = json!("2026-08-16T11:00:00Z");
    assert!(read_future_date(Some(&v), now).is_none());
}

#[test]
fn r678_read_future_date_future_returns_date() {
    let now = ts(2026, 8, 16, 12, 0);
    let v = json!("2026-08-16T13:00:00Z");
    assert!(read_future_date(Some(&v), now).is_some());
}

#[test]
fn r678_require_future_ok() {
    let now = ts(2026, 8, 16, 12, 0);
    let v = json!("2026-08-16T13:00:00Z");
    assert!(require_future_custom_image_setup_expiry(Some(&v), now).is_ok());
}

#[test]
fn r678_require_future_err() {
    let now = ts(2026, 8, 16, 12, 0);
    let v = json!("2026-08-16T11:00:00Z");
    assert!(require_future_custom_image_setup_expiry(Some(&v), now).is_err());
}

// =======================================================================
// SessionStore
// =======================================================================

fn ssh() -> ParsedCustomImageSetupSshCommand {
    ParsedCustomImageSetupSshCommand { username: "root".into(), host: "example.com".into(), port: 22 }
}

fn create_input(now: DateTime<Utc>, setup_expiry: DateTime<Utc>) -> CreateTerminalSessionInput {
    CreateTerminalSessionInput {
        setup_session_id: "setup-1".into(),
        company_id: "co-1".into(),
        environment_id: "env-1".into(),
        provider: "docker".into(),
        ssh: ssh(),
        setup_expires_at: Some(Value::String(setup_expiry.to_rfc3339())),
        connection_expires_at: None,
        now: Some(now),
    }
}

#[test]
fn r678_store_create_and_get() {
    let store = EnvironmentCustomImageTerminalSessionStore::new();
    let now = ts(2026, 8, 16, 12, 0);
    let setup_exp = now + Duration::hours(1);
    let minted = store.create(create_input(now, setup_exp)).unwrap();
    assert_eq!(minted.session.id.len(), 36);
    assert!(!minted.token.is_empty());
    // get within window returns record
    let got = store.get(&minted.session.id, &minted.token, now + Duration::minutes(1)).unwrap();
    assert_eq!(got.setup_session_id, "setup-1");
}

#[test]
fn r678_store_get_wrong_token_returns_none() {
    let store = EnvironmentCustomImageTerminalSessionStore::new();
    let now = ts(2026, 8, 16, 12, 0);
    let setup_exp = now + Duration::hours(1);
    let minted = store.create(create_input(now, setup_exp)).unwrap();
    assert!(store.get(&minted.session.id, "wrong-token", now).is_none());
}

#[test]
fn r678_store_get_after_connect_expired() {
    let store = EnvironmentCustomImageTerminalSessionStore::new();
    let now = ts(2026, 8, 16, 12, 0);
    let setup_exp = now + Duration::hours(1);
    let minted = store.create(create_input(now, setup_exp)).unwrap();
    // connectExpiresAt is min(now+5min, setupExp). setup_exp is 1h, so connect is 5min.
    let later = now + Duration::minutes(10);
    assert!(store.get(&minted.session.id, &minted.token, later).is_none());
}

#[test]
fn r678_store_setup_expired_rejects_create() {
    let store = EnvironmentCustomImageTerminalSessionStore::new();
    let now = ts(2026, 8, 16, 12, 0);
    let past = now - Duration::hours(1);
    let r = store.create(create_input(now, past));
    assert!(r.is_err());
}

#[test]
fn r678_store_get_by_id_session_expired() {
    let store = EnvironmentCustomImageTerminalSessionStore::new();
    let now = ts(2026, 8, 16, 12, 0);
    let setup_exp = now + Duration::seconds(30);
    let minted = store.create(create_input(now, setup_exp)).unwrap();
    let later = now + Duration::minutes(2);
    assert!(store.get_by_id(&minted.session.id, later).is_none());
}

#[test]
fn r678_store_verify_or_pin_host_key() {
    let store = EnvironmentCustomImageTerminalSessionStore::new();
    let now = ts(2026, 8, 16, 12, 0);
    let setup_exp = now + Duration::hours(1);
    let minted = store.create(create_input(now, setup_exp)).unwrap();
    // First pin: stores and returns true.
    assert_eq!(store.verify_or_pin_host_key(&minted.session.id, "ssh-ed25519 AAAA", now), true);
    // Same key returns true
    assert_eq!(store.verify_or_pin_host_key(&minted.session.id, "ssh-ed25519 AAAA", now), true);
    // Different key returns false
    assert_eq!(store.verify_or_pin_host_key(&minted.session.id, "different", now), false);
}

#[test]
fn r678_store_delete_and_by_setup_id() {
    let store = EnvironmentCustomImageTerminalSessionStore::new();
    let now = ts(2026, 8, 16, 12, 0);
    let setup_exp = now + Duration::hours(1);
    let m1 = store.create(create_input(now, setup_exp)).unwrap();
    let mut input2 = create_input(now, setup_exp);
    input2.setup_session_id = "setup-2".into();
    let m2 = store.create(input2).unwrap();
    let removed = store.delete_by_setup_session_id("setup-1");
    assert_eq!(removed, 1);
    assert!(store.get_by_id(&m1.session.id, now).is_none());
    assert!(store.get_by_id(&m2.session.id, now).is_some());
}

#[test]
fn r678_store_constants() {
    assert_eq!(DEFAULT_TERMINAL_SESSION_TOKEN_TTL_MS, 300_000);
    assert_eq!(TERMINAL_SESSION_TOKEN_BYTES, 32);
}

// =======================================================================
// ConnectionRegistry
// =======================================================================

fn make_close(label: String, counter: std::sync::Arc<std::sync::Mutex<Vec<String>>>) -> EnvironmentCustomImageTerminalConnectionClose {
    Box::new(move |reason: String| {
        counter.lock().unwrap().push(format!("{label}:{reason}"));
    })
}

#[test]
fn r678_registry_add_close_returns_unregister() {
    let reg = Arc::new(EnvironmentCustomImageTerminalConnectionRegistry::new());
    let counter = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let unregister = reg.clone().add("setup-A", make_close("a".into(), counter.clone()));
    unregister();
    // After unregister, closeBySetupSessionId should do nothing.
    let n = reg.close_by_setup_session_id("setup-A", "shutdown");
    assert_eq!(n, 0);
    assert!(counter.lock().unwrap().is_empty());
}

#[test]
fn r678_registry_close_by_setup_session_id() {
    let reg = Arc::new(EnvironmentCustomImageTerminalConnectionRegistry::new());
    let counter = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let _u1 = reg.clone().add("setup-A", make_close("a1".into(), counter.clone()));
    let _u2 = reg.clone().add("setup-A", make_close("a2".into(), counter.clone()));
    let _u3 = reg.clone().add("setup-B", make_close("b1".into(), counter.clone()));
    let n = reg.close_by_setup_session_id("setup-A", "reason-A");
    assert_eq!(n, 2);
    let n = reg.close_by_setup_session_id("setup-B", "reason-B");
    assert_eq!(n, 1);
    assert_eq!(counter.lock().unwrap().len(), 3);
}

#[test]
fn r678_registry_close_all() {
    let reg = Arc::new(EnvironmentCustomImageTerminalConnectionRegistry::new());
    let counter = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let _u1 = reg.clone().add("setup-A", make_close("a1".into(), counter.clone()));
    let _u2 = reg.clone().add("setup-B", make_close("b1".into(), counter.clone()));
    let n = reg.close_all("bye");
    assert_eq!(n, 2);
    assert_eq!(counter.lock().unwrap().len(), 2);
}

#[test]
fn r678_registry_close_unknown_returns_zero() {
    let reg = EnvironmentCustomImageTerminalConnectionRegistry::new();
    assert_eq!(reg.close_by_setup_session_id("setup-X", "reason"), 0);
    assert_eq!(reg.close_all("reason"), 0);
}

#[test]
fn r678_registry_clear() {
    let reg = Arc::new(EnvironmentCustomImageTerminalConnectionRegistry::new());
    let counter = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let _u = reg.clone().add("setup-A", make_close("a".into(), counter));
    reg.clear();
    assert_eq!(reg.close_by_setup_session_id("setup-A", "x"), 0);
}
