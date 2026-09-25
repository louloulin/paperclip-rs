//! `wecom::installation` 的用例（写者 M7-15）。
//!
//! 拆出来是**门 ⑩**（单文件 800 行硬限）的要求（`docs/32` §31 的 D8）；切点是
//! 「实现 / 用例」。装置是**端口替身**（内存安装表 + 记账探针），不需要数据库：
//! 真库那一半在 `mc-http` 的 `wecom` 路由用例里（门 ⑥）。

use super::*;
use crate::wecom::credentials::PlaintextSecret;
use crate::wecom::store::test_support::MemoryInstallStore;
use crate::wecom::types::{encode_ciphertext, InstallConfig};
use async_trait::async_trait;
use mc_core::channel::InstallationStatus;
use mc_core::id::Id;
use pretty_assertions::assert_eq;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

const KEY_BYTES: [u8; 32] = [
    3, 1, 4, 1, 5, 9, 2, 6, 5, 3, 5, 8, 9, 7, 9, 3, 2, 3, 8, 4, 6, 2, 6, 4, 3, 3, 8, 3, 2, 7, 9, 5,
];
const PLAINTEXT: &str = "wecom-secret-DO-NOT-LOG";

fn boxed() -> SecretBox {
    SecretBox::new(&KEY_BYTES).expect("32 字节密钥")
}

/// 探针替身：记账调用次数（"被拒的请求不该碰 `WeCom`"的判据）。
struct SpyProbe {
    calls: AtomicUsize,
    verdict: Mutex<Result<(), ProbeError>>,
}

impl SpyProbe {
    fn accepting() -> Self {
        Self {
            calls: AtomicUsize::new(0),
            verdict: Mutex::new(Ok(())),
        }
    }

    fn rejecting(errcode: i32) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            verdict: Mutex::new(Err(ProbeError::Rejected { errcode })),
        }
    }

    fn unverifiable(errcode: i32) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            verdict: Mutex::new(Err(ProbeError::Unverifiable { errcode })),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl CredentialProbe for SpyProbe {
    async fn probe(&self, _bot_id: &str, _secret: &PlaintextSecret) -> Result<(), ProbeError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.verdict.lock().expect("lock")
    }
}

type Fixture = (Arc<MemoryInstallStore>, Arc<SpyProbe>, InstallationService);

fn fixture() -> Fixture {
    let store = Arc::new(MemoryInstallStore::new());
    let probe = Arc::new(SpyProbe::accepting());
    let service = InstallationService::new(
        Arc::clone(&store) as Arc<dyn InstallationStore>,
        Arc::clone(&probe) as Arc<dyn CredentialProbe>,
        boxed(),
    );
    (store, probe, service)
}

fn params(workspace_id: Id, agent_id: Id, bot_id: &str) -> InstallationParams {
    InstallationParams::new(
        workspace_id,
        agent_id,
        Id::new(),
        bot_id,
        PLAINTEXT,
        "Multica Bot",
    )
}

fn revoked_row(workspace_id: Id, agent_id: Id, bot_id: &str) -> Installation {
    let now = chrono::Utc::now();
    Installation {
        id: Id::new(),
        workspace_id,
        agent_id,
        installer_user_id: Id::new(),
        status: InstallationStatus::Revoked,
        bot_id: bot_id.to_string(),
        secret_encrypted: boxed().seal(b"old").expect("seal"),
        bot_display_name: "Old Bot".to_string(),
        config: serde_json::Value::Null,
        installed_at: now,
        created_at: now,
        updated_at: now,
    }
}

/// **本片专属验收第 1 条**：BYO 凭据落库 = `secretbox` 密文。
///
/// 反例（"明文入库即失败"）就在这里：落进 `config` 的 JSON 里**搜不到明文**，
/// 而那条密文用同一把部署密钥解得回明文。
#[tokio::test]
async fn the_pasted_secret_is_stored_only_as_ciphertext() {
    let (store, probe, service) = fixture();
    let (workspace_id, agent_id) = (Id::new(), Id::new());
    let installed = service
        .upsert(params(workspace_id, agent_id, "bot_1"))
        .await
        .expect("upsert");

    assert_eq!(probe.calls(), 1, "探针必须恰好跑一次");
    assert_eq!(installed.bot_id, "bot_1");
    assert!(installed.is_active());
    assert_eq!(installed.channel_type(), "wecom");

    // 反例：`config` 列里逐字搜明文 —— 搜到即失败。
    let persisted = store
        .last_config
        .lock()
        .expect("lock")
        .clone()
        .expect("persist 收到了 config");
    let encoded = persisted.to_string();
    assert!(!encoded.contains(PLAINTEXT), "明文进了 config：{encoded}");
    assert!(
        encoded.contains(&encode_ciphertext(&installed.secret_encrypted)),
        "密文列必须是 base64(`secretbox`)：{encoded}"
    );
    // 正向：同一把盒解得回明文（往返闭合）。
    assert_eq!(
        service
            .credentials(&installed)
            .expect("credentials")
            .secret
            .expose(),
        PLAINTEXT
    );
    // 路由键与业务字段同源。
    let config = InstallConfig::from_value(&persisted).expect("config");
    assert_eq!(config.app_id, "bot_1");
    assert_eq!(config.bot_id, "bot_1");
    assert_eq!(config.bot_display_name, "Multica Bot");
}

