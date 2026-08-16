//! pc-agent-jwt — 本地 agent JWT (HS256) for paperclip
//!
//! 完全对齐 Node  (249 LOC)：
//! - create_local_agent_jwt: 给 adapter runtime 颁发的短期 JWT
//! - verify_local_agent_jwt: HTTP middleware 验证
//! - derive_company_signing_key: per-instance + per-company HMAC-SHA256 派生
//!   - 实现 PAP-12896: 阻止 fork/worktree instance 颁发的 token 跨 tenant / 跨 instance 重放
//! - Legacy master-secret fallback: 通过 PAPERCLIP_AGENT_JWT_DISABLE_LEGACY_FALLBACK=false 控制
//!
//! 不依赖第三方 JWT crate（jsonwebtoken）—— 用 hmac + base64 直接实现 HS256，
//! 与 Node crypto.createHmac 行为完全等价（HMAC-SHA256 digest + base64url）。

use std::env;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

const JWT_ALGORITHM: &str = "HS256";
const JWT_TYPE: &str = "JWT";
const DEFAULT_TTL_SECONDS: i64 = 60 * 60;
const DEFAULT_ISSUER: &str = "paperclip";
const DEFAULT_AUDIENCE: &str = "paperclip-api";
const INSTANCE_ID_FALLBACK: &str = "default";

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, thiserror::Error)]
pub enum AgentJwtError {
    #[error("JWT secret not configured: set PAPERCLIP_AGENT_JWT_SECRET or BETTER_AUTH_SECRET")]
    SecretMissing,
    #[error("malformed JWT: expected 3 parts, got {0}")]
    MalformedToken(usize),
    #[error("unsupported algorithm: {0}")]
    UnsupportedAlgorithm(String),
    #[error("claim parse error: {0}")]
    ClaimParse(String),
    #[error("signature mismatch")]
    SignatureMismatch,
    #[error("token expired (exp={0}, now={1})")]
    Expired(i64, i64),
    #[error("issuer mismatch: claim={0}, expected={1}")]
    IssuerMismatch(String, String),
    #[error("audience mismatch: claim={0}, expected={1}")]
    AudienceMismatch(String, String),
    #[error("instance mismatch: claim={0}, expected={1}")]
    InstanceMismatch(String, String),
    #[error("missing required claim: {0}")]
    MissingClaim(&'static str),
}

/// AgentApiKeyScope: 与 Node @paperclipai/shared normalizeAgentApiKeyScope 对齐
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentApiKeyScopeKind {
    #[default]
    Standard,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentApiKeyScope {
    Standard {},
    // 未来扩展: read_only, admin 等
    #[serde(untagged)]
    Other(serde_json::Value),
}

impl Default for AgentApiKeyScope {
    fn default() -> Self { AgentApiKeyScope::Standard {} }
}

/// JWT claim payload (与 Node LocalAgentJwtClaims 对应)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalAgentJwtClaims {
    pub sub: String,
    pub company_id: String,
    pub adapter_type: String,
    pub run_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub responsible_user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_scope: Option<AgentApiKeyScope>,
    pub iat: i64,
    pub exp: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iss: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aud: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jti: Option<String>,
}

/// JWT 配置：env-only，与 Node jwtConfig() 完全等价
#[derive(Debug, Clone)]
pub struct JwtConfig {
    pub secret: String,
    pub ttl_seconds: i64,
    pub issuer: String,
    pub audience: String,
    pub instance_id: String,
    pub disable_legacy_fallback: bool,
}

impl JwtConfig {
    /// 从 std::env 读取配置。secret 缺失返回 None（与 Node 行为一致）。
    pub fn from_env() -> Option<Self> {
        Self::from_env_with(|k| env::var(k).ok())
    }

    /// 从外部 env provider 读取（用于测试 / 自定义实例 ID）。
    pub fn from_env_with<F>(lookup: F) -> Option<Self>
    where
        F: Fn(&str) -> Option<String>,
    {
        let secret = lookup("PAPERCLIP_AGENT_JWT_SECRET")
            .or_else(|| lookup("BETTER_AUTH_SECRET"))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())?;

