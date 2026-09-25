//! `slack::slash` 的用例（写者 M7-4）。
//!
//! 三条上游纪律逐条钉住：**ACK 已由传输层做完 ⇒ 处理器从不返回错误**、
//! **控制命令按信封 id 去重**、**频道里不做控制命令**。外加 `/issue` 的四种结局与
//! 绑定卡的退路。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use mc_core::id::Id;
use mc_repos::channel::binding::ChannelUserBindingRow;
use mc_repos::RepoError;
use serde_json::Value;
use uuid::Uuid;

use super::*;
use crate::engine::resolvers::{EngineError, PipelineError};
use crate::slack::resolvers::InstallationRow;

// =====================================================================
// 替身
// =====================================================================

#[derive(Default)]
struct FakeInstallations {
    row: Mutex<Option<InstallationRow>>,
}

#[async_trait]
impl InstallationQueries for FakeInstallations {
    async fn find_active_by_app_id(
        &self,
        _app_id: &str,
    ) -> Result<Option<InstallationRow>, RepoError> {
        Ok(self.row.lock().expect("lock").clone())
    }
}

#[derive(Default)]
struct FakeIdentities {
    binding: Mutex<Option<Uuid>>,
    member: Mutex<bool>,
}

#[async_trait]
impl IdentityQueries for FakeIdentities {
    async fn find_user_binding(
        &self,
        installation_id: Id,
        channel_user_id: &str,
    ) -> Result<Option<ChannelUserBindingRow>, RepoError> {
        let Some(user) = *self.binding.lock().expect("lock") else {
            return Ok(None);
        };
        Ok(Some(ChannelUserBindingRow {
            id: Uuid::new_v4(),
            workspace_id: Uuid::nil(),
            multica_user_id: user,
            installation_id: installation_id.0,
            channel_type: "slack".to_string(),
            channel_user_id: channel_user_id.to_string(),
            config: Value::Null,
            bound_at: chrono::Utc::now(),
        }))
    }

    async fn is_workspace_member(
        &self,
        _workspace_id: Id,
        _user_id: Id,
    ) -> Result<bool, RepoError> {
        Ok(*self.member.lock().expect("lock"))
    }

    async fn upsert_user_binding(
        &self,
        _workspace_id: Id,
        _user_id: Id,
        _installation_id: Id,
        _channel_user_id: &str,
    ) -> Result<(), RepoError> {
        Ok(())
    }
}

#[derive(Default)]
struct FakeQuickCreate {
    calls: Mutex<Vec<QuickCreateParams>>,
    outcome: Mutex<u8>,
}

#[async_trait]
impl QuickCreateEnqueuer for FakeQuickCreate {
    async fn enqueue_quick_create(
        &self,
        params: &QuickCreateParams,
    ) -> Result<(), QuickCreateError> {
        self.calls.lock().expect("lock").push(params.clone());
        match *self.outcome.lock().expect("lock") {
            1 => Err(QuickCreateError::IssueLimitReached),
            2 => Err(QuickCreateError::Other {
                message: "boom".to_string(),
            }),
            _ => Ok(()),
        }
    }
}

#[derive(Default)]
struct FakeResponder {
    sent: Mutex<Vec<(String, String)>>,
}

#[async_trait]
impl EphemeralResponder for FakeResponder {
    async fn respond(&self, response_url: &str, text: &str) -> Result<(), String> {
        self.sent
            .lock()
            .expect("lock")
            .push((response_url.to_string(), text.to_string()));
        Ok(())
    }
}

#[derive(Default)]
struct FakeControl {
    starts: Mutex<Vec<String>>,
    fail_with_duplicate: Mutex<bool>,
    fail_other: Mutex<bool>,
}

#[async_trait]
impl ControlStarter for FakeControl {
    async fn start_dm_chat(
        &self,
        _installation: &ResolvedInstallation,
        _user_id: Id,
        command: &SlashCommand,
        envelope_id: &str,
    ) -> Result<(), EngineError> {
        self.starts
            .lock()
            .expect("lock")
            .push(format!("new:{envelope_id}:{}", command.text));
        if *self.fail_with_duplicate.lock().expect("lock") {
            return Err(PipelineError::Duplicate.into());
        }
        if *self.fail_other.lock().expect("lock") {
            return Err(EngineError::infra("db down"));
        }
        Ok(())
    }

    async fn clear_dm_context(
        &self,
        _installation: &ResolvedInstallation,
        _user_id: Id,
        command: &SlashCommand,
        envelope_id: &str,
    ) -> Result<(), EngineError> {
        self.starts
            .lock()
            .expect("lock")
            .push(format!("clear:{envelope_id}:{}", command.text));
        Ok(())
    }
}

struct Harness {
    processor: Arc<SlashCommandProcessor>,
    installations: Arc<FakeInstallations>,
    tasks: Arc<FakeQuickCreate>,
    control: Arc<FakeControl>,
    responder: Arc<FakeResponder>,
}

