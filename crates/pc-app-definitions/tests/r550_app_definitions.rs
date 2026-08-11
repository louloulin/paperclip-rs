//! R550 — pc-app-definitions 综合测试。

#![allow(clippy::doc_markdown)]

use pc_app_definitions::{
    connectable_app_definitions, connectable_app_slugs, credential_config_path,
    default_ownership_availability, filter_app_catalog_by_slugs, find_app_definition_by_slug,
    find_app_definition_for_url, get_app_definition_for_url, get_available_connection_method,
    get_connectable_app_definition, recommended_defaults_for_app, AppBranding, AppCategory,
    AppDefinition, ConnectionAuth, FieldDef, FieldType, MethodDefaults, RiskTier,
    ToolConnectionOwnership,
};
use serde_json::json;

fn make_field(key: &str) -> FieldDef {
    FieldDef {
        key: key.into(),
        label: key.into(),
        field_type: FieldType::Password,
        required: true,
        placeholder: Some(format!("Enter {key}")),
        helper_md: None,
        secret: true,
        prefix: None,
        validation: None,
        options: vec![],
    }
}

fn make_method(
    auth: ConnectionAuth,
    risk: RiskTier,
    ownerships: Vec<ToolConnectionOwnership>,
) -> pc_app_definitions::ConnectionMethodDef {
    pc_app_definitions::ConnectionMethodDef {
        key: "default".into(),
        transport: pc_app_definitions::ToolConnectionTransport::Http,
        auth,
        ownership_modes: ownerships,
        when_to_use: "use me".into(),
        defaults: None,
        tenant_fields: vec![],
        extension_fields: vec![],
        credential_fields: vec![make_field("token")],
        key_placement: None,
        guidance_md: String::new(),
        console_links: None,
        warnings: vec![],
        variants: vec![],
        risk_tier: risk,
        required_resource_filters: vec![],
    }
}

fn make_app(
    slug: &str,
    urls: &[&str],
    methods: Vec<pc_app_definitions::ConnectionMethodDef>,
) -> AppDefinition {
    AppDefinition {
        schema_version: 1,
        slug: slug.into(),
        name: slug.into(),
        description: format!("{slug} desc"),
        categories: vec![AppCategory::Developer],
        featured: false,
        branding: AppBranding {
            logo_url: "https://example/logo.png".into(),
            ..Default::default()
        },
        url_patterns: urls.iter().map(std::string::ToString::to_string).collect(),
        docs_url: None,
        methods,
        suggestable: false,
        availability: None,
        ownership_availability: None,
    }
}

#[test]
fn r550_connectable_slugs_match_node() {
    let slugs = connectable_app_slugs();
    for s in [
        "zapier",
        "github",
        "slack",
        "notion",
        "linear",
        "google-sheets",
        "context7",
    ] {
        assert!(slugs.contains(s), "missing {s}");
    }
}

#[test]
fn r550_default_ownership_availability() {
    let d = default_ownership_availability();
    assert!(!d[&ToolConnectionOwnership::PlatformShared]);
    assert!(!d[&ToolConnectionOwnership::PlatformProvisioned]);
    assert!(d[&ToolConnectionOwnership::Customer]);
    assert!(d[&ToolConnectionOwnership::Dcr]);
}

#[test]
fn r550_connectable_filters_catalog() {
    let catalog = vec![
        make_app("zapier", &[], vec![]),
        make_app("not-on-list", &[], vec![]),
        make_app("slack", &[], vec![]),
    ];
    let filtered = connectable_app_definitions(&catalog);
    assert_eq!(filtered.len(), 2);
    assert_eq!(filtered[0].slug, "zapier");
    assert_eq!(filtered[1].slug, "slack");
}

#[test]
fn r550_get_connectable_app_by_slug() {
    let catalog = vec![
        make_app("zapier", &[], vec![]),
        make_app("notion", &[], vec![]),
    ];
    assert!(get_connectable_app_definition("notion", &catalog).is_some());
    assert!(get_connectable_app_definition("missing", &catalog).is_none());
}

#[test]
fn r550_get_app_definition_for_url_exact() {
    let catalog = vec![
        make_app("github", &["https://github.com/*"], vec![]),
        make_app("slack", &["https://*.slack.com/*"], vec![]),
    ];
    let app = get_app_definition_for_url("https://github.com/foo/bar", &catalog).unwrap();
    assert_eq!(app.slug, "github");
    let app = get_app_definition_for_url("https://acme.slack.com/archives", &catalog).unwrap();
    assert_eq!(app.slug, "slack");
}

#[test]
fn r550_get_app_definition_for_url_no_match() {
    let catalog = vec![make_app("github", &["https://github.com/*"], vec![])];
    assert!(get_app_definition_for_url("https://gitlab.com/foo", &catalog).is_none());
}

#[test]
fn r550_get_app_definition_for_url_invalid() {
    let catalog = vec![make_app("github", &["https://github.com/*"], vec![])];
    assert!(get_app_definition_for_url("", &catalog).is_none());
}

