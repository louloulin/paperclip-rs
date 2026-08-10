#![forbid(unsafe_code)]
//! Company-scoped label CRUD service.
mod service;
pub use pc_repos::label::{LabelPatch, LabelRow, NewLabel};
pub use service::{
    LabelError, LabelHook, LabelHookEvent, LabelService, NoopLabelHook, RecordingLabelHook,
};