fn harness(binding: Option<Uuid>, member: bool) -> Harness {
    let installation = InstallationRow {
        id: Id::new(),
        workspace_id: Id::new(),
        agent_id: Id::new(),
        installer_user_id: Id::new(),
        status: "active".to_string(),
        config: serde_json::json!({ "app_id": "A1", "team_id": "T1" }),
    };
    let installations = Arc::new(FakeInstallations {
        row: Mutex::new(Some(installation)),
    });
    let identities = Arc::new(FakeIdentities {
        binding: Mutex::new(binding),
        member: Mutex::new(member),
    });
    let tasks = Arc::new(FakeQuickCreate::default());
    let control = Arc::new(FakeControl::default());
    let responder = Arc::new(FakeResponder::default());
    let processor = Arc::new(SlashCommandProcessor::with_responder(
        SlashCommandDeps {
            installations: Arc::clone(&installations) as Arc<dyn InstallationQueries>,
            identities: Arc::clone(&identities) as Arc<dyn IdentityQueries>,
            tasks: Arc::clone(&tasks) as Arc<dyn QuickCreateEnqueuer>,
            control: Some(Arc::clone(&control) as Arc<dyn ControlStarter>),
            binding: None,
            responder: Arc::clone(&responder) as Arc<dyn EphemeralResponder>,
            app_url: "https://app.example".to_string(),
            binding_path: None,
        },
        Arc::clone(&responder) as Arc<dyn EphemeralResponder>,
    ));
    Harness {
        processor,
        installations,
        tasks,
        control,
        responder,
    }
}

fn command(verb: &str, channel: &str, text: &str) -> SlashCommand {
    SlashCommand {
        command: verb.to_string(),
        text: text.to_string(),
        api_app_id: "A1".to_string(),
        team_id: "T1".to_string(),
        channel_id: channel.to_string(),
        user_id: "U1".to_string(),
        response_url: "https://hooks.slack.com/commands/T1/1/2".to_string(),
    }
}

async fn reply(harness: &Harness, command: &SlashCommand) -> String {
    harness.processor.handle(command, "ENV1").await;
    harness
        .responder
        .sent
        .lock()
        .expect("lock")
        .last()
        .map(|(_, text)| text.clone())
        .unwrap_or_default()
}

// =====================================================================
// 载荷
// =====================================================================

#[test]
fn a_payload_without_a_command_is_not_a_command() {
    assert!(SlashCommand::from_payload(&serde_json::json!({ "text": "hi" })).is_none());
    let parsed =
        SlashCommand::from_payload(&serde_json::json!({ "command": "/issue", "text": "x" }))
            .expect("解析");
    assert_eq!(parsed.command, "/issue");
    assert!(parsed.recognised());
    assert!(!parsed.is_control());
    assert!(SlashCommand::from_payload(&serde_json::json!({"command": "/new"})).is_some());
    assert!(
        !SlashCommand::from_payload(&serde_json::json!({"command": "/unknown"}))
            .expect("解析")
            .recognised()
    );
    assert_eq!(commands(), ["/issue", "/new", "/clear"]);
}

#[test]
fn the_payload_debug_redacts_the_response_url() {
    let payload = command("/issue", "D1", "x");
    let printed = format!("{payload:?}");
    assert!(!printed.contains("hooks.slack.com"));
    assert!(printed.contains("<redacted>"));
}

#[test]
fn direct_messages_are_detected_by_the_channel_id_prefix() {
    assert!(command("/new", "D123", "").is_direct_message());
    assert!(!command("/new", "C123", "").is_direct_message());
}

// =====================================================================
// `/issue`
// =====================================================================

#[tokio::test]
async fn issue_without_a_prompt_answers_with_the_usage_hint() {
    let harness = harness(Some(Uuid::new_v4()), true);
    assert_eq!(
        reply(&harness, &command("/issue", "D1", "   ")).await,
        SLASH_USAGE_TEXT
    );
    assert!(
        harness.tasks.calls.lock().expect("lock").is_empty(),
        "缺正文不排任务"
    );
}

#[tokio::test]
async fn issue_hands_the_raw_prompt_to_the_installations_agent() {
    let user = Uuid::new_v4();
    let harness = harness(Some(user), true);
    assert_eq!(
        reply(&harness, &command("/issue", "D1", "fix the login button")).await,
        SLASH_QUEUED_TEXT
    );
    let calls = harness.tasks.calls.lock().expect("lock");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].prompt, "fix the login button");
    assert_eq!(calls[0].requester_id, Id(user));
}

#[tokio::test]
async fn an_issue_limit_reached_has_its_own_message() {
    let harness = harness(Some(Uuid::new_v4()), true);
    *harness.tasks.outcome.lock().expect("lock") = 1;
    assert_eq!(
        reply(&harness, &command("/issue", "D1", "x")).await,
        SLASH_ISSUE_LIMIT_TEXT
    );
    *harness.tasks.outcome.lock().expect("lock") = 2;
    assert_eq!(
        reply(&harness, &command("/issue", "D1", "x")).await,
        SLASH_INTERNAL_ERROR_TEXT
    );
}

