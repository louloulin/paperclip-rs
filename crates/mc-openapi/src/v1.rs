//! 公开 Action API（`/v1/*`）的 OpenAPI 片段：**9 个 Operation + 4 种凭据 + 2 档限流**。
//!
//! - **状态**：M6-0 anchor 只落文件与边界（`LUM-1665`）—— 本文件目前**只有一个空的
//!   `OPERATIONS` 常量**，`M6-1` 落 `docs/57` §4.2 的那 9 个 Operation 与 `ProblemDetail`。
//! - **写者**：M6-1（**W**；`docs/57` §3.2：`mc-openapi/src/v1.rs` | M6-1 写）。M6-7 只读
//!   （公开 API 的 route 表要按这里声明的路径/方法落地）。
//! - **上游**：`pkg/publicapi/v1`。
//! - **为什么单独一个文件而不是塞进 `lib.rs`**：`lib.rs` 是既有 `OpenApiSpec` 的手写生成器
//!   （本波的 /v1 面与旧面**不共用路径前缀、不共用凭据**）；把它拆开可以保住「一个文件一个
//!   写者」且不让 `lib.rs` 长过门 ⑩。`lib.rs` 里只加一行 `pub mod v1;`（M6-0 已加）。
//! - **口径（`docs/57` §4.2 M6-1 的 DoD）**：0 路由 —— 这里是**声明**，路由在 M6-7 的
//!   `routes/v1/*`；两者不一致时以「route 表的实测」为准并在 `docs/32` §9 登记。
//! - **本仓约定**：`Operation` 是最小形状（`method` + `path` + 可选摘要）；不要在这里展开
//!   完整 OpenAPI schema（那是 `lib.rs` 的 `add_path` 体系，两套混用会在生成 JSON 时打架）。
//! - **不做什么**：不在这里做鉴权/限流的**实现**（凭据校验在 `routes/v1/policy.rs`
//!   + `mc-plugin-host::token`；限流档位靠 `tower_governor` 在 `routes/v1/mod.rs` 的合并点加一次）。

/// 一个 `/v1` Operation 的**声明**（不含 schema）。
///
/// `method` 用大写 HTTP 方法名（`GET` / `POST` / `PUT` / `DELETE`），`path` 用 axum 0.7 的
/// 冒号形态（`/v1/issues/:id`；`{id}` 在 matchit 0.7 里是**字面量**段，会静默不匹配）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Operation {
    /// HTTP 方法（大写）。
    pub method: &'static str,
    /// 路径模板（冒号参数形态）。
    pub path: &'static str,
    /// 一句话摘要（可为空串）。
    pub summary: &'static str,
}

/// `/v1` 的 Operation 表。
///
/// **M6-0 anchor 保持为空**：本常量此刻故意是 `&[]`，`M6-1` 落地 9 条（context 1 / issues 4 /
/// storage 4）。谁把这里写满谁负责让 `M6-7` 的 route 表与之逐条对齐（`docs/57` §9 的验收
/// 靠**实测**，不靠这里）。
pub const OPERATIONS: &[Operation] = &[];
