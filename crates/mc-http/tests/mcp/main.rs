//! `/api/workspaces/{id}/mcp-servers*` + `/api/agents/{id}/mcp-servers*` 端到端测试
//! （M8-3 / `LUM-1800`，`docs/61-M8-PLAN.md` §4.2 的 MCP 行 / §6.5 的 M8-3 行）。
//!
//! 覆盖 M8-3 的 **8** 条路由：库面 `GET/POST` + `PUT/DELETE {serverId}`、agent 面
//! `GET/POST` + `PUT {serverId}/enabled` + `DELETE {serverId}`。全部 `#[ignore]`：需要真库
//! （`MULTICA_TEST_DATABASE_URL`，门 ⑥ 用 `-- --ignored` 拉起）。
//!
//! ```text
//! MULTICA_TEST_DATABASE_URL=postgres://mc_lum1800:…@127.0.0.1:5432/mc_lum1800 \
//!   cargo test -p mc-http --test mcp --features test-util -- --ignored
//! ```
//!
//! **无需平台替身**（`docs/61` §4.2 的 MCP 行逐字）：这一面全是本地库语义 ⇒ 证据 = 真库
//! CRUD + 校验反例（空名 / 非法字符名 / 非对象条目）+ **write-only 断言**（响应原始字节里
//! `headers` / `env` 的值一个字节都不出现）+ overlay 合并纯函数用例（在 `mc-core`，见
//! `mcp::overlay` 的 25 条）。
//!
//! 文件布局（门 ⑩ 单文件 800 行硬上限）：
//! - `mcp/support.rs`：连接 / `AppState` 字面量 / 种子 / 清场 / 请求辅助
//! - `mcp/workspace.rs`：库面 4 条（授权矩阵 + write-only + 重名 + 删除扫绑定）
//! - `mcp/agent.rs`：agent 面 4 条（`loadAgentForUser` 语义 + 幂等 + 跨租户 404）
#![cfg(feature = "test-util")]

mod agent;
mod support;
mod workspace;
