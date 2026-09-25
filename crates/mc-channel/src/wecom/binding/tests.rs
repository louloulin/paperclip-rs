//! `wecom::binding` 的用例（写者 M7-15）。
//!
//! 拆出来是**门 ⑩**（单文件 800 行硬限）的要求（`docs/32` §31 的 D8）；切点是
//! 「实现 / 用例」。装置是**内存绑定口**（含上游那三段事务的回滚语义），不需要数据库：
//! 真库那一半在 `mc-http` 的 `wecom` 路由用例里（门 ⑥）。

use super::*;
use pretty_assertions::assert_eq;
use std::collections::HashMap;
use std::sync::Mutex;

/// 令牌行（替身把 `created_at` 也留下 —— 节流窗口判据吃它）。
#[derive(Clone)]
struct TokenRow {
    workspace_id: Id,
    installation_id: Id,
    channel_user_id: String,
    expires_at: DateTime<Utc>,
    created_at: DateTime<Utc>,
    channel_type: &'static str,
}

/// 内存绑定口（形态照上游 `binding_test.go` 的 fake，含事务回滚语义）。
#[derive(Default)]
struct MemoryBindingStore {
    tokens: Mutex<HashMap<String, TokenRow>>,
    bindings: Mutex<HashMap<(Id, String), Id>>,
    non_members: Mutex<Vec<Id>>,
    /// 写过的哈希（"明文不进库"的判据）。
    pub(crate) inserted: Mutex<Vec<String>>,
}

impl MemoryBindingStore {
    fn new() -> Self {
        Self::default()
    }

    fn seed_token(&self, hash: &str, row: TokenRow) {
        self.tokens
            .lock()
            .expect("lock")
            .insert(hash.to_string(), row);
    }

    fn make_non_member(&self, user_id: Id) {
        self.non_members.lock().expect("lock").push(user_id);
    }

    fn has_token(&self, hash: &str) -> bool {
        self.tokens.lock().expect("lock").contains_key(hash)
    }

    fn bound_to(&self, installation_id: Id, channel_user_id: &str) -> Option<Id> {
        self.bindings
            .lock()
            .expect("lock")
            .get(&(installation_id, channel_user_id.to_string()))
            .copied()
    }
}

#[async_trait]
impl BindingStore for MemoryBindingStore {
    async fn insert_token(&self, token: &NewBindingToken) -> Result<(), String> {
        let mut inserted = self.inserted.lock().expect("lock");
        if inserted.contains(&token.token_hash) {
            return Err("duplicate token hash".to_string());
        }
        inserted.push(token.token_hash.clone());
        drop(inserted);
        self.seed_token(
            &token.token_hash,
            TokenRow {
                workspace_id: token.workspace_id,
                installation_id: token.installation_id,
                channel_user_id: token.channel_user_id.clone(),
                expires_at: token.expires_at,
                created_at: Utc::now(),
                channel_type: "wecom",
            },
        );
        Ok(())
    }

    async fn find_live_token(
        &self,
        installation_id: Id,
        channel_user_id: &str,
        mint_interval: Duration,
        now: DateTime<Utc>,
    ) -> Result<Option<DateTime<Utc>>, String> {
        let tokens = self.tokens.lock().expect("lock");
        Ok(tokens
            .values()
            .filter(|row| {
                row.installation_id == installation_id
                    && row.channel_user_id == channel_user_id
                    && row.expires_at > now
                    && row.created_at > now - mint_interval
            })
            .map(|row| row.expires_at)
            .max())
    }

    /// 事务语义逐条照抄：认不出（别的 adapter）与"非成员"都**把行放回去**
    /// —— 上游靠 `Rollback`，本替身靠显式回插，效果等价（判据 3 与 4）。
    async fn redeem_and_bind(
        &self,
        token_hash: &str,
        multica_user_id: Id,
    ) -> Result<RedeemOutcome, String> {
        let Some(row) = self.tokens.lock().expect("lock").remove(token_hash) else {
            return Ok(RedeemOutcome::TokenInvalid);
        };
        if row.channel_type != "wecom" {
            self.seed_token(token_hash, row);
            return Ok(RedeemOutcome::TokenInvalid);
        }
        if self
            .non_members
            .lock()
            .expect("lock")
            .contains(&multica_user_id)
        {
            self.seed_token(token_hash, row);
            return Ok(RedeemOutcome::NotMember);
        }
        let key = (row.installation_id, row.channel_user_id.clone());
        let mut bindings = self.bindings.lock().expect("lock");
        match bindings.get(&key) {
            Some(existing) if *existing != multica_user_id => {
                self.seed_token(token_hash, row);
                Ok(RedeemOutcome::AlreadyAssigned)
            }
            _ => {
                bindings.insert(key, multica_user_id);
                Ok(RedeemOutcome::Bound(RedeemedBinding {
                    workspace_id: row.workspace_id,
                    installation_id: row.installation_id,
                    channel_user_id: row.channel_user_id,
                }))
            }
        }
    }
}

