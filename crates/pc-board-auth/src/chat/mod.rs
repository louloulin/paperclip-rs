//! Board chat business service（原 `pc-board-chat` 已下沉）。
mod service;
pub use pc_repos::board_chat::{
    BoardMessageRow, BoardThreadRow, ChatMessageStatus, ChatRole, NewMessage, NewThread,
};
pub use service::{
    BoardChatError, BoardChatHook, BoardChatHookEvent, BoardChatService, NoopBoardChatHook,
    RecordingBoardChatHook,
};