#[test]
fn r550_get_app_definition_for_url_normalizes_bare_host() {
    let catalog = vec![make_app("github", &["https://github.com/"], vec![])];
    let app = get_app_definition_for_url("https://github.com", &catalog).unwrap();
    assert_eq!(app.slug, "github");
}

#[test]
fn r550_get_available_method_uses_default_ownership() {
    let methods = vec![make_method(
        ConnectionAuth::ApiKey,
        RiskTier::S2,
        vec![ToolConnectionOwnership::PlatformProvisioned], // disabled by default
    )];
    let app = make_app("x", &[], methods);
    assert!(get_available_connection_method(&app).is_none());
}

#[test]
fn r550_get_available_method_customer_ownership() {
    let methods = vec![make_method(
        ConnectionAuth::ApiKey,
        RiskTier::S2,
        vec![ToolConnectionOwnership::Customer], // enabled by default
    )];
    let app = make_app("x", &[], methods);
    let m = get_available_connection_method(&app).unwrap();
    assert_eq!(m.auth, ConnectionAuth::ApiKey);
}

#[test]
fn r550_get_available_method_per_app_override() {
    let methods = vec![make_method(
        ConnectionAuth::ApiKey,
        RiskTier::S2,
        vec![ToolConnectionOwnership::PlatformShared],
    )];
    let mut app = make_app("x", &[], methods);
    let mut override_map = default_ownership_availability();
    override_map.insert(ToolConnectionOwnership::PlatformShared, true);
    app.ownership_availability = Some(override_map);
    assert!(get_available_connection_method(&app).is_some());
}

#[test]
fn r550_credential_config_path() {
    let f = make_field("apiKey");
    assert_eq!(credential_config_path(&f), "credentials.apiKey");
}

#[test]
fn r550_recommended_defaults_s1_method() {
    let methods = vec![make_method(
        ConnectionAuth::ApiKey,
        RiskTier::S1,
        vec![ToolConnectionOwnership::Customer],
    )];
    let app = make_app("x", &[], methods);
    let defaults = recommended_defaults_for_app(&app);
    assert_eq!(defaults["access"], "all_agents");
    assert_eq!(defaults["askFirstRiskLevels"], json!([]));
}

#[test]
fn r550_recommended_defaults_s2_method() {
    let methods = vec![make_method(
        ConnectionAuth::ApiKey,
        RiskTier::S2,
        vec![ToolConnectionOwnership::Customer],
    )];
    let app = make_app("x", &[], methods);
    let defaults = recommended_defaults_for_app(&app);
    assert_eq!(
        defaults["askFirstRiskLevels"],
        json!(["write", "destructive"])
    );
}

#[test]
fn r550_recommended_defaults_no_method() {
    let app = make_app("x", &[], vec![]);
    let defaults = recommended_defaults_for_app(&app);
    assert_eq!(
        defaults["askFirstRiskLevels"],
        json!(["write", "destructive"])
    );
}

#[test]
fn r550_method_defaults_scopes_hint() {
    let defaults = MethodDefaults {
        server_url: None,
        discovery_url: None,
        service_host: None,
        template_key: None,
        authorization_endpoint: None,
        token_endpoint: None,
        metadata_url: None,
        scopes_hint: vec!["read".into(), "write".into()],
    };
    assert_eq!(defaults.scopes_hint, vec!["read", "write"]);
}

#[test]
fn r550_json_find_by_slug() {
    let catalog = json!([
        { "slug": "zapier" },
        { "slug": "github" },
    ]);
    assert_eq!(
        find_app_definition_by_slug(&catalog, "github").unwrap(),
        &json!({ "slug": "github" })
    );
    assert!(find_app_definition_by_slug(&catalog, "missing").is_none());
}

#[test]
fn r550_json_filter_by_slugs() {
    let catalog = json!([
        { "slug": "zapier" },
        { "slug": "ghost" },
        { "slug": "github" },
    ]);
    let slugs: std::collections::HashSet<&str> = ["zapier", "github"].into_iter().collect();
    let filtered = filter_app_catalog_by_slugs(&catalog, &slugs);
    assert_eq!(filtered.len(), 2);
    assert_eq!(filtered[0]["slug"], "zapier");
    assert_eq!(filtered[1]["slug"], "github");
}

#[test]
fn r550_json_filter_by_slugs_non_array() {
    let catalog = json!({ "not": "an array" });
    let slugs: std::collections::HashSet<&str> = ["zapier"].into_iter().collect();
    assert!(filter_app_catalog_by_slugs(&catalog, &slugs).is_empty());
}

#[test]
fn r550_json_find_for_url() {
    let catalog = json!([
        {
            "slug": "github",
            "urlPatterns": ["https://github.com/*"]
        },
        {
            "slug": "slack",
            "urlPatterns": ["https://*.slack.com/*"]
        }
    ]);
    let app = find_app_definition_for_url(&catalog, "https://github.com/foo/bar").unwrap();
    assert_eq!(app["slug"], "github");
    let app = find_app_definition_for_url(&catalog, "https://acme.slack.com/x").unwrap();
    assert_eq!(app["slug"], "slack");
    assert!(find_app_definition_for_url(&catalog, "https://nope.example/").is_none());
}
