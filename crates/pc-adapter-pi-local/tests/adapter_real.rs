//! M13 真实验证：pi-local adapter descriptor。
use pc_adapter_api::{Adapter, AdapterDescriptor};
use pc_adapter_pi_local::{PiLocalAdapter, ADAPTER_TYPE};

#[test]
fn descriptor_uses_canonical_type() {
    let a = PiLocalAdapter::new();
    let d: AdapterDescriptor = a.descriptor();
    assert_eq!(d.adapter_type, ADAPTER_TYPE);
    assert_eq!(d.adapter_type, "pi_local");
}