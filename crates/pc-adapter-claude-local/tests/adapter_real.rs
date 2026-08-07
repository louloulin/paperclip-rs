//! M13 真实验证：claude-local adapter descriptor + 基础属性。
use pc_adapter_api::{Adapter, AdapterDescriptor};
use pc_adapter_claude_local::{ClaudeLocalAdapter, ADAPTER_TYPE};

#[test]
fn descriptor_uses_canonical_type() {
    let a = ClaudeLocalAdapter::new();
    let d: AdapterDescriptor = a.descriptor();
    assert_eq!(d.adapter_type, ADAPTER_TYPE);
    assert_eq!(d.adapter_type, "claude_local");
    assert!(d.supports_local_agent_jwt);
    assert!(d.supports_instructions_bundle);
}

#[test]
fn adapter_serializes_to_json() {
    let a = ClaudeLocalAdapter::new();
    let j = serde_json::to_string(&a.descriptor()).unwrap();
    assert!(j.contains("claude_local"));
}
