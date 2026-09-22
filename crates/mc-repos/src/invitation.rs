//! InvitationRepo — 由 M1 sub-issue C 实现。
//!
//! 当前为占位 stub：`lib.rs` 已声明 `pub mod invitation;`。

use mc_db::Db;

#[allow(dead_code)]
pub struct InvitationRepo {
    db: Db,
}

impl InvitationRepo {
    #[allow(dead_code)]
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}