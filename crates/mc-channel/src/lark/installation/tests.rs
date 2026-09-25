//! `lark::installation` 的用例（写者 M7-14）。
//!
//! 拆出来是**门 ⑩**（单文件 800 行硬限）的要求（`docs/32` §30 的 **D10**）；切点是
//! 「实现 / 用例」。装置是**端口替身**（内存安装表），不需要数据库 —— 真库那一半在
//! `mc-http` 的 lark 路由用例里（门 ⑥）。

use super::*;
use async_trait::async_trait;
use chrono::TimeZone as _;
use mc_core::channel::ChannelKind;
use pretty_assertions::assert_eq;
use std::sync::Mutex;

/// 32 字节测试密钥（**不是**任何部署在用的值）。
const KEY_BYTES: [u8; 32] = [
    7, 7, 4, 1, 9, 2, 6, 5, 3, 5, 8, 9, 7, 9, 3, 2, 3, 8, 4, 6, 2, 6, 4, 3, 3, 8, 3, 2, 7, 9, 5, 8,
];

/// 一段**必须永不出现**在任何 `{:?}` 输出里的明文。
const PLAINTEXT: &str = "lark-app-secret-DO-NOT-LOG";

fn boxed() -> SecretBox {
    SecretBox::new(&KEY_BYTES).expect("32 字节密钥")
}

fn id(n: u8) -> Id {
    Id(uuid::Uuid::from_bytes([n; 16]))
}

fn stamp(seconds: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(seconds, 0).single().expect("合法时间戳")
}

fn row(app_id: &str, status: &str) -> Installation {
    Installation {
        id: id(1),
        workspace_id: id(2),
        agent_id: id(3),
        app_id: app_id.to_string(),
        app_secret_encrypted: vec![0xAA; 30],
        tenant_key: Some("tk_1".to_string()),
        bot_open_id: OpenId::new("ou_bot"),
        bot_union_id: None,
        region: Region::Feishu,
        installer_user_id: id(4),
        status: status.to_string(),
        installed_at: stamp(1_700_000_000),
        created_at: stamp(1_700_000_000),
        updated_at: stamp(1_700_000_100),
    }
}

/// 内存安装表：**只**记账与返回预设值，不假装任何 SQL 语义。
///
/// `pub(crate)` 是因为 `registration` / `backfill` 的用例需要一个不碰库的
/// [`LarkInstallationStore`] —— 替身只有一个定义点。
#[derive(Default)]
pub(crate) struct MemoryStore {
    rows: Mutex<Vec<Installation>>,
    /// `persist` 的判决（默认 `Stored(第一行)`）。
    verdict: Mutex<Option<PersistOutcome>>,
    /// 记录 `persist` 收到的密文（断言"端口拿到的是封好的字节"）。
    seen_sealed: Mutex<Vec<u8>>,
    relabelled: Mutex<u64>,
    revoked: Mutex<u64>,
}

impl MemoryStore {
    pub(crate) fn with_rows(rows: Vec<Installation>) -> Self {
        Self {
            rows: Mutex::new(rows),
            ..Self::default()
        }
    }

    pub(crate) fn set_verdict(&self, outcome: PersistOutcome) {
        *self.verdict.lock().expect("锁") = Some(outcome);
    }

    pub(crate) fn sealed(&self) -> Vec<u8> {
        self.seen_sealed.lock().expect("锁").clone()
    }
}

#[async_trait]
impl LarkInstallationStore for MemoryStore {
    async fn list_by_workspace(&self, workspace_id: Id) -> Result<Vec<Installation>, String> {
        Ok(self
            .rows
            .lock()
            .expect("锁")
            .iter()
            .filter(|row| row.workspace_id == workspace_id)
            .cloned()
            .collect())
    }

    async fn get_in_workspace(
        &self,
        installation_id: Id,
        workspace_id: Id,
    ) -> Result<Option<Installation>, String> {
        Ok(self
            .rows
            .lock()
            .expect("锁")
            .iter()
            .find(|row| row.id == installation_id && row.workspace_id == workspace_id)
            .cloned())
    }

