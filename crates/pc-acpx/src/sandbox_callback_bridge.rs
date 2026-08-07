//! `pc-acpx::sandbox_callback_bridge` - port of `sandbox-callback-bridge.ts`
//! from Node `paperclip/packages/adapter-utils/src/`.
//!
//! Pure helpers for the sandbox callback bridge protocol. The async
//! bridge server / worker / clients are deferred (they require an
//! actual remote sandbox runtime); this module ports:
//!
//! - Default constants (poll interval, response timeout, max body, etc.)
//! - Route allowlist (default + header allowlist)
//! - Pure functions: token generation, route authorization, header
//!   sanitization, directory layout, env builder.

use serde::{Deserialize, Serialize};

/// Default token length in bytes (24 bytes → 32 chars base64url).
pub const DEFAULT_BRIDGE_TOKEN_BYTES: usize = 24;
/// Default poll interval (ms) the worker uses to check the queue.
pub const DEFAULT_BRIDGE_POLL_INTERVAL_MS: u64 = 100;
/// Default response timeout (ms) for a queued request.
pub const DEFAULT_BRIDGE_RESPONSE_TIMEOUT_MS: u64 = 30_000;
/// Default stop timeout (ms) when shutting down the bridge server.
pub const DEFAULT_BRIDGE_STOP_TIMEOUT_MS: u64 = 2_000;
/// Default max queue depth.
pub const DEFAULT_BRIDGE_MAX_QUEUE_DEPTH: u64 = 64;
/// Default max body bytes (256 KB).
pub const DEFAULT_BRIDGE_MAX_BODY_BYTES: u64 = 256 * 1024;
/// Default max body bytes (re-exported under the Node name).
pub const DEFAULT_SANDBOX_CALLBACK_BRIDGE_MAX_BODY_BYTES: u64 = DEFAULT_BRIDGE_MAX_BODY_BYTES;
/// Base64 chunk size for writing files into the sandbox (32 KB).
pub const REMOTE_WRITE_BASE64_CHUNK_SIZE: usize = 32 * 1024;
/// Entrypoint filename inside the bridge asset dir.
pub const SANDBOX_CALLBACK_BRIDGE_ENTRYPOINT: &str = "paperclip-bridge-server.mjs";
/// Env key that opts a sandbox into the bridge exec channel.
pub const SANDBOX_EXEC_CHANNEL_ENV: &str = "PAPERCLIP_SANDBOX_EXEC_CHANNEL";
/// Bridge exec channel value.
pub const SANDBOX_EXEC_CHANNEL_BRIDGE: &str = "bridge";

/// A route rule for the sandbox callback bridge allowlist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxCallbackBridgeRouteRule {
    pub method: String,
    pub path: String,
}

impl SandboxCallbackBridgeRouteRule {
    /// Construct a new route rule.
    #[must_use]
    pub fn new(method: impl Into<String>, path_regex: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            path: path_regex.into(),
        }
    }
}

