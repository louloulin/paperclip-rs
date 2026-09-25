//! `POST /api/issues/:id/squad-evaluated` 端到端测试（M2-A 尾-补 / LUM-1793）。
//!
//! 需要真实 PG：`workspace` / `member` / `user` / `agent` / `issue` / `squad` /
//! `agent_task_queue` / `activity_log` 都必须已迁移。用例全部 `#[ignore]`，靠
//! `MULTICA_TEST_DATABASE_URL` 触发；没有该 env 时静默 skip。
//!
//! 运行示例：
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://multica:multica@127.0.0.1:5432/multica_test \
//!   cargo test -p mc-http --test squad_evaluation --features test-util -- --ignored
//! ```
//!
//! # 为什么拆目录而不是一个文件
//!
//! R7 的单文件 800 行硬上限（门禁 ⑩）—— 三条用例 + 夹具合写是 945 行（与
//! `tests/squads/`、`tests/issue_pins.rs` 拆分的同款理由）。
//!
//! # 三条用例各管一件事，因为**检查顺序本身就是契约**
//!
//! 上游 `squad.go:1043-1051` 的注释写明：任何会回显 task 派生 id 的拒绝，都必须排在
//! 「租户收窄 + 调用者就是该任务的 agent」两道门之后。
//!
//! | 模块 | 用例 |
//! | --- | --- |
//! | [`happy`] | 正常路径 + 回读 `activity_log` 七列 + 「同一对 id 记错 issue」的 400 |
//! | [`ladder`] | 从 400 / 403 / 404 的**每一档**（按上游检查顺序平铺） |
//! | [`order`] | **顺序**这条契约本身：越权探测不得套出 task 的 issue id |
//!
//! 这条端点在本地是 **dev-mode 语义**：`AuthUser` 恒人类成员，agent 身份靠
//! `X-Agent-ID` + `X-Task-ID` 自校验（上游 `resolveActor` 的第二条分支）。偏离与理由见
//! `docs/22-ROUTE-PARITY.md` §7。

#![cfg(feature = "test-util")]

mod happy;
mod ladder;
mod order;
mod support;
