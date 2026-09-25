//! `lark::binding` 的用例（写者 M7-14）。
//!
//! 装置是**端口替身**（内存令牌表 + 内存绑定表），不需要数据库 —— 真库那一半在
//! `mc-http` 的 `POST /api/lark/binding/redeem` 用例里（门 ⑥）。

use super::*;
use async_trait::async_trait;
use chrono::TimeZone as _;
use pretty_assertions::assert_eq;
use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Mutex;

fn id(n: u8) -> Id {
    Id(uuid::Uuid::from_bytes([n; 16]))
}

fn stamp(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(seconds, 0).single().expect("合法时间戳")
}

/// 内存绑定口：令牌表按哈希索引，绑定表按 `(installation, open_id)` 索引。
#[derive(Default)]
struct MemoryBindingStore {
    tokens: Mutex<HashMap<String, TokenRow>>,
    bindings: Mutex<HashMap<(uuid::Uuid, String), uuid::Uuid>>,
    members: Mutex<Vec<(uuid::Uuid, uuid::Uuid)>>,
    must_be_member: bool,
}

struct TokenRow {
    workspace_id: Id,
    installation_id: Id,
    open_id: String,
    expires_at: DateTime<Utc>,
    consumed: bool,
}

impl MemoryBindingStore {
    /// 一个"成员检查总是通过"的替身。
    fn permissive() -> Self {
        Self::default()
    }

    /// 一个"成员检查总是失败"的替身。
    fn member_gate() -> Self {
        Self {
            must_be_member: true,
            ..Self::default()
        }
    }

    fn tokens(&self) -> usize {
        self.tokens.lock().expect("锁").len()
    }
}

