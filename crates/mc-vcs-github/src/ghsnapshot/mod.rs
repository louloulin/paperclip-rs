//! ghsnapshot：PR 卡片的**单一真值**管道（上游 `internal/integrations/ghsnapshot/`）。
//!
//! **状态：M8-0 anchor 只落文件与边界**（`LUM-1797` / `docs/61-M8-PLAN.md` §5）——
//! `client.rs` 落 `Client` 的形状与 **`api_base` 接缝**，`snapshot.rs` / `refresh.rs`
//! 是**桩**（签名 + `todo!()`）。实现归 M8-1（client 的 App 凭据链）与 M8-5
//! （snapshot 解析 + worker 池 / 限流 / 退避 / 停机）。
//!
//! # 三层（上游注释逐字）
//!
//! 1. **Client**（`client.rs`）：GitHub App 认证 —— App JWT → installation access token
//!    （按 installation 缓存、提前续期）→ GraphQL/REST 调用；
//! 2. **snapshot**（`snapshot.rs`）：那一条 GraphQL 查询、contexts 分页、归一化成扁平的
//!    逐 check 快照；
//! 3. **refresh**（`refresh.rs`）：出站工作队列（去重、每个 PR 同时只有一单在飞、有界并发、
//!    `Retry-After` 退避）、head-SHA 守卫的原子写、触发面（webhook / 页面访问 / TTL sweep）。
//!
//! # 宿主（`docs/61` §2.6）
//!
//! `refresh::Manager` 是**长期后台 worker**，宿主是 `apps/mc-server/src/integrations.rs`
//! （与 M5-9 的 `scheduler::start`、M7-0 的 `channels::start` 同造型）。停机顺序固定为
//! 「先停渠道连接 → 再停 PR 刷新 → 再停调度器 → 最后停 actor」。
//!
//! # 离线替身的接缝（R-M8-1）
//!
//! `client.rs` 的 `api_base` 字段（默认 `defaultAPIBase`）与上游逐字同款 ⇒ 本地 GraphQL
//! 替身按 `statusCheckRollup` 形状答，断言 `PRSnapshot::Decided` / 限流暂停 / 退避时间序列
//! （注入 `Now`）。

pub mod client;
pub mod refresh;
pub mod snapshot;

pub use client::Client;
pub use refresh::Manager;
pub use snapshot::parse_pr_snapshot;
