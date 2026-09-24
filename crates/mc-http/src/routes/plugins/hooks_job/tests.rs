use super::bridge::parse_bridge_trigger;
use super::outbound::{build_hook_headers, schedule_delivery_id};
use super::wire::{HookBody, HookBodyActor};
use super::*;
use mc_plugin_host::credentials::{hook_signing_secret, verify_hook_signature, DeploymentKey};
use mc_plugin_host::manifest::Hook;

const INSTALLATION: &str = "11111111-2222-3333-4444-555555555555";
const OTHER_INSTALLATION: &str = "22222222-2222-4222-8222-222222222222";
/// 与 `mc-plugin-host` 用例同一把密钥（`0x00..0x1f`）。
const KEY_B64: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";
/// 上游用例的字节向量：`{"hook_key":"summarize","input":{"issue_id":"abc"}}` @ 1700000000。
const BODY: &[u8] = br#"{"hook_key":"summarize","input":{"issue_id":"abc"}}"#;
const TIMESTAMP: &str = "1700000000";
const SIGNED_AT: u64 = 1_700_000_000;

fn key() -> DeploymentKey {
    DeploymentKey::from_base64(KEY_B64).expect("32 字节 base64")
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> &'a str {
    match headers.iter().find(|(key, _)| key == name) {
        Some((_, value)) => value.as_str(),
        None => panic!("missing header {name}: {headers:?}"),
    }
}

/// `DoD`：**四个出站头逐字** + 签名字节向量 + 接收侧（`VerifyHookSignature` 的本地形态）
/// 能验过同一批字节。
#[test]
fn outbound_headers_are_verbatim_and_verify_receiver_side() {
    let headers = build_hook_headers(
        Some(&key()),
        Id::parse(INSTALLATION).expect("uuid"),
        TIMESTAMP,
        BODY,
    )
    .expect("headers");
    let names: Vec<&str> = headers.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "Content-Type",
            "X-Multica-Timestamp",
            "X-Multica-Signature",
            "X-Multica-Plugin-Installation",
            "User-Agent",
        ]
    );
    assert_eq!(header(&headers, "Content-Type"), "application/json");
    assert_eq!(header(&headers, "X-Multica-Timestamp"), TIMESTAMP);
    assert_eq!(header(&headers, "User-Agent"), "Multica-Hooks/1");
    assert_eq!(
        header(&headers, "X-Multica-Plugin-Installation"),
        INSTALLATION
    );

    let signature = header(&headers, "X-Multica-Signature");
    // 签名字节向量（python 独立复算：hmac_sha256(derived, b"1700000000"+"."+body)）：
    //   derived = hmac_sha256(key, b"multica-plugin-hook-signature:v1:"+installation)
    //           = c546e0ba85cd275ae78732bc3e42e3785f113859c0393a1775ed18534ddf636c
    assert_eq!(
        signature,
        "v1=068443257d29e4262c7d72b9feb7b972471b0d713d09dd5d2ea5247b79fa2729"
    );

    // 接收侧：插件作者拿到的是 `whsec_…`，用它与同一批字节验签。
    let secret = hook_signing_secret(Some(&key()), Id::parse(INSTALLATION).unwrap()).unwrap();
    assert!(verify_hook_signature(secret.as_str(), TIMESTAMP, BODY, signature, SIGNED_AT).is_ok());
    // 三个反例：错签名 / 超出 ±5 分钟 / 错 installation。
    assert_eq!(
        verify_hook_signature(secret.as_str(), TIMESTAMP, BODY, "v1=deadbeef", SIGNED_AT),
        Err(CredentialError::SignatureMismatch),
        "错签名必须被拒"
    );
    assert_eq!(
        verify_hook_signature(secret.as_str(), TIMESTAMP, BODY, signature, SIGNED_AT + 301),
        Err(CredentialError::SignatureTimestampWindow),
        "±5 分钟之外必须被拒（重放窗口）"
    );
    let key_bytes: Vec<u8> = (0u8..32).collect();
    let other = hook_signing_secret(
        Some(&DeploymentKey::new(&key_bytes).expect("32 bytes")),
        Id::parse(OTHER_INSTALLATION).expect("uuid"),
    )
    .expect("secret");
    assert_eq!(
        verify_hook_signature(other.as_str(), TIMESTAMP, BODY, signature, SIGNED_AT),
        Err(CredentialError::SignatureMismatch),
        "另一个安装的密钥不得验过这次签名"
    );
    // 三个反例的稳定码（接收侧把它折成 401/403 时用的那一列）。
    for error in [
        CredentialError::SignatureMismatch,
        CredentialError::SignatureTimestampWindow,
    ] {
        assert_eq!(error.code(), "plugin_signature_invalid");
    }
    assert_eq!(
        CredentialError::HooksDisabled.code(),
        "plugin_credentials_unavailable"
    );
}

