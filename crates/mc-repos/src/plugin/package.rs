//! 插件包：`plugin_package` / `plugin_package_version` / `plugin_package_file` 的读写。
//!
//! - **写者**：M6-5（**W**；`docs/57` §3.2）。
//! - **上游**：`internal/handler/plugin_packages.go`（+ `392_plugin_package_publishing` 的语义）。
//! - **7 / 9 / 7 列**：见 `mc_repos::plugin` 的表；两个 digest 列都是**纯 hex**（`char_length = 64`
//!   的 CHECK），带 `sha256:` 前缀会直接撞约束（bundle 的**线上形态**才有前缀，见 `mc_core::skill`）。
//! - **两条硬语义**：
//!   1. **版本不可变**：`plugin_package_version` 一旦落库不得 UPDATE（老设计里的
//!      `enforce_plugin_release_immutable()` 触发器已被 `344` drop，但**语义保留** ——
//!      本地靠「不写 UPDATE」而不是靠触发器）；
//!   2. `content` 是 **BYTEA**：行结构用 `Vec<u8>`（不要 `String`，否则非 UTF-8 包直接炸）。
//! - **本仓约定**：`workspace_id` 在 `plugin_package_version` 里是**冗余列**（`392` 故意加的，
//!   便于按工作区收窄查询）—— 写入时必须与 `plugin_package.workspace_id` 一致，别只写一处。
//! - **不做什么**：不做校验（zip 白名单 / 体积上限在 `mc-plugin-host::bundle`，本文件信任入参）。
//!
//! **状态：M6-5 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 300 行以内。
