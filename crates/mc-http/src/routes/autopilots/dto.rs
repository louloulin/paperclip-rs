//! M5-0 anchor：autopilot 面**响应契约**共享模块（**非路由**，不含 `router()`）。
//!
//! - **写者**：M5-1（`docs/44` §3.2）。M5-2/3/4/5 只**读**（各自 handler 复用这里的投影）。
//! - **上游**：`autopilotToResponse`37 / `triggerToResponse`50 / `runToResponse`32 /
//!   `runToResponseSlim`88（`handler/autopilot.go`）。
//! - **最容易被抄错的字段**（`docs/44` §4.2 原文列举）：`assignee_type` / `pause_reason` /
//!   `execution_mode` / `can_write` / `can_manage_access`。
//! - **`can_write` 是 `Option<bool>`**：上游注释写明「不带 caller 时省略该字段，客户端按 unknown
//!   处理」⇒ 本地必须区分「省略」与 `false`（`Option` + `skip_serializing_if`，不要 `bool`）。
//! - **列表/slim 形态不同**：`runToResponseSlim`88 是列表用的裁剪版，别拿全量 DTO 顶列表。
//! - **时间戳用 `mc_core::Timestamp`**，不要在本文件各自 `to_rfc3339`。
