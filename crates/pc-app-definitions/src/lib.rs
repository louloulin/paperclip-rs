#![forbid(unsafe_code)]
#![allow(clippy::doc_markdown)]

//! App definition catalog helpers.
//!
//! R550: Direct port of `paperclip/packages/shared/src/app-definitions.ts`
//! (the runtime helpers — `APP_DEFINITIONS` itself is generated data in the
//! Node codebase and lives in a separate file; in Rust we expose the same
//! helper functions and let the caller inject the catalog).

use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};

// ---------- types ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AppCategory {
    Ai,
    Analytics,
    Commerce,
    Communication,
    Content,
    Data,
    Developer,
    Productivity,
    Other,
}

impl AppCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ai => "ai",
            Self::Analytics => "analytics",
            Self::Commerce => "commerce",
            Self::Communication => "communication",
            Self::Content => "content",
            Self::Data => "data",
            Self::Developer => "developer",
            Self::Productivity => "productivity",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldType {
    Text,
    Password,
    Textarea,
    Datetime,
    Select,
    Checkbox,
}

impl FieldType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Password => "password",
            Self::Textarea => "textarea",
            Self::Datetime => "datetime",
            Self::Select => "select",
            Self::Checkbox => "checkbox",
        }
    }
}

#[derive(Debug, Clone)]
pub struct FieldDef {
    pub key: String,
    pub label: String,
    pub field_type: FieldType,
    pub required: bool,
    pub placeholder: Option<String>,
    pub helper_md: Option<String>,
    pub secret: bool,
    pub prefix: Option<String>,
    pub validation: Option<FieldValidation>,
    pub options: Vec<FieldOption>,
}

#[derive(Debug, Clone)]
pub struct FieldValidation {
    pub pattern: Option<String>,
    pub max_length: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct FieldOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyPlacementLocation {
    Header,
    Query,
    BodyJson,
    Env,
}

impl KeyPlacementLocation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Header => "header",
            Self::Query => "query",
            Self::BodyJson => "body_json",
            Self::Env => "env",
        }
    }
}

