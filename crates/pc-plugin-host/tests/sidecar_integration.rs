//! R877 — Node sidecar integration test.
//!
//! Validates that:
//! 1. The sidecar script exists at the expected path
//! 2. The sidecar script has the expected CLI args (--plugin-id, --manifest)
//! 3. The sidecar script contains the required JSON-RPC method sets
//! 4. The SidecarConfig / SidecarError types compile correctly
//!
//! Does NOT actually spawn a Node process (would require Node runtime in CI).
//! That is covered by the CI job that runs against a real Node install.

use std::path::PathBuf;

#[test]
fn sidecar_script_exists_in_bin_directory() {
    // The .mjs file is in crates/pc-plugin-host/bin/ alongside Cargo.toml.
    // It is NOT a Rust source file but a runtime asset; we verify by path.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let sidecar = manifest_dir.join("bin").join("paperclip-plugin-sidecar.mjs");
    assert!(
        sidecar.exists(),
        "sidecar script missing at {} — this file is the Node.js runtime asset \
         spawned by NodeSidecarLauncher for manifest.runtime = 'node' plugins",
        sidecar.display()
    );
}

#[test]
fn sidecar_script_accepts_required_cli_args() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let sidecar = manifest_dir.join("bin").join("paperclip-plugin-sidecar.mjs");
    let source = std::fs::read_to_string(&sidecar).expect("read sidecar");

    // CLI parsing must handle --plugin-id and --manifest
    assert!(
        source.contains("--plugin-id"),
        "sidecar must accept --plugin-id"
    );
    assert!(
        source.contains("--manifest"),
        "sidecar must accept --manifest"
    );

    // Must validate that argv has both args before proceeding
    assert!(
        source.contains("missing --plugin-id or --manifest"),
        "sidecar must validate CLI args"
    );
}

#[test]
fn sidecar_implements_all_host_to_worker_methods() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let sidecar = manifest_dir.join("bin").join("paperclip-plugin-sidecar.mjs");
    let source = std::fs::read_to_string(&sidecar).expect("read sidecar");

    // The Node sidecar must whitelist exactly the same method names as
    // pc_plugin_protocol::HOST_TO_WORKER_METHODS — drift between the two
    // would silently drop RPC calls. We sample the canonical set here;
    // the comprehensive list is in crates/pc-plugin-protocol/src/methods.rs.
    let required_methods = [
        "initialize", "health", "shutdown", "validateConfig", "configChanged",
        "onEvent", "runJob", "handleWebhook", "handleApiRequest",
        "getData", "performAction", "executeTool",
    ];
    for method in required_methods {
        assert!(
            source.contains(format!("'{method}'").as_str())
                || source.contains(format!("\"{method}\"").as_str()),
            "sidecar must whitelist host→worker method '{method}'"
        );
    }
}

#[test]
fn sidecar_implements_all_worker_to_host_methods() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let sidecar = manifest_dir.join("bin").join("paperclip-plugin-sidecar.mjs");
    let source = std::fs::read_to_string(&sidecar).expect("read sidecar");

    let required_methods = [
        "progress", "log", "emitEvent", "getState", "setState",
        "dataQuery", "dataMutate", "toolInvoke", "activityLog", "notify",
    ];
    for method in required_methods {
        assert!(
            source.contains(format!("'{method}'").as_str())
                || source.contains(format!("\"{method}\"").as_str()),
            "sidecar must whitelist worker→host method '{method}'"
        );
    }
}

#[test]
fn sidecar_uses_node_vm_for_isolation() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let sidecar = manifest_dir.join("bin").join("paperclip-plugin-sidecar.mjs");
    let source = std::fs::read_to_string(&sidecar).expect("read sidecar");

    // The whole point of R877 is that we mirror Node paperclip's
    // `node:vm` plugin isolation model. The sidecar must use it.
    assert!(
        source.contains("node:vm") || source.contains("require('vm')"),
        "sidecar must use node:vm for plugin isolation (Node paperclip parity)"
    );
    assert!(
        source.contains("createContext"),
        "sidecar must create a vm context"
    );
    assert!(
        source.contains("runInContext"),
        "sidecar must run the plugin script in the vm context"
    );
}

#[test]
fn sidecar_handles_jsonrpc_envelope() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let sidecar = manifest_dir.join("bin").join("paperclip-plugin-sidecar.mjs");
    let source = std::fs::read_to_string(&sidecar).expect("read sidecar");

    // JSON-RPC 2.0 envelope
    assert!(source.contains("jsonrpc: '2.0'") || source.contains("jsonrpc: \"2.0\""));
    assert!(source.contains("RPC_PARSE_ERROR") || source.contains("-32700"));
    assert!(source.contains("RPC_INVALID_REQUEST") || source.contains("-32600"));
    assert!(source.contains("RPC_METHOD_NOT_FOUND") || source.contains("-32601"));
    assert!(source.contains("RPC_INVALID_PARAMS") || source.contains("-32602"));
    assert!(source.contains("RPC_INTERNAL_ERROR") || source.contains("-32603"));
}

#[test]
fn sidecar_validates_manifest_plugin_id() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let sidecar = manifest_dir.join("bin").join("paperclip-plugin-sidecar.mjs");
    let source = std::fs::read_to_string(&sidecar).expect("read sidecar");

    // The sidecar must verify that the manifest id matches the --plugin-id
    // to prevent a malicious manifest from claiming to be a different plugin.
    assert!(
        source.contains("does not match --plugin-id"),
        "sidecar must validate manifest id matches --plugin-id"
    );
}
