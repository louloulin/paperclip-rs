//! `/v1/*` + `/api/plugin-bridge/v1/*` + `/plugin-surfaces/:token` 端到端测试（M6-7 / LUM-1672）。
//!
//! 覆盖 `docs/57-M6-PLAN.md` §4.2「M6-7」的 19 条路由与四条硬 `DoD`：
//!
//! | 文件 | 覆盖面 |
//! |---|---|
//! | `guard.rs` | 两个信任面的**门**：`mpi_`/`mpc_` 前缀、会话头、开关关闭、停用安装、未知安装 |
//! | `mounts.rs` | **两侧挂载同 handler**：同一请求经 `/v1` 与 bridge 的**响应字节**比对（`DoD` 硬项） |
//! | `context.rs` | `/context` 的两个 actor 形态 + `mpc_` **可重复调用**（第二次不得 403） |
//! | `issues.rs` | `GET\|PATCH /issues/:ref`、`GET\|POST /issues/:ref/comments`（含 scope 门与 409） |
//! | `storage.rs` | `GET /storage/:scope`、`GET\|PUT\|DELETE /storage/:scope/:key`（含配额 507） |
//! | `rate_limit.rs` | 超档 ⇒ **429** |
//! | `surface.rs` | `/plugin-surfaces/:token`：Host 边界 + 篡改/过期/**错域**三种拒绝 |
//!
//! 需要真库的用例全部 `#[ignore]`（门 ⑥ 用 `--features mc-http/test-util -- --ignored` 拉起）；
//! `support::connect()` 在 env 缺失时打印跳过并 `return`，设了却连不上则 **panic**。
//!
//! `support.rs` 里另有一组**不依赖 DB** 的自检（`support_selfcheck.rs`），它们跟着默认门跑。
#![cfg(feature = "test-util")]

mod context;
mod guard;
mod issues;
mod mounts;
mod rate_limit;
mod storage;
mod support;
mod support_selfcheck;
mod surface;