/// 三类槽位冲突各一条，且**被拒的请求一次探针都不发**（上游第 2 条判决的反例面）。
#[tokio::test]
async fn refused_installs_never_probe_wecom() {
    let workspace_id = Id::new();
    let cases = [
        (
            PersistOutcome::OwnedByAnotherWorkspace,
            InstallError::OwnedByAnotherWorkspace,
            false,
        ),
        (
            PersistOutcome::OwnedBySameWorkspace,
            InstallError::OwnedBySameWorkspace,
            false,
        ),
        (
            PersistOutcome::OwnedByArchivedAgent,
            InstallError::OwnedByArchivedAgent,
            true,
        ),
    ];
    for (expected_outcome, expected_error, archived) in cases {
        let (store, probe, service) = fixture();
        let holder_workspace = if expected_error == InstallError::OwnedByAnotherWorkspace {
            Id::new()
        } else {
            workspace_id
        };
        let holder_agent = Id::new();
        // 活跃的持有行（`active_row`）；归档那一栏再把它标成已归档。
        store.insert_row(active_row(holder_workspace, holder_agent, "bot_taken"));
        if archived {
            store.mark_archived(holder_workspace, holder_agent);
        }
        let error = service
            .upsert(params(workspace_id, Id::new(), "bot_taken"))
            .await
            .unwrap_err();
        assert_eq!(error, expected_error, "{expected_outcome:?}");
        assert_eq!(
            probe.calls(),
            0,
            "被拒的请求不得碰 WeCom（探针会踢掉在线持有者）"
        );
        assert_eq!(store.persist_calls(), 0, "被拒的请求不得写库");
    }
}

/// 死主（撤销 / 孤儿）与"自己的槽"都不算冲突 ⇒ 探针照跑、落库照走。
#[tokio::test]
async fn dead_owners_and_own_slots_do_not_conflict() {
    let workspace_id = Id::new();
    let agent_id = Id::new();

    // ① 撤销的槽（另一个 workspace 持有）：可抢。
    let (store, probe, service) = fixture();
    store.insert_row(revoked_row(Id::new(), Id::new(), "bot_revoked"));
    assert!(service
        .upsert(params(workspace_id, agent_id, "bot_revoked"))
        .await
        .is_ok());
    assert_eq!(probe.calls(), 1);

    // ② 孤儿（holder 的 workspace / agent 行已消失）：可抢。
    let (store, probe, service) = fixture();
    let orphan = active_row(Id::new(), Id::new(), "bot_orphan");
    store.mark_missing(orphan.workspace_id);
    store.insert_row(orphan);
    assert!(service
        .upsert(params(workspace_id, agent_id, "bot_orphan"))
        .await
        .is_ok());
    assert_eq!(probe.calls(), 1);

    // ③ 自己的槽（重装 / 轮换密钥）：原地刷新。
    let (store, probe, service) = fixture();
    store.insert_row(active_row(workspace_id, agent_id, "bot_mine"));
    assert!(service
        .upsert(params(workspace_id, agent_id, "bot_mine"))
        .await
        .is_ok());
    assert_eq!(probe.calls(), 1);
    assert_eq!(store.persist_calls(), 1);
}

/// **本片专属验收**：凭据被 `WeCom` 拒 ⇒ 400 语义，且**什么都没写**。
/// 够不着 `WeCom` ⇒ 503 语义（"输入没问题，是检查做不成"）。
#[tokio::test]
async fn probe_verdicts_decide_400_versus_503_and_never_write() {
    for (probe, expected, status) in [
        (
            Arc::new(SpyProbe::rejecting(40001)),
            InstallError::CredentialsRejected { errcode: 40001 },
            400,
        ),
        (
            Arc::new(SpyProbe::unverifiable(45009)),
            InstallError::CredentialsUnverifiable { errcode: 45009 },
            503,
        ),
    ] {
        let store = Arc::new(MemoryInstallStore::new());
        let service = InstallationService::new(
            Arc::clone(&store) as Arc<dyn InstallationStore>,
            Arc::clone(&probe) as Arc<dyn CredentialProbe>,
            boxed(),
        );
        let error = service
            .upsert(params(Id::new(), Id::new(), "bot_x"))
            .await
            .unwrap_err();
        assert_eq!(error, expected);
        assert_eq!(error.http_status(), status);
        assert_eq!(store.persist_calls(), 0, "凭据没过关就不该写库");
        assert_eq!(store.len(), 0);
    }
}

