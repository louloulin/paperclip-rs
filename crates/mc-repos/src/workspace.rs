//! WorkspaceRepo — 由 M1 sub-issue A 实现。
//!
//! 当前为占位 stub：`lib.rs` 已声明 `pub mod workspace;`，sub-issue A 应在本文件
//! 内追加 `pub struct WorkspaceRepo { ... }` + `WorkspaceRepo::new(db)` + 各 CRUD
//! 方法 + 测试，并在测试里覆盖 round-trip。
//!
//! 不要修改 `lib.rs` 内的 `pub mod` 顺序 —— 这是并发分支的合并约定。

use mc_db::Db;

#[allow(dead_code)]
pub struct WorkspaceRepo {
    db: Db,
}

impl WorkspaceRepo {
    #[allow(dead_code)]
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}