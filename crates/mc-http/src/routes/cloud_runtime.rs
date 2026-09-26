//! `/api/cloud-runtime/*` 11 条（节点池管理面）—— **写者 M9-11**（`LUM-2116`）。
//!
//! ## ⚠️ 本文件的**来源**（M9-0 `LUM-1815` 的**预声明升级**）
//!
//! `docs/64-M10-PLAN.md` §5.2 对 M9-0 提了一条追加请求：请它**预声明 2 行**
//! （`crates/mc-cloud/src/lib.rs` 的 `pub mod runtime;` + `crates/mc-http/src/routes/cloud/mod.rs`
//! 的 `pub mod runtime;`），好让 M9-11 不必对 anchor 冻结文件做"最小破例"。
//!
//! **本 anchor 按 M9-11 自己的写集落点（`crate::routes::cloud_runtime`，顶层单文件）做了
//! 预声明升级**，而不是照抄那条请求里的 `routes/cloud/runtime.rs`：
//!
//! | 预声明件 | 谁需要 | 落点 |
//! | --- | --- | --- |
//! | `mc-cloud/src/runtime.rs` + `pub mod runtime;` | M9-11（**同上**） | `mc-cloud` ✅ |
//! | **本文件** + `routes/mod.rs` 的 `pub mod cloud_runtime;` + `mount.rs` 的 `mount_slice_cloud_runtime()` | M9-11 | `mc-http` ✅（按它自己的路径） |
//!
//! 两条判据 ① **M9-11 的写集逐字**写的是 `crates/mc-http/src/routes/cloud_runtime.rs`（**顶层
//! 单文件**），而 `routes/{mod,mount}.rs` 的追加列在**它自己**的写集里 ⇒ 若 anchor 只按
//! `routes/cloud/runtime.rs` 预声明，M9-11 **仍然**要回来改两个 frozen 文件（预声明就白做了），
//! 而且 `routes/cloud/` 里会留下一个**永远没人填**的模块（第二个真相源）；
//! ② 这三处（本文件 + `mod.rs` 一行 + `mount.rs` 两行）**没有任何其他写者候选**
//! ⇒ 预声明是纯收益。裁定登记 `docs/32-M3-DAEMON-FACE.md` §9.13。
//!
//! ## anchor 期（**零注册键**）
//!
//! **空** `Router::new()`。🔴 **不得**注册 501 占位：这 11 条是**上游键**（owner 单元格暂写
//! `M3`，波次账目归 M9），占位会让 ⑦ 把它们算成 `implemented_placeholder`（`owners.M3` 假清零），
//! 而 ⑨ 里它们**没有 fixture** ⇒ 不会变成 `mismatch`（陷阱更深：⑦ 的假清零没有任何门能抓到）。
//!
//! ## M9-11 要填什么
//!
//! 上游：`internal/handler/cloud_runtime.go`（208 行）+
//! `internal/cloudruntime/client.go`（255 行，**只读**复用 `mc_cloud::transport`）。
//! 11 条的逐行 `withUserID` / `withQuery` / `withBody` 开关与出站路径表在
//! `crates/mc-cloud/src/runtime.rs` 的模块头（**锚点期提示，M9-11 起手必须逐字复核**）。
//!
//! ## 两条反向验收（M9-11 的 `DoD` 原文）
//!
//! 1. `GET /api/cloud-runtime/healthz` 与 `.../readyz` **不是**上游的服务探针
//!    `/healthz` / `/readyz`（那两个在 `router.go:1400-1401`，属 M10）⇒ **不得**混实现、
//!    **不得**互相注册（路径不同、前缀不同，混了会撞 ⑦ 的 owner 归属）；
//! 2. 11 条**全部**在 workspace **member** 组（无 fixture、无 anonymous 面）。
//!
//! ## 与 M9-0 的锚点契约
//!
//! 写出站请求**只**能经 `mc_cloud::transport::Client`（`docs/62` §2.7 第 1 条：
//! `mc-http` 的任何 handler **不得**直接 `reqwest`）；`transport.rs` 的唯一写者是 anchor，
//! 此后**冻结**（R-M9-7）⇒ 传输若缺东西，登记到 `M9-10`，不要就地改。

use axum::Router;
use std::sync::Arc;

use crate::state::AppState;

/// cloud-runtime 切片（M9-11 在这里挂 11 条）。
pub fn router() -> Router<Arc<AppState>> {
    Router::new()
}
