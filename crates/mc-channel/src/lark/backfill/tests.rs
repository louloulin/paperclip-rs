//! `lark::backfill` 的用例（写者 M7-14）。
//!
//! 两条回填都是"启动期一次性修复" ⇒ 这里钉三件事：**gating**（不该发 `UPDATE` 的时候
//! 一行都不发）、**逐行幂等/尽力而为**（一行坏不阻断后面的行）、以及 `union_id` **软失败**
//! （Lark 回空串 ⇒ 留 `NULL` 并记 warn，而不是当错误）。

use super::*;
use crate::lark::installation::tests::MemoryStore;
use crate::lark::installation::{Installation, LarkInstallationStore};
use crate::lark::registration::tests::FakeApi;
use crate::lark::types::{OpenId, Region};
use chrono::{TimeZone as _, Utc};
use mc_core::id::Id;
use mc_secrets::secretbox::SecretBox;
use pretty_assertions::assert_eq;

const PLAINTEXT: &str = "lark-app-secret-DO-NOT-LOG";

fn id(n: u8) -> Id {
    Id(uuid::Uuid::from_bytes([n; 16]))
}

/// 本片测试用的封装盒（回填必须**真的**解开密文才拿得到凭据 ⇒ 夹具的行要用它来封）。
fn boxed() -> SecretBox {
    SecretBox::new(&[9_u8; 32]).expect("32 字节密钥")
}

/// 一段**必须永不出现**在日志/返回面里的明文。
const SECRET: &str = "lark-app-secret-DO-NOT-LOG";

/// 一段用 [`boxed`] 封好的密文（回填路径上唯一合法的形状）。
fn sealed() -> Vec<u8> {
    boxed().seal(SECRET.as_bytes()).expect("封")
}

fn sample_row(id_value: u8, app_id: &str, union_id: Option<&str>) -> Installation {
    Installation {
        id: id(id_value),
        workspace_id: id(2),
        agent_id: id(3),
        app_id: app_id.to_string(),
        app_secret_encrypted: sealed(),
        tenant_key: None,
        bot_open_id: OpenId::new("ou_bot"),
        bot_union_id: union_id.map(str::to_string),
        region: Region::Feishu,
        installer_user_id: id(4),
        status: "active".to_string(),
        installed_at: Utc.timestamp_opt(1_700_000_000, 0).single().expect("时间"),
        created_at: Utc.timestamp_opt(1_700_000_000, 0).single().expect("时间"),
        updated_at: Utc.timestamp_opt(1_700_000_000, 0).single().expect("时间"),
    }
}

/// 一条把 `Arc<MemoryStore>` 当端口用的短句（`as` 会**移动** `Arc` ⇒ 每个调用点给一份 `clone`）。
fn port(store: &Arc<MemoryStore>) -> Arc<dyn LarkInstallationStore> {
    Arc::clone(store) as Arc<dyn LarkInstallationStore>
}

/// 造一个"密码文能被解开"的安装行（回填要真的解密才拿得到凭据）。
fn service_with(rows: Vec<Installation>) -> (Arc<MemoryStore>, InstallationService) {
    let store = Arc::new(MemoryStore::with_rows(rows));
    (store.clone(), InstallationService::new(store, boxed()))
}

// =====================================================================
// region 回填
// =====================================================================

#[test]
fn only_the_international_open_host_triggers_the_relabel() {
    assert!(is_lark_international_host("https://open.larksuite.com"));
    assert!(is_lark_international_host("https://OPEN.LARKSUITE.COM/"));
    assert!(is_lark_international_host("  https://open.larksuite.com  "));
    // 大陆主机 / mock / staging / 空值 / 坏 URL 一律**不**触发。
    assert!(!is_lark_international_host("https://open.feishu.cn"));
    assert!(!is_lark_international_host(""));
    assert!(!is_lark_international_host("   "));
    assert!(!is_lark_international_host("http://127.0.0.1:8080"));
    assert!(!is_lark_international_host("not a url"));
    // 逐字比 host ⇒ 后缀伪装不算（只比字符串的实现会踩到这一条）。
    assert!(!is_lark_international_host(
        "https://open.larksuite.com.evil.test"
    ));
    assert!(!is_lark_international_host(
        "https://evil.test/?open.larksuite.com"
    ));
}

#[tokio::test]
async fn a_mainland_deployment_never_issues_the_relabel_update() {
    let (store, _) = service_with(vec![sample_row(1, "cli_a", None)]);
    let rows = backfill_region_from_legacy_override(&port(&store), "", "https://open.feishu.cn")
        .await
        .expect("backfill");
    assert_eq!(rows, 0);
    // 行还是一行都没动。
    let listed = store.list_by_workspace(id(2)).await.expect("list");
    assert_eq!(listed[0].region, Region::Feishu);
}

#[tokio::test]
async fn an_international_override_relabels_every_still_default_row() {
    let (store, _) = service_with(vec![sample_row(1, "cli_a", None), {
        let mut row = sample_row(5, "cli_b", None);
        row.id = id(5);
        row
    }]);
    let rows =
        backfill_region_from_legacy_override(&port(&store), "https://open.larksuite.com", "")
            .await
            .expect("backfill");
    assert_eq!(rows, 2);
    // 幂等：再跑一次没有任何行需要翻。
    let again =
        backfill_region_from_legacy_override(&port(&store), "https://open.larksuite.com", "")
            .await
            .expect("backfill");
    assert_eq!(again, 0);
}

