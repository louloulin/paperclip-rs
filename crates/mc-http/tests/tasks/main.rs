//! M3-6（LUM-1429）W3b 切片：任务/生命周期/用量/重试面 + agent-builder 的端到端测试。
//!
//! 与 `tests/agents/` 同构：缺 `MULTICA_TEST_DATABASE_URL` 时整体跳过（`setup()`
//! 返回 `None`），环境变量存在但连不上则直接 panic（不静默「绿」）。
#![cfg(feature = "test-util")]

mod builder;
mod cancel;
mod issue_runs;
mod rerun;
mod support;
mod usage;
mod working;
