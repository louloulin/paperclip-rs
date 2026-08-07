//! M14 真实验证：pc-plugin-protocol JSON-RPC envelope + manifest validation。

use pc_plugin_protocol::envelope::{
    JsonRpcError, JsonRpcErrorResponse, JsonRpcRequest, JsonRpcSuccessResponse,
};
use pc_plugin_protocol::manifest::{
    PaperclipPluginManifestV1, PluginManifestAuthor, PluginManifestCapability,
    PluginManifestCapabilityKind, PluginManifestUiContribution,
};
use pc_plugin_protocol::methods;

#[test]
fn envelope_request_roundtrip_json() {
    let r = JsonRpcRequest::new("1", "tools.invoke", serde_json::json!({"name": "echo"}));
    let s = serde_json::to_string(&r).unwrap();
    assert!(s.contains("\"jsonrpc\":\"2.0\""));
    assert!(s.contains("\"method\":\"tools.invoke\""));
    let back: JsonRpcRequest = serde_json::from_str(&s).unwrap();
    assert_eq!(back.id, "1");
    assert_eq!(back.method, "tools.invoke");
}

#[test]
fn envelope_success_response_carries_result() {
    let s = JsonRpcSuccessResponse {
        jsonrpc: "2.0".into(),
        id: "42".into(),
        result: serde_json::json!({"ok": true}),
    };
    let j = serde_json::to_string(&s).unwrap();
    assert!(j.contains("\"result\""));
    let back: JsonRpcSuccessResponse = serde_json::from_str(&j).unwrap();
    assert_eq!(back.id, "42");
}

#[test]
fn envelope_error_response_carries_code_and_message() {
    let err = JsonRpcError::new(-32601, "method not found");
    let resp = err.into_response("xyz");
    let j = serde_json::to_string(&resp).unwrap();
    assert!(j.contains("-32601"));
    assert!(j.contains("method not found"));
}

#[test]
fn envelope_response_variants_separate_json() {
    let ok = JsonRpcSuccessResponse {
        jsonrpc: "2.0".into(),
        id: "1".into(),
        result: serde_json::json!(1),
    };
    let err = JsonRpcErrorResponse {
        jsonrpc: "2.0".into(),
        id: "2".into(),
        error: JsonRpcError::new(-1, "fail"),
    };
    let ok_json = serde_json::to_string(&ok).unwrap();
    let err_json = serde_json::to_string(&err).unwrap();
    assert!(ok_json.contains("\"result\""));
    assert!(err_json.contains("\"error\""));
}

#[test]
fn manifest_validates_required_fields() {
    let m = PaperclipPluginManifestV1 {
        id: "test.paperclip".into(),
        version: "0.1.0".into(),
        manifest_version: pc_plugin_protocol::manifest::PLUGIN_MANIFEST_VERSION.into(),
        label: "Test".into(),
        description: "A test plugin".into(),
        author: Some(PluginManifestAuthor {
            name: "Alice".into(),
            email: None,
            url: None,
        }),
        entry: "index.js".into(),
        capabilities: vec![PluginManifestCapability {
            kind: PluginManifestCapabilityKind::Tools,
            requires: vec![],
        }],
        config_schema: serde_json::json!({}),
        ui_contributions: vec![],
        metadata: serde_json::json!({}),
        local_folders: vec![],
    };
    assert!(m.validate().is_ok());
    assert!(m.has_capability(&PluginManifestCapabilityKind::Tools));
    assert!(!m.has_capability(&PluginManifestCapabilityKind::Ui));
}

#[test]
fn manifest_rejects_empty_id() {
    let mut m = PaperclipPluginManifestV1 {
        id: "".into(),
        version: "0.1.0".into(),
        manifest_version: pc_plugin_protocol::manifest::PLUGIN_MANIFEST_VERSION.into(),
        label: "Test".into(),
        description: "Test".into(),
        author: Some(PluginManifestAuthor {
            name: "Alice".into(),
            email: None,
            url: None,
        }),
        entry: "index.js".into(),
        capabilities: vec![PluginManifestCapability {
            kind: PluginManifestCapabilityKind::Tools,
            requires: vec![],
        }],
        config_schema: serde_json::json!({}),
        ui_contributions: vec![PluginManifestUiContribution {
            kind: "sidebar".into(),
            entry: "ui/index.html".into(),
            label: None,
            metadata: serde_json::json!({}),
        }],
        metadata: serde_json::json!({}),
        local_folders: vec![],
    };
    assert!(m.validate().is_err());
    m.id = "ok.id".into();
    assert!(m.validate().is_ok());
}

#[test]
fn manifest_serializes_roundtrip() {
    let m = PaperclipPluginManifestV1 {
        id: "ok.id".into(),
        version: "1.2.3".into(),
        manifest_version: pc_plugin_protocol::manifest::PLUGIN_MANIFEST_VERSION.into(),
        label: "Plugin".into(),
        description: "Desc".into(),
        author: Some(PluginManifestAuthor {
            name: "Bob".into(),
            email: Some("b@x".into()),
            url: None,
        }),
        entry: "index.js".into(),
        capabilities: vec![PluginManifestCapability {
            kind: PluginManifestCapabilityKind::Events,
            requires: vec!["topic:issue.created".into()],
        }],
        config_schema: serde_json::json!({}),
        ui_contributions: vec![],
        metadata: serde_json::json!({}),
        local_folders: vec![],
    };
    let j = serde_json::to_string(&m).unwrap();
    let back: PaperclipPluginManifestV1 = serde_json::from_str(&j).unwrap();
    assert_eq!(back.id, "ok.id");
    assert!(back.has_capability(&PluginManifestCapabilityKind::Events));
}

#[test]
fn methods_module_exposes_known_strings() {
    // Stable names — used by workers/hosts to dispatch.
    use pc_plugin_protocol::methods::host_to_worker;
    assert_eq!(host_to_worker::INITIALIZE, "initialize");
    assert_eq!(host_to_worker::HEALTH, "health");
    assert_eq!(host_to_worker::SHUTDOWN, "shutdown");
    assert_eq!(host_to_worker::RUN_JOB, "runJob");
}