//! M3 anchor scaffold（LUM-1406）：agent 领域层 crate —— **占位，无实现**。
//!
//! 归属：M3-5（`feat/multica-rs-m3b-agents`）。docs/15 §5 M3-5 的原文是
//! 「`crates/mc-agent` 新 crate **只在需要领域逻辑时创建**（否则本片仅 repo + 路由）」；
//! 本片按 §7.2.1 的「建议一次建齐」（glob members 下多一个空 crate 的成本是 0，
//! 少一次 `Cargo.lock` 冲突）先建目录，M3-5 判定不需要领域逻辑时可保持空实现并在 PR 说明。
//!
//! M3-5 的实际交付面在别处（本 crate 不是必须项）：
//! - repo：`crates/mc-repos/src/agent.rs`（scaffold 已预置空文件 + `pub mod`）；
//! - 路由：`crates/mc-http/src/routes/agents.rs`（scaffold 已接好 `mount_slice_agent()`），
//!   覆盖 docs/15 §1.3 的 16 条，并**删除** `mount.rs` 的 M0 `/api/agents` 占位。
//!
//! scaffold 阶段本文件只有文档注释：一个类型都不定义。
