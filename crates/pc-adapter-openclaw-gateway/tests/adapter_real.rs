//! M13 真实验证：openclaw-gateway adapter descriptor。
use pc_adapter_api::{Adapter, AdapterDescriptor};
use pc_adapter_openclaw_gateway::{OpenclawGatewayAdapter, ADAPTER_TYPE};

#[test]
fn descriptor_uses_canonical_type() {
    let a = OpenclawGatewayAdapter::new();
    let d: AdapterDescriptor = a.descriptor();
    assert_eq!(d.adapter_type, ADAPTER_TYPE);
    assert_eq!(d.adapter_type, "openclaw_gateway");
}
