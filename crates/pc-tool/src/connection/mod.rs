//! Tool connection business service（原 `pc-tool-connection` 已下沉）。
mod service;
pub use pc_repos::tool_connection::ToolConnectionRow;
pub use service::{
    NoopToolConnectionHook, RecordingToolConnectionHook, ToolConnectionError, ToolConnectionHook,
    ToolConnectionHookEvent, ToolConnectionService,
};
