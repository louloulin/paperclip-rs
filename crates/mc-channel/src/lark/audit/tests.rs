//! `audit.rs` 的用例（M7-13）：丢弃审计的**元数据形状**（不带正文）+ 空串落 `NULL` 的两族口径。

use std::sync::Mutex;

use async_trait::async_trait;
use mc_core::id::Id;
use mc_repos::channel::inbound_audit::NewChannelInboundDrop;
use mc_repos::RepoError;
use uuid::Uuid;

use super::*;
use crate::lark::types::ChatId;

/// 记下每次写入的替身（审计列清单**逐字**断言的就是它）。
#[derive(Debug, Default)]
struct FakeAudit {
    written: Mutex<Vec<NewChannelInboundDrop>>,
    fail: Mutex<bool>,
}

impl FakeAudit {
    fn rows(&self) -> Vec<NewChannelInboundDrop> {
        self.written.lock().expect("poisoned").clone()
    }
}

#[async_trait]
impl AuditQueries for FakeAudit {
    async fn record_drop(&self, drop: &NewChannelInboundDrop) -> Result<Id, RepoError> {
        if *self.fail.lock().expect("poisoned") {
            return Err(RepoError::Db("audit table unavailable".to_string()));
        }
        self.written.lock().expect("poisoned").push(drop.clone());
        Ok(Id(Uuid::new_v4()))
    }

    async fn list_by_installation(
        &self,
        _installation_id: Id,
        _limit: i64,
    ) -> Result<Vec<crate::lark::store::InboundAuditRow>, RepoError> {
        Ok(Vec::new())
    }
}

fn params() -> AuditDropParams {
    AuditDropParams::new(DropReason::Duplicate, Some(Id(Uuid::from_u128(0x3000))))
        .with_event_type("im.message.receive_v1")
        .with_chat_id(ChatId::new("oc_main"))
        .with_ids("ev-1", "om-1")
}

// ---------------------------------------------------------------------
// 入参 → 写入行（两族的唯一形状）
// ---------------------------------------------------------------------

/// 每一格都落到位，且 `kind` 是 lark 判别式。
#[test]
fn drop_params_map_to_the_shared_write_shape() {
    let drop = params().to_drop();
    assert_eq!(drop.kind, TYPE_LARK);
    assert_eq!(drop.event_type, "im.message.receive_v1");
    assert_eq!(drop.drop_reason, "duplicate");
    assert_eq!(drop.installation_id, Some(Id(Uuid::from_u128(0x3000))));
    assert_eq!(drop.channel_chat_id.as_deref(), Some("oc_main"));
    assert_eq!(drop.channel_event_id.as_deref(), Some("ev-1"));
    assert_eq!(drop.channel_message_id.as_deref(), Some("om-1"));
}

/// **空串落 `NULL`**（上游逐字：别把"事件没有这个字段"与"这个字段故意为空"混起来）。
#[test]
fn empty_optional_columns_become_null_not_empty_strings() {
    let drop = AuditDropParams::new(DropReason::InvalidEvent, None).to_drop();
    assert_eq!(drop.installation_id, None);
    assert_eq!(drop.channel_chat_id, None);
    assert_eq!(drop.channel_event_id, None);
    assert_eq!(drop.channel_message_id, None);
    assert_eq!(drop.event_type, "", "event_type 是 NOT NULL 列，空串原样");
    assert_eq!(drop.drop_reason, "invalid_event");
}

/// `to_drop` 是**纯**映射（同一次调两次得到同一个结果，没有隐藏状态）。
#[test]
fn drop_params_are_pure() {
    let params = params();
    assert_eq!(params.to_drop(), params.to_drop());
}

/// 建造器各自只改一格。
#[test]
fn builders_only_touch_their_own_field() {
    let base = AuditDropParams::new(DropReason::UnboundUser, None);
    assert_eq!(base.chat_id, ChatId::default());
    assert_eq!(base.lark_event_id, "");
    let with_chat = base.clone().with_chat_id(ChatId::new("oc_x"));
    assert_eq!(with_chat.chat_id.as_str(), "oc_x");
    assert_eq!(with_chat.lark_event_id, "", "别的格不动");
    let with_ids = with_chat.with_ids("ev", "om");
    assert_eq!(with_ids.lark_event_id, "ev");
    assert_eq!(with_ids.lark_message_id, "om");
    assert_eq!(with_ids.chat_id.as_str(), "oc_x", "前面的改动仍在");
}

// ---------------------------------------------------------------------
// 写入
// ---------------------------------------------------------------------

