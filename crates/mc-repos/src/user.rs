//! UserRepo — 由 M1 sub-issue A 实现。
//!
//! 当前为占位 stub：`lib.rs` 已声明 `pub mod user;`。

use mc_db::Db;

#[allow(dead_code)]
pub struct UserRepo {
    db: Db,
}

impl UserRepo {
    #[allow(dead_code)]
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}