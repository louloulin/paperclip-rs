//! R874 — Node paperclip OPERATION_CAPABILITIES parity test.
//!
//! Loads the frozen JSON fixture (node_parity_fixture.json) and asserts that
//! `required_capabilities()` returns the same set for every operation listed
//! in the fixture. If a Node-side operation is added, removed, or its
//! capability mapping changes, this test fails — forcing a deliberate Rust
//! update to match.
//!
//! To refresh the fixture after Node upstream changes:
//! 1. Read paperclip/server/src/services/plugin-capability-validator.ts
//! 2. Extract the OPERATION_CAPABILITIES map
//! 3. Update node_parity_fixture.json with the new values
//! 4. Update operations.rs accordingly
//! 5. Re-run tests

use std::collections::BTreeMap;

use serde_json::Value;

use super::operations::{required_capabilities, ops};

const FIXTURE: &str = include_str!("node_parity_fixture.json");

fn load_fixture() -> BTreeMap<String, Vec<String>> {
    let v: Value = serde_json::from_str(FIXTURE)
        .expect("node_parity_fixture.json must be valid JSON");
    let ops = v.get("operations")
        .and_then(|x| x.as_object())
        .expect("fixture must have 'operations' object");
    let mut out = BTreeMap::new();
    for (k, val) in ops {
        let arr = val.as_array()
            .unwrap_or_else(|| panic!("fixture entry for {k} must be an array"));
        let caps: Vec<String> = arr.iter()
            .map(|c| c.as_str().unwrap_or_else(|| panic!("cap for {k} must be string")).to_string())
            .collect();
        out.insert(k.clone(), caps);
    }
    out
}

#[test]
fn fixture_json_is_well_formed() {
    let map = load_fixture();
    // Sanity: known operations from Node should be present
    assert!(map.contains_key(ops::COMPANIES_LIST));
    assert!(map.contains_key(ops::ISSUES_CREATE));
    assert!(map.contains_key(ops::TOOLS_INVOKE));
    assert!(map.contains_key(ops::ENVIRONMENTS_ACQUIRE_LEASE));
}

#[test]
fn rust_capabilities_match_node_fixture_for_all_operations() {
    let fixture = load_fixture();

    for (op_name, expected_caps) in &fixture {
        let rust_caps = required_capabilities(op_name);
        // Convert &[&str] to Vec<String> for comparison
        let rust_vec: Vec<String> = rust_caps.iter().map(|s| s.to_string()).collect();

        if rust_vec.is_empty() {
            panic!(
                "R874 parity violation: operation '{op_name}' is in Node fixture with caps {expected_caps:?} \
                 but Rust required_capabilities() returns empty. Add a mapping in operations.rs."
            );
        }

        if &rust_vec != expected_caps {
            panic!(
                "R874 parity violation: operation '{op_name}'\n  Node: {expected_caps:?}\n  Rust: {rust_vec:?}\n\
                 Update operations.rs to match Node fixture."
            );
        }
    }
}

#[test]
fn no_extra_operations_in_rust_not_in_node_fixture() {
    // This is the reverse check: every operation constant in Rust should have
    // an entry in the fixture. If you add a new operation constant, add it
    // to the fixture too (and to operations.rs).
    let fixture = load_fixture();

    let ops_consts = [
        ops::COMPANIES_LIST, ops::COMPANIES_GET,
        ops::PROJECTS_LIST, ops::PROJECTS_GET,
        ops::ISSUES_LIST, ops::ISSUES_GET,
        ops::APPROVALS_LIST, ops::APPROVALS_GET,
        ops::AGENTS_LIST, ops::AGENTS_GET,
        ops::ISSUES_CREATE, ops::ISSUES_UPDATE,
        ops::ISSUE_COMMENTS_CREATE, ops::APPROVALS_RESPOND,
        ops::PLUGIN_STATE_GET, ops::PLUGIN_STATE_LIST, ops::PLUGIN_STATE_SET,
        ops::LOCAL_FOLDERS_READ, ops::LOCAL_FOLDERS_WRITE,
        ops::DB_QUERY, ops::DB_MIGRATE,
        ops::EXTERNAL_OBJECTS_READ, ops::EXTERNAL_OBJECTS_WRITE,
        ops::ACTIVITY_LOG,
        ops::TOOLS_LIST, ops::TOOLS_INVOKE,
        ops::WEBHOOKS_SEND, ops::WEBHOOKS_RECEIVE,
        ops::EVENTS_PUBLISH, ops::EVENTS_SUBSCRIBE,
        ops::UI_RENDER, ops::UI_CONTRIBUTE,
        ops::ENVIRONMENTS_PROBE,
        ops::ENVIRONMENTS_ACQUIRE_LEASE, ops::ENVIRONMENTS_RESUME_LEASE,
        ops::ENVIRONMENTS_RELEASE_LEASE,
        ops::ENVIRONMENTS_REALIZE_WORKSPACE, ops::ENVIRONMENTS_DISPOSE_WORKSPACE,
        ops::ENVIRONMENTS_TICK,
        ops::JOBS_DISPATCH, ops::JOBS_CANCEL, ops::JOBS_LIST,
        ops::DECISIONS_CREATE, ops::DECISIONS_RESPOND,
        ops::SKILLS_UPLOAD, ops::SKILLS_PUBLISH,
        ops::CASES_CREATE, ops::CASES_UPDATE,
        ops::DOCUMENTS_UPLOAD, ops::DOCUMENTS_READ,
        ops::WORKFLOWS_TRIGGER,
        ops::AGENTS_INVOKE, ops::AGENTS_CREATE,
    ];

    let mut missing: Vec<&str> = Vec::new();
    for op in ops_consts {
        if !fixture.contains_key(op) {
            missing.push(op);
        }
    }

    assert!(
        missing.is_empty(),
        "R874: Rust has operation constants not in Node fixture. Add them to node_parity_fixture.json: {missing:?}"
    );
}

#[test]
fn operation_count_at_least_50() {
    // Sanity: as of R874, we expect at least 50 operations.
    // If this fails after a Node upstream drop, investigate the diff.
    let fixture = load_fixture();
    assert!(
        fixture.len() >= 50,
        "expected >= 50 operations in parity fixture, got {}. Check Node upstream.",
        fixture.len()
    );
}
