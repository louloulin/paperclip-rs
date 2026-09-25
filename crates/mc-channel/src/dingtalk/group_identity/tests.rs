//! `group_identity.rs` 的用例（不依赖数据库、不依赖网络）。
//!
//! 三块各一组：① 查询计划的四条 400 分支；② 清单装配（分组 / 排序 / 分页 / 可见性）；
//! ③ bot 名解析（缓存、失败关闭、权限拒绝的跨群共享）。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use crate::dingtalk::client::BotNameApi;
use crate::dingtalk::resolvers::GroupPresenceObserver;
use crate::engine::resolvers::ResolvedInstallation;
use async_trait::async_trait;
use mc_core::channel::message::{ChatType, InboundMessage, MessageKind, Source};
use mc_core::channel::ChannelKind;

use super::*;
use crate::dingtalk::config::Credentials;

// =====================================================================
// 替身
// =====================================================================

/// 一个可编程的清单存储替身。
#[derive(Default)]
struct FakeInventory {
    inner: Mutex<FakeInventoryState>,
}

#[derive(Default)]
struct FakeInventoryState {
    presences: Vec<GroupPresenceRow>,
    counts: Vec<InactiveGroupCount>,
    identities: Vec<GroupIdentityRow>,
    may_list: bool,
    credentials: HashMap<String, Credentials>,
    failures: bool,
}

#[async_trait]
impl GroupInventoryStore for FakeInventory {
    async fn list_presences(&self, query: &PresenceQuery) -> Result<Vec<GroupPresenceRow>, String> {
        let state = self.inner.lock().expect("lock");
        if state.failures {
            return Err("boom".to_string());
        }
        let mut rows = state.presences.clone();
        if query.page_limit > 0 {
            let limit = usize::try_from(query.page_limit).unwrap_or(usize::MAX);
            rows.truncate(limit + 1);
        }
        Ok(rows)
    }

    async fn count_inactive(
        &self,
        _workspace_id: Id,
        _agent_id: Option<Id>,
        _active_since: DateTime<Utc>,
    ) -> Result<Vec<InactiveGroupCount>, String> {
        let state = self.inner.lock().expect("lock");
        if state.failures {
            return Err("boom".to_string());
        }
        Ok(state.counts.clone())
    }

    async fn list_bot_identities(
        &self,
        _workspace_id: Id,
        _agent_id: Option<Id>,
    ) -> Result<Vec<GroupIdentityRow>, String> {
        let state = self.inner.lock().expect("lock");
        if state.failures {
            return Err("boom".to_string());
        }
        Ok(state.identities.clone())
    }

    async fn may_list_inactive_installation(
        &self,
        _workspace_id: Id,
        _installation_id: Id,
        _agent_id: Option<Id>,
    ) -> Result<bool, String> {
        Ok(self.inner.lock().expect("lock").may_list)
    }

    async fn forget_presence(
        &self,
        _workspace_id: Id,
        _installation_id: Id,
        _conversation_id: &str,
    ) -> Result<bool, String> {
        Ok(self.inner.lock().expect("lock").may_list)
    }

    async fn credentials_by_app_key(&self, app_key: &str) -> Result<Option<Credentials>, String> {
        Ok(self
            .inner
            .lock()
            .expect("lock")
            .credentials
            .get(app_key)
            .cloned())
    }
}

/// 一个可编程的 bot 名 API 替身。
struct FakeBotNameApi {
    answer: Mutex<Result<String, crate::dingtalk::outbound::openapi::DingTalkApiError>>,
    calls: Mutex<usize>,
}

#[async_trait]
impl BotNameApi for FakeBotNameApi {
    async fn bot_name_in_group(
        &self,
        _app_key: &str,
        _app_secret: &crate::dingtalk::stream::AppSecret,
        _robot_code: &str,
        _conversation_id: &str,
    ) -> Result<String, crate::dingtalk::outbound::openapi::DingTalkApiError> {
        *self.calls.lock().expect("lock") += 1;
        self.answer.lock().expect("lock").clone()
    }
}

