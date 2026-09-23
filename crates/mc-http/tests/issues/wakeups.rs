//! `/api/issues/:id/wakeups*` + workspace 级 issue-wakeup 面的端到端测试（M5-6 / LUM-1565）。
//!
//! `DoD` 的 7 个场景逐条对应本模块的 7 个 `#[tokio::test]`：
//!
//! | # | 场景 | 测试 |
//! | ---: | --- | --- |
//! | 1 | 四种 kind 建 + 列表回读 | [`crud::wakeup_kinds_create_and_readback`] |
//! | 2 | `mode` 与 `kind` 正交（once / continuous） | [`crud::wakeup_mode_is_orthogonal_to_kind`] |
//! | 3 | PUT upsert 复用 POST + revision 递增作废旧收据 | [`crud::wakeup_upsert_bumps_revision_and_drops_receipts`] |
//! | 4 | disable → enable | [`crud::wakeup_disable_then_enable`] |
//! | 5 | instruction 编辑（含 409 冲突 / 400 体上限） | [`crud::wakeup_instruction_edit`] |
//! | 6 | workspace 级列表 + summaries | [`listing::workspace_list_and_summaries`] |
//! | 7 | 非成员 403 / 跨 workspace 404 | [`isolation::wakeup_membership_and_workspace_isolation`] |
//!
//! 都需要真实 PG（`MULTICA_TEST_DATABASE_URL` + `--ignored`，与 `tests/issues` 其余分片同口径）。
//! 门禁跑法见 `scripts/gates.sh` 的 ⑧（`--ignored` 全量 e2e）。
//!
//! 单文件 800 行硬上限（`scripts/file_size_check.py` + 门 ⑩）⇒ 按场景拆到 `wakeups/` 子模块，
//! 公共种子在 [`support`]。
//!
//! 错误信封的口径：本仓 `ApiError` 是 `{"error":{"code","message"}}`，`message` 形如
//! `"<code 标签>: <上游 message>"`；断言用后缀匹配锚定上游原文（见各测试内的注释）。

mod crud;
mod isolation;
mod listing;
mod support;
