//! M13 真实验证：gemini-local adapter descriptor。
use pc_adapter_api::{Adapter, AdapterDescriptor};
use pc_adapter_gemini_local::{GeminiLocalAdapter, ADAPTER_TYPE};

#[test]
fn descriptor_uses_canonical_type() {
    let a = GeminiLocalAdapter::new();
    let d: AdapterDescriptor = a.descriptor();
    assert_eq!(d.adapter_type, ADAPTER_TYPE);
    assert_eq!(d.adapter_type, "gemini_local");
}