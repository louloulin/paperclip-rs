//! `/api/skills*` 端到端测试（M6-2 / LUM-1667）：12 条路由 + 5 个尾斜杠别名键。
//!
//! 需要真实 PG：`skill` / `skill_file` / `skill_to_label` / `issue_label` /
//! `member` / `workspace` / `"user"` 必须已迁移（`mc-migrate run --dir migrations`，
//! 即 `migrations/upstream/` 那一套 562 个文件）。
//!
//! 运行示例：
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://mc:mc@127.0.0.1:5432/multica_test \
//!   cargo test -p mc-http --test skills --features test-util -- --ignored
//! ```
//!
//! `MULTICA_TEST_DATABASE_URL` 未设置 → 每条用例打印跳过并 return（普通 `cargo test`
//! 不会红）；**设置了却连不上 / 没建表 → panic**（不许静默假装绿）。
//!
//! 文件布局（R7：单文件 800 行硬上限，门 ⑩ `scripts/file_size_check.py`）：
//! - `skills/support.rs`：连接 / `AppState` / 种子 / 请求小工具
//! - `skills/crud.rs`：列表 / 搜索 / 详情 / 创建 / 更新 / 删除 + 双形态 + 鉴权
//! - `skills/files.rs`：支持文件的读 / 单文件 upsert / 删
//! - `skills/labels.rs`：skill↔label 三条
//!
//! ⚠️ `GET /api/skills/search` 的**成功**路径要出站打 `clawhub.ai`（上游就是把
//! 第三方搜索当数据源）。用例对它是「200 或 502」双断言，见 `crud.rs`。
#![cfg(feature = "test-util")]

mod crud;
mod files;
mod labels;
mod support;
