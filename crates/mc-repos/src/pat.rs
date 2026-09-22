//! PatRepo — 由 M1 sub-issue C 实现。
//!
//! 当前为占位 stub：`lib.rs` 已声明 `pub mod pat;`。

use mc_db::Db;

#[allow(dead_code)]
pub struct PatRepo {
    db: Db,
}

impl PatRepo {
    #[allow(dead_code)]
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}