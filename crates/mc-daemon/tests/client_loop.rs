//! `mc-daemon` 客户端的线路 + 状态机用例（M3-7 / LUM-1438）。
//!
//! 全部走 `ScriptedTransport`（本文件自带）：它把每条出站请求原样记下来，
//! 按脚本回响应。这样断言的是**线形状**（路径 / body / 状态迁移），
//! 不依赖 HTTP 服务端 —— 真 socket 的用例在 `http_transport.rs`。

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use mc_daemon::client::plan_heartbeat_actions;
use mc_daemon::{
    ClaimOutcome, ClaimedTask, ClientConfig, ClientError, ClientState, DaemonClient,
    DaemonTransport, FailedProfile, HeartbeatOutcome, PendingWork, RegisterRuntime, TransportError,
    CLAIM_PATH, DEREGISTER_PATH, HEARTBEAT_PATH, REGISTER_PATH,
};

// ---------------------------------------------------------------------------
// 脚本化传输
// ---------------------------------------------------------------------------

/// 记录请求 + 按序回包。
///
/// 两个队列都用 `Arc<Mutex<…>>`：`Harness` 在客户端建好之后还要能往队列里追加响应，
/// 而客户端的 `into_transport` 会**消费**客户端 —— 夹具不该为了改脚本把被测对象拆掉。
///
/// 不持锁跨 `await`：`async_trait` 的 future 要 `Send`，`MutexGuard` 不是 —— 于是
/// 每条语句都只在自己的临界区里取值，锁在返回之前就释放了。
#[derive(Default, Clone)]
struct ScriptedTransport {
    requests: Arc<Mutex<Vec<(String, Value)>>>,
    responses: Arc<Mutex<VecDeque<Result<Value, TransportError>>>>,
}

#[async_trait::async_trait]
impl DaemonTransport for ScriptedTransport {
    async fn post_json(&self, path: &str, body: &Value) -> Result<Value, TransportError> {
        let queued = {
            self.requests
                .lock()
                .expect("requests lock")
                .push((path.to_owned(), body.clone()));
            self.responses.lock().expect("responses lock").pop_front()
        };
        // 脚本用尽 = 回空对象（`{}`）：服务端对绝大多数 endpoint 的「无事发生」
        // 就是这个形状，测试不必为每条请求都写一行脚本。
        queued.unwrap_or(Ok(json!({})))
    }
}

// ---------------------------------------------------------------------------
// 夹具
// ---------------------------------------------------------------------------

/// 一个客户端 + 它的可追加脚本 + 请求记录。
struct Harness {
    client: DaemonClient<ScriptedTransport>,
    transport: ScriptedTransport,
}

impl Harness {
    fn new(config: ClientConfig) -> Self {
        let transport = ScriptedTransport::default();
        Self {
            client: DaemonClient::new(transport.clone(), config),
            transport,
        }
    }

    /// 排一个响应。
    fn reply(&self, value: Value) -> &Self {
        self.transport
            .responses
            .lock()
            .expect("responses lock")
            .push_back(Ok(value));
        self
    }

    /// 排一个传输故障。
    fn fail(&self, err: TransportError) -> &Self {
        self.transport
            .responses
            .lock()
            .expect("responses lock")
            .push_back(Err(err));
        self
    }

    fn requests(&self) -> Vec<(String, Value)> {
        self.transport
            .requests
            .lock()
            .expect("requests lock")
            .clone()
    }

    fn paths(&self) -> Vec<String> {
        self.requests().into_iter().map(|(path, _)| path).collect()
    }

    fn last_request(&self) -> (String, Value) {
        self.requests()
            .pop()
            .expect("至少发过一条请求：调用方没发请求时这个断言本身就是结论")
    }

    fn state(&self) -> &ClientState {
        self.client.state()
    }
}

fn config() -> ClientConfig {
    let mut config = ClientConfig::new("ws-1", "machine-a", "devbox5", "0.4.21");
    config.max_tasks = 3;
    config
}

