//! M13 真实验证：cursor-cloud adapter descriptor。
use pc_adapter_api::{Adapter, AdapterDescriptor};
use pc_adapter_cursor_cloud::cloud_client::FakeCursorCloudClient;
use pc_adapter_cursor_cloud::execute::CursorCloudAdapter;
use pc_adapter_cursor_cloud::ADAPTER_TYPE;
use std::sync::Arc;

#[test]
fn descriptor_uses_canonical_type() {
    let client = Arc::new(FakeCursorCloudClient::new());
    let a = CursorCloudAdapter::new(client);
    let d: AdapterDescriptor = a.descriptor();
    assert_eq!(d.adapter_type, ADAPTER_TYPE);
    assert_eq!(d.adapter_type, "cursor_cloud");
}