// =====================================================================
// union_id 回填
// =====================================================================

#[tokio::test]
async fn a_stub_client_skips_the_union_id_pass_entirely() {
    let (store, installs) = service_with(vec![sample_row(1, "cli_a", None)]);
    let api: Arc<dyn ApiClient> = Arc::new(FakeApi::default());
    let stats = backfill_bot_union_ids(&port(&store), &installs, &api)
        .await
        .expect("backfill");
    assert_eq!(stats, BackfillStats::default(), "未接线的部署一行都不该碰");
}

#[tokio::test]
async fn rows_that_already_have_a_union_id_are_not_re_fetched() {
    let (store, installs) = service_with(vec![
        sample_row(1, "cli_done", Some("on_done")),
        sample_row(6, "cli_missing", None),
    ]);
    let api = Arc::new(FakeApi::serving("ou_bot", "on_filled"));
    let stats = backfill_bot_union_ids(
        &port(&store),
        &installs,
        &(Arc::clone(&api) as Arc<dyn ApiClient>),
    )
    .await
    .expect("backfill");
    assert_eq!(stats.attempted, 1, "已有 union_id 的行不该被再问一次");
    assert_eq!(stats.filled, 1);
    assert_eq!(
        api.bot_info_calls.load(std::sync::atomic::Ordering::SeqCst),
        1
    );

    let listed = store.list_by_workspace(id(2)).await.expect("list");
    let filled = listed.iter().find(|row| row.id == id(6)).expect("行在");
    assert_eq!(filled.bot_union_id.as_deref(), Some("on_filled"));
}

#[tokio::test]
async fn an_absent_union_id_is_a_soft_failure_not_an_error() {
    // Lark 在通讯录范围受限时回 `code=0` + **空** `union_id` ⇒ 记 missed、留 NULL。
    let (store, installs) = service_with(vec![sample_row(1, "cli_a", None)]);
    let api = Arc::new(FakeApi::serving("ou_bot", ""));
    let stats = backfill_bot_union_ids(&port(&store), &installs, &(api as Arc<dyn ApiClient>))
        .await
        .expect("backfill");
    assert_eq!(stats.missed, 1);
    assert_eq!(stats.filled, 0);
    assert_eq!(stats.errored, 0);
    let listed = store.list_by_workspace(id(2)).await.expect("list");
    assert_eq!(listed[0].bot_union_id, None, "软失败不该写半点东西");
}

#[tokio::test]
async fn a_failing_row_does_not_block_the_other_rows() {
    // 一行解不开密文（坏密文）⇒ errored，但后面的行照跑。
    let broken = {
        let mut row = sample_row(1, "cli_broken", None);
        row.app_secret_encrypted = vec![0x01, 0x02, 0x03];
        row
    };
    let (store, installs) = service_with(vec![broken, sample_row(6, "cli_ok", None)]);
    let api = Arc::new(FakeApi::serving("ou_bot", "on_filled"));
    let stats = backfill_bot_union_ids(&port(&store), &installs, &(api as Arc<dyn ApiClient>))
        .await
        .expect("backfill");
    assert_eq!(stats.attempted, 2);
    assert_eq!(stats.errored, 1, "坏密文那一行算 errored");
    assert_eq!(stats.filled, 1, "后面的行必须照跑");
}

#[tokio::test]
async fn a_failing_api_marks_every_row_errored_without_aborting() {
    let (store, installs) = service_with(vec![sample_row(1, "cli_a", None)]);
    let api = Arc::new(FakeApi::failing());
    let stats = backfill_bot_union_ids(&port(&store), &installs, &(api as Arc<dyn ApiClient>))
        .await
        .expect("backfill");
    assert_eq!(stats.errored, 1);
    assert_eq!(stats.attempted, 1);
}

#[tokio::test]
async fn the_boot_entry_point_runs_both_passes_in_order() {
    let (store, installs) = service_with(vec![sample_row(1, "cli_a", None)]);
    let api = Arc::new(FakeApi::serving("ou_bot", "on_filled"));
    let stats = run_boot_backfills(
        &port(&store),
        &installs,
        &(api as Arc<dyn ApiClient>),
        "https://open.larksuite.com",
        "",
    )
    .await;
    assert_eq!(stats.filled, 1);
    let listed = store.list_by_workspace(id(2)).await.expect("list");
    assert_eq!(listed[0].region, Region::Lark, "region 回填先跑且生效");
    assert_eq!(listed[0].bot_union_id.as_deref(), Some("on_filled"));
}

#[test]
fn the_plaintext_never_reaches_the_stats() {
    // 回填的返回面是一个四整数结构 —— 钉住它没有变成别的形状（凭据纪律的回归护栏）。
    let stats = BackfillStats {
        attempted: 1,
        filled: 0,
        missed: 0,
        errored: 1,
    };
    let rendered = format!("{stats:?}");
    assert!(!rendered.contains(PLAINTEXT), "{rendered}");
    assert!(rendered.contains("errored: 1"), "{rendered}");
}