/// 服务端对 register 的响应：台账里有一台 `rt-1`。
fn register_response(id: &str, provider: &str) -> Value {
    json!({
        "runtimes": [{
            "id": id,
            "workspace_id": "ws-1",
            "daemon_id": "machine-a",
            "name": "claude on devbox5",
            "provider": provider,
            "status": "online",
        }],
        "repos": [],
        "repos_version": 7,
        "settings": {},
    })
}

fn runtime_report(kind: &str) -> RegisterRuntime {
    RegisterRuntime {
        name: kind.to_owned(),
        kind: kind.to_owned(),
        version: "1.2.3".to_owned(),
        status: "online".to_owned(),
        profile_id: String::new(),
    }
}

/// 一个已登记的客户端（台账里有 `rt-1`）。
async fn registered() -> Harness {
    let mut harness = Harness::new(config());
    harness.reply(register_response("rt-1", "claude"));
    harness
        .client
        .register(vec![runtime_report("claude")], Vec::new())
        .await
        .expect("register");
    harness
}

fn not_found(message: &str) -> TransportError {
    TransportError::Status {
        status: 404,
        code: "not_found".into(),
        message: message.to_owned(),
    }
}

fn claimed_task(id: &str) -> ClaimedTask {
    ClaimedTask {
        id: id.to_owned(),
        runtime_id: "rt-1".into(),
        issue_id: "issue-1".into(),
        workspace_id: "ws-1".into(),
        status: "dispatched".into(),
        auth_token: "mul_task".into(),
    }
}

// ---------------------------------------------------------------------------
// register
// ---------------------------------------------------------------------------

#[tokio::test]
async fn register_sends_the_upstream_wire_shape() {
    let harness = registered().await;
    let (path, body) = harness.last_request();
    assert_eq!(path, REGISTER_PATH);
    assert_eq!(body["workspace_id"], json!("ws-1"));
    assert_eq!(body["daemon_id"], json!("machine-a"));
    assert_eq!(body["device_name"], json!("devbox5"));
    assert_eq!(body["cli_version"], json!("0.4.21"));
    assert_eq!(body["launched_by"], json!(""));
    assert_eq!(body["legacy_daemon_ids"], json!([]));
    assert_eq!(body["failed_profiles"], json!([]));
    // 上游字段名是 `type`（protocol family），不是 `provider`。
    assert_eq!(body["runtimes"][0]["type"], json!("claude"));
    assert_eq!(body["runtimes"][0]["name"], json!("claude"));
    assert_eq!(body["runtimes"][0]["version"], json!("1.2.3"));
    assert!(
        body["runtimes"][0].get("provider").is_none(),
        "register.runtimes[] 不该出现 provider 键"
    );
}

#[tokio::test]
async fn register_records_the_server_ledger_and_repos_version() {
    let harness = registered().await;
    let registration = harness.state().registration().expect("台账");
    assert_eq!(registration.workspace_id, "ws-1");
    assert_eq!(registration.daemon_id, "machine-a");
    assert_eq!(registration.runtime_ids(), vec!["rt-1".to_owned()]);
    assert_eq!(registration.repos_version, 7);
    assert_eq!(
        registration.runtime("rt-1").map(|rt| rt.provider.as_str()),
        Some("claude")
    );
    assert_eq!(harness.state().live_runtime_ids(), vec!["rt-1".to_owned()]);
}

#[tokio::test]
async fn register_requires_workspace_and_daemon_id() {
    let mut blank = ClientConfig::new("", "machine-a", "devbox5", "0.4.21");
    let mut harness = Harness::new(blank.clone());
    assert_eq!(
        harness.client.register(Vec::new(), Vec::new()).await,
        Err(ClientError::InvalidConfig("workspace_id is required"))
    );
    assert!(harness.requests().is_empty(), "不该发包");

    blank.workspace_id = "ws-1".into();
    blank.daemon_id = String::new();
    let mut harness = Harness::new(blank);
    assert_eq!(
        harness.client.register(Vec::new(), Vec::new()).await,
        Err(ClientError::InvalidConfig("daemon_id is required"))
    );
    assert!(harness.requests().is_empty(), "不该发包");
}

