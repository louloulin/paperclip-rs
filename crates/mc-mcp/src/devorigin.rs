//! 开发态 origin 白名单（本地插件的 `http://` origin 只在开发态放行）。
//!
//! - **写者**：M6-1（`docs/57` §3.2）。
//! - **上游**：`pkg/remotemcp` 的开发态 origin 白名单 + `internal/handler/plugin_surface.go` 的
//!   origin 校验（`MULTICA_PLUGIN_SURFACE_ORIGIN` 是**部署**配置，读取点在 `mc-http::state`）。
//! - **安全纪律**：`http://`（非 TLS）origin 只在 `dev_mode` 下允许，且只允许 `localhost` /
//!   回环地址；生产态一律要求 `https://` 且 host 必须在白名单里。**默认拒绝**（白名单为空
//!   ⇒ 全部拒），不要在实现里给一个「默认放行本机」的兜底。
//! - **本仓约定**：判定是纯函数（入参：origin、`dev_mode`、白名单），可单元测试；不要读环境变量
//!   （配置入口统一在 `mc-http::state`）。
//! - **不做什么**：不做 CORS 头（那是 `tower-http` 的 `cors` 层，在 `mc-http` 的 mount 处）。
//!
//! **状态：M6-1 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 110 行以内。