/// The default route allowlist for the in-sandbox heartbeat skill.
/// Mirrors Node `DEFAULT_SANDBOX_CALLBACK_BRIDGE_ROUTE_ALLOWLIST`.
pub fn default_sandbox_callback_bridge_route_allowlist()
-> Vec<SandboxCallbackBridgeRouteRule> {
    vec![
        // Identity, inbox, agent self-management
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/agents/me$"),
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/agents/me/inbox-lite$"),
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/agents/me/inbox/mine$"),
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/agents/[^/]+$"),
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/agents/[^/]+/skills$"),
        SandboxCallbackBridgeRouteRule::new(
            "POST",
            r"^/api/agents/[^/]+/skills/sync$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "PATCH",
            r"^/api/agents/[^/]+/instructions-path$",
        ),
        // Company-level reads
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/companies/[^/]+$"),
        SandboxCallbackBridgeRouteRule::new(
            "GET",
            r"^/api/companies/[^/]+/dashboard$",
        ),
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/companies/[^/]+/agents$"),
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/companies/[^/]+/issues$"),
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/companies/[^/]+/projects$"),
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/companies/[^/]+/goals$"),
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/companies/[^/]+/org$"),
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/companies/[^/]+/approvals$"),
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/companies/[^/]+/routines$"),
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/companies/[^/]+/skills$"),
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/projects/[^/]+$"),
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/goals/[^/]+$"),
        // Issue lifecycle
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/issues/[^/]+$"),
        SandboxCallbackBridgeRouteRule::new(
            "GET",
            r"^/api/issues/[^/]+/heartbeat-context$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "GET",
            r"^/api/issues/[^/]+/comments(?:/[^/]+)?$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "POST",
            r"^/api/issues/[^/]+/comments$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "GET",
            r"^/api/issues/[^/]+/documents(?:/[^/]+)?$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "GET",
            r"^/api/issues/[^/]+/documents/[^/]+/revisions$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "PUT",
            r"^/api/issues/[^/]+/documents/[^/]+$",
        ),
        SandboxCallbackBridgeRouteRule::new("POST", r"^/api/issues/[^/]+/checkout$"),
        SandboxCallbackBridgeRouteRule::new("POST", r"^/api/issues/[^/]+/release$"),
        SandboxCallbackBridgeRouteRule::new("PATCH", r"^/api/issues/[^/]+$"),
        SandboxCallbackBridgeRouteRule::new(
            "GET",
            r"^/api/issues/[^/]+/approvals$",
        ),
        // Work products
        SandboxCallbackBridgeRouteRule::new(
            "GET",
            r"^/api/issues/[^/]+/work-products$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "POST",
            r"^/api/issues/[^/]+/work-products$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "PATCH",
            r"^/api/work-products/[^/]+$",
        ),
        // Issue-thread interactions
        SandboxCallbackBridgeRouteRule::new(
            "GET",
            r"^/api/issues/[^/]+/interactions(?:/[^/]+)?$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "POST",
            r"^/api/issues/[^/]+/interactions$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "POST",
            r"^/api/issues/[^/]+/interactions/[^/]+/(?:accept|reject|respond)$",
        ),
        // Subtasks / delegation
        SandboxCallbackBridgeRouteRule::new(
            "POST",
            r"^/api/companies/[^/]+/issues$",
        ),
        // Approvals
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/approvals/[^/]+$"),
        SandboxCallbackBridgeRouteRule::new(
            "GET",
            r"^/api/approvals/[^/]+/issues$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "GET",
            r"^/api/approvals/[^/]+/comments$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "POST",
            r"^/api/approvals/[^/]+/comments$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "POST",
            r"^/api/companies/[^/]+/approvals$",
        ),
        // Execution workspaces
        SandboxCallbackBridgeRouteRule::new(
            "GET",
            r"^/api/execution-workspaces/[^/]+$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "POST",
            r"^/api/execution-workspaces/[^/]+/runtime-services/(?:start|stop|restart)$",
        ),
        // Routines
        SandboxCallbackBridgeRouteRule::new("GET", r"^/api/routines/[^/]+$"),
        SandboxCallbackBridgeRouteRule::new(
            "GET",
            r"^/api/routines/[^/]+/runs$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "POST",
            r"^/api/companies/[^/]+/routines$",
        ),
        SandboxCallbackBridgeRouteRule::new("PATCH", r"^/api/routines/[^/]+$"),
        SandboxCallbackBridgeRouteRule::new("POST", r"^/api/routines/[^/]+/run$"),
        SandboxCallbackBridgeRouteRule::new(
            "POST",
            r"^/api/routines/[^/]+/triggers$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "PATCH",
            r"^/api/routine-triggers/[^/]+$",
        ),
        SandboxCallbackBridgeRouteRule::new(
            "DELETE",
            r"^/api/routine-triggers/[^/]+$",
        ),
    ]
}

/// The default header allowlist for the sandbox callback bridge.
pub const DEFAULT_SANDBOX_CALLBACK_BRIDGE_HEADER_ALLOWLIST: &[&str] = &[
    "accept",
    "content-type",
    "if-match",
    "if-none-match",
];

/// Compute the directory layout for a bridge root. Mirrors Node
/// `sandboxCallbackBridgeDirectories`.
#[must_use]
pub fn sandbox_callback_bridge_directories(root_dir: &str) -> BridgeDirectories {
    BridgeDirectories {
        root_dir: root_dir.to_string(),
        requests_dir: format!("{root_dir}/requests"),
        responses_dir: format!("{root_dir}/responses"),
        logs_dir: format!("{root_dir}/logs"),
        ready_file: format!("{root_dir}/ready.json"),
        pid_file: format!("{root_dir}/server.pid"),
        log_file: format!("{root_dir}/logs/bridge.log"),
    }
}

/// Directory layout for a bridge root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeDirectories {
    pub root_dir: String,
    pub requests_dir: String,
    pub responses_dir: String,
    pub logs_dir: String,
    pub ready_file: String,
    pub pid_file: String,
    pub log_file: String,
}