fn credentials(app_key: &str) -> Credentials {
    Credentials {
        app_key: app_key.to_string(),
        app_secret: crate::dingtalk::stream::AppSecret::new("s"),
        robot_code: app_key.to_string(),
    }
}

fn presence(
    installation_id: Id,
    agent_id: Id,
    conversation_id: &str,
    title: &str,
    last_active_at: Option<DateTime<Utc>>,
    mention_count: i64,
) -> GroupPresenceRow {
    GroupPresenceRow {
        installation_id,
        agent_id,
        conversation_id: conversation_id.to_string(),
        conversation_title: title.to_string(),
        bot_name: String::new(),
        bot_identity_issue: String::new(),
        last_active_at,
        mention_count,
    }
}

fn group_message(chat_id: &str, title: &str) -> InboundMessage {
    InboundMessage {
        event_id: "e1".to_string(),
        message_id: "m1".to_string(),
        source: Source {
            channel_type: ChannelKind::DingTalk,
            chat_id: chat_id.to_string(),
            chat_type: ChatType::Group,
            sender_id: "staff-1".to_string(),
            sender_stable_id: String::new(),
            thread_id: String::new(),
        },
        kind: MessageKind::Text,
        text: "hello".to_string(),
        command_text: String::new(),
        has_selected_context: false,
        media_refs: Vec::new(),
        reply_to: None,
        addressed_to_bot: true,
        force_fresh: false,
        skip_agent_run: false,
        raw: serde_json::json!({ "conversation_title": title }),
    }
}

// =====================================================================
// ① 查询计划
// =====================================================================

#[test]
fn the_query_plan_reproduces_the_four_upstream_400_branches() {
    let empty = GroupQuery::default();
    let plan = GroupQueryPlan::parse(false, &empty).expect("default");
    assert!(!plan.include_inactive);
    assert_eq!(plan.page_limit, 0);
    assert_eq!(plan.page_offset, 0);
    assert!(plan.installation_id.is_none());

    let bad_activity = GroupQuery {
        activity: "active".to_string(),
        ..GroupQuery::default()
    };
    assert_eq!(
        GroupQueryPlan::parse(false, &bad_activity),
        Err(GroupQueryError::Activity)
    );

    let no_installation = GroupQuery {
        activity: "inactive".to_string(),
        ..GroupQuery::default()
    };
    assert_eq!(
        GroupQueryPlan::parse(false, &no_installation),
        Err(GroupQueryError::InstallationRequired)
    );

    let bad_id = GroupQuery {
        activity: "inactive".to_string(),
        installation_id: "not-a-uuid".to_string(),
        ..GroupQuery::default()
    };
    assert_eq!(
        GroupQueryPlan::parse(false, &bad_id),
        Err(GroupQueryError::InstallationId)
    );

    let installation_id = Id::new();
    let bad_limit = GroupQuery {
        activity: "inactive".to_string(),
        installation_id: installation_id.to_string(),
        limit: "101".to_string(),
        ..GroupQuery::default()
    };
    assert_eq!(
        GroupQueryPlan::parse(false, &bad_limit),
        Err(GroupQueryError::Limit)
    );
    let bad_limit = GroupQuery {
        activity: "inactive".to_string(),
        installation_id: installation_id.to_string(),
        limit: "0".to_string(),
        ..GroupQuery::default()
    };
    assert_eq!(
        GroupQueryPlan::parse(false, &bad_limit),
        Err(GroupQueryError::Limit)
    );

    let bad_offset = GroupQuery {
        activity: "inactive".to_string(),
        installation_id: installation_id.to_string(),
        offset: "-1".to_string(),
        ..GroupQuery::default()
    };
    assert_eq!(
        GroupQueryPlan::parse(false, &bad_offset),
        Err(GroupQueryError::Offset)
    );

    let good = GroupQuery {
        activity: "inactive".to_string(),
        installation_id: installation_id.to_string(),
        limit: "5".to_string(),
        offset: "10".to_string(),
    };
    let plan = GroupQueryPlan::parse(true, &good).expect("ok");
    assert!(plan.filter_by_agent && plan.include_inactive);
    assert_eq!(plan.installation_id, Some(installation_id));
    assert_eq!(plan.page_limit, 5);
    assert_eq!(plan.page_offset, 10);

    // 每条错误码各不相同（别合并成一条"参数不对"）。
    let codes: HashSet<&str> = [
        GroupQueryError::Activity,
        GroupQueryError::InstallationRequired,
        GroupQueryError::InstallationId,
        GroupQueryError::Limit,
        GroupQueryError::Offset,
    ]
    .iter()
    .map(GroupQueryError::code)
    .collect();
    assert_eq!(codes.len(), 5);
}

