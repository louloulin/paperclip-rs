//! M13 真实验证：cursor-cloud adapter descriptor。
use pc_adapter_api::{Adapter, AdapterDescriptor};
use pc_adapter_cursor_cloud::{CursorCloudAdapter, ADAPTER_TYPE};

#[test]
fn descriptor_uses_canonical_type() {
    let a = CursorCloudAdapter::new();
    let d: AdapterDescriptor = a.descriptor();
    assert_eq!(d.adapter_type, ADAPTER_TYPE);
    assert_eq!(d.adapter_type, "cursor_cloud");
}
