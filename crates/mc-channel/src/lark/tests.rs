//! `lark` 模块级用例（M7-13）：端到端收发回路 + 共用装置的出口。
//!
//! - **写者**：M7-13（`docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §32）。
//! - 与 `slack/tests.rs` 同款：`tests/` 子目录放**装置**，`tests.rs` 放模块表与跨子模块的
//!   窄出口。本片自己的断言在 [`crate::lark::outbound::tests`] 与 `tests::round_trip`。

#[cfg(test)]
pub(crate) mod support;

#[cfg(test)]
mod round_trip;
