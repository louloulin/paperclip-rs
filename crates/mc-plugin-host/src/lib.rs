//! `mc-plugin-host`：插件宿主的**纯逻辑**层 —— 声明式 manifest/bundle 校验器、能力裁决、
//! 凭据派生与安装令牌。
//!
//! **状态：M6-0 anchor 只落文件与边界**（`LUM-1665`）—— 本文件只有模块声明与下面的归属表；
//! 六个子模块都是 doc-only 桩，由 M6-1 实现（M6-5…M6-8 只读）。
//!
//! ## ⚠️ 先纠正一个流传很广的误读（`docs/57` §9.3）
//!
//! `pkg/plugincontract` **不是 JSON-RPC**。它是一个**声明式** manifest/bundle/capabilities
//! 校验器：插件在 `multica.plugin.json` 里声明 surfaces / hooks / resources / capabilities，
//! 宿主按声明裁决「能不能跑」。真正的 RPC 在两处 —— remote MCP（`pkg/remotemcp/client.go`，
//! 本仓对应 `mc-mcp`）与浏览器侧（`packages/plugin-sdk/protocol.ts`，不在本仓范围）。
//!
//! 因此本仓那个 stdio JSON-RPC 的 `mc-plugin-protocol`（343 行、零代码依赖者）由 M6-0
//! **删除**，不再作为「插件协议」的落点。M6 的验收口径随之改为**端到端行为**：hook 的
//! HTTP 签名调用 + MCP 工具的采纳/调用（`docs/57` §9.3）。
//!
//! ## 上游与写者（`docs/57` §3.2 矩阵）
//!
//! | 文件 | 写者 | 上游 |
//! | --- | :-: | --- |
//! | `src/lib.rs` | M6-0（本 anchor） | —— |
//! | `src/manifest.rs` | M6-1 | `pkg/plugincontract/manifest.go`（`Manifest` / `Author` / `Contributes` / `Surface` / `Hook` / `HookSchedule` / `HookTransport` / `Resource` / `ConfigField` / `ConfigSchema`） |
//! | `src/capabilities.rs` | M6-1 | `pkg/plugincontract/capabilities.go`（`Capabilities` / `ErrCapabilityUnavailable`） |
//! | `src/bundle.rs` | M6-1 | `pkg/plugincontract/bundle.go`（`BundleFile` / `Bundle` / 入口白名单 / 体积上限） |
//! | `src/scope.rs` | M6-1 | 授权 scope 的裁决（**唯一实现点**，`docs/57` §4.2 M6-1） |
//! | `src/credentials.rs` | M6-1 | `internal/util/secretbox`（`LoadKey` / `Seal` / `Open`）+ hook 签名密钥派生 |
//! | `src/token.rs` | M6-1 | `internal/service/plugin_token.go`（`mpi_` / `mpc_` 两族令牌 + `hashToken`） |
//! | `routes/*`（读侧） | M6-5…M6-8 | ——（只读本 crate，**不**改本 crate 的文件） |
//!
//! ## 与 `mc-core::plugin` 的分工（别把两处真值立起来）
//!
//! `mc-core::plugin` 只放**列投影**：`plugin_installation` / `plugin_storage` / `plugin_secret`
//! / `plugin_invocation` / `plugin_hook_schedule` / `plugin_package*` 的字段与封闭词表。本 crate
//! 放**声明式契约**：`Manifest` / `Contributes` / `Surface` / `Hook` / `Resource` /
//! `ConfigSchema` / `Capabilities`。`plugin_installation.manifest` 是 JSONB，写入时由本 crate
//! 校验后原样落库 ⇒ 结构化的那一份**只在本 crate**（`mc-core::plugin` 因此**故意不定义**
//! `PluginSurface`/`PluginHook`，见该文件的「不做什么」段与 `docs/32` §9）。
//!
//! ## 不做什么
//!
//! - 不出网、不碰数据库、不依赖 `mc-http`（凭据的**读取**在 `mc-http::state` 的
//!   `PluginSecretKey`；本 crate 只接受 `&[u8]`，绝不去读环境变量 —— 否则「配置的入口」会有两处）；
//! - 不实现 remote MCP 的传输（归 `mc-mcp`）；
//! - 不新增迁移：`plugin_*` 八张表在 `migrations/upstream/344` + `362` + `369` + `392` + `399` 里都已存在；
//! - 不定义 HTTP 错误码映射（route 层按 `plugin_disabled` / `plugin_surfaces_not_configured`
//!   两个既有码降级，见 M6-5…M6-7）。

pub mod bundle;
pub mod capabilities;
pub mod credentials;
pub mod manifest;
pub mod scope;
pub mod token;