// =====================================================================
// ② 清单装配
// =====================================================================

#[test]
fn groups_are_grouped_sorted_and_paginated_like_upstream() {
    let agent = Id::new();
    let other_agent = Id::new();
    let installation_a = Id::new();
    let installation_b = Id::new();
    let active = GroupQueryPlan {
        filter_by_agent: false,
        include_inactive: false,
        installation_id: None,
        page_limit: 0,
        page_offset: 0,
    };
    let rows = vec![
        presence(installation_b, agent, "c2", "Beta", None, 1),
        presence(installation_a, agent, "c1", "", None, 2),
        presence(installation_a, agent, "c1", "Alpha", None, 3),
        presence(installation_a, other_agent, "c3", "Gamma", None, 0),
    ];
    let counts = vec![InactiveGroupCount {
        installation_id: installation_a,
        agent_id: agent,
        group_count: 4,
    }];
    let identities = vec![GroupIdentityRow {
        installation_id: installation_a,
        agent_id: agent,
        bot_name: "My Bot".to_string(),
        bot_identity_issue: String::new(),
    }];
    let inventory = assemble_inventory::<std::collections::hash_map::RandomState>(
        &active,
        Some(agent),
        rows,
        counts,
        identities,
        None,
    );
    // `agent_id = Some(agent)` ⇒ 只留这个 agent 的行。
    assert_eq!(inventory.groups.len(), 2);
    assert_eq!(inventory.groups[0].conversation_id, "c1");
    assert_eq!(
        inventory.groups[0].conversation_title, "Alpha",
        "同群后面的行只在标题为空时补标题"
    );
    assert_eq!(inventory.groups[1].conversation_id, "c2");
    // c1 上有**两行**（同一安装的两次观察都会投影成一行 presence）⇒ 两个 bot 条目，
    // 顺序按 `installation_id` 稳定升序（上游 `sort.SliceStable`）。
    assert_eq!(inventory.groups[0].bots.len(), 2);
    assert_eq!(
        inventory.groups[0].bots[0].installation_id,
        installation_a.to_string()
    );
    assert_eq!(inventory.groups[0].bots[0].mention_count, 2);
    assert_eq!(inventory.groups[0].bots[1].mention_count, 3);
    assert!(inventory.group_discovery_supported);
    // 计数与身份都按可见性过滤。
    assert_eq!(inventory.inactive_group_counts.len(), 1);
    assert!(inventory
        .bot_identities
        .contains_key(&installation_a.to_string()));
    assert!(inventory.next_offset.is_none(), "活跃查询不分页");
    assert_eq!(inventory.groups[0].bots[0].last_active_at, "");

    // 空标题排最后（上游三级比较）。
    let rows = vec![
        presence(installation_a, agent, "c1", "", None, 0),
        presence(installation_a, agent, "c2", "Beta", None, 0),
    ];
    let inventory = assemble_inventory::<std::collections::hash_map::RandomState>(
        &active,
        None,
        rows,
        Vec::new(),
        Vec::new(),
        None,
    );
    assert_eq!(inventory.groups[0].conversation_id, "c2");
    assert_eq!(inventory.groups[1].conversation_id, "c1");
}

