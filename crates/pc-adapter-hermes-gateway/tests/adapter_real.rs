//! M13 真实验证：hermes-gateway adapter descriptor。
use pc_adapter_api::{Adapter, AdapterDescriptor};
use pc_adapter_hermes_gateway::{HermesGatewayAdapter, ADAPTER_TYPE};

#[test]
fn descriptor_uses_canonical_type() {
    let a = HermesGatewayAdapter::new();
    let d: AdapterDescriptor = a.descriptor();
    assert_eq!(d.adapter_type, ADAPTER_TYPE);
    assert_eq!(d.adapter_type, "hermes_gateway");
}
