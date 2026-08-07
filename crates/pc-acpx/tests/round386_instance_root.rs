//! R386 — Integration tests for `instance_root` (Node parity surface).
//!
//! Mirrors Node parity surface in `adapter-utils/src/server-utils.ts`:
//! - `DEFAULT_PAPERCLIP_INSTANCE_ID` (L106)
//! - `PATH_SEGMENT_RE` (L107)
//! - `expandHomePrefix` (L133-137)
//! - `resolvePaperclipInstanceRootForAdapter` (L139-149)
//!
//! The unit tests inside `instance_root::tests` already exercise every
//! branch in isolation; the integration tests below focus on the
//! cross-module behaviour:
//!
//! - env precedence and trim parity with Node
//! - the `default` helper (which reads `std::env`)
//! - error type stability across crate boundaries
//! - cross-check with `paths::resolve_paperclip_instance_root` to
//!   ensure both implementations agree on lexical semantics.

use pc_acpx::{
    default_resolve_paperclip_instance_root_for_adapter, is_valid_paperclip_instance_id,
    resolve_paperclip_instance_root_for_adapter, ResolvePaperclipInstanceRootError,
    ResolvePaperclipInstanceRootInput, DEFAULT_PAPERCLIP_HOME_SUFFIX,
    DEFAULT_PAPERCLIP_INSTANCE_ID, INSTANCES_DIR_NAME, PAPERCLIP_HOME_ENV,
    PAPERCLIP_INSTANCE_ID_ENV,
};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Default helpers
// ---------------------------------------------------------------------------

#[test]
fn default_resolve_helper_matches_default_const() {
    let resolved = default_resolve_paperclip_instance_root_for_adapter()
        .expect("default must always be valid");
    assert!(resolved.ends_with("/instances/default"));
    assert!(resolved.contains(DEFAULT_PAPERCLIP_HOME_SUFFIX));
}

#[test]
fn default_const_matches_node_literal() {
    assert_eq!(DEFAULT_PAPERCLIP_INSTANCE_ID, "default");
    assert_eq!(INSTANCES_DIR_NAME, "instances");
    assert_eq!(DEFAULT_PAPERCLIP_HOME_SUFFIX, ".paperclip");
    assert_eq!(PAPERCLIP_HOME_ENV, "PAPERCLIP_HOME");
    assert_eq!(PAPERCLIP_INSTANCE_ID_ENV, "PAPERCLIP_INSTANCE_ID");
}

// ---------------------------------------------------------------------------
// Node trim / null-guard parity
// ---------------------------------------------------------------------------

#[test]
fn empty_input_resolves_with_defaults_when_env_empty() {
    let input = ResolvePaperclipInstanceRootInput {
        home_dir: None,
        instance_id: None,
        env: Some(BTreeMap::new()),
    };
    let resolved = resolve_paperclip_instance_root_for_adapter(&input).expect("valid");
    assert!(resolved.ends_with("/instances/default"));
    assert!(resolved.contains(DEFAULT_PAPERCLIP_HOME_SUFFIX));
}

#[test]
fn whitespace_inputs_are_treated_as_absent() {
    let mut env = BTreeMap::new();
    env.insert(
        PAPERCLIP_HOME_ENV.to_string(),
        "/srv/paperclip-from-env".to_string(),
    );
    env.insert(
        PAPERCLIP_INSTANCE_ID_ENV.to_string(),
        "id-from-env".to_string(),
    );
    let input = ResolvePaperclipInstanceRootInput {
        home_dir: Some("   ".to_string()),
        instance_id: Some("\t\n".to_string()),
        env: Some(env),
    };
    let resolved = resolve_paperclip_instance_root_for_adapter(&input).expect("valid");
    assert_eq!(resolved, "/srv/paperclip-from-env/instances/id-from-env");
}

// ---------------------------------------------------------------------------
// Precedence
// ---------------------------------------------------------------------------

#[test]
fn home_dir_input_beats_env() {
    let mut env = BTreeMap::new();
    env.insert(PAPERCLIP_HOME_ENV.to_string(), "/srv/from-env".to_string());
    let input = ResolvePaperclipInstanceRootInput {
        home_dir: Some("/opt/from-input".to_string()),
        instance_id: Some("a".to_string()),
        env: Some(env),
    };
    let resolved = resolve_paperclip_instance_root_for_adapter(&input).expect("valid");
    assert_eq!(resolved, "/opt/from-input/instances/a");
}