    async fn revoke(&self, workspace_id: Id, installation_id: Id) -> Result<bool, String> {
        let mut rows = self.rows.lock().expect("锁");
        let Some(row) = rows
            .iter_mut()
            .find(|row| row.id == installation_id && row.workspace_id == workspace_id)
        else {
            return Ok(false);
        };
        row.status = "revoked".to_string();
        *self.revoked.lock().expect("锁") += 1;
        Ok(true)
    }

    async fn persist(
        &self,
        params: &InstallationParams,
        sealed: &[u8],
    ) -> Result<PersistOutcome, String> {
        *self.seen_sealed.lock().expect("锁") = sealed.to_vec();
        // `PersistOutcome` 里带 `Box<Installation>` ⇒ 不 `Clone`；一个用例只调一次 `persist`，
        // 所以「取走」就是它需要的语义。
        if let Some(outcome) = self.verdict.lock().expect("锁").take() {
            return Ok(outcome);
        }
        let mut stored = row(&params.app_id, "active");
        stored.id = id(9);
        stored.workspace_id = params.workspace_id;
        stored.agent_id = params.agent_id;
        stored.app_secret_encrypted = sealed.to_vec();
        stored.bot_open_id = params.bot_open_id.clone();
        stored.bot_union_id = params.bot_union_id.clone();
        stored.region = params.region;
        Ok(PersistOutcome::Stored(Box::new(stored)))
    }

    async fn list_active_missing_union_id(&self) -> Result<Vec<Installation>, String> {
        Ok(self
            .rows
            .lock()
            .expect("锁")
            .iter()
            .filter(|row| row.is_active() && row.bot_union_id_or_empty().is_empty())
            .cloned()
            .collect())
    }

    async fn set_bot_union_id(&self, installation_id: Id, union_id: &str) -> Result<(), String> {
        let mut rows = self.rows.lock().expect("锁");
        if let Some(row) = rows.iter_mut().find(|row| row.id == installation_id) {
            row.bot_union_id = Some(union_id.to_string());
        }
        Ok(())
    }

    async fn relabel_region_to_lark(&self) -> Result<u64, String> {
        let mut rows = self.rows.lock().expect("锁");
        let mut count = 0_u64;
        for row in rows.iter_mut() {
            if row.region == Region::Feishu {
                row.region = Region::Lark;
                count += 1;
            }
        }
        *self.relabelled.lock().expect("锁") = count;
        Ok(count)
    }
}

fn service(store: std::sync::Arc<MemoryStore>) -> InstallationService {
    InstallationService::new(store, boxed())
}

fn params() -> InstallationParams {
    InstallationParams::new(
        id(2),
        id(3),
        "cli_app_1",
        PLAINTEXT,
        OpenId::new("ou_bot"),
        id(4),
    )
}

// =====================================================================
// 预检
// =====================================================================

#[test]
fn validate_reports_the_first_missing_field_in_upstream_order() {
    let ok = params();
    assert_eq!(validate_installation_params(&ok), Ok(()));

    // 上游 `validateInstallationParams` 的 switch 顺序：workspace → agent → installer →
    // app_id → app_secret → bot_open_id（**逐条同序**，所以只报第一个缺的）。
    let missing_ws = ok.clone().with_workspace(Id::nil());
    assert_eq!(
        validate_installation_params(&missing_ws),
        Err(InstallError::InvalidParams {
            field: "workspace_id"
        })
    );
    let missing_bot = ok.clone().with_bot_open_id(OpenId::default());
    assert_eq!(
        validate_installation_params(&missing_bot),
        Err(InstallError::InvalidParams {
            field: "bot_open_id"
        })
    );
}

#[test]
fn empty_optional_fields_normalise_to_none() {
    let blanked = params().with_tenant_key("   ").with_bot_union_id("");
    assert_eq!(blanked.tenant_key, None);
    assert_eq!(blanked.bot_union_id, None);
    let trimmed = params().with_bot_union_id(" on_1 ");
    assert_eq!(trimmed.bot_union_id.as_deref(), Some("on_1"));
}