#[test]
fn visibility_filters_every_section_and_never_leaks_other_agents() {
    let visible_agent = Id::new();
    let hidden_agent = Id::new();
    let installation = Id::new();
    let visible: HashSet<Id> = [visible_agent].into_iter().collect();
    let plan = GroupQueryPlan {
        filter_by_agent: false,
        include_inactive: false,
        installation_id: None,
        page_limit: 0,
        page_offset: 0,
    };
    let inventory = assemble_inventory(
        &plan,
        None,
        vec![
            presence(installation, visible_agent, "c1", "Seen", None, 1),
            presence(installation, hidden_agent, "c2", "Hidden", None, 1),
        ],
        vec![InactiveGroupCount {
            installation_id: installation,
            agent_id: hidden_agent,
            group_count: 9,
        }],
        vec![GroupIdentityRow {
            installation_id: installation,
            agent_id: hidden_agent,
            bot_name: "Hidden Bot".to_string(),
            bot_identity_issue: String::new(),
        }],
        Some(&visible),
    );
    assert_eq!(inventory.groups.len(), 1);
    assert_eq!(inventory.groups[0].conversation_title, "Seen");
    assert!(inventory.inactive_group_counts.is_empty());
    assert!(inventory.bot_identities.is_empty());
}

#[test]
fn inactive_pagination_uses_a_next_offset_only_when_there_is_more() {
    let agent = Id::new();
    let installation = Id::new();
    let plan = GroupQueryPlan {
        filter_by_agent: false,
        include_inactive: true,
        installation_id: Some(installation),
        page_limit: 2,
        page_offset: 4,
    };
    let rows: Vec<GroupPresenceRow> = (0..3)
        .map(|index| {
            presence(
                installation,
                agent,
                &format!("c{index}"),
                &format!("T{index}"),
                None,
                0,
            )
        })
        .collect();
    let inventory = assemble_inventory::<std::collections::hash_map::RandomState>(
        &plan,
        None,
        rows,
        Vec::new(),
        Vec::new(),
        None,
    );
    assert_eq!(inventory.groups.len(), 2, "多取的那一行只用来判下一页");
    assert_eq!(inventory.next_offset, Some(6));

    let rows: Vec<GroupPresenceRow> = (0..2)
        .map(|index| {
            presence(
                installation,
                agent,
                &format!("c{index}"),
                &format!("T{index}"),
                None,
                0,
            )
        })
        .collect();
    let inventory = assemble_inventory::<std::collections::hash_map::RandomState>(
        &plan,
        None,
        rows,
        Vec::new(),
        Vec::new(),
        None,
    );
    assert!(inventory.next_offset.is_none());
}

#[test]
fn the_empty_inventory_is_discovery_supported_not_unsupported() {
    let inventory = GroupInventory::empty();
    assert!(inventory.groups.is_empty());
    assert!(
        inventory.group_discovery_supported,
        "与 lark 的 install_supported 语义不同"
    );
    assert!(inventory.inactive_group_counts.is_empty());
    assert!(inventory.bot_identities.is_empty());
    assert!(inventory.next_offset.is_none());
}

// =====================================================================
// ③ bot 名解析
// =====================================================================

fn resolver_with(
    answer: Result<String, crate::dingtalk::outbound::openapi::DingTalkApiError>,
) -> (Arc<FakeInventory>, Arc<FakeBotNameApi>, BotNameResolver) {
    let inventory = Arc::new(FakeInventory::default());
    inventory
        .inner
        .lock()
        .expect("lock")
        .credentials
        .insert("dingkey".to_string(), credentials("dingkey"));
    let api = Arc::new(FakeBotNameApi {
        answer: Mutex::new(answer),
        calls: Mutex::new(0),
    });
    let resolver = BotNameResolver::new(
        Arc::clone(&api) as Arc<dyn BotNameApi>,
        Arc::clone(&inventory) as Arc<dyn GroupInventoryStore>,
    );
    (inventory, api, resolver)
}

#[test]
fn a_resolved_name_is_cached_for_every_group_of_the_app() {
    let (_inventory, api, resolver) = resolver_with(Ok("My Bot".to_string()));
    assert_eq!(
        BotNameSource::bot_name(&resolver, "dingkey", "cid-1").as_deref(),
        Some("My Bot")
    );
    // 同一个应用的**另一个**群也命中同一条应用级缓存（上游逐字）。
    assert_eq!(
        BotNameSource::bot_name(&resolver, "dingkey", "cid-2").as_deref(),
        Some("My Bot")
    );
    assert_eq!(*api.calls.lock().expect("lock"), 1);
    let (name, issue) = resolver.describe("dingkey", "dingkey", "cid-3");
    assert_eq!(name, "My Bot");
    assert!(issue.is_empty());
}