/// 未配置部署密钥 ⇒ **fail closed**：不发未签名的请求（503 `plugin_disabled`）。
#[test]
fn signing_without_a_deployment_key_is_refused() {
    let error = build_hook_headers(None, Id::parse(INSTALLATION).unwrap(), TIMESTAMP, BODY)
        .expect_err("缺密钥必须报错");
    assert_eq!(error.status(), 503);
    assert_eq!(error.code(), "plugin_disabled");
    assert_eq!(
        error.message(),
        "hooks are disabled: MULTICA_PLUGIN_SECRET_KEY is not configured"
    );
}

/// `delivery_id` 跨重试稳定、跨安装/换代/计划格子不同 —— 它就是「同一次计划投递」的身份。
#[test]
fn schedule_delivery_id_is_stable_per_occurrence() {
    let installation = Id::parse(INSTALLATION).unwrap();
    let generation = Id::parse(OTHER_INSTALLATION).unwrap();
    let plan_time = DateTime::from_timestamp(1_700_000_000, 0).expect("ts");
    let first = schedule_delivery_id(installation, "sync", generation, plan_time);
    assert_eq!(
        first,
        schedule_delivery_id(installation, "sync", generation, plan_time),
        "同一个格子必须得到同一个 id（重试要认得出是同一次投递）"
    );
    assert!(first.starts_with("psd_"));
    assert_eq!(first.len(), 4 + 64);
    assert_ne!(
        first,
        schedule_delivery_id(installation, "digest", generation, plan_time)
    );
    assert_ne!(
        first,
        schedule_delivery_id(
            installation,
            "sync",
            generation,
            plan_time + chrono::Duration::seconds(1)
        )
    );
}

/// 触发器必须在 manifest 里声明过。
#[test]
fn only_declared_triggers_are_allowed() {
    let hook = Hook {
        key: "sync".into(),
        name: "Sync".into(),
        description: "d".into(),
        input_schema: None,
        triggers: vec![HookTrigger::Manual.as_str().to_owned()],
        events: Vec::new(),
        schedule: None,
        transport: mc_plugin_host::manifest::HookTransport {
            kind: HookTransport::Http.as_str().to_owned(),
            url: "https://example.com/hook".into(),
        },
        timeout_ms: 0,
    };
    assert!(hook_allows_trigger(&hook, HookTrigger::Manual));
    for undeclared in [
        HookTrigger::Ui,
        HookTrigger::Event,
        HookTrigger::Agent,
        HookTrigger::Schedule,
    ] {
        assert!(!hook_allows_trigger(&hook, undeclared));
    }
}

/// `:key` 是不透明引用；`ui`/`manual` 之外的触发器 400。
#[test]
fn bridge_trigger_is_limited_to_ui_and_manual() {
    assert_eq!(parse_bridge_trigger("ui").expect("ui"), HookTrigger::Ui);
    assert_eq!(
        parse_bridge_trigger("manual").expect("manual"),
        HookTrigger::Manual
    );
    for rejected in ["event", "agent", "schedule", "", "UI"] {
        let error = parse_bridge_trigger(rejected).expect_err("must be refused");
        assert_eq!(error.status, StatusCode::BAD_REQUEST);
        assert_eq!(error.message, "trigger must be ui or manual");
    }
}

/// 请求体的省略规则：`None` 字段**不出现**（不是 `null`）。
#[test]
fn hook_body_omits_absent_fields() {
    let body = HookBody {
        version: 1,
        invocation_id: "i".into(),
        delivery_id: None,
        attempt: 1,
        occurred_at: DateTime::from_timestamp(1_700_000_000, 0).expect("ts"),
        hook_key: "sync".into(),
        trigger: "manual".into(),
        event_type: None,
        workspace_id: "w".into(),
        installation_id: INSTALLATION.into(),
        issue_id: None,
        actor: HookBodyActor {
            kind: "member".into(),
            id: String::new(),
        },
        input: None,
        config: None,
        callback_token: None,
        callback_url: None,
        schedule: None,
    };
    let encoded = serde_json::to_string(&body).expect("encode");
    for absent in [
        "delivery_id",
        "event_type",
        "issue_id",
        "input",
        "config",
        "callback_token",
        "schedule",
    ] {
        assert!(!encoded.contains(absent), "{absent} 不该出现：{encoded}");
    }
    assert!(encoded.contains("\"version\":1"));
    assert!(encoded.contains("\"hook_key\":\"sync\""));
}