#[tokio::test]
async fn register_reports_failed_profiles_on_the_wire() {
    let mut harness = Harness::new(config());
    harness.reply(json!({ "runtimes": [], "repos_version": 0 }));
    harness
        .client
        .register(
            Vec::new(),
            vec![FailedProfile {
                profile_id: "p-1".into(),
                command_name: "my-cli".into(),
                reason: "not found in PATH".into(),
            }],
        )
        .await
        .expect("register");
    let (_, body) = harness.last_request();
    assert_eq!(body["failed_profiles"][0]["profile_id"], json!("p-1"));
    assert_eq!(body["failed_profiles"][0]["command_name"], json!("my-cli"));
    assert_eq!(
        body["failed_profiles"][0]["reason"],
        json!("not found in PATH")
    );
}

// ---------------------------------------------------------------------------
// heartbeat
// ---------------------------------------------------------------------------

#[tokio::test]
async fn heartbeat_without_registration_is_an_error() {
    let mut harness = Harness::new(config());
    assert_eq!(
        harness.client.heartbeat("rt-1").await,
        Err(ClientError::NotRegistered)
    );
    assert!(harness.requests().is_empty(), "不该发包");
}

#[tokio::test]
async fn heartbeat_sends_runtime_id_and_refreshes_the_waterline() {
    let mut harness = registered().await;
    let outcome = harness.client.heartbeat("rt-1").await.expect("heartbeat");
    assert_eq!(
        outcome,
        HeartbeatOutcome::Acked {
            actions: Vec::new()
        }
    );
    let (path, body) = harness.last_request();
    assert_eq!(path, HEARTBEAT_PATH);
    assert_eq!(body["runtime_id"], json!("rt-1"));
    assert_eq!(body["supports_batch_import"], json!(true));
    assert!(harness.state().last_heartbeat_at("rt-1").is_some());
}

#[tokio::test]
async fn heartbeat_omits_false_supports_batch_import() {
    let mut config = config();
    config.supports_batch_import = false;
    let mut harness = Harness::new(config);
    harness.reply(json!({ "runtimes": [], "repos_version": 0 }));
    harness
        .client
        .register(Vec::new(), Vec::new())
        .await
        .expect("register");
    harness.client.heartbeat("rt-1").await.expect("heartbeat");
    let (_, body) = harness.last_request();
    assert!(
        body.get("supports_batch_import").is_none(),
        "上游 `omitempty`：false 必须缺席，老服务端按字段在不在区分新老 daemon"
    );
}

#[tokio::test]
async fn heartbeat_ack_drives_the_action_plan() {
    let mut harness = registered().await;
    harness.reply(json!({
        "status": "ok",
        "server_capabilities": ["rpc-v1"],
        "pending_update": { "id": "u-1", "target_version": "0.5.0" },
        "pending_model_list": { "id": "m-1" },
        "pending_local_skills": { "id": "s-1" },
        "pending_local_skill_import": { "id": "i-1", "skill_key": "code-review" },
    }));
    let outcome = harness.client.heartbeat("rt-1").await.expect("heartbeat");
    assert_eq!(
        outcome.actions(),
        [
            PendingWork::Update {
                request_id: "u-1".into(),
                target_version: "0.5.0".into(),
            },
            PendingWork::ListModels {
                request_id: "m-1".into(),
            },
            PendingWork::ListLocalSkills {
                request_id: "s-1".into(),
            },
            PendingWork::ImportLocalSkill {
                request_id: "i-1".into(),
                skill_key: "code-review".into(),
            },
        ]
        .as_slice()
    );
    assert!(
        harness.state().last_heartbeat_at("rt-1").is_some(),
        "ack 成功后必须记水位"
    );
}

