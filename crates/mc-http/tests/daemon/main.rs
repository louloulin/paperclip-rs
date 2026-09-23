//! daemon 面（M3-7 / LUM-1438）端到端测试入口。
//!
//! 全部用例 `#[ignore]`（需要真 PostgreSQL，`MULTICA_TEST_DATABASE_URL`）：
//!
//! ```text
//! MULTICA_TEST_DATABASE_URL=… cargo test -p mc-http --features mc-http/test-util -- --ignored
//! ```
//!
//! 模块划分（每个文件都在 R7 的 800 行以内）：
//!
//! | 文件 | 覆盖 |
//! |------|------|
//! | `loop.rs` | 注册 → 心跳 → claim → prepare-lease → start → progress → messages → usage → complete；fail / cancel-ack / recover-orphans / pending |
//! | `gc.rs` | 5 条 `gc-check`（多进程残留清扫的探测面） |
//! | `async_face.rs` | 用户面 8 条异步往返（update / models / local-skills ×2） |
//! | `ws.rs` | 真 socket 的 WS 握手、无主体连接、RPC 回包与未知 method 404 |

mod async_face;
mod gc;
mod loop_routes;
mod support;
mod ws;
