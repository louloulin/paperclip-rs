//! toolkit 目录与 auth-config 解析 —— 上游 `integrations/composio/service.go` 的目录部分
//! （M8-0 anchor 建桩，**实现归 M8-6**）。
//!
//! 契约（`docs/61` §6.5 的 M8-6 行）：**auth-config 未配置的 toolkit 不出现在目录里**
//! （动态解析），所以本文件把「哪些 auth config 可用」当输入，输出可见 toolkit 集。

use mc_core::composio::ComposioToolkit;

/// 过滤出可见的 toolkit 目录 —— **anchor 期是桩**，实现归 M8-6。
pub fn visible_toolkits(
    _all: Vec<ComposioToolkit>,
    _available_auth_config_ids: &[String],
) -> Vec<ComposioToolkit> {
    todo!("M8-6：toolkit 目录的动态解析（docs/61 §6.5 的 M8-6 行）")
}
