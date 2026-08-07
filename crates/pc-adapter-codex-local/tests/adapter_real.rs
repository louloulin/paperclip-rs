//! M13 真实验证：codex-local adapter descriptor。
use pc_adapter_api::{Adapter, AdapterDescriptor};
use pc_adapter_codex_local::{CodexLocalAdapter, ADAPTER_TYPE};

#[test]
fn descriptor_uses_canonical_type() {
    let a = CodexLocalAdapter::new();
    let d: AdapterDescriptor = a.descriptor();
    assert_eq!(d.adapter_type, ADAPTER_TYPE);
    assert_eq!(d.adapter_type, "codex_local");
}
