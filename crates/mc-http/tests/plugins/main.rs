//! `/api/workspaces/{id}/plugins*` 端到端测试（M6-5 / LUM-1670）。
//!
//! 覆盖 `docs/57-M6-PLAN.md` §4.2「M6-5」的 13 条路由 —— plan 的通用规则 5 要求
//! 「该片路由**每条都有至少一条测试**」，所以文件按路由族分片：
//!
//! | 文件 | 路由 |
//! |---|---|
//! | `guard.rs` | 13 条的**门**（非法 workspace id / 非成员 / 非管理员 / 开关关闭） |
//! | `lifecycle.rs` | `GET\|POST /plugins`、`POST /plugins/preview`、`PUT …/config`、`POST …/enable\|disable`、`DELETE …/{installationId}` |
//! | `packages.rs` | `GET\|POST /plugins/packages`、`POST /plugins/packages/local`、`DELETE /plugins/packages/{packageId}` |
//! | `token.rs` | `POST\|DELETE /plugins/{installationId}/token` |
//!
//! 全部 `#[ignore]`：需要真库（`MULTICA_TEST_DATABASE_URL`），由门 ⑥ 用 `-- --ignored`
//! 拉起；`connect()` 在 env 缺失时打印跳过并 `return`，设了却连不上则 **panic**。
//!
//! 另有 `zipfixture.rs` 里一个不依赖 DB 的自检（夹具 CRC 写错会让全部上传用例一起假红）。
#![cfg(feature = "test-util")]

mod guard;
mod lifecycle;
mod packages;
mod support;
mod token;
mod zipfixture;
