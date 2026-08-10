#![forbid(unsafe_code)]
//! Board chat business service.
mod service;
pub use pc_repos::board_chat::{
    BoardMessageRow, BoardThreadRow, ChatMessageStatus, ChatRole, NewMessage, NewThread,
};
pub use service::{
    BoardChatError, BoardChatHook, BoardChatHookEvent, BoardChatService, NoopBoardChatHook,
    RecordingBoardChatHook,
};