#[tokio::test]
async fn heartbeat_404_is_runtime_gone_not_an_error() {
    let mut harness = registered().await;
    harness.fail(not_found("runtime not found"));
    assert_eq!(
        harness
            .client
            .heartbeat("rt-1")
            .await
            .expect("404 不是错误"),
        HeartbeatOutcome::RuntimeGone
    );
    // 服务端说它没了 ⇒ 它必须离开「活着的 runtime」集合（上游 handleRuntimeGone）。
    assert!(harness.state().live_runtime_ids().is_empty());
    assert_eq!(harness.state().gone_runtime_ids(), vec!["rt-1".to_owned()]);
    assert!(harness.state().last_heartbeat_at("rt-1").is_none());
}

#[tokio::test]
async fn heartbeat_ack_runtime_gone_marks_it_too() {
    let mut harness = registered().await;
    harness.reply(json!({ "status": "runtime_gone", "runtime_gone": true }));
    assert_eq!(
        harness.client.heartbeat("rt-1").await.expect("ack"),
        HeartbeatOutcome::RuntimeGone
    );
    assert_eq!(harness.state().gone_runtime_ids(), vec!["rt-1".to_owned()]);
}

#[tokio::test]
async fn heartbeat_5xx_stays_an_error_and_is_retriable() {
    let mut harness = registered().await;
    harness.fail(TransportError::Status {
        status: 503,
        code: "internal_error".into(),
        message: "database error".into(),
    });
    let err = harness
        .client
        .heartbeat("rt-1")
        .await
        .expect_err("5xx 是错误");
    assert!(err.is_retriable(), "5xx 该退避重试");
    assert!(!err.is_not_found(), "5xx 不是 runtime 没了");
    assert_eq!(
        harness.state().live_runtime_ids(),
        vec!["rt-1".to_owned()],
        "5xx 不该把 runtime 从台账里摘掉"
    );
}

#[tokio::test]
async fn heartbeat_all_walks_every_live_runtime() {
    let mut harness = Harness::new(config());
    harness.reply(json!({
        "runtimes": [
            { "id": "rt-1", "provider": "claude", "status": "online" },
            { "id": "rt-2", "provider": "codex", "status": "online" },
        ],
        "repos_version": 1,
    }));
    harness
        .client
        .register(Vec::new(), Vec::new())
        .await
        .expect("register");
    let outcomes = harness.client.heartbeat_all().await.expect("heartbeat_all");
    let ids: Vec<&str> = outcomes.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(ids, vec!["rt-1", "rt-2"]);
    assert!(harness.state().last_heartbeat_at("rt-2").is_some());
    assert_eq!(harness.paths().len(), 3, "一条 register + 两条 heartbeat");
}

// ---------------------------------------------------------------------------
// claim
// ---------------------------------------------------------------------------

#[tokio::test]
async fn claim_without_runtimes_does_not_send_a_request() {
    let mut harness = Harness::new(config());
    assert_eq!(
        harness.client.claim().await.expect("claim"),
        ClaimOutcome::default()
    );
    assert!(harness.requests().is_empty(), "不该发包");
}

#[tokio::test]
async fn claim_sends_daemon_id_and_live_runtimes() {
    let mut harness = registered().await;
    harness.reply(json!({
        "tasks": [
            { "id": "t-1", "runtime_id": "rt-1", "issue_id": "issue-1",
              "workspace_id": "ws-1", "status": "dispatched", "auth_token": "mul_task" }
        ],
        "claim_poll_hint_supported": true,
        "next_deferred_task_after_ms": 4000,
    }));
    let outcome = harness.client.claim().await.expect("claim");
    let (path, body) = harness.last_request();
    assert_eq!(path, CLAIM_PATH);
    assert_eq!(body["daemon_id"], json!("machine-a"));
    assert_eq!(body["runtime_ids"], json!(["rt-1"]));
    assert_eq!(body["max_tasks"], json!(3));
    assert_eq!(outcome.tasks, vec![claimed_task("t-1")]);
    assert!(outcome.claim_poll_hint_supported);
    assert_eq!(outcome.next_deferred_task_after_ms, Some(4000));
}

