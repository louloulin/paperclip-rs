//! `telegram::binding` 的用例（写者 M7-5）。
//!
//! 上游 `binding_test.go` 的三条（令牌唯一且 URL 安全、哈希确定性且不残留明文、缺事务启动器
//! 即失败）+ 本片专属的**幂等**验收。
//!
//! 这里注入一个**真正的内存替身**（而不是"返回固定值"的假实现）：它把"消费是 CAS、绑定行有
//! 唯一键"这两条语义照实现一遍 ⇒ "重复兑换不重复插行"这条断言才有内容。

use std::collections::HashMap;
use std::sync::Mutex;

use super::*;

// ---------------------------------------------------------------------------
// 内存替身
// ---------------------------------------------------------------------------

/// 一枚被存下来的令牌。
#[derive(Clone)]
struct StoredToken {
    consumed: bool,
    expires_at: DateTime<Utc>,
    workspace_id: Id,
    installation_id: Id,
    channel_user_id: String,
    /// 该令牌属于哪个 adapter（用例用它模拟"别的渠道的令牌"）。
    channel_type: String,
}

/// 内存实现：语义逐条照上游（消费是 CAS；绑定行按 `(installation, 平台用户 id)` 唯一；
/// 已属于**另一个**用户则拒绝）。
#[derive(Default)]
struct MemoryBindingStore {
    tokens: Mutex<HashMap<String, StoredToken>>,
    bindings: Mutex<HashMap<(Id, String), Id>>,
    now: Mutex<DateTime<Utc>>,
}

impl MemoryBindingStore {
    fn with_now(now: DateTime<Utc>) -> Self {
        Self {
            now: Mutex::new(now),
            ..Self::default()
        }
    }

    /// 直接塞一枚令牌（`channel_type` 可就地改成别的 adapter）。
    fn seed(&self, raw: &str, token: StoredToken) {
        self.tokens
            .lock()
            .expect("tokens")
            .insert(hash_binding_token(raw), token);
    }

    fn binding_count(&self) -> usize {
        self.bindings.lock().expect("bindings").len()
    }
}

#[async_trait]
impl BindingStore for MemoryBindingStore {
    async fn insert_token(&self, token: &NewBindingToken) -> Result<(), String> {
        let mut tokens = self.tokens.lock().expect("tokens");
        if tokens.contains_key(&token.token_hash) {
            return Err("duplicate token hash".to_string());
        }
        tokens.insert(
            token.token_hash.clone(),
            StoredToken {
                consumed: false,
                expires_at: token.expires_at,
                workspace_id: token.workspace_id,
                installation_id: token.installation_id,
                channel_user_id: token.channel_user_id.clone(),
                channel_type: "telegram".to_string(),
            },
        );
        Ok(())
    }

    async fn redeem_and_bind(
        &self,
        token_hash: &str,
        multica_user_id: Id,
    ) -> Result<RedeemOutcome, String> {
        let now = *self.now.lock().expect("now");
        let mut tokens = self.tokens.lock().expect("tokens");
        // consume（CAS：`consumed = false` 且未过期且**属于本 adapter**）。
        let Some(stored) = tokens.get_mut(token_hash) else {
            return Ok(RedeemOutcome::TokenInvalid);
        };
        if stored.consumed || stored.expires_at <= now || stored.channel_type != "telegram" {
            return Ok(RedeemOutcome::TokenInvalid);
        }
        stored.consumed = true;
        let (workspace_id, installation_id, channel_user_id) = (
            stored.workspace_id,
            stored.installation_id,
            stored.channel_user_id.clone(),
        );
        drop(tokens);

        // 成员闸门（用例里 0 号 uuid 之外的都算成员）。
        if multica_user_id == Id(uuid::Uuid::nil()) {
            let mut tokens = self.tokens.lock().expect("tokens");
            if let Some(stored) = tokens.get_mut(token_hash) {
                stored.consumed = false;
            }
            return Ok(RedeemOutcome::NotMember);
        }

        // 绑定行：同一个 `(installation, 平台用户 id)` 唯一；已属于**另一个**用户则拒绝。
        let mut bindings = self.bindings.lock().expect("bindings");
        let key = (installation_id, channel_user_id.clone());
        match bindings.get(&key) {
            Some(existing) if *existing != multica_user_id => {
                drop(bindings);
                let mut tokens = self.tokens.lock().expect("tokens");
                if let Some(stored) = tokens.get_mut(token_hash) {
                    stored.consumed = false;
                }
                Ok(RedeemOutcome::AlreadyAssigned)
            }
            _ => {
                bindings.insert(key, multica_user_id);
                Ok(RedeemOutcome::Bound(RedeemedBinding {
                    workspace_id,
                    installation_id,
                    channel_user_id,
                }))
            }
        }
    }
}