/// 显示名承接：同一个 bot 的轮换继承旧名；**换机器人不继承**。
#[tokio::test]
async fn display_name_is_carried_over_only_for_the_same_bot() {
    let workspace_id = Id::new();
    let agent_id = Id::new();
    let (store, _probe, service) = fixture();
    store.insert_row(active_row(workspace_id, agent_id, "bot_same"));

    // 留空 ⇒ 继承旧名。
    let mut blank = params(workspace_id, agent_id, "bot_same");
    blank.bot_display_name = String::new();
    let refreshed = service.upsert(blank).await.expect("refresh");
    assert_eq!(refreshed.bot_display_name, "The Bot");

    // 显式给名 ⇒ 用给的。
    let mut named = params(workspace_id, agent_id, "bot_same");
    named.bot_display_name = "Renamed".into();
    assert_eq!(
        service
            .upsert(named)
            .await
            .expect("rename")
            .bot_display_name,
        "Renamed"
    );

    // 换机器人 + 留空 ⇒ **不**继承旧名。
    let mut swapped = params(workspace_id, agent_id, "bot_other");
    swapped.bot_display_name = String::new();
    assert_eq!(
        service
            .upsert(swapped)
            .await
            .expect("swap")
            .bot_display_name,
        ""
    );
}

/// 预检矩阵：三个必填字段各一条，且都不碰端口。
#[tokio::test]
async fn validation_covers_every_required_field() {
    let (store, probe, service) = fixture();
    let mut missing_bot = params(Id::new(), Id::new(), "bot_1");
    missing_bot.bot_id = String::new();
    let mut missing_secret = params(Id::new(), Id::new(), "bot_1");
    missing_secret.secret = PlaintextSecret::new("");
    let mut missing_installer = params(Id::new(), Id::new(), "bot_1");
    missing_installer.installer_user_id = Id::nil();

    for (params, field) in [
        (missing_installer, "installer_user_id"),
        (missing_bot, "bot_id"),
        (missing_secret, "secret"),
    ] {
        assert_eq!(
            validate_installation_params(&params),
            Err(InstallError::InvalidParams { field })
        );
        assert_eq!(
            service.upsert(params).await.unwrap_err().code(),
            "wecom_install_rejected"
        );
    }
    assert_eq!(probe.calls(), 0);
    assert_eq!(store.persist_calls(), 0);
    // `New` 会 trim 两端空白（上游 `strings.TrimSpace`）⇒ 空白的 bot_id 等于空。
    assert_eq!(
        InstallationParams::new(Id::new(), Id::new(), Id::new(), "  ", "  ", "").bot_id,
        ""
    );
}

/// 读面：列表含 revoked、workspace 收窄、撤销翻状态、跨 workspace 读=不存在。
#[tokio::test]
async fn list_get_and_revoke_respect_the_workspace_scope() {
    let workspace_id = Id::new();
    let other = Id::new();
    let (store, _probe, service) = fixture();
    let row = active_row(workspace_id, Id::new(), "bot_a");
    let row_id = row.id;
    store.insert_row(row);
    store.insert_row(revoked_row(workspace_id, Id::new(), "bot_b"));
    store.insert_row(active_row(other, Id::new(), "bot_c"));

    assert_eq!(
        service
            .list_by_workspace(workspace_id)
            .await
            .expect("list")
            .len(),
        2
    );
    assert!(service.get_in_workspace(row_id, workspace_id).await.is_ok());
    assert_eq!(
        service.get_in_workspace(row_id, other).await.unwrap_err(),
        InstallError::NotFound
    );
    assert!(service.revoke(workspace_id, row_id).await.expect("revoke"));
    assert!(
        !service
            .revoke(workspace_id, row_id)
            .await
            .expect("revoke again"),
        "撤销是幂等的（第二次没有 active 行可翻）"
    );
    let after = service
        .get_in_workspace(row_id, workspace_id)
        .await
        .expect("get");
    assert_eq!(after.status, InstallationStatus::Revoked);
    assert!(!after.is_active());
}

/// 凭据纪律：`InstallationParams` 的 `Debug` 不回显明文；错误文案同样。
#[test]
fn debug_and_error_texts_never_echo_the_secret() {
    let params = InstallationParams::new(Id::new(), Id::new(), Id::new(), "bot_1", PLAINTEXT, "");
    let rendered = format!("{params:?}");
    assert!(rendered.contains("bot_1"));
    assert!(!rendered.contains(PLAINTEXT), "{rendered}");

    for error in [
        InstallError::CredentialsRejected { errcode: 40001 },
        InstallError::CredentialsUnverifiable { errcode: 45009 },
        InstallError::InvalidParams { field: "secret" },
        InstallError::Seal,
        InstallError::Encode,
        InstallError::Store {
            message: "connection refused".into(),
        },
    ] {
        let rendered = format!("{error:?}{error}");
        assert!(!rendered.contains(PLAINTEXT), "{rendered}");
    }
}

/// 活跃行装置（`status = active`）。
fn active_row(workspace_id: Id, agent_id: Id, bot_id: &str) -> Installation {
    let mut row = revoked_row(workspace_id, agent_id, bot_id);
    row.status = InstallationStatus::Active;
    row.bot_display_name = "The Bot".to_string();
    row
}
