use super::*;
use pc_config::PaperclipHomePaths;
use serde_json::json;
use std::fs;
use std::path::Path;
use std::sync::Arc;

const TEST_SECRET: &str = "0123456789abcdef0123456789abcdef";

#[test]
fn canonical_nested_fixture_matches_node() {
    let value = json!({
        "targetSnapshots": {
            "issue-2": { "version": 2, "status": "todo" },
            "issue-1": { "status": "in_progress", "version": 1 }
        },
        "options": [{ "effects": [], "label": "Approve", "id": "yes" }],
        "id": "decision-1"
    });

    assert_eq!(
        canonical::canonical(&value),
        r#"{"id":"decision-1","options":[{"effects":[],"id":"yes","label":"Approve"}],"targetSnapshots":{"issue-1":{"status":"in_progress","version":1},"issue-2":{"status":"todo","version":2}}}"#
    );
    assert_eq!(
        sign_decision_spec_with_secret(&value, TEST_SECRET).unwrap(),
        "decision-spec-v1.702d3e8d25b1d9dd48105bcc831f38fe7daad3088253ad0c5c92b8f575261b78"
    );
}

#[test]
fn canonical_number_fixture_matches_ecmascript() {
    let value = json!({
        "integerFloat": 1.0,
        "negativeZero": -0.0,
        "smallFixed": 0.000001,
        "smallExponent": 0.0000001,
        "largeFixed": 1.0e20,
        "largeExponent": 1.0e21
    });

    assert_eq!(
        canonical::canonical(&value),
        r#"{"integerFloat":1,"largeExponent":1e+21,"largeFixed":100000000000000000000,"negativeZero":0,"smallExponent":1e-7,"smallFixed":0.000001}"#
    );
    assert_eq!(
        sign_decision_spec_with_secret(&value, TEST_SECRET).unwrap(),
        "decision-spec-v1.aaa9b94a8ece2eaa6f3701522e12fa9a5ae91fbf7bd68dd48e0f0e3e0e9a3051"
    );
}

#[test]
fn canonical_string_fixture_matches_node() {
    let value = json!({
        "newline": "line1\nline2",
        "quote": "say \"yes\"",
        "unicode": "目录/技能",
        "slash": "</script>"
    });

    assert_eq!(
        sign_decision_spec_with_secret(&value, TEST_SECRET).unwrap(),
        "decision-spec-v1.0cd715f508ca693d4966378cfc8edebf95ff62457d99d909b5cf2fa056413b98"
    );
}

#[test]
fn object_insertion_order_does_not_change_signature() {
    let left = json!({ "decisionId": "d1", "options": [], "targetSnapshots": {} });
    let right: serde_json::Value =
        serde_json::from_str(r#"{"targetSnapshots":{},"options":[],"decisionId":"d1"}"#).unwrap();
    assert_eq!(
        sign_decision_spec_with_secret(&left, TEST_SECRET).unwrap(),
        sign_decision_spec_with_secret(&right, TEST_SECRET).unwrap()
    );
}

#[test]
fn valid_signature_verifies() {
    let value = json!({ "decisionId": "d1", "options": [] });
    let signature = sign_decision_spec_with_secret(&value, TEST_SECRET).unwrap();
    assert!(verify_decision_spec_with_secret(&value, &signature, TEST_SECRET).unwrap());
}

#[test]
fn tampered_value_and_wrong_secret_fail_closed() {
    let value = json!({ "decisionId": "d1", "options": [] });
    let signature = sign_decision_spec_with_secret(&value, TEST_SECRET).unwrap();
    assert!(!verify_decision_spec_with_secret(
        &json!({ "decisionId": "d1", "options": [{ "id": "tampered" }] }),
        &signature,
        TEST_SECRET
    )
    .unwrap());
    assert!(!verify_decision_spec_with_secret(
        &value,
        &signature,
        "fedcba9876543210fedcba9876543210"
    )
    .unwrap());
}

#[test]
fn malformed_signatures_are_rejected() {
    let value = json!({ "decisionId": "d1" });
    let signature = sign_decision_spec_with_secret(&value, TEST_SECRET).unwrap();
    assert!(!verify_decision_spec_with_secret(&value, "", TEST_SECRET).unwrap());
    assert!(!verify_decision_spec_with_secret(
        &value,
        "decision-spec-v2.0000000000000000000000000000000000000000000000000000000000000000",
        TEST_SECRET
    )
    .unwrap());
    assert!(
        !verify_decision_spec_with_secret(&value, &signature.to_uppercase(), TEST_SECRET).unwrap()
    );
    assert!(
        !verify_decision_spec_with_secret(&value, "decision-spec-v1.deadbeef", TEST_SECRET)
            .unwrap()
    );
}

#[test]
fn short_explicit_secret_is_rejected() {
    assert!(matches!(
        sign_decision_spec_with_secret(&json!({}), "too-short"),
        Err(DecisionSigningError::ExplicitSecretTooShort)
    ));
}

#[test]
fn secret_length_uses_javascript_utf16_units() {
    let sixteen_astral_symbols = "😀".repeat(16);
    assert_eq!(javascript_string_length(&sixteen_astral_symbols), 32);
    assert!(sign_decision_spec_with_secret(&json!({}), &sixteen_astral_symbols).is_ok());
}

#[test]
fn explicit_secret_is_trimmed_without_creating_a_file() {
    let temp = tempfile::tempdir().unwrap();
    let key_path = temp.path().join("secrets/decision-signing.key");
    let store = DecisionSigningKeyStore::new(&key_path);

    let secret = store
        .resolve_secret(Some("  0123456789abcdef0123456789abcdef  "))
        .unwrap();

    assert_eq!(secret, TEST_SECRET);
    assert!(!key_path.exists());
}

#[test]
fn generated_secret_is_persisted_and_reused() {
    let temp = tempfile::tempdir().unwrap();
    let key_path = temp.path().join("secrets/decision-signing.key");
    let store = DecisionSigningKeyStore::new(&key_path);

    let first = store.resolve_secret(None).unwrap();
    let second = store.resolve_secret(None).unwrap();

    assert!(javascript_string_length(&first) >= 32);
    assert_eq!(first, second);
    assert_eq!(fs::read_to_string(key_path).unwrap(), first);
}

#[test]
fn concurrent_generation_publishes_one_complete_key() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(DecisionSigningKeyStore::new(
        temp.path().join("secrets/decision-signing.key"),
    ));
    let threads = (0..8)
        .map(|_| {
            let store = Arc::clone(&store);
            std::thread::spawn(move || store.resolve_secret(None).unwrap())
        })
        .collect::<Vec<_>>();
    let secrets = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect::<Vec<_>>();

    assert!(secrets.iter().all(|secret| secret == &secrets[0]));
    assert_eq!(fs::read_to_string(store.key_path()).unwrap(), secrets[0]);
}

