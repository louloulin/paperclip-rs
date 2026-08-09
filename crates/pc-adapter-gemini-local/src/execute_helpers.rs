//! Gemini-local execute 助手函数。
//!
//! 完整复刻 Node `packages/adapters/gemini-local/src/server/execute.ts`
//! 中与 billing 解析、headless env 规范化、skills 路径、提示注记相关的纯函数。

use std::collections::BTreeMap;

use pc_acpx::env_helpers::has_non_empty_env_value;

/// Gemini 的 billing 模式。
///
/// Node 等价：`resolveGeminiBillingType` 的返回类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeminiBillingType {
    /// API key 认证（`GEMINI_API_KEY` 或 `GOOGLE_API_KEY`）。
    Api,
    /// Gemini 订阅（无 API key）。
    Subscription,
}

impl GeminiBillingType {
    pub fn as_str(&self) -> &'static str {
        match self {
            GeminiBillingType::Api => "api",
            GeminiBillingType::Subscription => "subscription",
        }
    }
}

/// 解析 Gemini 的 billing 类型。
///
/// Node 等价：`resolveGeminiBillingType`。`GEMINI_API_KEY` 或 `GOOGLE_API_KEY`
/// 任一非空 → `Api`；否则 `Subscription`。
pub fn resolve_gemini_billing_type(env: &BTreeMap<String, String>) -> GeminiBillingType {
    if has_non_empty_env_value(env, "GEMINI_API_KEY")
        || has_non_empty_env_value(env, "GOOGLE_API_KEY")
    {
        GeminiBillingType::Api
    } else {
        GeminiBillingType::Subscription
    }
}

/// 为 headless 模式规范化环境变量。
///
/// Node 等价：`buildGeminiHeadlessEnv`：
/// - `TERM` 为空 / "dumb" / "vt100" → 设为 `"xterm-256color"`。
/// - `COLORTERM` 为空 → 设为 `"truecolor"`。
/// - 强制 `NO_BROWSER=1`。
/// - 删除 `NO_COLOR`。
/// - `GEMINI_CLI_HOME` 转绝对路径（如已设置）。
pub fn build_gemini_headless_env(env: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let mut next: BTreeMap<String, String> = env.clone();

    let term = env
        .get("TERM")
        .map(|v| v.trim().to_ascii_lowercase())
        .unwrap_or_default();
    if term.is_empty() || term == "dumb" || term == "vt100" {
        next.insert("TERM".to_owned(), "xterm-256color".to_owned());
    }

    let colorterm = env
        .get("COLORTERM")
        .map(|v| v.trim().is_empty())
        .unwrap_or(true);
    if colorterm {
        next.insert("COLORTERM".to_owned(), "truecolor".to_owned());
    }

    next.insert("NO_BROWSER".to_owned(), "1".to_owned());
    next.remove("NO_COLOR");
    next
}

/// 解析 Gemini skills 目录路径。
///
/// Node 等价：`geminiSkillsHome`。返回绝对路径 `<homedir>/.gemini/skills`。
/// 提供 `homedir` 参数以便测试可注入（生产环境传 `std::env::var("HOME")`）。
pub fn gemini_skills_home(homedir: &str) -> String {
    let home_trimmed = homedir.trim_end_matches('/');
    if home_trimmed.is_empty() {
        return "/.gemini/skills".to_owned();
    }
    if home_trimmed == "/" {
        return "/.gemini/skills".to_owned();
    }
    format!("{home_trimmed}/.gemini/skills")
}

/// 通用 `render_paperclip_env_note` / `render_api_access_note`
/// 在 `pc_acpx::session_config_options` 中已有权威实现，这里直接 re-export，
/// 保持 `pc-acapter-gemini-local` API surface 不变。
pub use pc_acpx::session_config_options::{render_api_access_note, render_paperclip_env_note};
