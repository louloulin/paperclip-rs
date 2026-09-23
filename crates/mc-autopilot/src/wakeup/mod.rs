//! issue wakeup 面（`issue_wakeup` / `issue_wakeup_receipt`）。
//!
//! - **写者**：M5-6（`wakeup/**` 整组）。
//! - **上游**：`handler/issue_wakeup.go`320 + `wakeup_actor.go`63（≈64）+
//!   `service/issue_wakeup.go`831（`Validate`108 / `save`221 / `dispatch`185 / `CheckClaim`32 / `Tick`35）
//!   以及 `service/issue_wakeup_evidence.go`135（136）、`db/queries/wakeup.sql`162/23 查询 +
//!   `workspace_wakeup.sql`70/1 查询。
//! - **两个正交维度**：`kind ∈ {event, at, every, cron}` × `mode ∈ {once, continuous}`。
//!   旧桩把它们压成了一个 `source` 枚举（`mc_core::wakeup` 的「旧 stub 错在哪」表）。
//! - **`revision` 是两件事**：乐观并发的版本号 **且** 合并作用域
//!   （`issue_wakeup_receipt` 的唯一索引含 `revision`）⇒ **配置变更必须 bump `revision`**，
//!   否则旧的 pending receipt 会继续生效。
//! - **DB 侧还有 5 个迁移**（`528`–`532`：合并 / pending 事件 / 有界捕获 / actor 过滤 / actor 捕获）
//!   —— 本波 0 新迁移，但要**照抄** `capture_issue_wakeup()` 的语义，见 `evidence.rs`。
//! - **路由**：8 条里 7 条挂在 `/api/issues/{id}/wakeups*` 下（M5-0 已把 501 占位**搬进**
//!   `mc-http/src/routes/issues/wakeups.rs`，本片只填实现、不改注册位置）；
//!   `GET /api/issue-wakeup-summaries` 本波**不注册**（保持 `known_gap`）。
pub mod evidence;
pub mod service;
