//! `/api/workspaces/{id}/plugins*` 端到端测试（M6-5 / LUM-1670 建；M6-6 / LUM-1671 加 `runtime.rs`）。
//!
//! 覆盖 `docs/57-M6-PLAN.md` §4.2 的 17 条 `plugin_*` 路由（M6-5 的 13 + M6-6 的 4）—— plan 的
//! 通用规则 5 要求「该片路由**每条都有至少一条测试**」，所以文件按路由族分片：
//!
//! | 文件 | 路由 |
//! |---|---|
//! | `guard.rs` | M6-5 13 条的**门**（非法 workspace id / 非成员 / 非管理员 / 开关关闭） |
//! | `lifecycle.rs` | `GET\|POST /plugins`、`POST /plugins/preview`、`PUT …/config`、`POST …/enable\|disable`、`DELETE …/{installationId}` |
//! | `packages.rs` | `GET\|POST /plugins/packages`、`POST /plugins/packages/local`、`DELETE /plugins/packages/{packageId}` |
//! | `token.rs` | `POST\|DELETE /plugins/{installationId}/token` |
//! | `runtime.rs` / `runtime_surface.rs` | **M6-6 的 4 条**：`GET` invocations、`GET\|PUT` mcp tools（+ 共用的 `runtime_support.rs`）、`GET` surface launch |
//!
//! 全部 `#[ignore]`：需要真库（`MULTICA_TEST_DATABASE_URL`），由门 ⑥ 用 `-- --ignored`
//! 拉起；`connect()` 在 env 缺失时打印跳过并 `return`，设了却连不上则 **panic**。
//!
//! 另有 `zipfixture.rs` 里一个不依赖 DB 的自检（夹具 CRC 写错会让全部上传用例一起假红）。
#![cfg(feature = "test-util")]

mod guard;
mod lifecycle;
mod packages;
mod runtime;
mod runtime_support;
mod runtime_surface;
mod support;
mod token;
mod zipfixture;
