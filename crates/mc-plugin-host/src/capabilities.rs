//! 宿主能力裁决：manifest 的声明 × 宿主开放的能力 ⇒ 能不能跑。
//!
//! - **写者**：M6-1（`docs/57` §3.2）。
//! - **上游**：`pkg/plugincontract/capabilities.go` 的 `Capabilities`(16) 与
//!   `ErrCapabilityUnavailable`(61)。
//! - **语义**：能力是**宿主**的开关（不是插件声明的结果）。插件只声明它想要什么；
//!   宿主在这里裁决「本部署是否提供」。裁决失败不能崩、不能静默跳过 —— 返回
//!   `ErrCapabilityUnavailable` 同义的错误，由 route 层映射成明确的降级码
//!   （`plugin_disabled` / `plugin_surfaces_not_configured`）。
//! - **与 scope 的分工**：`capabilities` 判「这类能力本部署有没有」，`scope` 判「这个安装
//!   被授予了没有」。两者都在 M6-1（唯一实现点）；route 层不要自己写判定。
//! - **本仓约定**：能力集合用封闭枚举 + `ALL`（照 `mc-core` 的既有风格），**不要**用裸字符串
//!   比较散落在各 route 文件里。
//! - **不做什么**：不做 feature flag（部署级开关走 `mc-feature-flags`，是另一套东西，别混）。
//!
//! **状态：M6-1 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 140 行以内。
