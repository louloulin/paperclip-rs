//! 插件 manifest 的结构化类型与校验（声明式契约的**唯一**真值）。
//!
//! - **写者**：M6-1（`docs/57` §3.2）。M6-5…M6-8 只读本文件，不得改。
//! - **上游**：`pkg/plugincontract/manifest.go` —— `Manifest`(177) / `Author`(190) /
//!   `Contributes`(195) / `Surface`(203) / `Hook`(213) / `HookSchedule`(228) /
//!   `HookTransport`(233) / `Resource`(240) / `ConfigField`(248) / `ConfigSchema`(264)。
//! - **落库形态**：`plugin_installation.manifest` 与 `plugin_package_version.manifest` 都是
//!   JSONB —— 校验通过后**原样**落库（不要「先转成 Rust 结构再序列化回去」，那会改键序、
//!   丢未知字段、并把上游的 omitempty 语义抹平）。Rust 类型只用于校验与读取。
//! - **hook 的 `input_schema` 是 `json.RawMessage`**（原样透传的 JSON）。上游的
//!   `pluginHookResponse`（`internal/handler/plugin.go:82`）**不下发** `input_schema` ——
//!   本仓的响应投影要照抄这个「有字段但不外发」的取舍。
//! - **本仓约定**：校验失败返回 `thiserror` 错误 + 稳定错误码（插件作者能据此改 manifest）；
//!   未知字段**不报错**（前向兼容：上游 `Manifest` 有新增字段时老包不能被判死）。
//! - **不做什么**：不做 manifest 的**迁移**（`manifest_version` 不是 v1 的包直接拒，不升级）；
//!   不做 schema 的语义校验（`input_schema` 只做「是不是合法 JSON」）。
//!
//! **状态：M6-1 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 320 行以内（十来个类型 + 校验 + 用例）。**接近 800 行门时先按
//! 「类型 / 校验」拆兄弟文件，不要把门撬开。**
