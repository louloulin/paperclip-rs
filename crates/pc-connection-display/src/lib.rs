#![forbid(unsafe_code)]

//! Humanize connection / tool identifiers for prosumer UI surfaces.
//!
//! R529: Direct port of `paperclip/packages/shared/src/humanize-connection.ts`.
//!
//! 设计原则:
//! - 所有 pub fn 都是纯函数 (无 IO, 无副作用)
//! - 接受 `&str` 或 `Option<&HumanizableConnection>`, 返回 `String` 或 `Option<String>`
//! - regex 编译成 `Lazy<Regex>` 一次, 后续零成本
//!
//! 范围 (本 crate):
//! - [`humanize_connection_display_name`] — 主函数, 把 identifier 转成 prosumer-friendly label
//! - [`connection_display_secondary_hint`] — 可选副标题, 仅对网络地址显示 `hosted at …`
//!
//! **不** 范围 (留给集成层):
//! - UI `src/lib/connection-display.ts` (TS 端保留, UI 是冻结契约)
//! - server 端任何 service 集成 (Node 上游这个模块是 pure UI helper, 仅 UI 端调用)
//!
//! Node 上游该模块**只**给 UI `/apps` `/apps/attention` `/apps/advanced` 等页面用,
//! 让 raw identifier (IPs, `Plugin:` 前缀, `vendor:tool` id) 不漏到 prosumer surface.
//! Rust port 保留完全相同语义, 给 pc-server 在生成 `connection_display_name` 字段时用 (避免 UI 重新 derive).

use once_cell::sync::Lazy;
use regex::Regex;

/// Object with a `name` field — the canonical shape of a `Connection` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HumanizableConnection {
    pub name: String,
}

/// Match a bare IPv4 address with optional `:port`.
/// Mirrors Node upstream: `^\d{1,3}(\.\d{1,3}){3}(:\d+)?$`
static IPV4_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^\d{1,3}(\.\d{1,3}){3}(:\d+)?$").expect("valid regex pattern"));

/// Match `host:port` (no scheme).
/// Mirrors Node upstream: `^[a-z0-9.-]+:\d+$`
static HOST_PORT_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[a-z0-9.-]+:\d+$").expect("valid regex pattern"));

/// Optional overrides for [`humanize_connection_display_name`].
#[derive(Debug, Clone, Default)]
pub struct HumanizeOptions {
    /// A known human title (e.g. catalog entry title) — when set, always wins
    /// over any derivation. Whitespace-only falls back to derivation.
    pub title: Option<String>,
}

/// Options for [`humanize_connection_display_name`].
impl HumanizeOptions {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_title<T: Into<String>>(mut self, title: T) -> Self {
        self.title = Some(title.into());
        self
    }
}

/// Extract the raw identifier name from any input shape.
///
/// Mirrors Node upstream `rawNameOf`:
/// - string → trimmed
/// - object → `name` field, trimmed (default to empty)
/// - null / undefined → empty
fn raw_name_of(input: ConnectionInput<'_>) -> String {
    match input {
        ConnectionInput::Raw(s) => s.trim().to_string(),
        ConnectionInput::Object(conn) => conn.name.trim().to_string(),
        ConnectionInput::None => String::new(),
    }
}

/// Tagged union mirroring Node's overloaded function signature.
#[derive(Debug, Clone, Copy)]
pub enum ConnectionInput<'a> {
    Raw(&'a str),
    Object(&'a HumanizableConnection),
    None,
}

/// True if `raw` reads as a network address (IP, host:port, URL, localhost).
///
/// Mirrors Node upstream `looksLikeNetworkAddress`.
fn looks_like_network_address(raw: &str) -> bool {
    let v = raw.to_lowercase();
    if v.contains("://") {
        return true; // any URL
    }
    if v == "localhost" || v.starts_with("localhost:") {
        return true;
    }
    if IPV4_RE.is_match(&v) {
        return true; // IPv4 (optional :port)
    }
    if HOST_PORT_RE.is_match(&v) {
        return true; // host:port
    }
    false
}

/// Title-case a snake/kebab/dotted identifier: `update_note` → `Update Note`.
///
/// Mirrors Node upstream `titleCaseIdentifier`.
fn title_case_identifier(value: &str) -> String {
    value
        .split(|c: char| c.is_whitespace() || c == '_' || c == '-' || c == '.')
        .filter(|s| !s.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            let first = chars
                .next()
                .map(|c| c.to_uppercase().to_string())
                .unwrap_or_default();
            format!("{first}{}", chars.as_str().to_lowercase())
        })
        .collect::<Vec<String>>()
        .join(" ")
}

