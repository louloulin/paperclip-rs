//! `mc-autopilot`：M5 波的**领域服务层**（autopilot 写面 / trigger / dispatch / webhook 入口 / wakeup）。
//!
//! **状态：M5-0 anchor（`LUM-1563` / `docs/44-M5-PLAN.md` §5.2）只落文件与边界** —— 本 crate 当前
//! 0 类型、0 实现、0 路由、0 SQL：每个 `src/**.rs` 只有模块文档，声明「谁写、上游在哪、要做什么、
//! 不许做什么」。函数签名随各自切片落（anchor 不放 `todo!()` 桩：桩会污染 ③ 的 pedantic 面，
//! 也会在每个切片 PR 里制造无意义 diff）。
//!
//! ## 依赖纪律
//!
//! `Cargo.toml` 的依赖在本 anchor **一次声明到位**，此后 M5 各切片**不得**再新增三方依赖
//! （要加走 `docs/15-M3-PLAN.md` §8.4 的仲裁，由集成方加）。`Cargo.lock` 由本片独占写。
//!
//! ## 技术选型：cron 解析 = **手写 5 字段**，不引 `cron` crate（`docs/44` §5.4 第 1 项，实测）
//!
//! 上游用 `robfig/cron`，且 `service/cron.go:12` 是
//! `cron.NewParser(Minute|Hour|Dom|Month|Dow)` —— **5 字段、无秒**。对 `cron` crate `0.12.1`
//! 实测（一次性探针 crate，5 组用例）后否掉了它：
//!
//! 1. **5 字段直接被拒**：`Schedule::from_str("0 9 * * *")` → `Invalid expression`（只收 6/7 字段；
//!    `@daily` 这类宏可以）。
//! 2. **星期编号不同**：该 crate 是 `1=SUN..7=SAT`（源码 `time_unit/days_of_week.rs` 里
//!    `"sun" => 1`、`inclusive_min() == 1`、`inclusive_max() == 7`），上游 robfig 是 `0=SUN..6=SAT`。
//!    于是 `0 0 0 * * 1` 在该 crate 里落到**周日**、`* * 0` 直接报错 ⇒ 静默错一天，且不报错。
//! 3. **`dom` 与 `dow` 同时受限时是 AND，不是 Vixie 的 OR**：`0 0 0 1 * 1` 只出「1 号**且**是周日」
//!    （实测 2026 年只有 2/1、3/1、8/1、11/1），上游 robfig 是 OR。**这一条单独就否掉了该 crate。**
//! 4. **没有 `TZ=` / `CRON_TZ=` 前缀**：`"TZ=Asia/Shanghai 0 0 9 * * *"` → 报错（时区只能由调用方传）。
//! 5. 兼容的一点：`0 0 0 30 2 *`（2 月 30 日）返回**空迭代器**（`next()` = `None`，不报错），
//!    与 robfig 的「零值时间 = 无下次触发」一致 ⇒ 本地手写版同样用 `Option<Timestamp>` 表达
//!    「无下次触发」，不要用 `Result::Err`。
//!
//! 结论：`service/cron.go`(138) 的移植落在 `src/trigger.rs`（M5-3；cron 解析的**唯一**落点，
//! `cron-preview` 路由与 M5-7 的 `plan_time` 共用它），**`cron` 不在依赖表里**。
//!
//! ## 与 `mc-core` 的边界（对 §5.2 的唯一偏离，必须记）
//!
//! §5.4 第 2 项希望「在 `mc-core` 暴露一个 `Timezone` 校验」，但 **`mc-core/Cargo.toml` 不在本
//! anchor 的写集里**（`docs/44` §3.1）⇒ 不把 `chrono-tz` 引进 `mc-core`。落地方式是：`Timezone`
//! newtype + IANA 名校验跟**它的 owner 片**走，即 `src/trigger.rs`（M5-3；上游
//! `resolveAutopilotTriggerTimezone`(29) 也归 M5-3）⇒ 写者仍然唯一。`chrono-tz` 本身在本 crate
//! 的依赖表里（workspace 依赖 `0.10`）。
//!
//! ## 子模块与写者（`docs/44` §3.2 的写集矩阵）
//!
//! | 文件 | 写者 | 上游主源 |
//! | --- | :-: | --- |
//! | `src/lib.rs` / `src/error.rs` / `src/dto.rs` | M5-1 | crate 框架 + 跨切片共享的 DTO / 错误基座 |
//! | `src/quota.rs` / `src/notification.rs` | M5-1 | `service/autopilot_quota.go`414 / `autopilot_quota_notifications.go`198 / `autopilot_notification_recipient.go`91 |
//! | `src/write.rs` / `src/collaborator.rs` | M5-2 | `CreateAutopilot`159 / `UpdateAutopilot`260 / `DeleteAutopilot`64 / 协作者与订阅者 |
//! | `src/trigger.rs` / `src/credential.rs` | M5-3 | trigger CRUD 5 条 + `service/cron.go`138 + `signingSecretHint`15 |
//! | `src/dispatch/**` | M5-4 | `service/autopilot.go` 的 dispatch 段 ≈2,600 |
//! | `src/webhook/**` | M5-5 | `handler/autopilot_webhook.go`1,010 + admission 段 |
//! | `src/wakeup/**` | M5-6 | `handler/issue_wakeup.go`320 + `wakeup_actor.go`63 + `service/issue_wakeup.go`831 |
//!
//! `src/lib.rs` / `src/error.rs` / `src/dto.rs` 这三行在 §3.2 的矩阵里**没有对应行**（矩阵只列了上面
//! 六组）⇒ 本 anchor 把它们判给 **M5-1**（与 `mc-http` 侧 `autopilots/{access,dto,list}.rs` 同片），
//! 免得 8 个切片抢同一个文件。规则：**各切片私有的错误类型 / DTO 放自己的文件**，只有跨切片共享的
//! 才进 `error.rs` / `dto.rs`。
//!
//! ## 不做什么
//!
//! - 不建 `mc-analytics`（`docs/44` §1.2：M5 的 "analytics" 落在 quota/usage 与调度 job 上）
//! - 不实现 daemon 面（`agent_task_queue` 的 `queued` 行由 M3-7 的 daemon loop 执行；R9 明令本波不做）
//! - 不新增迁移（12 张 M5 表全部已存在，本波 0 个新迁移）

pub mod collaborator;
pub mod credential;
pub mod dispatch;
pub mod dto;
pub mod error;
pub mod notification;
pub mod quota;
pub mod trigger;
pub mod wakeup;
pub mod webhook;
pub mod write;