#[derive(Debug, Clone)]
pub struct KeyPlacement {
    pub location: KeyPlacementLocation,
    pub name: String,
    pub prefix: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConnectionAuth {
    OAuth,
    ApiKey,
    None,
}

impl ConnectionAuth {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OAuth => "oauth",
            Self::ApiKey => "api_key",
            Self::None => "none",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolConnectionTransport {
    Http,
    LocalProcess,
    Sdk,
    Plugin,
    WebSocket,
    Grpc,
    Embedded,
}

impl ToolConnectionTransport {
    /// Placeholder — actual transport routing lives in `pc-adapter-api`
    /// using JSON helpers. This enum only tags a method's transport category.
    pub fn as_str(self) -> &'static str {
        "http"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RiskTier {
    S1,
    S2,
    S3,
    S4,
}

impl RiskTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::S1 => "S1",
            Self::S2 => "S2",
            Self::S3 => "S3",
            Self::S4 => "S4",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolConnectionOwnership {
    PlatformShared,
    PlatformProvisioned,
    Customer,
    Dcr,
}

impl ToolConnectionOwnership {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PlatformShared => "platform_shared",
            Self::PlatformProvisioned => "platform_provisioned",
            Self::Customer => "customer",
            Self::Dcr => "dcr",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConnectionMethodDef {
    pub key: String,
    pub transport: ToolConnectionTransport,
    pub auth: ConnectionAuth,
    pub ownership_modes: Vec<ToolConnectionOwnership>,
    pub when_to_use: String,
    pub defaults: Option<MethodDefaults>,
    pub tenant_fields: Vec<FieldDef>,
    pub extension_fields: Vec<FieldDef>,
    pub credential_fields: Vec<FieldDef>,
    pub key_placement: Option<KeyPlacement>,
    pub guidance_md: String,
    pub console_links: Option<ConsoleLinks>,
    pub warnings: Vec<String>,
    pub variants: Vec<MethodVariant>,
    pub risk_tier: RiskTier,
    pub required_resource_filters: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct MethodDefaults {
    pub server_url: Option<String>,
    pub discovery_url: Option<String>,
    pub service_host: Option<String>,
    pub template_key: Option<String>,
    pub authorization_endpoint: Option<String>,
    pub token_endpoint: Option<String>,
    pub metadata_url: Option<String>,
    pub scopes_hint: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ConsoleLinks {
    pub register: Option<String>,
    pub keys: Option<String>,
    pub settings: Option<String>,
    pub docs: Option<String>,
}

#[derive(Debug, Clone)]
pub struct MethodVariant {
    pub key: String,
    pub label: String,
    pub when_to_use: String,
    pub tenant_fields: Vec<FieldDef>,
}

#[derive(Debug, Clone, Default)]
pub struct AppBranding {
    pub logo_url: String,
    pub dark_logo_url: Option<String>,
    pub background_color: Option<String>,
    pub accent_color: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AppAvailability {
    pub available: bool,
    pub reason: Option<String>,
    pub robot_email: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AppDefinition {
    pub schema_version: u32,
    pub slug: String,
    pub name: String,
    pub description: String,
    pub categories: Vec<AppCategory>,
    pub featured: bool,
    pub branding: AppBranding,
    pub url_patterns: Vec<String>,
    pub docs_url: Option<String>,
    pub methods: Vec<ConnectionMethodDef>,
    pub suggestable: bool,
    pub availability: Option<AppAvailability>,
    pub ownership_availability: Option<HashMap<ToolConnectionOwnership, bool>>,
}

// ---------- constants ----------

/// Default ownership availability — copied verbatim from
/// `DEFAULT_OWNERSHIP_AVAILABILITY` in `app-definitions.ts`.
pub fn default_ownership_availability() -> HashMap<ToolConnectionOwnership, bool> {
    let mut m = HashMap::with_capacity(4);
    m.insert(ToolConnectionOwnership::PlatformShared, false);
    m.insert(ToolConnectionOwnership::PlatformProvisioned, false);
    m.insert(ToolConnectionOwnership::Customer, true);
    m.insert(ToolConnectionOwnership::Dcr, true);
    m
}

/// Connectable app slugs — the subset of `APP_DEFINITIONS` exposed to UI flows.
/// Mirrors `CONNECTABLE_APP_SLUGS`.
pub fn connectable_app_slugs() -> HashSet<&'static str> {
    [
        "zapier",
        "github",
        "slack",
        "notion",
        "linear",
        "google-sheets",
        "context7",
    ]
    .into_iter()
    .collect()
}

/// Filter a catalog of definitions to only the connectable ones.
pub fn connectable_app_definitions(all: &[AppDefinition]) -> Vec<AppDefinition> {
    let slugs = connectable_app_slugs();
    all.iter()
        .filter(|app| slugs.contains(app.slug.as_str()))
        .cloned()
        .collect()
}

// ---------- helpers ----------

/// Find a connectable app definition by exact slug match.
pub fn get_connectable_app_definition<'a>(
    slug: &str,
    definitions: &'a [AppDefinition],
) -> Option<&'a AppDefinition> {
    definitions.iter().find(|app| app.slug == slug)
}

/// Build a case-insensitive RegExp from a wildcard pattern (only `*` is special).
/// Find the first app whose `urlPatterns` (with `*` wildcards) matches the
/// given URL. Returns `None` if the URL is not parseable.
pub fn get_app_definition_for_url<'a>(
    link: &str,
    definitions: &'a [AppDefinition],
) -> Option<&'a AppDefinition> {
    let normalized = normalize_link(link)?;
    definitions.iter().find(|app| {
        app.url_patterns
            .iter()
            .any(|pattern| match_pattern(pattern, &normalized))
    })
}

fn normalize_link(link: &str) -> Option<String> {
    let trimmed = link.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Node `new URL(link).toString()` always adds a trailing slash for bare
    // hosts. We mirror that by appending `/` only if no path is present.
    if let Some(scheme_end) = trimmed.find("://") {
        let after_scheme = &trimmed[scheme_end + 3..];
        if !after_scheme.contains('/') && !after_scheme.is_empty() {
            return Some(format!("{trimmed}/"));
        }
    }
    Some(trimmed.to_string())
}

fn match_pattern(pattern: &str, value: &str) -> bool {
    // We use a simple `Regex`-like matcher that supports `.*` and literal text,
    // evaluated manually to avoid pulling in the `regex` crate for a 2-line feature.
    match_wildcard(pattern, value)
}

fn match_wildcard(pattern: &str, value: &str) -> bool {
    // Case-insensitive comparison.
    let p = pattern.to_ascii_lowercase();
    let v = value.to_ascii_lowercase();
    wildcard_match_recursive(p.as_bytes(), v.as_bytes())
}

fn wildcard_match_recursive(pattern: &[u8], value: &[u8]) -> bool {
    let mut p_idx = 0;
    let mut v_idx = 0;
    let mut star_idx: Option<usize> = None;
    let mut match_idx: usize = 0;

    while v_idx < value.len() {
        if p_idx < pattern.len() && pattern[p_idx] == b'*' {
            star_idx = Some(p_idx);
            match_idx = v_idx;
            p_idx += 1;
        } else if p_idx < pattern.len() && pattern[p_idx] == value[v_idx] {
            p_idx += 1;
            v_idx += 1;
        } else if let Some(si) = star_idx {
            p_idx = si + 1;
            match_idx += 1;
            v_idx = match_idx;
        } else {
            return false;
        }
    }
    while p_idx < pattern.len() && pattern[p_idx] == b'*' {
        p_idx += 1;
    }
    p_idx == pattern.len()
}

/// Returns the first connection method whose ownership modes are all enabled
/// per the app's `ownershipAvailability` (or the global default).
pub fn get_available_connection_method(app: &AppDefinition) -> Option<&ConnectionMethodDef> {
    let availability = app
        .ownership_availability
        .clone()
        .unwrap_or_else(default_ownership_availability);
    app.methods.iter().find(|method| {
        method
            .ownership_modes
            .iter()
            .any(|ownership| availability.get(ownership).copied().unwrap_or(false))
    })
}

/// Build the canonical config path used to store a credential field value.
pub fn credential_config_path(field: &FieldDef) -> String {
    format!("credentials.{}", field.key)
}

/// Recommended defaults for a freshly provisioned app config.
pub fn recommended_defaults_for_app(app: &AppDefinition) -> Map<String, Value> {
    let method = get_available_connection_method(app);
    let ask_first_risk_levels = match method {
        Some(m) if m.risk_tier == RiskTier::S1 => vec![],
        _ => vec![
            Value::String("write".into()),
            Value::String("destructive".into()),
        ],
    };
    let mut out = Map::new();
    out.insert("access".into(), Value::String("all_agents".into()));
    out.insert(
        "askFirstRiskLevels".into(),
        Value::Array(ask_first_risk_levels),
    );
    out
}

// ---------- JSON helpers (for callers using raw JSON catalogs) ----------

/// Find an app definition in a JSON array of catalog entries by exact slug match.
pub fn find_app_definition_by_slug<'a>(catalog: &'a Value, slug: &str) -> Option<&'a Value> {
    let arr = catalog.as_array()?;
    arr.iter()
        .find(|entry| entry.get("slug").and_then(Value::as_str) == Some(slug))
}

/// Filter a JSON catalog to entries whose slug is in `slugs`.
pub fn filter_app_catalog_by_slugs<S: std::hash::BuildHasher>(
    catalog: &Value,
    slugs: &HashSet<&str, S>,
) -> Vec<Value> {
    let Some(arr) = catalog.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter(|entry| {
            entry
                .get("slug")
                .and_then(Value::as_str)
                .is_some_and(|s| slugs.contains(s))
        })
        .cloned()
        .collect()
}

/// Find a JSON app definition whose url patterns match `link`.
pub fn find_app_definition_for_url<'a>(catalog: &'a Value, link: &str) -> Option<&'a Value> {
    let arr = catalog.as_array()?;
    let normalized = normalize_link(link)?;
    arr.iter().find(|entry| {
        let Some(patterns) = entry.get("urlPatterns").and_then(Value::as_array) else {
            return false;
        };
        patterns.iter().any(|p| {
            p.as_str()
                .is_some_and(|pat| match_pattern(pat, &normalized))
        })
    })
}