#[test]
fn invalid_existing_key_is_not_silently_regenerated() {
    let temp = tempfile::tempdir().unwrap();
    let key_path = temp.path().join("secrets/decision-signing.key");
    fs::create_dir_all(key_path.parent().unwrap()).unwrap();
    fs::write(&key_path, "too-short").unwrap();
    let store = DecisionSigningKeyStore::new(&key_path);

    assert!(matches!(
        store.resolve_secret(None),
        Err(DecisionSigningError::GeneratedSecretTooShort { .. })
    ));
    assert_eq!(fs::read_to_string(key_path).unwrap(), "too-short");
}

#[cfg(unix)]
#[test]
fn permissive_permissions_are_repaired() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("secrets");
    let key_path = directory.join("decision-signing.key");
    fs::create_dir_all(&directory).unwrap();
    fs::write(&key_path, TEST_SECRET).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o777)).unwrap();
    fs::set_permissions(&key_path, fs::Permissions::from_mode(0o644)).unwrap();

    let store = DecisionSigningKeyStore::new(&key_path);
    assert_eq!(store.resolve_secret(None).unwrap(), TEST_SECRET);
    assert_eq!(
        fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&key_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[cfg(unix)]
#[test]
fn symlink_key_is_rejected() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("secrets");
    let key_path = directory.join("decision-signing.key");
    let target = temp.path().join("planted.key");
    fs::create_dir_all(&directory).unwrap();
    fs::write(&target, TEST_SECRET).unwrap();
    symlink(&target, &key_path).unwrap();

    let store = DecisionSigningKeyStore::new(&key_path);
    assert!(matches!(
        store.resolve_secret(None),
        Err(DecisionSigningError::KeyNotRegularFile { .. })
    ));
    assert_eq!(fs::read_to_string(target).unwrap(), TEST_SECRET);
}

#[test]
fn non_directory_secrets_path_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("secrets");
    fs::write(&directory, "not-a-directory").unwrap();
    let store = DecisionSigningKeyStore::new(directory.join("decision-signing.key"));
    assert!(matches!(
        store.resolve_secret(None),
        Err(DecisionSigningError::SecretsPathNotDirectory { .. })
    ));
}

#[test]
fn home_paths_place_key_beside_master_key() {
    let paths = PaperclipHomePaths::build_with(
        Some("/paperclip"),
        Some("default"),
        None,
        None,
        Path::new("/home/test"),
        Path::new("/workspace"),
    )
    .unwrap();
    let store = DecisionSigningKeyStore::from_home_paths(&paths);
    assert_eq!(
        store.key_path(),
        Path::new("/paperclip/instances/default/secrets/decision-signing.key")
    );
}

#[test]
fn constants_match_node() {
    assert_eq!(DECISION_SIGNING_VERSION, "decision-spec-v1");
    assert_eq!(MIN_DECISION_SIGNING_SECRET_LENGTH, 32);
}
