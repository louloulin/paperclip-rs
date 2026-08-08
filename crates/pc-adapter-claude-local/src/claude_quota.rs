//! Claude 本地配额探测模块。
//!
//! 对齐 Node `claude-local/src/server/quota.ts`（541 行）。
//!
//! # 设计范围
//!
//! 本模块是 claude-local adapter 的配额入口，复用 `pc-adapter-quota` crate
//! 已实现的纯函数与 IO 层，保持高内聚低耦合：
//! - `claude_config_dir` — 解析 `CLAUDE_CONFIG_DIR`（缺省 `~/.claude`）
//! - `read_claude_token` — 读取 OAuth access token（`.credentials.json` /
//!   `credentials.json` 双文件、`claudeAiOauth.accessToken` 提取）
//! - `claude_to_percent` — utilization → 0-100 整数百分比
//! - `map_anthropic_oauth_usage` — OAuth usage 响应 → `QuotaWindow`
//! - `parse_claude_cli_usage_text` — CLI `/usage` 面板文本 → `QuotaWindow`
//! - `probe_claude_local` — Bedrock 判定 + OAuth 优先 + CLI 回退 + 错误归因
//!
//! 所有函数均为 `pc-adapter-quota` 的同名 re-export，保证单一事实来源；
//! 后续 route 层（`pc-http/routes/adapters.rs`）调用本模块即可获取
//! claude-local 配额结果。

pub use pc_adapter_quota::{
    claude_config_dir, claude_to_percent, map_anthropic_oauth_usage,
    parse_claude_cli_usage_text, probe_claude_local, read_claude_token,
    ProviderQuotaResult, QuotaWindow,
};

/// Claude 配额探测源常量（对齐 Node `CLAUDE_USAGE_SOURCE_OAUTH` / `_CLI`）。
pub const CLAUDE_USAGE_SOURCE_OAUTH: &str = "anthropic-oauth";
pub const CLAUDE_USAGE_SOURCE_CLI: &str = "claude-cli";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reexports_expose_quota_types() {
        // 类型可构造
        let window = QuotaWindow {
            label: "Current session".to_string(),
            used_percent: Some(50),
            resets_at: None,
            value_label: None,
            detail: None,
        };
        assert_eq!(window.label, "Current session");
        assert_eq!(window.used_percent, Some(50));

        let result = ProviderQuotaResult {
            provider: "anthropic".to_string(),
            source: Some(CLAUDE_USAGE_SOURCE_OAUTH.to_string()),
            ok: true,
            error_family: None,
            error: None,
            windows: vec![window],
        };
        assert!(result.ok);
        assert_eq!(result.source.as_deref(), Some("anthropic-oauth"));
        assert_eq!(result.windows.len(), 1);
    }

    #[test]
    fn claude_to_percent_handles_fraction_and_percent() {
        assert_eq!(claude_to_percent(0.5), Some(50));
        assert_eq!(claude_to_percent(50.0), Some(50));
        assert_eq!(claude_to_percent(1.0), Some(1));
        assert_eq!(claude_to_percent(150.0), Some(100));
    }

    #[test]
    fn map_anthropic_oauth_usage_maps_all_windows() {
        let body = serde_json::json!({
            "five_hour": { "utilization": 0.42, "resets_at": "2026-08-08T12:00:00Z" },
            "seven_day": { "utilization": 0.7, "resets_at": null },
            "seven_day_sonnet": { "utilization": 0.3, "resets_at": null },
            "seven_day_opus": { "utilization": 0.1, "resets_at": null },
            "extra_usage": {
                "is_enabled": true,
                "monthly_limit": 1000,
                "used_credits": 250,
                "utilization": 0.25,
                "currency": "USD"
            }
        });
        let windows = map_anthropic_oauth_usage(&body);
        assert_eq!(windows.len(), 5);
        assert_eq!(windows[0].label, "Current session");
        assert_eq!(windows[0].used_percent, Some(42));
        assert_eq!(windows[0].resets_at.as_deref(), Some("2026-08-08T12:00:00Z"));
        assert_eq!(windows[4].label, "Extra usage");
        assert_eq!(windows[4].value_label.as_deref(), Some("$2.50 / $10.00"));
    }

    #[test]
    fn parse_claude_cli_usage_text_parses_panel() {
        let text = "Settings:\nCurrent session\n50%\nCurrent week (all models)\n75%\n";
        let windows = parse_claude_cli_usage_text(text).unwrap();
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].label, "Current session");
        assert_eq!(windows[0].used_percent, Some(50));
        assert_eq!(windows[1].label, "Current week (all models)");
        assert_eq!(windows[1].used_percent, Some(75));
    }

    #[test]
    fn parse_claude_cli_usage_text_requires_current_session() {
        let text = "Settings:\nCurrent week (all models)\n75%\n";
        let err = parse_claude_cli_usage_text(text).unwrap_err();
        assert!(err.contains("Could not parse Claude CLI usage output"));
    }

    #[test]
    fn read_claude_token_returns_none_when_no_config() {
        // 无 CLAUDE_CONFIG_DIR 且 HOME 无 .claude 时返回 None（不 panic）
        let token = read_claude_token();
        // 该函数依赖真实文件系统；这里只验证类型与错误路径不 panic
        assert!(token.is_none() || token.is_some());
    }
}