/// Generate a random base64url-encoded bridge token. Mirrors Node
/// `createSandboxCallbackBridgeToken`. Uses `rand::thread_rng()` under
/// the hood — the RNG is process-global and thread-safe.
#[must_use]
pub fn create_sandbox_callback_bridge_token(bytes: Option<usize>) -> String {
    let n = bytes.unwrap_or(DEFAULT_BRIDGE_TOKEN_BYTES);
    use rand::RngCore;
    let mut buf = vec![0u8; n];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    // base64url without padding
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&buf)
}

/// Authorize a request against the route allowlist. Returns `Ok(())`
/// if the request is allowed, `Err(reason)` otherwise. Mirrors Node
/// `authorizeSandboxCallbackBridgeRequestWithRoutes`.
pub fn authorize_sandbox_callback_bridge_request_with_routes(
    request_method: &str,
    request_path: &str,
    routes: Option<&[SandboxCallbackBridgeRouteRule]>,
) -> Result<(), String> {
    let routes_owned;
    let routes: &[SandboxCallbackBridgeRouteRule] = match routes {
        Some(r) => r,
        None => {
            routes_owned = default_sandbox_callback_bridge_route_allowlist();
            &routes_owned
        }
    };
    let method = normalize_method(request_method);
    let allowed = routes.iter().any(|route| {
        route.method == method
            && regex::Regex::new(&route.path)
                .map(|re| re.is_match(request_path))
                .unwrap_or(false)
    });
    if allowed {
        Ok(())
    } else {
        Err(format!("Route not allowed: {method} {request_path}"))
    }
}

