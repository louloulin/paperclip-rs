//! M4 anchor scaffold（LUM-1470）：chat 领域层 crate —— **占位，无实现**。
//!
//! 归属：M4-3（chat 会话 / 消息读面 / 快捷栏 / draft-restore）+ M4-4（chat 派发与生成面）。
//! `docs/42-M4-PLAN.md` §5.1 第 1 项把本 crate 与 `mc-project` / `mc-squad` 一次建齐
//! （根 manifest 已是 glob members ⇒ 不改根 `Cargo.toml`），依赖也在这里**一次声明到位**
//! （`serde` / `serde_json` / `uuid` / `chrono` / `thiserror` / `tracing` + 内部
//! `mc-core` / `mc-db` / `mc-errors`）——此后 M4 各切片**不得**再新增三方依赖。
//!
//! 上游体量（`f41fae6b`，非测试行数）：`handler/chat.go` 2105 行、`chat_history.go` 418 行、
//! `chat_title.go` 296 行、`chat_pinned_agent.go` 169 行；`service/chat_quick_actions*.go`
//! 790 行。对照门 ⑩ 的 800 行/文件硬上限 ⇒ **本 crate 必须按子域拆文件**（见下），
//! 且新文件一律不得进 `scripts/file_size_baseline.tsv`（基线只减不增）。
//!
//! 预判的模块边界（与 `routes/chat/*`、`mc-repos/src/chat_*.rs` 一一对应）：
//!
//! | 模块 | 内容 | 切片 |
//! | --- | --- | --- |
//! | `session` | 会话状态（pin / archive / 未读计数）、生命周期校验 | M4-3 |
//! | `message` | 消息分页游标、`latest_visible` / `pending` / `prioritized` 排序语义 | M4-3 |
//! | `pinned` | 快捷栏（`chat_pinned_agent`）的去重与顺序 | M4-3 |
//! | `draft` | `chat_draft_restore` 的幂等消费 | M4-3 |
//! | `task` | `agent_task_queue` 的排队位置 / pending 语义（**只读** M3 域的表） | M4-4 |
//! | `history` | `/api/chat/history` + `/api/chat/thread` 的**非渠道分支** | M4-4 |
//! | `quick_action` | `quick_action` 表（`237_quick_action`）的读写与 regenerate | M4-4 |
//!
//! 上述名字是**建议边界**，不是硬契约：切片若实测需要合并/改名，在 PR 里说明并同步
//! `docs/42` §4.2 的写集矩阵即可（但**不得**改 `mc-repos/src/lib.rs` / `routes/mod.rs` /
//! `mount.rs` —— 那是本 anchor 的交付面）。
//!
//! # 不要做什么（anchor 边界）
//!
//! - 本文件 scaffold 阶段**只有文档注释**：0 类型、0 实现、0 路由、0 SQL。
//! - **不建表、不写迁移**：M4 的 11 张表（`chat_session` `chat_message`
//!   `chat_draft_restore` `chat_pinned_agent` `quick_action` + M4-1/M4-2 的 6 张）
//!   实测**全部已在** `migrations/upstream/` ⇒ M4 一个迁移文件都不新增
//!   （`docs/42` §2）。
//! - **不引入本仓自造列**；真值是上游迁移的列与约束。
//! - 尾斜杠形态纪律（`docs/42` §1.1「形态纪律」）：上游 `chi` 的 `Route("/x") + Get("/")`
//!   同时服务 `/x` 与 `/x/`，axum 0.7 不做归一化 ⇒ **两个形态都要注册**。本域 45 条里
//!   `/api/chat/sessions/`、`/api/projects/`（含 `{id}/`）、`/api/squads/`（含 `{id}/`）
//!   是带斜杠形态；`/api/chat/pending-tasks`、`/api/projects/search` 是**无**斜杠形态 ——
//!   逐字照抄上游，不要"顺手"统一。
//! - 路径参数一律写 `:id`（matchit 0.7 把 `{id}` 当字面量段：编译过、恒 404）。