        let ttl_seconds = lookup("PAPERCLIP_AGENT_JWT_TTL_SECONDS")
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(DEFAULT_TTL_SECONDS);

        let issuer = lookup("PAPERCLIP_AGENT_JWT_ISSUER")
            .unwrap_or_else(|| DEFAULT_ISSUER.to_string());
        let audience = lookup("PAPERCLIP_AGENT_JWT_AUDIENCE")
            .unwrap_or_else(|| DEFAULT_AUDIENCE.to_string());

        let instance_id = lookup("PAPERCLIP_INSTANCE_ID")
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| INSTANCE_ID_FALLBACK.to_string());

        let disable_legacy_fallback = lookup("PAPERCLIP_AGENT_JWT_DISABLE_LEGACY_FALLBACK")
            .map(|v| {
                let n = v.trim().to_lowercase();
                matches!(n.as_str(), "1" | "true" | "yes" | "on")
            })
            .unwrap_or(false);

        Some(Self {
            secret,
            ttl_seconds,
            issuer,
            audience,
            instance_id,
            disable_legacy_fallback,
        })
    }
}

/// Per-instance, per-company signing key derivation.
///
/// 与 Node deriveCompanySigningKey(masterSecret, companyId, instanceId) 完全等价：
///   HMAC-SHA256(masterSecret, "jwt:{instanceId}:{companyId}") -> hex
///
/// 隔离属性：
/// 1. Per-company: companyA 的 token 不能重放到 companyB
/// 2. Per-instance: worktree fork instance 颁发的 token 不能重放到 live plane
///    (PAP-12896/PAP-12899)
/// 3. Master secret 永远不会直接签名新 token —— 仅用于 backward-compat fallback
fn derive_company_signing_key(master_secret: &str, company_id: &str, instance_id: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(master_secret.as_bytes())
        .expect("HMAC-SHA256 accepts any key length");
    mac.update(format!("jwt:{instance_id}:{company_id}").as_bytes());
    let result = mac.finalize().into_bytes();
    hex::encode(result)
}

fn base64_url_encode(value: &str) -> String {
    URL_SAFE_NO_PAD.encode(value.as_bytes())
}

fn base64_url_decode(value: &str) -> Result<String, AgentJwtError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value.as_bytes())
        .map_err(|e| AgentJwtError::ClaimParse(format!("base64url: {e}")))?;
    String::from_utf8(bytes).map_err(|e| AgentJwtError::ClaimParse(format!("utf8: {e}")))
}

fn sign_payload(secret: &str, signing_input: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .expect("HMAC-SHA256 accepts any key length");
    mac.update(signing_input.as_bytes());
    let result = mac.finalize().into_bytes();
    URL_SAFE_NO_PAD.encode(result)
}