/// Extract a friendly plugin package label from `Plugin: vendor.plugin-leaf`.
///
/// Mirrors Node upstream `pluginPackageLabel`.
/// Returns `None` if `raw` is not a `Plugin: …` label.
fn plugin_package_label(raw: &str) -> Option<String> {
    // Case-insensitive `Plugin:` prefix
    let lower = raw.to_lowercase();
    if !lower.starts_with("plugin:") {
        return None;
    }
    let mut leaf = raw[7..].trim().to_string();
    // Keep the package leaf only: `paperclipai.plugin-briefs` → `plugin-briefs`.
    if let Some(idx) = leaf.rfind('.') {
        leaf = leaf[idx + 1..].to_string();
    }
    // Drop the `plugin-` / `plugin_` scaffolding leftover.
    if leaf.len() > 7
        && (leaf[..7].eq_ignore_ascii_case("plugin-") || leaf[..7].eq_ignore_ascii_case("plugin_"))
    {
        leaf = leaf[7..].to_string();
    } else if leaf.len() > 7 && leaf.to_lowercase().starts_with("plugin") {
        // Shorter forms like `pluginX` — fallback: keep as-is
    }
    let titled = title_case_identifier(&leaf);
    if titled.is_empty() {
        Some("Custom app".to_string())
    } else {
        Some(titled)
    }
}

/// Turn an app/connection identifier (or a tool id) into a prosumer-friendly label.
///
/// Pass `options.title` (e.g. a catalog entry's `title`) to prefer a known human
/// title over any derivation.
///
/// Examples:
/// - `127.0.0.1` → `"Custom app"`
/// - `Plugin: paperclipai.plugin-briefs` → `"Briefs"`
/// - `mcp-remote-fixture:update_note` → `"Update Note"`
/// - `Zapier` / `Notion` → unchanged
#[must_use]
pub fn humanize_connection_display_name(
    input: ConnectionInput<'_>,
    options: &HumanizeOptions,
) -> String {
    // Explicit title always wins (when non-blank).
    if let Some(t) = &options.title {
        let trimmed = t.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }

    let raw = raw_name_of(input);
    if raw.is_empty() {
        return "Custom app".to_string();
    }

    if looks_like_network_address(&raw) {
        return "Custom app".to_string();
    }

    if let Some(label) = plugin_package_label(&raw) {
        return label;
    }

    // `vendor:tool` id (e.g. `mcp-remote-fixture:update_note`) → tool segment.
    if raw.contains(':') && !raw.contains("://") {
        if let Some(idx) = raw.rfind(':') {
            let tool = raw[idx + 1..].trim();
            if !tool.is_empty() {
                return title_case_identifier(tool);
            }
        }
    }

    // Already human (whitespace or capital) → pass through untouched.
    if raw.chars().any(|c| c.is_whitespace()) || raw.chars().any(|c| c.is_uppercase()) {
        return raw;
    }

    // Bare snake/kebab/dotted identifier → Title Case With Spaces.
    if raw.chars().any(|c| c == '_' || c == '-' || c == '.') {
        return title_case_identifier(&raw);
    }

    raw
}