fn stored(now: DateTime<Utc>, consumed: bool, channel_type: &str) -> StoredToken {
    StoredToken {
        consumed,
        expires_at: now + BINDING_TOKEN_TTL,
        workspace_id: Id::new(),
        installation_id: Id::new(),
        channel_user_id: "42".to_string(),
        channel_type: channel_type.to_string(),
    }
}

// ---------------------------------------------------------------------------
// 用例
// ---------------------------------------------------------------------------

/// 上游 `TestRandomBindingTokenIsUniqueAndURLSafe`：32 字节 → 43 个 base64url 字符，
/// 只含 `A-Za-z0-9-_`。
#[test]
fn random_tokens_are_unique_and_url_safe() {
    let mut seen = std::collections::HashSet::new();
    for _ in 0..64 {
        let token = random_binding_token();
        assert_eq!(token.len(), 43, "32 字节无填充 base64 = 43 字符：{token}");
        assert!(
            token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'),
            "不是 URL 安全字符集：{token}"
        );
        assert!(seen.insert(token), "令牌重复了");
    }
}

/// 上游 `TestHashBindingTokenIsDeterministicAndDoesNotRetainRawValue`。
#[tokio::test]
async fn hashing_is_deterministic_and_does_not_retain_the_raw_value() {
    let raw = "abc-DEF_ghi";
    let digest = hash_binding_token(raw);
    assert_eq!(digest, hash_binding_token(raw), "确定性");
    assert_eq!(digest.len(), 64, "sha256 的 hex");
    assert!(!digest.contains(raw));
    assert_ne!(digest, hash_binding_token("abc-DEF_ghj"));

    // 令牌的形态：明文只在 mint 的返回值里出现一次。
    let service = BindingTokenService::new(Arc::new(MemoryBindingStore::default()));
    let minted = service
        .mint(Id::new(), Id::new(), "42")
        .await
        .expect("mint");
    let rendered = format!("{minted:?}");
    assert_eq!(hash_binding_token(&minted.raw).len(), 64);
    assert!(
        rendered.contains(&minted.raw),
        "mint 的返回值本身就是唯一出口"
    );
}

/// 铸令牌：库里只有哈希、`expires_at = now + 15 分钟`、明文不在落库入参里。
#[tokio::test]
async fn minting_stores_only_the_hash_and_a_fifteen_minute_deadline() {
    let now = Utc::now();
    let store = Arc::new(MemoryBindingStore::with_now(now));
    let workspace_id = Id::new();
    let installation_id = Id::new();
    // 时钟注入同一个瞬时，`expires_at` 才是可断言的确定值。
    let service = BindingTokenService::new(store.clone()).with_now(Arc::new(move || now));
    let minted = service
        .mint(workspace_id, installation_id, "42")
        .await
        .expect("mint");
    assert_eq!(minted.expires_at, now + BINDING_TOKEN_TTL);
    assert_eq!(BINDING_TOKEN_TTL.num_minutes(), 15);

    let tokens = store.tokens.lock().expect("tokens");
    let (hash, stored) = tokens.iter().next().expect("one token");
    assert_eq!(hash, &hash_binding_token(&minted.raw), "库里存的是哈希");
    assert!(
        !hash.contains(&minted.raw),
        "落库键里出现了明文令牌：{hash}"
    );
    assert!(!stored.consumed);
    assert_eq!(stored.channel_type, "telegram");
    assert_eq!(stored.workspace_id, workspace_id);
    assert_eq!(stored.installation_id, installation_id);
    assert_eq!(stored.channel_user_id, "42");
}