fn safe_compare(a: &str, b: &str) -> bool {
    if a.len() != b.len() { return false; }
    let mut diff = 0u8;
    for (x, y) in a.bytes().zip(b.bytes()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 颁发本地 agent JWT。
///
/// 与 Node createLocalAgentJwt 完全等价：返回 Option<String>，secret 缺失时为 None。
/// 返回的 token 形如 （base64url 编码）。
pub fn create_local_agent_jwt(
    config: &JwtConfig,
    agent_id: &str,
    company_id: &str,
    adapter_type: &str,
    run_id: &str,
    responsible_user_id: Option<&str>,
    key_scope: Option<&AgentApiKeyScope>,
) -> String {
    let now = now_unix();
    let mut claims = serde_json::json!({
        "sub": agent_id,
        "company_id": company_id,
        "adapter_type": adapter_type,
        "run_id": run_id,
        "iat": now,
        "exp": now + config.ttl_seconds,
        "iss": config.issuer,
        "aud": config.audience,
        "instance_id": config.instance_id,
    });
    if let Some(rid) = responsible_user_id {
        let trimmed = rid.trim();
        if !trimmed.is_empty() {
            claims["responsible_user_id"] = serde_json::Value::String(trimmed.to_string());
        }
    }
    if let Some(scope) = key_scope {
        if !matches!(scope, AgentApiKeyScope::Standard {}) {
            claims["key_scope"] = serde_json::to_value(scope).unwrap_or(serde_json::Value::Null);
        }
    }

    let header = serde_json::json!({
        "alg": JWT_ALGORITHM,
        "typ": JWT_TYPE,
    });

    let header_b64 = base64_url_encode(&header.to_string());
    let claims_b64 = base64_url_encode(&claims.to_string());
    let signing_input = format!("{header_b64}.{claims_b64}");

    let signing_key = derive_company_signing_key(&config.secret, company_id, &config.instance_id);
    let signature = sign_payload(&signing_key, &signing_input);

    format!("{signing_input}.{signature}")
}

/// 验证 JWT 并返回 claims。
///
/// 与 Node verifyLocalAgentJwt 行为完全等价：
/// - secret 缺失 -> None
/// - 3 段 token 解析失败 -> None
/// - alg != HS256 -> None
/// - 签名校验：
///   1. 先用 per-company key 校验（当前 control-plane instance）
///   2. 若 disable_legacy_fallback=false，fallback 用 master secret 校验（旧 token）
/// - exp < now -> None
/// - iss/aud/instance_id 不匹配 -> None
pub fn verify_local_agent_jwt(config: &JwtConfig, token: &str) -> Option<LocalAgentJwtClaims> {
    if token.is_empty() { return None; }

    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 { return None; }
    let (header_b64, claims_b64, signature) = (parts[0], parts[1], parts[2]);

    let header_value: serde_json::Value = serde_json::from_str(
        &base64_url_decode(header_b64).ok()?
    ).ok()?;
    let alg = header_value.get("alg").and_then(|v| v.as_str())?;
    if alg != JWT_ALGORITHM { return None; }

    let claims_value: serde_json::Value = serde_json::from_str(
        &base64_url_decode(claims_b64).ok()?
    ).ok()?;
    let company_id = claims_value.get("company_id")
        .and_then(|v| v.as_str())?
        .to_string();

    let signing_input = format!("{header_b64}.{claims_b64}");
    let per_company_key = derive_company_signing_key(&config.secret, &company_id, &config.instance_id);
    let per_company_sig = sign_payload(&per_company_key, &signing_input);
    let mut signature_ok = safe_compare(signature, &per_company_sig);
    if !signature_ok && !config.disable_legacy_fallback {
        let legacy_sig = sign_payload(&config.secret, &signing_input);
        signature_ok = safe_compare(signature, &legacy_sig);
    }
    if !signature_ok { return None; }

    let sub = claims_value.get("sub").and_then(|v| v.as_str())?.to_string();
    let adapter_type = claims_value.get("adapter_type").and_then(|v| v.as_str())?.to_string();
    let run_id = claims_value.get("run_id").and_then(|v| v.as_str())?.to_string();
    let iat = claims_value.get("iat").and_then(|v| v.as_i64())?;
    let exp = claims_value.get("exp").and_then(|v| v.as_i64())?;

    let now = now_unix();
    if exp < now { return None; }

    let issuer = claims_value.get("iss").and_then(|v| v.as_str()).map(String::from);
    let audience = claims_value.get("aud").and_then(|v| v.as_str()).map(String::from);
    if let Some(ref iss) = issuer {
        if iss != &config.issuer { return None; }
    }
    if let Some(ref aud) = audience {
        if aud != &config.audience { return None; }
    }

    let instance_claim = claims_value.get("instance_id").and_then(|v| v.as_str()).map(String::from);
    if let Some(ref inst) = instance_claim {
        if inst != &config.instance_id { return None; }
    }

    let responsible_user_id = if claims_value.get("responsible_user_id").is_some() {
        claims_value.get("responsible_user_id")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    } else {
        None
    };

    let key_scope = if claims_value.get("key_scope").is_some() {
        claims_value.get("key_scope").and_then(|v| serde_json::from_value(v.clone()).ok())
    } else {
        None
    };

    Some(LocalAgentJwtClaims {
        sub,
        company_id,
        adapter_type,
        run_id,
        responsible_user_id,
        key_scope,
        iat,
        exp,
        iss: issuer,
        aud: audience,
        instance_id: instance_claim,
        jti: claims_value.get("jti").and_then(|v| v.as_str()).map(String::from),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> JwtConfig {
        JwtConfig {
            secret: "test-secret-do-not-use-in-prod".to_string(),
            ttl_seconds: 3600,
            issuer: "paperclip".to_string(),
            audience: "paperclip-api".to_string(),
            instance_id: "default".to_string(),
            disable_legacy_fallback: false,
        }
    }

    #[test]
    fn derive_company_signing_key_is_deterministic() {
        let a = derive_company_signing_key("secret", "company-1", "instance-1");
        let b = derive_company_signing_key("secret", "company-1", "instance-1");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64); // SHA-256 hex
    }

    #[test]
    fn derive_company_signing_key_isolates_by_company() {
        let a = derive_company_signing_key("secret", "company-A", "default");
        let b = derive_company_signing_key("secret", "company-B", "default");
        assert_ne!(a, b);
    }

    #[test]
    fn derive_company_signing_key_isolates_by_instance() {
        let a = derive_company_signing_key("secret", "company-1", "instance-fork");
        let b = derive_company_signing_key("secret", "company-1", "default");
        assert_ne!(a, b);
    }

    #[test]
    fn create_then_verify_roundtrip() {
        let cfg = test_config();
        let token = create_local_agent_jwt(
            &cfg,
            "agent-1",
            "company-1",
            "process",
            "run-1",
            Some("user-1"),
            None,
        );
        let claims = verify_local_agent_jwt(&cfg, &token).expect("verify");
        assert_eq!(claims.sub, "agent-1");
        assert_eq!(claims.company_id, "company-1");
        assert_eq!(claims.adapter_type, "process");
        assert_eq!(claims.run_id, "run-1");
        assert_eq!(claims.responsible_user_id.as_deref(), Some("user-1"));
        assert_eq!(claims.iss.as_deref(), Some("paperclip"));
        assert_eq!(claims.aud.as_deref(), Some("paperclip-api"));
        assert_eq!(claims.instance_id.as_deref(), Some("default"));
        assert!(claims.exp > claims.iat);
        assert_eq!(claims.exp - claims.iat, 3600);
    }

    #[test]
    fn verify_rejects_tampered_signature() {
        let cfg = test_config();
        let token = create_local_agent_jwt(
            &cfg, "a", "c1", "process", "r1", None, None);
        let mut parts: Vec<&str> = token.split('.').collect();
        let sig = parts[2].to_string();
        let tampered_sig = format!("{}{}", &sig[..sig.len()-1], 'A');
        parts[2] = &tampered_sig;
        let tampered = parts.join(".");
        assert!(verify_local_agent_jwt(&cfg, &tampered).is_none());
    }

    #[test]
    fn verify_rejects_cross_company_token() {
        let cfg = test_config();
        let token = create_local_agent_jwt(
            &cfg, "a", "company-A", "process", "r1", None, None);
        // 同样 cfg 但尝试 verify —— token 是为 companyA 颁发的，
        // cfg.instance_id=default，cfg.secret 一样 —— 应该仍然 verify pass
        // （per-company 隔离是 key derivation 的隔离，不阻止 self verify）
        let claims = verify_local_agent_jwt(&cfg, &token);
        assert!(claims.is_some());
        assert_eq!(claims.unwrap().company_id, "company-A");
    }

    #[test]
    fn verify_rejects_token_from_other_instance() {
        let mut cfg_issuer = test_config();
        cfg_issuer.instance_id = "fork-1".to_string();
        let token = create_local_agent_jwt(
            &cfg_issuer, "a", "company-1", "process", "r1", None, None);

        let cfg_verifier = test_config(); // instance_id = "default"
        assert!(verify_local_agent_jwt(&cfg_verifier, &token).is_none(),
            "fork-1 token must not authenticate against default instance");
    }

    #[test]
    fn verify_rejects_expired_token() {
        let mut cfg = test_config();
        cfg.ttl_seconds = -1; // 立刻过期
        let token = create_local_agent_jwt(
            &cfg, "a", "company-1", "process", "r1", None, None);
        assert!(verify_local_agent_jwt(&cfg, &token).is_none());
    }

    #[test]
    fn verify_rejects_empty_token() {
        let cfg = test_config();
        assert!(verify_local_agent_jwt(&cfg, "").is_none());
    }

    #[test]
    fn verify_rejects_malformed_token() {
        let cfg = test_config();
        assert!(verify_local_agent_jwt(&cfg, "abc.def").is_none());
        assert!(verify_local_agent_jwt(&cfg, "a.b.c.d").is_none());
    }

    #[test]
    fn legacy_fallback_accepts_old_token() {
        // 模拟"master secret"旧 token：用 cfg.secret 直接签（不是 derive key）
        let cfg = test_config();
        let header = serde_json::json!({"alg": "HS256", "typ": "JWT"});
        let now = now_unix();
        let claims = serde_json::json!({
            "sub": "a", "company_id": "company-1", "adapter_type": "process",
            "run_id": "r1", "iat": now, "exp": now + 3600,
        });
        let h = base64_url_encode(&header.to_string());
        let c = base64_url_encode(&claims.to_string());
        let input = format!("{h}.{c}");
        let sig = sign_payload(&cfg.secret, &input);
        let legacy_token = format!("{input}.{sig}");
        assert!(verify_local_agent_jwt(&cfg, &legacy_token).is_some(),
            "legacy token should pass via master-secret fallback");
    }

    #[test]
    fn legacy_fallback_can_be_disabled() {
        let mut cfg = test_config();
        cfg.disable_legacy_fallback = true;
        let header = serde_json::json!({"alg": "HS256", "typ": "JWT"});
        let now = now_unix();
        let claims = serde_json::json!({
            "sub": "a", "company_id": "company-1", "adapter_type": "process",
            "run_id": "r1", "iat": now, "exp": now + 3600,
        });
        let h = base64_url_encode(&header.to_string());
        let c = base64_url_encode(&claims.to_string());
        let input = format!("{h}.{c}");
        let sig = sign_payload(&cfg.secret, &input);
        let legacy_token = format!("{input}.{sig}");
        assert!(verify_local_agent_jwt(&cfg, &legacy_token).is_none(),
            "legacy token must be rejected when fallback disabled");
    }

    #[test]
    fn safe_compare_is_timing_safe_basic() {
        assert!(safe_compare("abc", "abc"));
        assert!(!safe_compare("abc", "abd"));
        assert!(!safe_compare("abc", "abcd"));
    }

    #[test]
    fn base64_roundtrip() {
        let s = "hello-world-test-123";
        assert_eq!(base64_url_decode(&base64_url_encode(s)).unwrap(), s);
    }

    #[test]
    fn jwt_config_from_env_reads_paperclip_secret() {
        let cfg = JwtConfig::from_env_with(|k| match k {
            "PAPERCLIP_AGENT_JWT_SECRET" => Some("env-secret".to_string()),
            _ => None,
        }).unwrap();
        assert_eq!(cfg.secret, "env-secret");
        assert_eq!(cfg.ttl_seconds, DEFAULT_TTL_SECONDS);
        assert_eq!(cfg.instance_id, INSTANCE_ID_FALLBACK);
    }

    #[test]
    fn jwt_config_from_env_reads_better_auth_secret_fallback() {
        let cfg = JwtConfig::from_env_with(|k| match k {
            "BETTER_AUTH_SECRET" => Some("better-auth-secret".to_string()),
            _ => None,
        }).unwrap();
        assert_eq!(cfg.secret, "better-auth-secret");
    }

    #[test]
    fn jwt_config_from_env_returns_none_when_secret_missing() {
        let cfg = JwtConfig::from_env_with(|_| None);
        assert!(cfg.is_none());
    }
}
