//! M13 真实验证：grok-local adapter descriptor。
use pc_adapter_api::{Adapter, AdapterDescriptor};
use pc_adapter_grok_local::{GrokLocalAdapter, ADAPTER_TYPE};

#[test]
fn descriptor_uses_canonical_type() {
    let a = GrokLocalAdapter::new();
    let d: AdapterDescriptor = a.descriptor();
    assert_eq!(d.adapter_type, ADAPTER_TYPE);
    assert_eq!(d.adapter_type, "grok_local");
}