#[tokio::test]
async fn an_unbound_user_gets_the_link_account_fallback_when_the_minter_is_missing() {
    let harness = harness(None, true);
    assert_eq!(
        reply(&harness, &command("/issue", "D1", "x")).await,
        SLASH_LINK_ACCOUNT_FALLBACK
    );
    assert!(harness.tasks.calls.lock().expect("lock").is_empty());
}

#[tokio::test]
async fn a_non_member_is_told_so() {
    let harness = harness(Some(Uuid::new_v4()), false);
    assert_eq!(
        reply(&harness, &command("/issue", "D1", "x")).await,
        SLASH_NOT_MEMBER_TEXT
    );
}

#[tokio::test]
async fn an_unknown_or_revoked_app_answers_disabled() {
    let harness = harness(Some(Uuid::new_v4()), true);
    // 认不出的 app id ⇒ 没有安装行。
    let mut payload = command("/issue", "D1", "x");
    payload.api_app_id = "A9".to_string();
    // 替身按 app id 忽略入参 ⇒ 直接把行清空来模拟"没有活跃安装"。
    *harness.installations.row.lock().expect("lock") = None;
    assert_eq!(reply(&harness, &payload).await, SLASH_DISABLED_TEXT);
}

/// 团队不匹配 ⇒ 与"没有安装"同一结果（上游 `installationServesTeam`）。
#[tokio::test]
async fn an_event_from_another_team_is_refused() {
    let harness = harness(Some(Uuid::new_v4()), true);
    let mut payload = command("/issue", "D1", "x");
    payload.team_id = "T9".to_string();
    assert_eq!(reply(&harness, &payload).await, SLASH_DISABLED_TEXT);
}

// =====================================================================
// `/new` / `/clear`
// =====================================================================

#[tokio::test]
async fn control_commands_only_work_in_direct_messages() {
    let harness = harness(Some(Uuid::new_v4()), true);
    assert_eq!(
        reply(&harness, &command("/new", "C1", "")).await,
        SLASH_NEW_THREAD_GUIDE_TEXT
    );
    assert_eq!(
        reply(&harness, &command("/clear", "C1", "")).await,
        SLASH_CLEAR_THREAD_GUIDE_TEXT
    );
    assert!(
        harness.control.starts.lock().expect("lock").is_empty(),
        "频道里绝不猜线程根"
    );
}

#[tokio::test]
async fn a_dm_control_command_starts_a_session() {
    let harness = harness(Some(Uuid::new_v4()), true);
    assert_eq!(
        reply(&harness, &command("/new", "D1", "hello")).await,
        SLASH_NEW_STARTED_TEXT
    );
    assert_eq!(
        reply(&harness, &command("/clear", "D1", "")).await,
        SLASH_CLEAR_STARTED_TEXT
    );
    let starts = harness.control.starts.lock().expect("lock");
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[0], "new:ENV1:hello");
    assert!(starts[1].starts_with("clear:ENV1"));
}

/// 去重命中 ⇒ 答复照旧（幂等）：重连重投**不**报错，也不重复生效。
#[tokio::test]
async fn a_replayed_envelope_answers_the_same_way() {
    let harness = harness(Some(Uuid::new_v4()), true);
    *harness.control.fail_with_duplicate.lock().expect("lock") = true;
    assert_eq!(
        reply(&harness, &command("/new", "D1", "")).await,
        SLASH_NEW_STARTED_TEXT
    );
}

#[tokio::test]
async fn a_session_control_failure_is_logged_and_answered_generically() {
    let harness = harness(Some(Uuid::new_v4()), true);
    *harness.control.fail_other.lock().expect("lock") = true;
    assert_eq!(
        reply(&harness, &command("/new", "D1", "")).await,
        SLASH_INTERNAL_ERROR_TEXT
    );
}

/// 没有正文要回时不打 `response_url`（边界：`text` 空或 URL 空）。
#[tokio::test]
async fn nothing_is_sent_when_there_is_nothing_to_say() {
    let harness = harness(Some(Uuid::new_v4()), true);
    let mut payload = command("/issue", "D1", ""); // ⇒ 有文案
    payload.response_url = String::new();
    harness.processor.handle(&payload, "ENV1").await;
    assert!(harness.responder.sent.lock().expect("lock").is_empty());

    // 认不出的命令 ⇒ 什么都不做。
    let unknown = command("/nope", "D1", "x");
    harness.processor.handle(&unknown, "ENV1").await;
    assert!(harness.responder.sent.lock().expect("lock").is_empty());
}

#[test]
fn the_default_binding_path_is_slash_slack_bind() {
    assert_eq!(DEFAULT_BINDING_PATH, "/slack/bind");
}
