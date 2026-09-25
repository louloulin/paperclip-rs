//! `slack::binding` 的用例（写者 M7-4）。
//!
//! 三条安全判据逐条钉住：**明文不进库**、**三种失败同一个不透明错误**、
//! **兑换原子**（非成员不烧令牌）。外加本片的专属验收：**重复兑换幂等**。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use mc_core::id::Id;

use super::*;

/// 一个内存替身：语义照上游 SQL（consume → membership → insert，同事务）。
/// 铸造与兑换共用同一个固定时刻（生产上两者都取自真时钟；用例把它钉住，
/// 否则"15 分钟寿命"这条判据只能靠 sleep 测）。
fn pinned_now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-09-25T00:00:00Z")
        .expect("ts")
        .with_timezone(&Utc)
}

#[derive(Default)]
struct FakeStore {
    tokens: Mutex<Vec<StoredToken>>,
    bindings: Mutex<Vec<(Id, String, Id)>>,
    members: Mutex<Vec<(Id, Id)>>,
    /// 是否让 consume 成功但后续一步失败（验证"回滚"）。
    fail_after_consume: Mutex<bool>,
}

#[derive(Clone)]
struct StoredToken {
    hash: String,
    workspace_id: Id,
    installation_id: Id,
    channel_user_id: String,
    expires_at: DateTime<Utc>,
    consumed: bool,
}

impl FakeStore {
    fn with_member(workspace_id: Id, user_id: Id) -> Self {
        let store = Self::default();
        store
            .members
            .lock()
            .expect("lock")
            .push((workspace_id, user_id));
        store
    }

    fn binding_rows(&self, installation_id: Id, channel_user_id: &str) -> usize {
        self.bindings
            .lock()
            .expect("lock")
            .iter()
            .filter(|(_, user, installation)| {
                user == channel_user_id && *installation == installation_id
            })
            .count()
    }
}

#[async_trait]
impl BindingStore for FakeStore {
    async fn insert_token(&self, token: &NewBindingToken) -> Result<(), String> {
        let mut tokens = self.tokens.lock().expect("lock");
        if tokens.iter().any(|stored| stored.hash == token.token_hash) {
            return Err("duplicate token hash".to_string());
        }
        tokens.push(StoredToken {
            hash: token.token_hash.clone(),
            workspace_id: token.workspace_id,
            installation_id: token.installation_id,
            channel_user_id: token.channel_user_id.clone(),
            expires_at: token.expires_at,
            consumed: false,
        });
        Ok(())
    }

    async fn redeem_and_bind(
        &self,
        token_hash: &str,
        multica_user_id: Id,
    ) -> Result<RedeemOutcome, String> {
        // 1) consume（单次性由 `consumed` 的 CAS 保证 —— 与上游 `UPDATE … WHERE consumed_at IS NULL` 同形）
        let consumed = {
            let mut tokens = self.tokens.lock().expect("lock");
            let Some(position) = tokens
                .iter()
                .position(|stored| stored.hash == token_hash && !stored.consumed)
            else {
                return Ok(RedeemOutcome::TokenInvalid);
            };
            if tokens[position].expires_at <= pinned_now() {
                return Ok(RedeemOutcome::TokenInvalid);
            }
            tokens[position].consumed = true;
            tokens[position].clone()
        };
        // 2) membership：不通过 ⇒ 回滚（把 consume 放开）后返回 NotMember
        let is_member = self
            .members
            .lock()
            .expect("lock")
            .iter()
            .any(|(workspace, user)| {
                *workspace == consumed.workspace_id && *user == multica_user_id
            });
        if !is_member {
            let mut tokens = self.tokens.lock().expect("lock");
            if let Some(stored) = tokens.iter_mut().find(|stored| stored.hash == token_hash) {
                stored.consumed = false;
            }
            return Ok(RedeemOutcome::NotMember);
        }
        if *self.fail_after_consume.lock().expect("lock") {
            let mut tokens = self.tokens.lock().expect("lock");
            if let Some(stored) = tokens.iter_mut().find(|stored| stored.hash == token_hash) {
                stored.consumed = false;
            }
            return Err("boom".to_string());
        }
        // 3) 已属于另一个用户 ⇒ AlreadyAssigned（上游是 `ON CONFLICT … WHERE` 拒绝）
        {
            let bindings = self.bindings.lock().expect("lock");
            if let Some((existing, _, _)) = bindings.iter().find(|(_, user, installation)| {
                user == &consumed.channel_user_id && *installation == consumed.installation_id
            }) {
                if *existing != multica_user_id {
                    let mut tokens = self.tokens.lock().expect("lock");
                    if let Some(stored) = tokens.iter_mut().find(|stored| stored.hash == token_hash)
                    {
                        stored.consumed = false;
                    }
                    return Ok(RedeemOutcome::AlreadyAssigned);
                }
            }
        }
        self.bindings.lock().expect("lock").push((
            multica_user_id,
            consumed.channel_user_id.clone(),
            consumed.installation_id,
        ));
        Ok(RedeemOutcome::Bound(RedeemedBinding {
            workspace_id: consumed.workspace_id,
            installation_id: consumed.installation_id,
            channel_user_id: consumed.channel_user_id,
        }))
    }
}

fn service(store: Arc<FakeStore>) -> BindingTokenService {
    BindingTokenService::new(store).with_now(Arc::new(pinned_now))
}

