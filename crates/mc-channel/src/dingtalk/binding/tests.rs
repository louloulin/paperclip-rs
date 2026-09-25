//! `binding.rs` 的用例（不依赖数据库）。
//!
//! 四条安全判据逐条：① 明文不进库（只有哈希）；② 三种失败同一个不透明结果；
//! ③ 兑换原子的三段顺序由端口保证（这里钉调用参数）；④ 令牌寿命上限与线的长度。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{TimeZone as _, Utc};

use super::*;

/// 一个可编程的绑定存储替身。
#[derive(Default)]
struct FakeStore {
    inner: Mutex<FakeState>,
}

#[derive(Default)]
struct FakeState {
    tokens: Vec<NewBindingToken>,
    outcome: Option<RedeemOutcome>,
    calls: Vec<(String, Id)>,
    failure: bool,
}

#[async_trait]
impl BindingStore for FakeStore {
    async fn insert_token(&self, token: &NewBindingToken) -> Result<(), String> {
        let mut state = self.inner.lock().expect("lock");
        if state.failure {
            return Err("boom".to_string());
        }
        state.tokens.push(token.clone());
        Ok(())
    }

    async fn redeem_and_bind(
        &self,
        token_hash: &str,
        multica_user_id: Id,
    ) -> Result<RedeemOutcome, String> {
        let mut state = self.inner.lock().expect("lock");
        if state.failure {
            return Err("boom".to_string());
        }
        state.calls.push((token_hash.to_string(), multica_user_id));
        Ok(state.outcome.clone().expect("outcome"))
    }
}

fn service_with(outcome: RedeemOutcome) -> (Arc<FakeStore>, BindingTokenService) {
    let store = Arc::new(FakeStore::default());
    store.inner.lock().expect("lock").outcome = Some(outcome);
    let service = BindingTokenService::new(Arc::clone(&store) as Arc<dyn BindingStore>).with_now(
        Arc::new(|| Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap()),
    );
    (store, service)
}

#[tokio::test]
async fn minting_stores_only_the_hash_and_a_capped_expiry() {
    let (store, service) = service_with(RedeemOutcome::TokenInvalid);
    let workspace_id = Id::new();
    let installation_id = Id::new();
    let token = service
        .mint(workspace_id, installation_id, "staff-1")
        .await
        .expect("mint");

    let state = store.inner.lock().expect("lock");
    assert_eq!(state.tokens.len(), 1);
    let stored = &state.tokens[0];
    // ① 明文不进库。
    assert_ne!(stored.token_hash, token.raw);
    assert_eq!(stored.token_hash, hash_binding_token(&token.raw));
    assert!(!state.tokens[0].token_hash.contains(&token.raw));
    // 15 分钟上限（DB 的 CHECK 钉着同一条）。
    assert_eq!(token.expires_at, stored.expires_at);
    assert_eq!(
        token.expires_at - (Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap()),
        BINDING_TOKEN_TTL
    );
    assert_eq!(stored.workspace_id, workspace_id);
    assert_eq!(stored.installation_id, installation_id);
    assert_eq!(stored.channel_user_id, "staff-1");
}

#[test]
fn the_raw_token_is_32_bytes_of_base64url_without_padding() {
    use base64::Engine as _;

    let raw = random_binding_token();
    assert!(!raw.contains('='), "RawURLEncoding 无填充：{raw}");
    assert!(!raw.contains('+') && !raw.contains('/'), "URL 安全：{raw}");
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(&raw)
        .expect("decode");
    assert_eq!(
        bytes.len(),
        32,
        "与上游 crypto/rand 读 32 字节等价（线长相同）"
    );
    assert_ne!(raw, random_binding_token(), "每次都不一样");
}

#[test]
fn the_storage_hash_is_lowercase_hex_sha256() {
    let hash = hash_binding_token("abc");
    assert_eq!(hash.len(), 64);
    assert!(hash
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    // 已知向量（`sha256("abc")`）。
    assert_eq!(
        hash,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[tokio::test]
async fn the_three_redeem_verdicts_map_to_410_409_403() {
    let user_id = Id::new();
    let cases = [
        (
            RedeemOutcome::TokenInvalid,
            "dingtalk_binding_token_invalid",
            410,
        ),
        (
            RedeemOutcome::AlreadyAssigned,
            "dingtalk_binding_already_assigned",
            409,
        ),
        (RedeemOutcome::NotMember, "dingtalk_binding_not_member", 403),
    ];
    for (outcome, code, status) in cases {
        let (store, service) = service_with(outcome.clone());
        let verdict = service.redeem("raw-token", user_id).await.expect("redeem");
        assert_eq!(verdict, outcome);
        let error = match outcome {
            RedeemOutcome::TokenInvalid => BindingError::TokenInvalid,
            RedeemOutcome::AlreadyAssigned => BindingError::AlreadyAssigned,
            RedeemOutcome::NotMember => BindingError::NotMember,
            RedeemOutcome::Bound(_) => unreachable!(),
        };
        assert_eq!(error.code(), code);
        assert_eq!(error.http_status(), status);
        // 端口只收到**哈希**（明文令牌绝不进存储层）。
        let calls = &store.inner.lock().expect("lock").calls;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, hash_binding_token("raw-token"));
        assert_eq!(calls[0].1, user_id);
    }
}

#[tokio::test]
async fn a_bound_redeem_carries_the_channel_user_id_from_the_token() {
    let bound = RedeemedBinding {
        workspace_id: Id::new(),
        installation_id: Id::new(),
        channel_user_id: "staff-9".to_string(),
    };
    let (_store, service) = service_with(RedeemOutcome::Bound(bound.clone()));
    let verdict = service.redeem("raw", Id::new()).await.expect("redeem");
    assert_eq!(verdict, RedeemOutcome::Bound(bound));
}

#[tokio::test]
async fn store_failures_are_opaque() {
    let store = Arc::new(FakeStore::default());
    store.inner.lock().expect("lock").failure = true;
    let service = BindingTokenService::new(Arc::clone(&store) as Arc<dyn BindingStore>);
    let error = service
        .mint(Id::new(), Id::new(), "staff")
        .await
        .expect_err("store");
    assert_eq!(error.http_status(), 500);
    assert_eq!(error.code(), "dingtalk_binding_store_error");
    assert!(!error.to_string().contains("staff"));
    let error = service.redeem("raw", Id::new()).await.expect_err("store");
    assert_eq!(error.http_status(), 500);
    // 明文令牌不进错误文案。
    assert!(!error.to_string().contains("raw"));
}