/// 兑换的 happy path + **幂等**：第二次兑换同一枚令牌 ⇒ `TokenInvalid`，
/// 且 `channel_user_binding` 只有一行（本片的专属验收）。
#[tokio::test]
async fn redeeming_twice_is_single_use_and_never_duplicates_the_binding_row() {
    let now = Utc::now();
    let store = Arc::new(MemoryBindingStore::with_now(now));
    let service = BindingTokenService::new(store.clone());
    let installation_id = Id::new();
    let minted = service
        .mint(Id::new(), installation_id, "42")
        .await
        .expect("mint");

    let user = Id::new();
    let bound = service.redeem(&minted.raw, user).await.expect("redeem");
    let RedeemOutcome::Bound(bound) = bound else {
        panic!("expected Bound, got {bound:?}");
    };
    assert_eq!(bound.installation_id, installation_id);
    assert_eq!(bound.channel_user_id, "42", "令牌里的平台用户 id");
    assert_eq!(store.binding_count(), 1);

    let replay = service.redeem(&minted.raw, user).await.expect("replay");
    assert_eq!(replay, RedeemOutcome::TokenInvalid, "令牌是单次的");
    assert_eq!(store.binding_count(), 1, "重放**不**重复插行");
}

/// 同一个用户**再次**铸令牌并兑换到同一 `(installation, 平台用户 id)` ⇒ 原地更新，不插新行；
/// **另一个**用户来兑换 ⇒ `AlreadyAssigned`（转移必须显式解绑），且绑定行不变。
#[tokio::test]
async fn rebinding_the_same_user_is_idempotent_while_another_user_is_refused() {
    let now = Utc::now();
    let store = Arc::new(MemoryBindingStore::with_now(now));
    let service = BindingTokenService::new(store.clone());
    let installation_id = Id::new();
    let user = Id::new();
    let other = Id::new();

    for _ in 0..2 {
        let minted = service
            .mint(Id::new(), installation_id, "42")
            .await
            .expect("mint");
        let outcome = service.redeem(&minted.raw, user).await.expect("redeem");
        assert!(matches!(outcome, RedeemOutcome::Bound(_)));
        assert_eq!(store.binding_count(), 1, "同一个用户重绑不插新行");
    }

    let minted = service
        .mint(Id::new(), installation_id, "42")
        .await
        .expect("mint");
    let outcome = service.redeem(&minted.raw, other).await.expect("redeem");
    assert_eq!(outcome, RedeemOutcome::AlreadyAssigned);
    assert_eq!(store.binding_count(), 1);
    // 被拒之后令牌**没被烧掉**（回滚语义），同一个用户仍能兑换它。
    let outcome = service.redeem(&minted.raw, user).await.expect("redeem");
    assert!(matches!(outcome, RedeemOutcome::Bound(_)), "{outcome:?}");
}

/// 不存在 / 已消费 / 已过期 / **属于别的 adapter**：四种都回同一个不透明结果。
#[tokio::test]
async fn four_kinds_of_useless_tokens_collapse_into_one_opaque_outcome() {
    let now = Utc::now();
    let store = Arc::new(MemoryBindingStore::with_now(now));
    let service = BindingTokenService::new(store.clone());

    // 不存在。
    assert_eq!(
        service
            .redeem("never-minted", Id::new())
            .await
            .expect("redeem"),
        RedeemOutcome::TokenInvalid
    );
    // 已消费。
    store.seed("consumed", stored(now, true, "telegram"));
    assert_eq!(
        service.redeem("consumed", Id::new()).await.expect("redeem"),
        RedeemOutcome::TokenInvalid
    );
    // 已过期（`expires_at` 在过去）。
    let mut expired = stored(now, false, "telegram");
    expired.expires_at = now - Duration::minutes(1);
    store.seed("expired", expired);
    assert_eq!(
        service.redeem("expired", Id::new()).await.expect("redeem"),
        RedeemOutcome::TokenInvalid
    );
    // 别的 adapter 的令牌（令牌表跨 adapter 共享）。
    store.seed("slack-token", stored(now, false, "slack"));
    assert_eq!(
        service
            .redeem("slack-token", Id::new())
            .await
            .expect("redeem"),
        RedeemOutcome::TokenInvalid
    );
    assert_eq!(store.binding_count(), 0, "一次都不该插绑定行");
}