/// Optional secondary line for the App-detail page: when the raw name is a
/// network address we hide it from the header but may still show `hosted at …`
/// underneath as a small trust/clarity hint. Returns `None` when nothing
/// worth surfacing.
#[must_use]
pub fn connection_display_secondary_hint(input: ConnectionInput<'_>) -> Option<String> {
    let raw = raw_name_of(input);
    if raw.is_empty() {
        return None;
    }
    if looks_like_network_address(&raw) {
        Some(format!("hosted at {raw}"))
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Convenience wrappers for the most common single-argument call sites.
// ---------------------------------------------------------------------------

/// Convenience wrapper for the raw-string form.
#[must_use]
pub fn humanize_connection_display_name_str(raw: &str, options: &HumanizeOptions) -> String {
    humanize_connection_display_name(ConnectionInput::Raw(raw), options)
}

/// Convenience wrapper for the object form.
#[must_use]
pub fn humanize_connection_display_name_obj(
    conn: &HumanizableConnection,
    options: &HumanizeOptions,
) -> String {
    humanize_connection_display_name(ConnectionInput::Object(conn), options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r529_hides_raw_ips_and_hosts_behind_generic_label() {
        let opts = HumanizeOptions::new();
        assert_eq!(
            humanize_connection_display_name_str("127.0.0.1", &opts),
            "Custom app"
        );
        assert_eq!(
            humanize_connection_display_name_str("127.0.0.1:8931", &opts),
            "Custom app"
        );
        assert_eq!(
            humanize_connection_display_name_str("localhost", &opts),
            "Custom app"
        );
        assert_eq!(
            humanize_connection_display_name_str("example.com:8080", &opts),
            "Custom app"
        );
        assert_eq!(
            humanize_connection_display_name_str("https://mcp.example.com/sse", &opts),
            "Custom app"
        );
    }

    #[test]
    fn r529_drops_plugin_prefix_and_title_cases_leaf() {
        let opts = HumanizeOptions::new();
        assert_eq!(
            humanize_connection_display_name_str("Plugin: paperclipai.plugin-briefs", &opts),
            "Briefs"
        );
        assert_eq!(
            humanize_connection_display_name_str("Plugin: acme.plugin-weekly-report", &opts),
            "Weekly Report"
        );
    }

    #[test]
    fn r529_vendor_tool_ids_become_title_case() {
        let opts = HumanizeOptions::new();
        assert_eq!(
            humanize_connection_display_name_str("mcp-remote-fixture:update_note", &opts),
            "Update Note"
        );
        assert_eq!(
            humanize_connection_display_name_str("github:create_issue", &opts),
            "Create Issue"
        );
    }

    #[test]
    fn r529_title_cases_bare_snake_kebab_identifier() {
        let opts = HumanizeOptions::new();
        assert_eq!(
            humanize_connection_display_name_str("update_note", &opts),
            "Update Note"
        );
        assert_eq!(
            humanize_connection_display_name_str("send-email", &opts),
            "Send Email"
        );
    }

    #[test]
    fn r529_passes_through_normal_human_app_names() {
        let opts = HumanizeOptions::new();
        assert_eq!(
            humanize_connection_display_name_str("Zapier", &opts),
            "Zapier"
        );
        assert_eq!(
            humanize_connection_display_name_str("Notion", &opts),
            "Notion"
        );
        assert_eq!(
            humanize_connection_display_name_str("Google Drive", &opts),
            "Google Drive"
        );
    }

    #[test]
    fn r529_prefers_explicit_title_when_provided() {
        let opts = HumanizeOptions::new().with_title("Update note");
        assert_eq!(
            humanize_connection_display_name_str("mcp-remote-fixture:update_note", &opts),
            "Update note"
        );
    }

    #[test]
    fn r529_blank_title_falls_back_to_derivation() {
        let opts = HumanizeOptions::new().with_title("   ");
        assert_eq!(
            humanize_connection_display_name_str("update_note", &opts),
            "Update Note"
        );
    }

    #[test]
    fn r529_accepts_connection_like_object_and_handles_empty() {
        let opts = HumanizeOptions::new();
        let conn = HumanizableConnection {
            name: "Plugin: acme.plugin-briefs".to_string(),
        };
        assert_eq!(humanize_connection_display_name_obj(&conn, &opts), "Briefs");
        assert_eq!(
            humanize_connection_display_name_str("", &opts),
            "Custom app"
        );
        assert_eq!(
            humanize_connection_display_name_str("  ", &opts),
            "Custom app"
        );
    }

    #[test]
    fn r529_handles_dotted_identifier() {
        let opts = HumanizeOptions::new();
        assert_eq!(
            humanize_connection_display_name_str("paperclip.briefs", &opts),
            "Paperclip Briefs"
        );
        assert_eq!(
            humanize_connection_display_name_str("notion.database", &opts),
            "Notion Database"
        );
    }

    #[test]
    fn r529_handles_uppercase_plugin_prefix() {
        // Plugin label is case-insensitive (PLUGIN: also matches)
        let opts = HumanizeOptions::new();
        assert_eq!(
            humanize_connection_display_name_str("PLUGIN: acme.plugin-briefs", &opts),
            "Briefs"
        );
    }

    #[test]
    fn r529_handles_plugin_with_underscore_separator() {
        let opts = HumanizeOptions::new();
        assert_eq!(
            humanize_connection_display_name_str("Plugin: acme.plugin_weekly-report", &opts),
            "Weekly Report"
        );
    }

    #[test]
    fn r529_handles_plugin_with_no_dotted_package() {
        // Plugin: Briefs (no dot in package name)
        let opts = HumanizeOptions::new();
        assert_eq!(
            humanize_connection_display_name_str("Plugin: plugin-briefs", &opts),
            "Briefs"
        );
    }

    #[test]
    fn r529_secondary_hint_for_network_addresses() {
        assert_eq!(
            connection_display_secondary_hint(ConnectionInput::Raw("127.0.0.1")),
            Some("hosted at 127.0.0.1".to_string())
        );
        assert_eq!(
            connection_display_secondary_hint(ConnectionInput::Raw("127.0.0.1:8931")),
            Some("hosted at 127.0.0.1:8931".to_string())
        );
    }

    #[test]
    fn r529_secondary_hint_null_for_non_network() {
        let conn = HumanizableConnection {
            name: "Zapier".to_string(),
        };
        assert_eq!(
            connection_display_secondary_hint(ConnectionInput::Object(&conn)),
            None
        );
        assert_eq!(
            connection_display_secondary_hint(ConnectionInput::Raw("Plugin: acme.plugin-briefs")),
            None
        );
        assert_eq!(
            connection_display_secondary_hint(ConnectionInput::Raw("")),
            None
        );
    }

    #[test]
    fn r529_secondary_hint_with_object_form() {
        let conn = HumanizableConnection {
            name: "https://example.com:443".to_string(),
        };
        assert_eq!(
            connection_display_secondary_hint(ConnectionInput::Object(&conn)),
            Some("hosted at https://example.com:443".to_string())
        );
    }

    #[test]
    fn r529_title_case_identifier_handles_mixed_input() {
        assert_eq!(title_case_identifier("update_note"), "Update Note");
        assert_eq!(title_case_identifier("send-email"), "Send Email");
        assert_eq!(
            title_case_identifier("paperclip.briefs"),
            "Paperclip Briefs"
        );
        assert_eq!(
            title_case_identifier("create_issue-event"),
            "Create Issue Event"
        );
        assert_eq!(title_case_identifier(""), "");
        assert_eq!(title_case_identifier("---"), "");
        assert_eq!(title_case_identifier("single"), "Single");
    }

    #[test]
    fn r529_looks_like_network_address_edge_cases() {
        // 4-segment but not all 1-3 digits
        assert!(looks_like_network_address("999.999.999.999"));
        assert!(!looks_like_network_address("999.999.999")); // only 3 segments
        assert!(!looks_like_network_address("1.2.3")); // only 3 segments
        assert!(!looks_like_network_address("abc.def.ghi.jkl")); // not digits
                                                                 // host:port requires digits after colon
        assert!(!looks_like_network_address("example.com")); // no port
        assert!(looks_like_network_address("example.com:443"));
    }

    #[test]
    fn r529_none_input_returns_custom_app() {
        // No input at all
        let opts = HumanizeOptions::new();
        assert_eq!(
            humanize_connection_display_name(ConnectionInput::None, &opts),
            "Custom app"
        );
    }
}