fn row(
    workspace_id: Id,
    installation_id: Id,
    channel_user_id: &str,
    now: DateTime<Utc>,
    ttl: Duration,
) -> TokenRow {
    TokenRow {
        workspace_id,
        installation_id,
        channel_user_id: channel_user_id.to_string(),
        expires_at: now + ttl,
        created_at: now,
        channel_type: "wecom",
    }
}

fn service_with(store: &Arc<MemoryBindingStore>, now: DateTime<Utc>) -> BindingTokenService {
    BindingTokenService::new(Arc::clone(store) as Arc<dyn BindingStore>)
        .with_now(Arc::new(move || now))
}

/// 寿命上限与数据库的 `CHECK` 逐字一致（15 分钟），且节流窗口明显短于它。
#[test]
fn ttl_matches_the_check_constraint_and_is_longer_than_the_window() {
    assert_eq!(binding_token_ttl().as_seconds(), 900);
    assert_eq!(
        binding_token_ttl().as_seconds(),
        BindingTokenTtl::MAX_SECONDS
    );
    assert_eq!(BINDING_TOKEN_TTL, Duration::seconds(900));
    assert!(
        BINDING_TOKEN_MINT_INTERVAL.num_seconds() < 900,
        "节流窗口必须明显短于寿命，否则被指回去的链接可能正好过期"
    );
}

/// 铸令牌：明文只在返回值里；库里只有 `sha256(raw)` 的小写 hex；`expires_at` 精确可算。
#[tokio::test]
async fn minting_stores_only_the_hash() {
    let store = Arc::new(MemoryBindingStore::new());
    let now = Utc::now();
    let service = service_with(&store, now);
    let token = service
        .mint(Id::new(), Id::new(), "T-user-1")
        .await
        .expect("mint");
    assert!(!token.is_reused());
    assert_eq!(token.raw.len(), 43, "32 字节 base64url 无填充 = 43 字符");
    assert_eq!(token.expires_at, now + BINDING_TOKEN_TTL);
    assert_eq!(
        store.inserted.lock().expect("lock").clone(),
        vec![hash_binding_token(&token.raw)]
    );
    assert_eq!(hash_binding_token(&token.raw).len(), 64, "sha256 hex");
    assert!(
        store.has_token(&hash_binding_token(&token.raw)),
        "落库的是哈希形态"
    );
    assert!(!store.has_token(&token.raw), "明文不得作为存储键");
    // 两枚令牌互不相同（随机源没退化）。
    let second = service
        .mint(Id::new(), Id::new(), "T-user-2")
        .await
        .expect("mint");
    assert_ne!(second.raw, token.raw);
}

/// 节流：窗口内有活令牌 ⇒ 不写新行、`raw` 为空，且回的是**那条活令牌的**过期时间。
/// 窗口外的老令牌**不**抑制。
#[tokio::test]
async fn a_live_token_suppresses_the_mint_without_inventing_a_raw_value() {
    let now = Utc::now();
    let workspace_id = Id::new();
    let installation_id = Id::new();
    let live_expiry = now + Duration::seconds(800);

    // ① 窗口内（`created_at == now`）⇒ 抑制。
    let store = Arc::new(MemoryBindingStore::new());
    store.seed_token(
        "hash-of-the-live-one",
        TokenRow {
            expires_at: live_expiry,
            ..row(
                workspace_id,
                installation_id,
                "T-user-1",
                now,
                BINDING_TOKEN_TTL,
            )
        },
    );
    let service = service_with(&store, now);
    let token = service
        .mint(workspace_id, installation_id, "T-user-1")
        .await
        .expect("mint");
    assert!(token.is_reused());
    assert!(token.raw.is_empty(), "明文从未存下来 ⇒ 拿不回来");
    assert_eq!(token.expires_at, live_expiry, "回的是活令牌的过期时间");
    assert!(
        store.inserted.lock().expect("lock").is_empty(),
        "节流命中不得写新行"
    );

    // ② 窗口外（`created_at = now - 120s`）⇒ 允许再铸一枚。
    let stale = Arc::new(MemoryBindingStore::new());
    stale.seed_token(
        "hash-of-the-stale-one",
        TokenRow {
            created_at: now - Duration::seconds(120),
            ..row(
                workspace_id,
                installation_id,
                "T-user-1",
                now,
                BINDING_TOKEN_TTL,
            )
        },
    );
    let service = service_with(&stale, now);
    assert!(!service
        .mint(workspace_id, installation_id, "T-user-1")
        .await
        .expect("mint")
        .is_reused());
    assert_eq!(stale.inserted.lock().expect("lock").len(), 1);
}