#[test]
fn instance_id_input_beats_env() {
    let mut env = BTreeMap::new();
    env.insert(
        PAPERCLIP_INSTANCE_ID_ENV.to_string(),
        "id-from-env".to_string(),
    );
    let input = ResolvePaperclipInstanceRootInput {
        home_dir: Some("/opt/from-input".to_string()),
        instance_id: Some("id-from-input".to_string()),
        env: Some(env),
    };
    let resolved = resolve_paperclip_instance_root_for_adapter(&input).expect("valid");
    assert_eq!(resolved, "/opt/from-input/instances/id-from-input");
}

#[test]
fn env_falls_back_to_default_when_unset() {
    let input = ResolvePaperclipInstanceRootInput {
        home_dir: Some("/opt/h".to_string()),
        instance_id: None,
        env: Some(BTreeMap::new()),
    };
    let resolved = resolve_paperclip_instance_root_for_adapter(&input).expect("valid");
    assert_eq!(resolved, "/opt/h/instances/default");
}

// ---------------------------------------------------------------------------
// Validator
// ---------------------------------------------------------------------------

#[test]
fn validator_rejects_path_segments_and_punctuation() {
    assert!(!is_valid_paperclip_instance_id("../bad"));
    assert!(!is_valid_paperclip_instance_id("a/b"));
    assert!(!is_valid_paperclip_instance_id("a.b"));
    assert!(!is_valid_paperclip_instance_id(""));
    assert!(!is_valid_paperclip_instance_id("hello world"));
}

#[test]
fn validator_accepts_underscores_dashes_alphanumeric() {
    assert!(is_valid_paperclip_instance_id("default"));
    assert!(is_valid_paperclip_instance_id("prod-east-1"));
    assert!(is_valid_paperclip_instance_id("staging_2"));
    assert!(is_valid_paperclip_instance_id("ABC"));
    assert!(is_valid_paperclip_instance_id("a1-b2_c3"));
}

#[test]
fn invalid_instance_id_returns_typed_error() {
    let input = ResolvePaperclipInstanceRootInput {
        home_dir: Some("/opt/h".to_string()),
        instance_id: Some("with/slash".to_string()),
        env: Some(BTreeMap::new()),
    };
    let err =
        resolve_paperclip_instance_root_for_adapter(&input).expect_err("must reject with/slash");
    assert_eq!(
        err,
        ResolvePaperclipInstanceRootError::InvalidInstanceId("with/slash".to_string())
    );
    // Display impl must mirror the Node `throw new Error(...)` string.
    assert_eq!(
        err.to_string(),
        "Invalid PAPERCLIP_INSTANCE_ID 'with/slash'."
    );
}

#[test]
fn invalid_instance_id_via_env_returns_typed_error() {
    let mut env = BTreeMap::new();
    env.insert(
        PAPERCLIP_INSTANCE_ID_ENV.to_string(),
        "../escape".to_string(),
    );
    let input = ResolvePaperclipInstanceRootInput {
        home_dir: Some("/opt/h".to_string()),
        instance_id: None,
        env: Some(env),
    };
    let err = resolve_paperclip_instance_root_for_adapter(&input)
        .expect_err("env-supplied invalid id must be rejected");
    assert_eq!(
        err,
        ResolvePaperclipInstanceRootError::InvalidInstanceId("../escape".to_string())
    );
}

// ---------------------------------------------------------------------------
// Cross-check with paths::resolve_paperclip_instance_root
// ---------------------------------------------------------------------------

#[test]
fn instance_root_agrees_with_paths_resolver() {
    use pc_acpx::resolve_paperclip_instance_root as paths_resolve;

    let mut env = BTreeMap::new();
    env.insert(PAPERCLIP_HOME_ENV.to_string(), "/srv/paperclip".to_string());
    env.insert(PAPERCLIP_INSTANCE_ID_ENV.to_string(), "beta".to_string());

    let mut hash_env = HashMap::new();
    for (k, v) in &env {
        hash_env.insert(k.clone(), v.clone());
    }
    let paths_root = paths_resolve(Some("/srv/paperclip"), Some("beta"), &hash_env)
        .expect("paths resolver must accept valid ids");

    let input = ResolvePaperclipInstanceRootInput {
        home_dir: Some("/srv/paperclip".to_string()),
        instance_id: Some("beta".to_string()),
        env: Some(env),
    };
    let instance_root_value = resolve_paperclip_instance_root_for_adapter(&input).expect("valid");

    // Both helpers must agree on the lexical path. We compare the
    // canonical PathBuf forms to avoid trailing-separator noise.
    let paths_normalized = paths_root.components().collect::<PathBuf>();
    let instance_normalized: PathBuf = PathBuf::from(&instance_root_value).components().collect();
    assert_eq!(paths_normalized, instance_normalized);
    assert_eq!(
        paths_normalized,
        PathBuf::from("/srv/paperclip/instances/beta")
    );
}