#[tokio::test]
async fn claim_drops_duplicate_tasks() {
    let mut harness = registered().await;
    let task = json!({
        "id": "t-1", "runtime_id": "rt-1", "issue_id": "issue-1",
        "workspace_id": "ws-1", "status": "dispatched", "auth_token": "mul_task",
    });
    // 两条腿（HTTP + WS）各回一次同一条任务：客户端必须只交出去一次。
    harness.reply(json!({ "tasks": [task.clone()] }));
    harness.reply(json!({ "tasks": [task] }));
    let first = harness.client.claim().await.expect("claim 1");
    let second = harness.client.claim().await.expect("claim 2");
    assert_eq!(first.tasks.len(), 1);
    assert!(second.tasks.is_empty(), "重复认领必须被丢掉");
    assert_eq!(harness.state().in_flight_task_ids(), vec!["t-1".to_owned()]);
    assert!(harness.state().is_in_flight("t-1"));
    assert_eq!(harness.state().in_flight_len(), 1);
}

#[tokio::test]
async fn finish_task_frees_the_in_flight_slot() {
    let mut harness = registered().await;
    harness.reply(json!({ "tasks": [{ "id": "t-1", "runtime_id": "rt-1" }] }));
    harness.client.claim().await.expect("claim");
    assert_eq!(harness.client.finish_task("t-1"), Some("rt-1".to_owned()));
    assert_eq!(harness.client.finish_task("t-1"), None);
    assert_eq!(harness.state().in_flight_len(), 0);
    // 服务端重派同一条：跑完之后必须能重新领进来。
    harness.reply(json!({ "tasks": [{ "id": "t-1", "runtime_id": "rt-1" }] }));
    assert_eq!(harness.client.claim().await.expect("claim").tasks.len(), 1);
}

#[tokio::test]
async fn claim_zero_max_tasks_short_circuits() {
    let mut config = config();
    config.max_tasks = 0;
    let mut harness = Harness::new(config);
    harness.reply(register_response("rt-1", "claude"));
    harness
        .client
        .register(Vec::new(), Vec::new())
        .await
        .expect("register");
    assert_eq!(
        harness.client.claim().await.expect("claim"),
        ClaimOutcome::default()
    );
    assert_eq!(
        harness.paths(),
        vec![REGISTER_PATH.to_owned()],
        "max_tasks = 0 是「明确不领」，不该发包"
    );
}

// ---------------------------------------------------------------------------
// deregister
// ---------------------------------------------------------------------------

#[tokio::test]
async fn deregister_sends_reasons_and_clears_the_ledger() {
    let mut harness = registered().await;
    let mut reasons = BTreeMap::new();
    reasons.insert("rt-1".to_owned(), json!("user quit the desktop app"));
    harness
        .client
        .deregister(Vec::new(), reasons)
        .await
        .expect("deregister");
    let (path, body) = harness.last_request();
    assert_eq!(path, DEREGISTER_PATH);
    assert_eq!(body["runtime_ids"], json!(["rt-1"]));
    assert_eq!(
        body["offline_reasons"]["rt-1"],
        json!("user quit the desktop app")
    );
    assert!(harness.state().live_runtime_ids().is_empty());
    assert!(
        harness.state().registration().is_some(),
        "登记本身还在（只是台账空了）"
    );
}

#[tokio::test]
async fn deregister_does_not_mark_the_runtime_gone() {
    let mut harness = registered().await;
    harness
        .client
        .deregister(vec!["rt-1".to_owned()], BTreeMap::new())
        .await
        .expect("deregister");
    assert!(
        harness.state().gone_runtime_ids().is_empty(),
        "主动下线不是「服务端说它没了」：不该进 gone（否则重新登记前会一直跳过它）"
    );
}