#[test]
fn a_failure_is_fail_closed_and_never_invents_a_name() {
    let (_inventory, _api, resolver) = resolver_with(Err(
        crate::dingtalk::outbound::openapi::DingTalkApiError::InvalidTarget {
            reason: "robot is absent from the group bot list",
        },
    ));
    assert_eq!(BotNameSource::bot_name(&resolver, "dingkey", "cid-1"), None);
    // 群相关的失败只在**这一个**群缓存 ⇒ 另一个群会再打一次。
    assert_eq!(BotNameSource::bot_name(&resolver, "dingkey", "cid-2"), None);
    let (_name, issue) = resolver.describe("dingkey", "dingkey", "cid-3");
    assert!(issue.is_empty(), "非权限失败不写 bot_identity_issue");
}

#[test]
fn a_permission_denial_is_cached_across_every_group() {
    let (_inventory, api, resolver) = resolver_with(Err(
        crate::dingtalk::outbound::openapi::DingTalkApiError::Refused {
            path: "/v1.0/robot/groups/robots/query",
            code: "Forbidden.AccessDenied.AccessTokenPermissionDenied".to_string(),
        },
    ));
    assert_eq!(BotNameSource::bot_name(&resolver, "dingkey", "cid-1"), None);
    assert_eq!(BotNameSource::bot_name(&resolver, "dingkey", "cid-2"), None);
    assert_eq!(
        *api.calls.lock().expect("lock"),
        1,
        "权限是按应用的 ⇒ 跨群共享那条缓存（否则每条消息打一次 OpenAPI）"
    );
    let (_name, issue) = resolver.describe("dingkey", "dingkey", "cid-3");
    assert_eq!(issue, ISSUE_MISSING_CHAT_MANAGE);
}

#[test]
fn a_missing_installation_is_fail_closed_without_a_platform_call() {
    let inventory = Arc::new(FakeInventory::default());
    let api = Arc::new(FakeBotNameApi {
        answer: Mutex::new(Ok("never".to_string())),
        calls: Mutex::new(0),
    });
    let resolver = BotNameResolver::new(
        Arc::clone(&api) as Arc<dyn BotNameApi>,
        Arc::clone(&inventory) as Arc<dyn GroupInventoryStore>,
    );
    assert_eq!(BotNameSource::bot_name(&resolver, "unknown", "cid"), None);
    assert_eq!(*api.calls.lock().expect("lock"), 0);
}

// =====================================================================
// ④ 观察者
// =====================================================================

/// `GroupPresenceStore` 的替身（在 `group_identity/tests.rs` 内联，因为它只被本文件的
/// 观察者用例用）。
#[derive(Default)]
struct FakePresence {
    observed: Mutex<Vec<(String, String, String)>>,
    activities: Mutex<Vec<String>>,
    fail: bool,
}

#[async_trait]
impl GroupPresenceStore for FakePresence {
    async fn observe_presence(
        &self,
        _workspace_id: Id,
        _installation_id: Id,
        conversation_id: &str,
        conversation_title: &str,
        bot_name: &str,
        bot_identity_issue: &str,
    ) -> Result<(), String> {
        if self.fail {
            return Err("boom".to_string());
        }
        self.observed.lock().expect("lock").push((
            conversation_id.to_string(),
            conversation_title.to_string(),
            format!("{bot_name}/{bot_identity_issue}"),
        ));
        Ok(())
    }

    async fn observe_bot_identity(
        &self,
        _workspace_id: Id,
        _installation_id: Id,
        _bot_name: &str,
        _bot_identity_issue: &str,
    ) -> Result<(), String> {
        if self.fail {
            return Err("boom".to_string());
        }
        Ok(())
    }

    async fn record_activity(
        &self,
        _installation_id: Id,
        conversation_id: &str,
    ) -> Result<(), String> {
        if self.fail {
            return Err("boom".to_string());
        }
        self.activities
            .lock()
            .expect("lock")
            .push(conversation_id.to_string());
        Ok(())
    }
}

