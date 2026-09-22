//! VerificationCodeRepo — 由 M1 sub-issue B 实现。
//!
//! 当前为占位 stub：`lib.rs` 已声明 `pub mod verification_code;`。

use mc_db::Db;

#[allow(dead_code)]
pub struct VerificationCodeRepo {
    db: Db,
}

impl VerificationCodeRepo {
    #[allow(dead_code)]
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}