#[cfg(test)]
mod internal_tests {
    use super::*;

    #[test]
    fn wildcard_match_exact() {
        assert!(match_pattern(
            "https://example.com/path",
            "https://example.com/path"
        ));
        assert!(!match_pattern(
            "https://example.com/path",
            "https://example.com/other"
        ));
    }

    #[test]
    fn wildcard_match_star() {
        assert!(match_pattern(
            "https://github.com/*",
            "https://github.com/foo/bar"
        ));
        assert!(match_pattern(
            "https://*.slack.com/*",
            "https://acme.slack.com/archives"
        ));
        assert!(!match_pattern(
            "https://github.com/*",
            "https://gitlab.com/foo"
        ));
    }

    #[test]
    fn wildcard_match_case_insensitive() {
        assert!(match_pattern(
            "https://*.SLACK.com/*",
            "https://acme.slack.com/archives"
        ));
    }

    #[test]
    fn normalize_link_appends_slash_for_bare_host() {
        assert_eq!(
            normalize_link("https://example.com"),
            Some("https://example.com/".to_string())
        );
        assert_eq!(
            normalize_link("https://example.com/path"),
            Some("https://example.com/path".to_string())
        );
    }

    #[test]
    fn default_ownership_availability_matches_node() {
        let d = default_ownership_availability();
        assert_eq!(
            d.get(&ToolConnectionOwnership::PlatformShared),
            Some(&false)
        );
        assert_eq!(
            d.get(&ToolConnectionOwnership::PlatformProvisioned),
            Some(&false)
        );
        assert_eq!(d.get(&ToolConnectionOwnership::Customer), Some(&true));
        assert_eq!(d.get(&ToolConnectionOwnership::Dcr), Some(&true));
    }
}