/// 非成员：`NotMember`，且令牌**没被烧掉**（上游逐字：回滚消费）。
#[tokio::test]
async fn a_non_member_attempt_does_not_burn_the_token() {
    let now = Utc::now();
    let store = Arc::new(MemoryBindingStore::with_now(now));
    let service = BindingTokenService::new(store.clone());
    let minted = service
        .mint(Id::new(), Id::new(), "42")
        .await
        .expect("mint");

    let outcome = service
        .redeem(&minted.raw, Id(uuid::Uuid::nil()))
        .await
        .expect("redeem");
    assert_eq!(outcome, RedeemOutcome::NotMember);
    assert_eq!(store.binding_count(), 0);

    // 成员随后仍能兑换同一枚令牌。
    let outcome = service
        .redeem(&minted.raw, Id::new())
        .await
        .expect("redeem");
    assert!(matches!(outcome, RedeemOutcome::Bound(_)), "{outcome:?}");
}

/// 存储层故障不透明（不回显 SQL / 哈希），且状态码是 500。
#[tokio::test]
async fn store_failures_are_opaque() {
    struct FailingStore;

    #[async_trait]
    impl BindingStore for FailingStore {
        async fn insert_token(&self, token: &NewBindingToken) -> Result<(), String> {
            Err(format!("boom on {}", token.token_hash))
        }

        async fn redeem_and_bind(
            &self,
            token_hash: &str,
            _multica_user_id: Id,
        ) -> Result<RedeemOutcome, String> {
            Err(format!("boom on {token_hash}"))
        }
    }

    let service = BindingTokenService::new(Arc::new(FailingStore));
    let mint_error = service
        .mint(Id::new(), Id::new(), "42")
        .await
        .expect_err("store error");
    assert_eq!(mint_error.http_status(), 500);
    assert_eq!(mint_error.code(), "telegram_binding_store_error");
    let redeem_error = service
        .redeem("raw", Id::new())
        .await
        .expect_err("store error");
    // 结构与码是稳定的（HTTP 层只回码与通用文案，**不**回底层消息）。
    for error in [&mint_error, &redeem_error] {
        assert_eq!(error.code(), "telegram_binding_store_error");
        assert!(matches!(error, BindingError::Store { .. }));
    }
    assert_eq!(redeem_error.http_status(), 500);
    // 承载的细节只留在**服务端**（`Debug` / `Display`）；它不带明文令牌。
    let rendered = format!("{mint_error} {redeem_error:?}");
    assert!(!rendered.contains("raw"), "{rendered}");
}

/// 判决 → 状态码：410 / 409 / 403（上游 `handler/telegram.go` 的 redeem switch）。
#[test]
fn outcomes_map_to_the_upstream_statuses() {
    assert_eq!(BindingError::TokenInvalid.http_status(), 410);
    assert_eq!(BindingError::AlreadyAssigned.http_status(), 409);
    assert_eq!(BindingError::NotMember.http_status(), 403);
    assert_eq!(
        BindingError::Store {
            message: "x".to_string()
        }
        .http_status(),
        500
    );
    let service = BindingTokenService::new(Arc::new(MemoryBindingStore::default()));
    assert!(format!("{service:?}").contains("dyn BindingStore"));
    assert!(
        format!("{service:?}").contains("<dyn"),
        "手写 Debug 只印端口存在性"
    );
}