/// 兑换的四种判决，以及"身份来自会话、不来自令牌"。
#[tokio::test]
async fn redeem_covers_every_outcome_and_takes_identity_from_the_session() {
    let store = Arc::new(MemoryBindingStore::new());
    let now = Utc::now();
    let service = service_with(&store, now);
    let workspace_id = Id::new();
    let installation_id = Id::new();
    let raw = "token-clicked-through";
    let raw_hash = hash_binding_token(raw);
    store.seed_token(
        &raw_hash,
        row(
            workspace_id,
            installation_id,
            "T-user-1",
            now,
            BINDING_TOKEN_TTL,
        ),
    );

    // 非成员 ⇒ 403 语义，且**不烧令牌**（判据 3）。
    let outsider = Id::new();
    store.make_non_member(outsider);
    assert_eq!(
        service.redeem(raw, outsider).await.expect("redeem"),
        RedeemOutcome::NotMember
    );
    assert!(store.has_token(&raw_hash), "非成员不得烧掉令牌");

    // 成员 ⇒ 绑好；绑的是**会话**身份（`T-user-1` 只做外部 id）。
    let member = Id::new();
    assert_eq!(
        service.redeem(raw, member).await.expect("redeem"),
        RedeemOutcome::Bound(RedeemedBinding {
            workspace_id,
            installation_id,
            channel_user_id: "T-user-1".into(),
        })
    );
    assert_eq!(store.bound_to(installation_id, "T-user-1"), Some(member));

    // 已消费 ⇒ 同一个不透明错误（重放拿不到信号）。
    assert_eq!(
        service.redeem(raw, member).await.expect("redeem"),
        RedeemOutcome::TokenInvalid
    );
    // 未知令牌 ⇒ 同一个结果。
    assert_eq!(
        service
            .redeem("never-issued", member)
            .await
            .expect("redeem"),
        RedeemOutcome::TokenInvalid
    );

    // 别的 adapter 的令牌 ⇒ 同一个结果，且**没被消费**（判据 4）。
    let foreign = "slack-token";
    let foreign_hash = hash_binding_token(foreign);
    store.seed_token(
        foreign_hash.as_str(),
        TokenRow {
            channel_type: "slack",
            ..row(
                workspace_id,
                installation_id,
                "U-slack",
                now,
                BINDING_TOKEN_TTL,
            )
        },
    );
    assert_eq!(
        service.redeem(foreign, member).await.expect("redeem"),
        RedeemOutcome::TokenInvalid
    );
    assert!(store.has_token(&foreign_hash), "别人的令牌不得被消费");

    // 这个 WeCom userid 已属于别人 ⇒ `AlreadyAssigned`。
    let second_raw = "second-token";
    store.seed_token(
        &hash_binding_token(second_raw),
        row(
            workspace_id,
            installation_id,
            "T-user-1",
            now,
            BINDING_TOKEN_TTL,
        ),
    );
    assert_eq!(
        service.redeem(second_raw, Id::new()).await.expect("redeem"),
        RedeemOutcome::AlreadyAssigned
    );
}

/// 错误映射：410 / 409 / 403 / 500，且四个文案都不含明文令牌。
#[test]
fn error_mapping_and_texts() {
    for (outcome, expected, status) in [
        (RedeemOutcome::TokenInvalid, BindingError::TokenInvalid, 410),
        (
            RedeemOutcome::AlreadyAssigned,
            BindingError::AlreadyAssigned,
            409,
        ),
        (RedeemOutcome::NotMember, BindingError::NotMember, 403),
    ] {
        let mapped = BindingError::from_redeem(&outcome).expect("错误");
        assert_eq!(mapped, expected);
        assert_eq!(mapped.http_status(), status);
        assert_eq!(mapped.code(), expected.code());
    }
    assert!(
        BindingError::from_redeem(&RedeemOutcome::Bound(RedeemedBinding {
            workspace_id: Id::new(),
            installation_id: Id::new(),
            channel_user_id: "T".into(),
        }))
        .is_none()
    );
    let store_error = BindingError::Store {
        message: "connection refused".into(),
    };
    assert_eq!(store_error.http_status(), 500);

    for error in [
        BindingError::TokenInvalid,
        BindingError::AlreadyAssigned,
        BindingError::NotMember,
        store_error,
    ] {
        let rendered = format!("{error:?}{error}");
        assert!(!rendered.contains("token-clicked-through"), "{rendered}");
    }
}

/// 凭据纪律：`BindingToken` 的 `Debug` 不回显明文；服务的 `Debug` 不打印端口。
#[test]
fn debug_never_exposes_the_raw_token() {
    let token = BindingToken {
        raw: "raw-DO-NOT-LOG".into(),
        expires_at: Utc::now(),
        reused: false,
    };
    let rendered = format!("{token:?}");
    assert!(rendered.contains("<redacted>"), "{rendered}");
    assert!(!rendered.contains("DO-NOT-LOG"), "{rendered}");
    assert_eq!(
        format!(
            "{:?}",
            BindingTokenService::new(Arc::new(MemoryBindingStore::new()))
        ),
        "BindingTokenService { store: \"<dyn BindingStore>\" }"
    );
}