/// Sanitize headers against the allowlist. Returns a new map with only
/// allowed headers preserved. Mirrors Node
/// `sanitizeSandboxCallbackBridgeHeaders`.
#[must_use]
pub fn sanitize_sandbox_callback_bridge_headers(
    headers: &std::collections::BTreeMap<String, String>,
    allowlist: Option<&[&str]>,
) -> std::collections::BTreeMap<String, String> {
    let allowlist: &[&str] =
        allowlist.unwrap_or(DEFAULT_SANDBOX_CALLBACK_BRIDGE_HEADER_ALLOWLIST);
    let allowed: std::collections::HashSet<String> = allowlist
        .iter()
        .map(|h| h.to_lowercase())
        .collect();
    headers
        .iter()
        .filter(|(k, _)| allowed.contains(&k.to_lowercase()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Normalize an HTTP method. Mirrors Node `normalizeMethod`.
fn normalize_method(method: &str) -> String {
    method.trim().to_uppercase()
}

/// Input for [`build_sandbox_callback_bridge_env`].
pub struct BridgeEnvInput {
    pub queue_dir: String,
    pub bridge_token: String,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub poll_interval_ms: Option<u64>,
    pub response_timeout_ms: Option<u64>,
    pub max_queue_depth: Option<u64>,
    pub max_body_bytes: Option<u64>,
}

/// Build the env vars that the bridge server / worker consume.
/// Mirrors Node `buildSandboxCallbackBridgeEnv`.
#[must_use]
pub fn build_sandbox_callback_bridge_env(input: &BridgeEnvInput) -> std::collections::BTreeMap<String, String> {
    let host = input
        .host
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("127.0.0.1")
        .to_string();
    let port = match input.port {
        Some(p) if p > 0 => p.to_string(),
        _ => "0".to_string(),
    };
    let poll_interval = input
        .poll_interval_ms
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_BRIDGE_POLL_INTERVAL_MS);
    let response_timeout = input
        .response_timeout_ms
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_BRIDGE_RESPONSE_TIMEOUT_MS);
    let max_queue_depth = input
        .max_queue_depth
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_BRIDGE_MAX_QUEUE_DEPTH);
    let max_body_bytes = input
        .max_body_bytes
        .filter(|&v| v > 0)
        .unwrap_or(DEFAULT_BRIDGE_MAX_BODY_BYTES);

    let mut env = std::collections::BTreeMap::new();
    env.insert(
        "PAPERCLIP_API_BRIDGE_MODE".to_string(),
        "queue_v1".to_string(),
    );
    env.insert("PAPERCLIP_BRIDGE_QUEUE_DIR".to_string(), input.queue_dir.clone());
    env.insert("PAPERCLIP_BRIDGE_TOKEN".to_string(), input.bridge_token.clone());
    env.insert("PAPERCLIP_BRIDGE_HOST".to_string(), host);
    env.insert("PAPERCLIP_BRIDGE_PORT".to_string(), port);
    env.insert(
        "PAPERCLIP_BRIDGE_POLL_INTERVAL_MS".to_string(),
        poll_interval.to_string(),
    );
    env.insert(
        "PAPERCLIP_BRIDGE_RESPONSE_TIMEOUT_MS".to_string(),
        response_timeout.to_string(),
    );
    env.insert(
        "PAPERCLIP_BRIDGE_MAX_QUEUE_DEPTH".to_string(),
        max_queue_depth.to_string(),
    );
    env.insert(
        "PAPERCLIP_BRIDGE_MAX_BODY_BYTES".to_string(),
        max_body_bytes.to_string(),
    );
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_have_expected_values() {
        assert_eq!(DEFAULT_BRIDGE_TOKEN_BYTES, 24);
        assert_eq!(DEFAULT_BRIDGE_POLL_INTERVAL_MS, 100);
        assert_eq!(DEFAULT_BRIDGE_RESPONSE_TIMEOUT_MS, 30_000);
        assert_eq!(DEFAULT_BRIDGE_STOP_TIMEOUT_MS, 2_000);
        assert_eq!(DEFAULT_BRIDGE_MAX_QUEUE_DEPTH, 64);
        assert_eq!(DEFAULT_BRIDGE_MAX_BODY_BYTES, 256 * 1024);
        assert_eq!(REMOTE_WRITE_BASE64_CHUNK_SIZE, 32 * 1024);
        assert_eq!(SANDBOX_CALLBACK_BRIDGE_ENTRYPOINT, "paperclip-bridge-server.mjs");
    }

    #[test]
    fn default_route_allowlist_has_expected_routes() {
        let routes = default_sandbox_callback_bridge_route_allowlist();
        // Spot-check a few entries
        let methods: Vec<&str> = routes.iter().map(|r| r.method.as_str()).collect();
        assert!(methods.contains(&"GET"));
        assert!(methods.contains(&"POST"));
        assert!(methods.contains(&"PATCH"));
        assert!(methods.contains(&"PUT"));
        assert!(methods.contains(&"DELETE"));
        assert!(routes.len() >= 50);
    }

    #[test]
    fn bridge_directories_computes_correct_layout() {
        let dirs = sandbox_callback_bridge_directories("/bridge");
        assert_eq!(dirs.root_dir, "/bridge");
        assert_eq!(dirs.requests_dir, "/bridge/requests");
        assert_eq!(dirs.responses_dir, "/bridge/responses");
        assert_eq!(dirs.logs_dir, "/bridge/logs");
        assert_eq!(dirs.ready_file, "/bridge/ready.json");
        assert_eq!(dirs.pid_file, "/bridge/server.pid");
        assert_eq!(dirs.log_file, "/bridge/logs/bridge.log");
    }

    #[test]
    fn create_bridge_token_produces_unique_values() {
        let a = create_sandbox_callback_bridge_token(None);
        let b = create_sandbox_callback_bridge_token(None);
        assert_ne!(a, b);
        assert!(a.len() >= 32);
    }

    #[test]
    fn create_bridge_token_with_custom_bytes() {
        let token = create_sandbox_callback_bridge_token(Some(16));
        // base64url: 16 bytes → ceil(16*4/3) = 22 chars
        assert_eq!(token.len(), 22);
    }

    #[test]
    fn authorize_allows_listed_routes() {
        assert!(
            authorize_sandbox_callback_bridge_request_with_routes(
                "GET",
                "/api/agents/me",
                None,
            )
            .is_ok()
        );
        assert!(
            authorize_sandbox_callback_bridge_request_with_routes(
                "POST",
                "/api/issues/abc/checkout",
                None,
            )
            .is_ok()
        );
    }

    #[test]
    fn authorize_rejects_unlisted_routes() {
        let err = authorize_sandbox_callback_bridge_request_with_routes(
            "GET",
            "/api/admin/secret",
            None,
        );
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("Route not allowed"));
    }

    #[test]
    fn authorize_rejects_wrong_method() {
        // /api/agents/me is GET-only
        let err = authorize_sandbox_callback_bridge_request_with_routes(
            "DELETE",
            "/api/agents/me",
            None,
        );
        assert!(err.is_err());
    }

    #[test]
    fn authorize_normalizes_method_case() {
        assert!(
            authorize_sandbox_callback_bridge_request_with_routes(
                "get",
                "/api/agents/me",
                None,
            )
            .is_ok()
        );
    }

    #[test]
    fn authorize_with_custom_routes() {
        let custom = vec![SandboxCallbackBridgeRouteRule::new(
            "GET",
            r"^/custom/.*$",
        )];
        assert!(
            authorize_sandbox_callback_bridge_request_with_routes(
                "GET",
                "/custom/path",
                Some(&custom),
            )
            .is_ok()
        );
        assert!(
            authorize_sandbox_callback_bridge_request_with_routes(
                "GET",
                "/api/agents/me",
                Some(&custom),
            )
            .is_err()
        );
    }

    #[test]
    fn sanitize_headers_preserves_allowed() {
        let mut headers = std::collections::BTreeMap::new();
        headers.insert("Accept".to_string(), "application/json".to_string());
        headers.insert("Content-Type".to_string(), "application/json".to_string());
        headers.insert("Authorization".to_string(), "Bearer secret".to_string());
        headers.insert("X-Custom".to_string(), "value".to_string());

        let result = sanitize_sandbox_callback_bridge_headers(&headers, None);
        assert_eq!(result.len(), 2);
        assert!(result.contains_key("Accept"));
        assert!(result.contains_key("Content-Type"));
        assert!(!result.contains_key("Authorization"));
        assert!(!result.contains_key("X-Custom"));
    }

    #[test]
    fn sanitize_headers_with_custom_allowlist() {
        let mut headers = std::collections::BTreeMap::new();
        headers.insert("X-Custom".to_string(), "value".to_string());
        let custom = vec!["x-custom"];
        let result = sanitize_sandbox_callback_bridge_headers(&headers, Some(&custom));
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn build_bridge_env_uses_defaults() {
        let env = build_sandbox_callback_bridge_env(&BridgeEnvInput {
            queue_dir: "/q".to_string(),
            bridge_token: "tok".to_string(),
            host: None,
            port: None,
            poll_interval_ms: None,
            response_timeout_ms: None,
            max_queue_depth: None,
            max_body_bytes: None,
        });
        assert_eq!(env["PAPERCLIP_API_BRIDGE_MODE"], "queue_v1");
        assert_eq!(env["PAPERCLIP_BRIDGE_QUEUE_DIR"], "/q");
        assert_eq!(env["PAPERCLIP_BRIDGE_TOKEN"], "tok");
        assert_eq!(env["PAPERCLIP_BRIDGE_HOST"], "127.0.0.1");
        assert_eq!(env["PAPERCLIP_BRIDGE_PORT"], "0");
        assert_eq!(env["PAPERCLIP_BRIDGE_POLL_INTERVAL_MS"], "100");
        assert_eq!(env["PAPERCLIP_BRIDGE_RESPONSE_TIMEOUT_MS"], "30000");
        assert_eq!(env["PAPERCLIP_BRIDGE_MAX_QUEUE_DEPTH"], "64");
        assert_eq!(env["PAPERCLIP_BRIDGE_MAX_BODY_BYTES"], "262144");
    }

    #[test]
    fn build_bridge_env_with_custom_values() {
        let env = build_sandbox_callback_bridge_env(&BridgeEnvInput {
            queue_dir: "/q".to_string(),
            bridge_token: "tok".to_string(),
            host: Some("0.0.0.0".to_string()),
            port: Some(8080),
            poll_interval_ms: Some(500),
            response_timeout_ms: Some(60_000),
            max_queue_depth: Some(128),
            max_body_bytes: Some(1024 * 1024),
        });
        assert_eq!(env["PAPERCLIP_BRIDGE_HOST"], "0.0.0.0");
        assert_eq!(env["PAPERCLIP_BRIDGE_PORT"], "8080");
        assert_eq!(env["PAPERCLIP_BRIDGE_POLL_INTERVAL_MS"], "500");
        assert_eq!(env["PAPERCLIP_BRIDGE_RESPONSE_TIMEOUT_MS"], "60000");
        assert_eq!(env["PAPERCLIP_BRIDGE_MAX_QUEUE_DEPTH"], "128");
        assert_eq!(env["PAPERCLIP_BRIDGE_MAX_BODY_BYTES"], "1048576");
    }

    #[test]
    fn build_bridge_env_handles_empty_host() {
        let env = build_sandbox_callback_bridge_env(&BridgeEnvInput {
            queue_dir: "/q".to_string(),
            bridge_token: "tok".to_string(),
            host: Some("   ".to_string()),
            port: None,
            poll_interval_ms: None,
            response_timeout_ms: None,
            max_queue_depth: None,
            max_body_bytes: None,
        });
        assert_eq!(env["PAPERCLIP_BRIDGE_HOST"], "127.0.0.1");
    }
}
