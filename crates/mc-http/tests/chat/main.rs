//! `/api/chat/**`（M4-3 的 15 条路线 = 20 个「方法 × 路径」键）端到端测试。
//!
//! - [`route_paths_are_mounted`]：**不需要数据库**，用 `Db::connect_lazy` 装配完整 router，
//!   逐个打 20 个键。命中路由时「没带用户头」必然 401、「带了用户头没带 workspace」必然
//!   400（workspace 解析在成员校验之前，不碰 DB）；一旦某条路径写错（漏了尾斜杠别名、
//!   或用了 axum 0.8 的 `{id}` 字面量写法），就会掉到 404 兜底而失败。CI 无库也能挡回归。
//! - 其余 5 条需要真实 PG（`MULTICA_TEST_DATABASE_URL`），全部 `#[ignore]`。
//!
//! 夹具在 `tests/chat/support.rs`（门 ⑩ 的 800 行上限把两边分开）。**本文件就是
//! `tests/chat/main.rs`** —— cargo 把它当 target `chat` 的 crate 根，同目录下的
//! `broadcast.rs` / `routes.rs` / `session.rs` / `messaging.rs` / `support.rs` 只是它的
//! 子模块，不会各自编成一个测试二进制。
//!
//! 运行示例：
//! ```
//! cargo test -p mc-http --test chat --features test-util                 # 仅路由守卫
//! MULTICA_TEST_DATABASE_URL=postgres://u:p@host:5432/db \
//!   cargo test -p mc-http --test chat --features test-util -- --ignored  # 全量
//! ```
//!
//! 断言取向：**逐字对齐上游 Go**（错误文案、空体语义、秒精度时间戳、游标里的纳秒、幂等
//! 204、`ON CONFLICT` 位置复用），而不是「本仓实现现在返回什么」—— 实现漂了测试会红。
//!
//! # 文件拆分（门 ⑩）
//!
//! 本文件与 `routes` / `session` / `messaging` 是同一份代码的**纯移动**（单文件 800 行
//! 上限，`scripts/file_size_check.py`）：本文件只留 crate 根（`mod` 声明 + `open_ctx!` 宏），
//! 六节用例按「守卫 / 会话 / 消息与快捷栏」三块落到子模块。先例 = `docs/32` §30 的 **D10**。

#![cfg(feature = "test-util")]

/// 建库夹具 + router；没有 `MULTICA_TEST_DATABASE_URL` 就跳过（`#[ignore]` 下的双保险）。
macro_rules! open_ctx {
    () => {{
        let Some((pool, db)) = connect().await else {
            eprintln!("skipping: set MULTICA_TEST_DATABASE_URL");
            return;
        };
        Ctx::open(pool, db).await
    }};
}

mod broadcast;
mod messaging;
mod routes;
mod session;
mod support;
