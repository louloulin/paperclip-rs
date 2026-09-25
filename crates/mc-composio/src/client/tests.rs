//! `client` 的单元测试（从 `client.rs` 拆出：门 ⑩ 的单文件 800 行硬上限）。

use crate::client::*;

#[test]
fn client_debug_is_redacted_and_base_is_overridable() {
    let client = ComposioClient::new(Some("ak_live_secret".into()));
    assert!(client.enabled());
    let rendered = format!("{client:?}");
    assert!(!rendered.contains("ak_live_secret"));
    assert_eq!(
        client.with_api_base("http://127.0.0.1:9").api_base(),
        "http://127.0.0.1:9"
    );
    assert!(!ComposioClient::new(None).enabled());
}

#[test]
fn default_base_matches_the_upstream_sdk() {
    assert_eq!(DEFAULT_API_BASE, "https://backend.composio.dev/api/v3.1");
    assert_eq!(DEFAULT_USER_AGENT, "multica-rs-composio/0.1");
    assert_eq!(ComposioClient::new(None).api_base(), DEFAULT_API_BASE);
    assert_eq!(DEFAULT_TIMEOUT_SECS, 30);
}

#[test]
fn urls_join_without_double_slashes() {
    let client = ComposioClient::new(None).with_api_base("http://127.0.0.1:9/");
    assert_eq!(client.url("/toolkits"), "http://127.0.0.1:9/toolkits");
    assert_eq!(toolkit_path(""), "/toolkits?limit=1000&sort_by=usage");
    assert_eq!(
        toolkit_path("cur sor"),
        "/toolkits?limit=1000&sort_by=usage&cursor=cur%20sor"
    );
    assert_eq!(auth_config_path(""), "/auth_configs?limit=1000");
    assert_eq!(auth_config_path("c1"), "/auth_configs?limit=1000&cursor=c1");
}

#[test]
fn percent_encoding_matches_query_escape() {
    assert_eq!(percent_encode("ca_abc-1.2~3"), "ca_abc-1.2~3");
    assert_eq!(percent_encode("a/b+c=d&e"), "a%2Fb%2Bc%3Dd%26e");
}

#[test]
fn auth_headers_require_a_key_and_carry_it_only_there() {
    let client = ComposioClient::new(Some("ak_1".into()));
    assert_eq!(
        client.auth_headers().expect("headers"),
        vec![("x-api-key".to_string(), "ak_1".to_string())]
    );
    assert_eq!(
        ComposioClient::new(None).auth_headers(),
        Err(ComposioError::NotConfigured)
    );
}

#[test]
fn check_status_maps_401_and_others_without_echoing_anything() {
    assert!(check_status(reqwest::StatusCode::OK, "ctx").is_ok());
    assert_eq!(
        check_status(reqwest::StatusCode::UNAUTHORIZED, "ctx"),
        Err(ComposioError::Unauthorized)
    );
    assert_eq!(
        check_status(reqwest::StatusCode::TOO_MANY_REQUESTS, "ctx"),
        Err(ComposioError::Upstream {
            status: 429,
            context: "ctx".to_string()
        })
    );
    // 错误值的 Display 只带状态与调用点，不带 body / 头。
    let rendered = ComposioError::Upstream {
        status: 500,
        context: "list toolkits".to_string(),
    }
    .to_string();
    assert!(rendered.contains("500"));
    assert!(!rendered.to_lowercase().contains("api"));
}

#[test]
fn wire_shapes_tolerate_missing_fields() {
    let page: ToolkitPage = serde_json::from_str("{}").expect("empty page");
    assert!(page.items.is_empty() && page.next_cursor.is_empty());
    let page: AuthConfigPage =
        serde_json::from_str(r#"{"items":[{"id":"ac_1","toolkit":{"slug":"notion"}}]}"#)
            .expect("page");
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].toolkit.slug, "notion");
    assert!(!page.items[0].is_composio_managed);
    let account: ConnectedAccountWire = serde_json::from_str(
        r#"{"id":"ca_1","user_id":"u","auth_config":{"id":"ac_1"},"toolkit":{"slug":"notion"}}"#,
    )
    .expect("account");
    assert_eq!(account.auth_config.id, "ac_1");
    assert_eq!(account.toolkit.slug, "notion");
    let session: SessionResponse =
        serde_json::from_str(r#"{"session_id":"s","mcp":{"type":"http","url":"https://m"}}"#)
            .expect("session");
    assert_eq!(session.mcp.kind, "http");
}

#[test]
fn link_request_omits_an_empty_callback_url() {
    let body = serde_json::to_value(LinkRequest {
        auth_config_id: "ac_1",
        user_id: "u1",
        callback_url: "",
    })
    .expect("json");
    assert_eq!(body["auth_config_id"], serde_json::json!("ac_1"));
    assert!(body.get("callback_url").is_none(), "空回调地址不发");
    let body = serde_json::to_value(LinkRequest {
        auth_config_id: "ac_1",
        user_id: "u1",
        callback_url: "https://cb",
    })
    .expect("json");
    assert_eq!(body["callback_url"], serde_json::json!("https://cb"));
}

#[test]
fn session_request_pins_accounts_per_toolkit_and_enables_the_same_slugs() {
    let pinned = BTreeMap::from([
        ("notion".to_string(), vec!["ca_1".to_string()]),
        ("gmail".to_string(), vec!["ca_2".to_string()]),
    ]);
    let body = serde_json::to_value(SessionRequest {
        user_id: "u1",
        toolkits: SessionToolkits {
            enable: &["gmail".to_string(), "notion".to_string()],
        },
        connected_accounts: pinned,
    })
    .expect("json");
    assert_eq!(body["user_id"], serde_json::json!("u1"));
    assert_eq!(
        body["toolkits"]["enable"],
        serde_json::json!(["gmail", "notion"])
    );
    assert_eq!(
        body["connected_accounts"]["notion"],
        serde_json::json!(["ca_1"])
    );
}