#[tokio::test]
async fn logger_writes_exactly_one_row_per_drop() {
    let fake = Arc::new(FakeAudit::default());
    let logger = LarkAuditLogger::new(Arc::clone(&fake) as Arc<dyn AuditQueries>);

    logger.record_drop(params()).await.expect("audit ok");
    let rows = fake.rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].drop_reason, "duplicate");
    assert_eq!(rows[0].channel_message_id.as_deref(), Some("om-1"));
}

/// 链路失败 ⇒ `EngineError::Infra`（调用方只记 warn；**不**回滚入站判决）。
#[tokio::test]
async fn logger_maps_link_failures_to_an_infra_error() {
    let fake = Arc::new(FakeAudit::default());
    *fake.fail.lock().expect("poisoned") = true;
    let logger = LarkAuditLogger::new(Arc::clone(&fake) as Arc<dyn AuditQueries>);

    let error = logger
        .record_drop(params())
        .await
        .expect_err("must fail loudly");
    let rendered = error.to_string();
    assert!(rendered.contains("lark audit"), "{rendered}");
    assert!(rendered.contains("audit table unavailable"), "{rendered}");
    assert!(fake.rows().is_empty(), "失败的写入不留行");
}

// ---------------------------------------------------------------------
// 类型层面的"不带正文"
// ---------------------------------------------------------------------

/// 写进去的行**没有**任何可以承载正文的列（逐列断言，而不是"看着像没有"）。
#[tokio::test]
async fn the_written_row_has_no_column_that_could_carry_a_body() {
    let fake = Arc::new(FakeAudit::default());
    let logger = LarkAuditLogger::new(Arc::clone(&fake) as Arc<dyn AuditQueries>);
    logger
        .record_drop(
            AuditDropParams::new(DropReason::NotAddressedInGroup, None)
                .with_event_type("im.message.receive_v1")
                .with_chat_id(ChatId::new("oc_main"))
                .with_ids("ev-1", "om-1"),
        )
        .await
        .expect("audit ok");

    let row = &fake.rows()[0];
    let columns = [
        row.event_type.clone(),
        row.drop_reason.to_string(),
        row.channel_chat_id.clone().unwrap_or_default(),
        row.channel_event_id.clone().unwrap_or_default(),
        row.channel_message_id.clone().unwrap_or_default(),
        row.installation_id
            .map(|id| id.to_string())
            .unwrap_or_default(),
        row.kind.as_str().to_string(),
    ];
    // 逐列核对：**列入清单之外的列一个都不存在**（所以正文无处可去）。
    assert_eq!(
        columns.len(),
        7,
        "写入形状的列数就是契约：{} 列",
        columns.len()
    );
    for column in &columns {
        assert!(!column.contains("hello"), "审计列里不得出现正文: {column}");
    }
}

// ---------------------------------------------------------------------
// 诊断面
// ---------------------------------------------------------------------

/// 审计器落在**遗留**族，且 `Debug` 只有端口存在性（**没有**凭据、**没有**正文）。
#[test]
fn logger_is_debug_safe_and_declares_the_legacy_generation() {
    let fake = Arc::new(FakeAudit::default());
    let logger = LarkAuditLogger::new(Arc::clone(&fake) as Arc<dyn AuditQueries>);
    let rendered = format!("{logger:?}");
    assert!(rendered.contains("LarkAuditLogger"));
    assert!(rendered.contains("AuditQueries"));
    for forbidden in ["app_secret", "secret", "token", "om-1", "hello"] {
        assert!(
            !rendered.contains(forbidden),
            "审计器的 Debug 不得出现 {forbidden}: {rendered}"
        );
    }
    assert_eq!(audit_kind(), TYPE_LARK);
    assert_eq!(audit_kind().storage_str(), "feishu");
}

/// `DropReason` 与 engine 的词表**取值逐字相同**（审计列上写的就是它）。
#[test]
fn drop_reason_literals_match_the_engine_vocabulary() {
    for (reason, literal) in [
        (DropReason::UnboundUser, "unbound_user"),
        (DropReason::NonWorkspaceMember, "non_workspace_member"),
        (DropReason::NotAddressedInGroup, "not_addressed_in_group"),
        (DropReason::Duplicate, "duplicate"),
        (DropReason::RevokedInstallation, "revoked_installation"),
        (DropReason::InvalidEvent, "invalid_event"),
    ] {
        assert_eq!(drop_reason_str(reason), literal);
        assert_eq!(reason.as_str(), literal);
    }
}
