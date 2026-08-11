//! Company-scoped label CRUD service（原 `pc-label` 已下沉）。
mod service;
pub use pc_repos::label::{LabelPatch, LabelRow, NewLabel};
pub use service::{
    LabelError, LabelHook, LabelHookEvent, LabelService, NoopLabelHook, RecordingLabelHook,
};
