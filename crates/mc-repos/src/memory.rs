//! 内存版 Repo 实现，仅供单元测试使用。
//!
//! 设计：每个 Repo 暴露一个 `Memory*Repo`，使用 `tokio::sync::RwLock<HashMap>` 串行化访问。
//! 真实生产路径通过 `*_pg.rs`（同目录下）连接 PostgreSQL。

use std::sync::Arc;

use tokio::sync::RwLock;

use mc_core::Id;

/// 共享的内存后端 —— 给一个进程里的多个 Repo 注入同一份存储，便于构建测试 fixture。
#[derive(Default, Clone)]
pub struct MemoryStore {
    pub users: Arc<RwLock<std::collections::HashMap<Id, mc_core::user::User>>>,
    pub workspaces:
        Arc<RwLock<std::collections::HashMap<Id, super::workspace::WorkspaceRow>>>,
    pub members: Arc<RwLock<std::collections::HashMap<Id, super::member::MemberRow>>>,
    pub invitations: Arc<
        RwLock<std::collections::HashMap<Id, super::invitation::InvitationRow>>,
    >,
    pub share_links:
        Arc<RwLock<std::collections::HashMap<Id, super::share_link::ShareLinkRow>>>,
    pub verification_codes: Arc<
        RwLock<std::collections::HashMap<Id, super::verification::VerificationRow>>,
    >,
    pub pats: Arc<RwLock<std::collections::HashMap<Id, super::pat::PatRow>>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}