fn resolved_installation(app_key: &str) -> ResolvedInstallation {
    let row = super::super::resolvers::InstallationRow {
        id: Id::new(),
        workspace_id: Id::new(),
        agent_id: Id::new(),
        installer_user_id: Id::new(),
        status: "active".to_string(),
        config: serde_json::json!({ "app_id": app_key }),
    };
    ResolvedInstallation {
        id: row.id,
        workspace_id: row.workspace_id,
        agent_id: row.agent_id,
        installer_user_id: row.installer_user_id,
        active: true,
        kind: ChannelKind::DingTalk,
        platform: Some(Arc::new(row)),
    }
}

#[tokio::test]
async fn only_group_conversations_are_observed() {
    let store = Arc::new(FakePresence::default());
    let observer = PresenceObserver::new(Arc::clone(&store) as Arc<dyn GroupPresenceStore>, None);
    let installation = resolved_installation("dingkey");

    observer
        .observe(&installation, &group_message("cid-1", "Team"))
        .await
        .expect("observe");
    observer
        .record_activity(installation.id, &group_message("cid-1", "Team"))
        .await
        .expect("activity");

    let mut direct = group_message("cid-2", "");
    direct.source.chat_type = ChatType::P2p;
    observer.observe(&installation, &direct).await.expect("p2p");
    observer
        .record_activity(installation.id, &direct)
        .await
        .expect("p2p");

    let seen = store.observed.lock().expect("lock");
    assert_eq!(seen.len(), 1, "直聊没有群存在性这回事");
    assert_eq!(seen[0].0, "cid-1");
    assert_eq!(seen[0].1, "Team", "标题来自原始载荷");
    assert_eq!(
        store.activities.lock().expect("lock").len(),
        1,
        "活动计数同样只对群"
    );
}

#[tokio::test]
async fn observation_failures_never_fail_the_message() {
    let store = Arc::new(FakePresence {
        fail: true,
        ..FakePresence::default()
    });
    let observer = PresenceObserver::new(Arc::clone(&store) as Arc<dyn GroupPresenceStore>, None);
    let installation = resolved_installation("dingkey");
    // 上游逐字：观察是**尽力而为**的 —— 失败只记 warn，绝不改判决。
    observer
        .observe(&installation, &group_message("cid", "Team"))
        .await
        .expect("must stay Ok");
    observer
        .record_activity(installation.id, &group_message("cid", "Team"))
        .await
        .expect("must stay Ok");
}

#[tokio::test]
async fn the_observer_resolves_the_bot_name_through_the_resolver() {
    let inventory = Arc::new(FakeInventory::default());
    inventory
        .inner
        .lock()
        .expect("lock")
        .credentials
        .insert("dingkey".to_string(), credentials("dingkey"));
    let api = Arc::new(FakeBotNameApi {
        answer: Mutex::new(Ok("My Bot".to_string())),
        calls: Mutex::new(0),
    });
    let resolver = Arc::new(BotNameResolver::new(
        Arc::clone(&api) as Arc<dyn BotNameApi>,
        Arc::clone(&inventory) as Arc<dyn GroupInventoryStore>,
    ));
    let store = Arc::new(FakePresence::default());
    let observer = PresenceObserver::new(
        Arc::clone(&store) as Arc<dyn GroupPresenceStore>,
        Some(Arc::clone(&resolver)),
    );
    observer
        .observe(
            &resolved_installation("dingkey"),
            &group_message("cid-1", "Team"),
        )
        .await
        .expect("observe");
    let seen = store.observed.lock().expect("lock");
    assert_eq!(seen[0].2, "My Bot/");
}

#[test]
fn the_timestamp_helper_matches_the_upstream_rfc3339_form() {
    let value = chrono::DateTime::parse_from_rfc3339("2026-01-02T03:04:05Z")
        .expect("parse")
        .with_timezone(&Utc);
    assert_eq!(rfc3339(value), "2026-01-02T03:04:05Z");
    assert_eq!(
        active_since(value),
        value - chrono::Duration::days(ACTIVE_GROUP_WINDOW_DAYS)
    );
}