#[test]
fn the_raw_token_is_returned_once_and_only_its_hash_is_stored() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let store = Arc::new(FakeStore::default());
    let svc = service(Arc::clone(&store));
    let workspace = Id::new();
    let installation = Id::new();
    let minted = runtime
        .block_on(svc.mint(workspace, installation, "U1"))
        .expect("mint");

    // 明文是 base64url、无填充（上游 `RawURLEncoding`）。
    assert!(minted.raw.len() >= 42, "32 字节的 base64url 至少 43 字符");
    assert!(!minted.raw.contains('='));
    assert!(!minted.raw.contains('+'));
    assert!(!minted.raw.contains('/'));

    // 库里只有哈希，且哈希 = sha256(raw) 的 hex。
    let tokens = store.tokens.lock().expect("lock");
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens[0].hash, hash_binding_token(&minted.raw));
    assert_ne!(tokens[0].hash, minted.raw, "明文绝不入库");
    assert_eq!(tokens[0].hash.len(), 64, "sha256 hex");
    // 寿命 = 15 分钟（上游 TTL），且起点是注入的时钟。
    let expected = DateTime::parse_from_rfc3339("2026-09-25T00:15:00Z")
        .expect("ts")
        .with_timezone(&Utc);
    assert_eq!(minted.expires_at, expected);
}

#[test]
fn redeem_is_idempotent_and_never_inserts_a_second_binding_row() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let workspace = Id::new();
    let user = Id::new();
    let installation = Id::new();
    let store = Arc::new(FakeStore::with_member(workspace, user));
    let svc = service(Arc::clone(&store));

    let minted = runtime
        .block_on(svc.mint(workspace, installation, "U1"))
        .expect("mint");
    let first = runtime
        .block_on(svc.redeem(&minted.raw, user))
        .expect("first redeem");
    assert!(matches!(first, RedeemOutcome::Bound(_)));
    assert_eq!(store.binding_rows(installation, "U1"), 1);

    // 重复兑换：令牌已被消费 ⇒ TokenInvalid，**不**新增绑定行。
    let second = runtime
        .block_on(svc.redeem(&minted.raw, user))
        .expect("second redeem");
    assert_eq!(second, RedeemOutcome::TokenInvalid);
    assert_eq!(
        store.binding_rows(installation, "U1"),
        1,
        "幂等：重复兑换不重复插行"
    );
}

#[test]
fn a_non_member_does_not_burn_the_token() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let workspace = Id::new();
    let member = Id::new();
    let outsider = Id::new();
    let installation = Id::new();
    let store = Arc::new(FakeStore::with_member(workspace, member));
    let svc = service(Arc::clone(&store));

    let minted = runtime
        .block_on(svc.mint(workspace, installation, "U1"))
        .expect("mint");
    let refused = runtime
        .block_on(svc.redeem(&minted.raw, outsider))
        .expect("redeem");
    assert_eq!(refused, RedeemOutcome::NotMember);
    assert_eq!(store.binding_rows(installation, "U1"), 0);

    // 令牌没被烧掉 ⇒ 真正的成员还能兑换（上游注释逐字：returning before Commit rolls the
    // consume back）。
    let accepted = runtime
        .block_on(svc.redeem(&minted.raw, member))
        .expect("redeem");
    assert!(matches!(accepted, RedeemOutcome::Bound(_)));
    assert_eq!(store.binding_rows(installation, "U1"), 1);
}

#[test]
fn the_same_platform_user_cannot_be_transferred_to_another_account() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let workspace = Id::new();
    let first = Id::new();
    let second = Id::new();
    let installation = Id::new();
    let store = Arc::new(FakeStore::with_member(workspace, first));
    store
        .members
        .lock()
        .expect("lock")
        .push((workspace, second));
    let svc = service(Arc::clone(&store));

    let one = runtime
        .block_on(svc.mint(workspace, installation, "U1"))
        .expect("mint");
    assert!(matches!(
        runtime
            .block_on(svc.redeem(&one.raw, first))
            .expect("redeem"),
        RedeemOutcome::Bound(_)
    ));
    let two = runtime
        .block_on(svc.mint(workspace, installation, "U1"))
        .expect("mint");
    assert_eq!(
        runtime
            .block_on(svc.redeem(&two.raw, second))
            .expect("redeem"),
        RedeemOutcome::AlreadyAssigned,
        "转移必须走显式解绑"
    );
}

#[test]
fn store_failures_surface_as_an_opaque_error() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let workspace = Id::new();
    let user = Id::new();
    let installation = Id::new();
    let store = Arc::new(FakeStore::with_member(workspace, user));
    let svc = service(Arc::clone(&store));
    let minted = runtime
        .block_on(svc.mint(workspace, installation, "U1"))
        .expect("mint");
    *store.fail_after_consume.lock().expect("lock") = true;
    let error = runtime
        .block_on(svc.redeem(&minted.raw, user))
        .expect_err("存储故障");
    assert!(matches!(error, BindingError::Store { .. }));
    assert!(!error.to_string().contains(&minted.raw), "错误不回显明文");
}

#[test]
fn token_shape_is_32_bytes_of_base64url() {
    let raw = random_binding_token();
    assert_eq!(raw.len(), 43, "32 字节 base64url 无填充 = 43 字符");
    let other = random_binding_token();
    assert_ne!(raw, other, "两次铸造必须不同");
}