#[tokio::test]
async fn upsert_surfaces_the_port_conflict_verbatim() {
    let store = std::sync::Arc::new(MemoryStore::default());
    store.set_verdict(PersistOutcome::Conflict(
        InstallError::OwnedByAnotherWorkspace,
    ));
    let service = service(store);
    assert_eq!(
        service.upsert(&params()).await,
        Err(InstallError::OwnedByAnotherWorkspace)
    );
}

// =====================================================================
// 凭据纪律（DoD 第 6 条）
// =====================================================================

#[test]
fn params_and_rows_never_render_credentials() {
    let rendered = format!("{:?}", params());
    assert!(
        !rendered.contains(PLAINTEXT),
        "InstallationParams 的 Debug 回显了明文 app_secret: {rendered}"
    );
    assert!(rendered.contains("<redacted>"), "{rendered}");

    let rendered = format!("{:?}", row("cli_app_1", "active"));
    assert!(
        rendered.contains("app_secret_encrypted_len"),
        "安装行的 Debug 应当只报密文长度: {rendered}"
    );
    assert!(!rendered.contains("AA"), "{rendered}");
    // 但 app_id 是**可以**打印的（上游日志逐字打印它）。
    assert!(rendered.contains("cli_app_1"), "{rendered}");
}

#[tokio::test]
async fn upsert_seals_before_the_store_and_never_passes_plaintext() {
    let store = std::sync::Arc::new(MemoryStore::default());
    let service = service(store.clone());
    let stored = service.upsert(&params()).await.expect("upsert");

    let sealed = store.sealed();
    assert!(!sealed.is_empty(), "端口必须拿到一段密文");
    assert!(
        !sealed
            .windows(PLAINTEXT.len())
            .any(|w| w == PLAINTEXT.as_bytes()),
        "密文里出现了明文"
    );
    // 真的能解回来 ⇒ 是同一把密钥封的。
    assert_eq!(
        service.decrypt_app_secret(&stored).expect("open").expose(),
        PLAINTEXT
    );
    assert_eq!(stored.app_id, "cli_app_1");
}

#[tokio::test]
async fn upsert_rejects_invalid_params_before_touching_the_store() {
    let store = std::sync::Arc::new(MemoryStore::default());
    let service = service(store.clone());
    let bad = params().with_workspace(Id::nil());
    assert_eq!(
        service.upsert(&bad).await,
        Err(InstallError::InvalidParams {
            field: "workspace_id"
        })
    );
    assert!(store.sealed().is_empty(), "被拒的请求不该封任何东西");
}

#[test]
fn decrypting_a_corrupt_ciphertext_is_an_opaque_seal_error() {
    let store = std::sync::Arc::new(MemoryStore::default());
    let service = service(store);
    let mut broken = row("cli_app_1", "active");
    broken.app_secret_encrypted = vec![0x01, 0x02, 0x03];
    let error = service.decrypt_app_secret(&broken).expect_err("必须失败");
    assert_eq!(error, InstallError::Seal);
    // 错误文案里**没有**任何密文片段。
    assert!(!error.to_string().contains("010203"), "{error}");
}

// =====================================================================
// 槽位分类（三档 + 竞态档）
// =====================================================================

#[test]
fn live_owner_classification_names_the_three_recoveries() {
    let mine = id(2);
    let other = id(9);
    assert_eq!(
        classify_live_owner(
            &LiveOwner {
                workspace_id: other,
                agent_archived: false
            },
            mine
        ),
        InstallError::OwnedByAnotherWorkspace
    );
    assert_eq!(
        classify_live_owner(
            &LiveOwner {
                workspace_id: mine,
                agent_archived: true
            },
            mine
        ),
        InstallError::OwnedByArchivedAgent
    );
    assert_eq!(
        classify_live_owner(
            &LiveOwner {
                workspace_id: mine,
                agent_archived: false
            },
            mine
        ),
        InstallError::OwnedBySameWorkspace
    );
}

#[test]
fn unclassified_conflict_is_its_own_bucket_not_another_workspace() {
    // 上游在读不到持有者时回"别人的 workspace"那句兜底文案；本仓单列一档，
    // 于是用例能钉住"竞态没有被伪装成三分类之一"。
    let outcome = PersistOutcome::UnclassifiedConflict;
    assert_eq!(
        outcome.into_result(),
        Err(InstallError::ConflictUnclassified)
    );
    assert_ne!(
        InstallError::ConflictUnclassified,
        InstallError::OwnedByAnotherWorkspace
    );
}

