//! `mc-mcp`：remote MCP（Model Context Protocol）客户端 —— 这是本仓**真正**的插件 RPC 面。
//!
//! **状态：M6-0 anchor 只落文件与边界**（`LUM-1665`）—— 本文件只有模块声明与下面的归属表；
//! 四个子模块都是 doc-only 桩，由 M6-1 实现（M6-6 / M6-9 只读）。
//!
//! ## 为什么它和 `mc-plugin-host` 是两个 crate
//!
//! `mc-plugin-host` 是**声明式**契约（manifest/bundle 校验，纯函数、零 IO），而本 crate 是
//! **网络客户端**（出网、OAuth、超时、错误重试）。把两者塞进一个 crate 会让「校验器」被迫
//! 依赖 `reqwest`/`tokio`，也会让 route 层的两个使用场景（安装校验 vs 调用远端的工具）
//! 共用同一套错误类型。分开后：M6-1 两个都写，但依赖方向单一（本 crate 不依赖 host crate）。
//!
//! ## 上游与写者（`docs/57` §3.2 矩阵）
//!
//! | 文件 | 写者 | 上游 |
//! | --- | :-: | --- |
//! | `src/lib.rs` | M6-0（本 anchor） | —— |
//! | `src/client.rs` | M6-1 | `pkg/remotemcp/client.go`（`initialize` → `notifications/initialized` → `tools/list` → `tools/call`） |
//! | `src/types.rs` | M6-1 | `pkg/remotemcp` 的 wire 类型（JSON-RPC 信封 + tool 描述 + 调用结果） |
//! | `src/oauth.rs` | M6-1 | remote MCP 的 OAuth 授权（token 交换 / 刷新） |
//! | `src/devorigin.rs` | M6-1 | 开发态 origin 白名单（本地插件用 http origin，生产禁） |
//!
//! ## 调用顺序（协议口径，逐字照抄上游）
//!
//! 1. `initialize`（客户端能力 + 协议版本）；
//! 2. **通知** `notifications/initialized`（无 id，不等回包）；
//! 3. `tools/list` —— 结果要与用户**已采纳**的工具集合做交叉校验
//!    （上游 `validatePinnedRemoteMCPTools`：远端改了 schema 就要重新采纳，不能静默沿用旧
//!    授权 —— 采纳记录的落库列是 `plugin_installation.mcp_approvals`，
//!    形状 `{"<hook_key>": {"tools":[…], "approved_at":…, "approved_by":…}}`，见迁移 `369`）；
//! 4. `tools/call` —— 每次调用落一行 `plugin_invocation`（`trigger='agent'` 或 `'event'`）。
//!
//! ## 本仓约定
//!
//! - `reqwest` 用 workspace 的既有版本（`default-features = false` + `json`/`rustls-tls`），
//!   **不引 openssl**；
//! - 所有出站调用要有**显式超时**（照 `mc-http` 的 `HTTP_TIMEOUT_SECS` 风格），并把超时映射
//!   成可区分的错误（route 层要落 `plugin_invocation.status='timeout'`）；
//! - 错误用 `thiserror` + 稳定码，**不要**把 reqwest 的错误字符串直接透给用户。
//!
//! ## 不做什么
//!
//! - 不做 MCP **服务端**（本仓只当客户端）；
//! - 不做 stdio 传输（被删掉的 `mc-plugin-protocol` 那套 stdio JSON-RPC **不是**本协议，
//!   见 `mc-plugin-host` 的 §9.3 说明）；
//! - 不碰数据库（`plugin_invocation` 的写入归 M6-6 / M6-8 的 route 层 + `mc-repos`）；
//! - 不新增迁移。

pub mod client;
pub mod devorigin;
pub mod oauth;
pub mod types;