#[async_trait]
impl BindingStore for MemoryBindingStore {
    async fn insert_token(
        &self,
        token_hash: &str,
        workspace_id: Id,
        installation_id: Id,
        lark_open_id: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<(), String> {
        self.tokens.lock().expect("锁").insert(
            token_hash.to_string(),
            TokenRow {
                workspace_id,
                installation_id,
                open_id: lark_open_id.to_string(),
                expires_at,
                consumed: false,
            },
        );
        Ok(())
    }

    async fn redeem_and_bind(
        &self,
        token_hash: &str,
        multica_user_id: Id,
    ) -> Result<RedeemOutcome, String> {
        let mut tokens = self.tokens.lock().expect("锁");
        let Some(row) = tokens.get_mut(token_hash) else {
            return Ok(RedeemOutcome::TokenInvalid);
        };
        if row.consumed {
            return Ok(RedeemOutcome::TokenInvalid);
        }
        // 事务语义的替身：三个判决都**不**提交（`consumed` 保持 false）。
        if self.must_be_member
            && !self
                .members
                .lock()
                .expect("锁")
                .contains(&(row.workspace_id.0, multica_user_id.0))
        {
            return Ok(RedeemOutcome::NotWorkspaceMember);
        }
        let key = (row.installation_id.0, row.open_id.clone());
        if let Some(existing) = self.bindings.lock().expect("锁").get(&key) {
            if *existing != multica_user_id.0 {
                return Ok(RedeemOutcome::AlreadyAssigned);
            }
        }
        row.consumed = true;
        let bound = RedeemedBinding {
            workspace_id: row.workspace_id,
            installation_id: row.installation_id,
            lark_open_id: row.open_id.clone(),
        };
        self.bindings
            .lock()
            .expect("锁")
            .insert(key, multica_user_id.0);
        Ok(RedeemOutcome::Bound(bound))
    }
}

/// 假时钟（可推进）。
fn clock(offset: Arc<AtomicI64>) -> Arc<dyn Fn() -> DateTime<Utc> + Send + Sync> {
    Arc::new(move || {
        stamp(1_700_000_000) + chrono::Duration::seconds(offset.load(Ordering::SeqCst))
    })
}

// =====================================================================
// 令牌的形态
// =====================================================================

#[test]
fn tokens_are_url_safe_and_unguessable() {
    let first = random_binding_token();
    let second = random_binding_token();
    assert_ne!(first, second, "两次铸的令牌必须不同");
    assert!(
        first
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'),
        "令牌必须是 URL-safe 的（无需转义就能嵌进绑定 URL）: {first}"
    );
    // 32 字节 base64url 无填充 ⇒ 43 个字符。
    assert_eq!(first.len(), 43);
}

#[test]
fn the_hash_is_stable_and_never_equals_the_plaintext() {
    let raw = random_binding_token();
    assert_eq!(hash_token(&raw), hash_token(&raw));
    assert_ne!(hash_token(&raw), raw);
    // 64 个小写十六进制字符 = sha256。
    assert_eq!(hash_token(&raw).len(), 64);
    assert!(hash_token(&raw).chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(
        hash_token("abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

// =====================================================================
// 铸
// =====================================================================

#[tokio::test]
async fn mint_persists_only_the_hash_and_returns_the_plaintext_once() {
    let store = Arc::new(MemoryBindingStore::permissive());
    let service =
        BindingTokenService::with_clock(store.clone(), clock(Arc::new(AtomicI64::new(0))));

    let minted = service.mint(id(2), id(3), "ou_1").await.expect("mint");
    assert_eq!(store.tokens(), 1);
    let stored = store.tokens.lock().expect("锁").keys().next().cloned();
    assert_eq!(stored.as_deref(), Some(hash_token(&minted.raw).as_str()));
    assert_ne!(
        stored.as_deref(),
        Some(minted.raw.as_str()),
        "明文**绝不**落库"
    );
    // TTL 是 15 分钟（迁移 109 的 CHECK 也钉着同一个上界），落库的那一行与返回值同值。
    assert_eq!(minted.expires_at, stamp(1_700_000_000) + BINDING_TOKEN_TTL);
    let rows = store.tokens.lock().expect("锁");
    assert_eq!(
        rows.values().next().expect("一行").expires_at,
        minted.expires_at,
        "落库的过期时刻必须就是返回给用户的那一个（否则 UI 的\"15 分钟\"会撒谎）"
    );
}

#[tokio::test]
async fn mint_satisfies_the_replier_port() {
    // M7-13 的出站回复器只认 `BindingTokenMinter`；本服务必须满足它
    // （否则"点这里绑定"那张卡烘不出来）。
    let service = BindingTokenService::with_clock(
        Arc::new(MemoryBindingStore::permissive()),
        clock(Arc::new(AtomicI64::new(0))),
    );
    let port: Arc<dyn BindingTokenMinter> = Arc::new(service);
    let minted = port.mint(id(2), id(3), "ou_1").await.expect("mint");
    assert_eq!(minted.raw.len(), 43);
}

// =====================================================================
// 兑换
// =====================================================================

#[tokio::test]
async fn redeem_consumes_the_token_and_writes_the_binding() {
    let store = Arc::new(MemoryBindingStore::permissive());
    let service = BindingTokenService::with_clock(store, clock(Arc::new(AtomicI64::new(0))));
    let minted = service.mint(id(2), id(3), "ou_1").await.expect("mint");

    let bound = service
        .redeem_and_bind(&minted.raw, id(4))
        .await
        .expect("redeem");
    assert_eq!(bound.workspace_id, id(2));
    assert_eq!(bound.installation_id, id(3));
    assert_eq!(bound.lark_open_id, "ou_1");

    // 第二次兑换同一枚 ⇒ 已消费 ⇒ 410 那一档。
    assert_eq!(
        service.redeem_and_bind(&minted.raw, id(4)).await,
        Err(BindingError::TokenInvalid)
    );
}

#[tokio::test]
async fn an_unknown_token_is_the_same_error_as_a_consumed_one() {
    let service = BindingTokenService::with_clock(
        Arc::new(MemoryBindingStore::permissive()),
        clock(Arc::new(AtomicI64::new(0))),
    );
    let error = service
        .redeem_and_bind("never-minted", id(4))
        .await
        .expect_err("必须失败");
    assert_eq!(error, BindingError::TokenInvalid);
    // 文案里**没有**令牌本身（连哈希都没有）。
    assert!(!error.to_string().contains("never-minted"), "{error}");
}

#[tokio::test]
async fn a_foreign_holder_blocks_the_rebind_without_burning_the_token() {
    let store = Arc::new(MemoryBindingStore::permissive());
    let service =
        BindingTokenService::with_clock(store.clone(), clock(Arc::new(AtomicI64::new(0))));
    // 第一个用户先绑上。
    let first = service.mint(id(2), id(3), "ou_1").await.expect("mint");
    service
        .redeem_and_bind(&first.raw, id(4))
        .await
        .expect("redeem");

    // 第二个人拿到的令牌指向同一个 open_id ⇒ 409，且令牌**没有**被烧掉
    // （账号转移必须走显式解绑，不能靠一枚令牌抢）。
    let second = service.mint(id(2), id(3), "ou_1").await.expect("mint");
    assert_eq!(
        service.redeem_and_bind(&second.raw, id(5)).await,
        Err(BindingError::AlreadyAssigned)
    );
    let still_live = store.tokens.lock().expect("锁");
    let row = still_live.get(&hash_token(&second.raw)).expect("令牌还在");
    assert!(!row.consumed, "被拒的兑换不该烧掉令牌");
}

#[tokio::test]
async fn a_non_member_is_refused_and_the_token_survives() {
    let store = Arc::new(MemoryBindingStore::member_gate());
    let service =
        BindingTokenService::with_clock(store.clone(), clock(Arc::new(AtomicI64::new(0))));
    let minted = service.mint(id(2), id(3), "ou_1").await.expect("mint");
    assert_eq!(
        service.redeem_and_bind(&minted.raw, id(4)).await,
        Err(BindingError::NotWorkspaceMember)
    );
    assert!(
        !store
            .tokens
            .lock()
            .expect("锁")
            .get(&hash_token(&minted.raw))
            .expect("令牌还在")
            .consumed,
        "非成员的失败不该消费令牌（回滚语义）"
    );
}

// =====================================================================
// 错误矩阵
// =====================================================================

#[test]
fn error_matrix_matches_the_upstream_switch() {
    assert_eq!(BindingError::TokenInvalid.http_status(), 410);
    assert_eq!(BindingError::AlreadyAssigned.http_status(), 409);
    assert_eq!(BindingError::NotWorkspaceMember.http_status(), 403);
    assert_eq!(
        BindingError::Store {
            message: "sqlstate 08006".to_string()
        }
        .http_status(),
        500
    );
    assert_eq!(
        BindingError::from_redeem(&RedeemOutcome::Bound(RedeemedBinding {
            workspace_id: id(2),
            installation_id: id(3),
            lark_open_id: "ou_1".to_string(),
        })),
        None
    );
    assert_eq!(
        BindingError::from_redeem(&RedeemOutcome::TokenInvalid),
        Some(BindingError::TokenInvalid)
    );
}