#[tokio::test]
async fn deregister_without_registration_is_an_error() {
    let mut harness = Harness::new(config());
    assert_eq!(
        harness.client.deregister(Vec::new(), BTreeMap::new()).await,
        Err(ClientError::NotRegistered)
    );
}

// ---------------------------------------------------------------------------
// 心跳动作规划（纯函数）
// ---------------------------------------------------------------------------

type Ack = mc_daemon_proto::messages::daemon::DaemonHeartbeatAckPayload;

fn parse_ack(value: Value) -> Ack {
    serde_json::from_value(value).expect("ack")
}

#[test]
fn batch_import_field_wins_over_the_singular_one() {
    // 新服务端**同时**填两个键（兼容老 daemon）：新 daemon 必须只处理复数键，
    // 否则第一条导入会被处理两遍（上游 `daemon.go:4584`）。
    let ack = parse_ack(json!({
        "pending_local_skill_import": { "id": "i-1", "skill_key": "first" },
        "pending_local_skill_imports": [
            { "id": "i-1", "skill_key": "first" },
            { "id": "i-2", "skill_key": "second" },
        ],
    }));
    assert_eq!(
        plan_heartbeat_actions(&ack),
        vec![
            PendingWork::ImportLocalSkill {
                request_id: "i-1".into(),
                skill_key: "first".into(),
            },
            PendingWork::ImportLocalSkill {
                request_id: "i-2".into(),
                skill_key: "second".into(),
            },
        ]
    );
}

#[test]
fn singular_import_field_is_the_fallback() {
    let ack = parse_ack(json!({
        "pending_local_skill_import": { "id": "i-9", "skill_key": "legacy" },
    }));
    assert_eq!(
        plan_heartbeat_actions(&ack),
        vec![PendingWork::ImportLocalSkill {
            request_id: "i-9".into(),
            skill_key: "legacy".into(),
        }]
    );
}

#[test]
fn empty_ack_plans_nothing_and_unknown_fields_are_ignored() {
    let ack = parse_ack(json!({ "status": "ok", "invented_by_a_newer_server": 1 }));
    assert!(plan_heartbeat_actions(&ack).is_empty());
    assert!(!ack.runtime_gone);
}

// ---------------------------------------------------------------------------
// 传输错误分类
// ---------------------------------------------------------------------------

#[test]
fn transport_errors_classify_into_retriable_and_fatal() {
    let unreachable = TransportError::Unreachable {
        url: "http://x/api/daemon/heartbeat".into(),
        reason: "connection refused".into(),
    };
    assert!(unreachable.is_retriable());
    assert!(!unreachable.is_not_found());
    assert_eq!(unreachable.status(), None);
    assert_eq!(unreachable.code(), None);

    let rate_limited = TransportError::Status {
        status: 429,
        code: "too_many_requests".into(),
        message: "slow down".into(),
    };
    assert!(rate_limited.is_retriable());
    assert_eq!(rate_limited.status(), Some(429));
    assert_eq!(rate_limited.code(), Some("too_many_requests"));

    assert!(!not_found("runtime not found").is_retriable());
    assert!(not_found("runtime not found").is_not_found());

    let bad_request = TransportError::Status {
        status: 400,
        code: "validation_error".into(),
        message: "workspace_id is required".into(),
    };
    assert!(
        !bad_request.is_retriable(),
        "4xx 重试只是把同一个错误再打一遍"
    );

    let malformed = TransportError::Malformed {
        url: "http://x/api/daemon/register".into(),
        reason: "expected value at line 1".into(),
    };
    assert!(!malformed.is_retriable(), "同样的字节重试一百次还是错的");
}

#[test]
fn client_error_retriability_follows_the_transport() {
    assert!(!ClientError::NotRegistered.is_retriable());
    assert!(!ClientError::InvalidConfig("workspace_id is required").is_retriable());
    assert!(!ClientError::NotRegistered.is_not_found());
    let offline = ClientError::Transport(not_found("runtime not found"));
    assert!(offline.is_not_found());
    assert!(!offline.is_retriable());
}