#[test]
fn persist_conflict_maps_to_the_classified_error() {
    let outcome = PersistOutcome::Conflict(InstallError::OwnedByArchivedAgent);
    assert_eq!(
        outcome.into_result(),
        Err(InstallError::OwnedByArchivedAgent)
    );
}

#[test]
fn error_codes_and_statuses_match_the_upstream_matrix() {
    assert_eq!(InstallError::NotFound.http_status(), 404);
    assert_eq!(
        InstallError::InvalidParams { field: "app_id" }.http_status(),
        400
    );
    for conflict in [
        InstallError::OwnedByAnotherWorkspace,
        InstallError::OwnedBySameWorkspace,
        InstallError::OwnedByArchivedAgent,
        InstallError::ConflictUnclassified,
    ] {
        assert_eq!(conflict.http_status(), 409, "{conflict}");
        assert!(conflict.code().starts_with("lark_app_"), "{conflict}");
    }
    assert_eq!(InstallError::NotWorkspaceMember.http_status(), 403);
    assert_eq!(
        InstallError::Store {
            message: "sqlstate 08006".to_string()
        }
        .http_status(),
        500
    );
}

// =====================================================================
// 读 / 撤销
// =====================================================================

#[tokio::test]
async fn list_and_revoke_are_workspace_scoped() {
    let store = std::sync::Arc::new(MemoryStore::with_rows(vec![row("cli_a", "active"), {
        let mut other = row("cli_b", "active");
        other.id = id(8);
        other.workspace_id = id(9);
        other
    }]));
    let service = service(store);

    let rows = service.list_by_workspace(id(2)).await.expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].app_id, "cli_a");

    // 另一个 workspace 猜 id ⇒ 与不存在**同一个**结果（不泄露存在性）。
    assert_eq!(
        service.get_in_workspace(id(1), id(9)).await,
        Err(InstallError::NotFound)
    );
    assert!(
        !service.revoke(id(9), id(1)).await.expect("revoke"),
        "另一个 workspace 猜 id 不该动到行"
    );
    let revoked = service.revoke(id(2), id(1)).await.expect("revoke");
    assert!(revoked);
    assert_eq!(
        service.list_by_workspace(id(2)).await.expect("list")[0].status,
        "revoked"
    );
}

#[tokio::test]
async fn store_failures_are_opaque() {
    struct Broken;
    #[async_trait]
    impl LarkInstallationStore for Broken {
        async fn list_by_workspace(&self, _workspace_id: Id) -> Result<Vec<Installation>, String> {
            Err("sqlstate 08006".to_string())
        }
        async fn get_in_workspace(
            &self,
            _installation_id: Id,
            _workspace_id: Id,
        ) -> Result<Option<Installation>, String> {
            Err("sqlstate 08006".to_string())
        }
        async fn revoke(&self, _workspace_id: Id, _installation_id: Id) -> Result<bool, String> {
            Err("sqlstate 08006".to_string())
        }
        async fn persist(
            &self,
            _params: &InstallationParams,
            _sealed: &[u8],
        ) -> Result<PersistOutcome, String> {
            Err("sqlstate 08006".to_string())
        }
        async fn list_active_missing_union_id(&self) -> Result<Vec<Installation>, String> {
            Err("sqlstate 08006".to_string())
        }
        async fn set_bot_union_id(
            &self,
            _installation_id: Id,
            _union_id: &str,
        ) -> Result<(), String> {
            Err("sqlstate 08006".to_string())
        }
        async fn relabel_region_to_lark(&self) -> Result<u64, String> {
            Err("sqlstate 08006".to_string())
        }
    }

    let service = InstallationService::new(std::sync::Arc::new(Broken), boxed());
    assert_eq!(
        service.list_by_workspace(id(2)).await,
        Err(InstallError::Store {
            message: "sqlstate 08006".to_string()
        })
    );
    assert_eq!(kind(), ChannelKind::Lark);
}
