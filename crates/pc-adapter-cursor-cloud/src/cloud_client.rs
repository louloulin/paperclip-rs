//! Cursor Cloud HTTP client abstraction — 对齐 Node `@cursor/sdk`
//! SDK surface（`Agent.create` / `Agent.resume` / `Agent.getRun` /
//! `Run.send` / `Run.stream` / `Run.wait` / `Run.result`）。
//!
//! 本模块**不**直接调用 `@cursor/sdk`（无 Rust 绑定）；
//! 而是用强类型 trait 抽象 SDK 接口，提供：
//! 1. `CursorCloudClient` trait —— mockable 接口（生产实现可在 R6xx 后切换到 reqwest）
//! 2. `FakeCursorCloudClient` —— in-memory scripted 实现（E2E 测试用）
//! 3. `CloudError` —— 错误类型统一抽象（包含 gatewayCode / details）

#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::session_codec::{CursorCloudRepo, RuntimeEnvType};

// ─── Core types ──────────────────────────────────────────────────────

/// Cloud Agent 句柄（持久化到 session.params.cursorAgentId）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudAgent {
    pub agent_id: String,
    pub env_type: RuntimeEnvType,
    pub env_name: Option<String>,
    pub repos: Vec<CursorCloudRepo>,
}

/// Cloud Run handle —— a single request/response cycle within an agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CloudRun {
    pub id: String,
    pub agent_id: String,
    pub status: CloudRunStatus,
    pub model: Option<String>,
    pub result: Option<String>,
    pub duration_ms: Option<u64>,
    pub git: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudRunStatus {
    Running,
    Finished,
    Error,
}

/// SDK streaming message（与 `event_codec::SdkMessageKind` 一致）。
///
/// 这里用 `Value` 是因为 SDK message 实际是 union-like JSON；
/// `event_codec::SdkMessageKind` 已经做了 typed 提取，
/// 本类型只作为 transport-level（向外发不解析）的载体。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SdkTransportMessage {
    Assistant {
        text: String,
    },
    User {
        text: String,
    },
    Thinking {
        text: String,
    },
    ToolCall {
        name: String,
        status: String,
        args: Option<Value>,
        result: Option<Value>,
    },
    ToolResult {
        name: String,
        is_error: bool,
        content: Option<Value>,
    },
    Status {
        status: String,
        message: Option<String>,
    },
    Task {
        text: String,
    },
}

/// 创建 / resume agent 选项。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentOptions {
    pub api_key: String,
    pub name: String,
    pub model: Option<String>,
    pub env_type: RuntimeEnvType,
    pub env_name: Option<String>,
    pub repos: Vec<CursorCloudRepo>,
    pub work_on_current_branch: bool,
    pub auto_create_pr: bool,
    pub skip_reviewer_request: bool,
    pub env_vars: HashMap<String, String>,
}

/// 发送 prompt 选项。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SendOptions {
    pub model: Option<String>,
}

/// 获取运行选项。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunFetchOptions {
    pub runtime: String, // always "cloud"
    pub agent_id: String,
    pub api_key: String,
}

/// 错误类型 —— 与 Node `GatewayResponseError`（SDK 抛出的 `Error & { gatewayCode, details }`）一致。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudError {
    pub message: String,
    pub gateway_code: Option<String>,
    pub details: Option<Value>,
}

impl std::fmt::Display for CloudError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for CloudError {}

impl CloudError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            gateway_code: None,
            details: None,
        }
    }
    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.gateway_code = Some(code.into());
        self
    }
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

// ─── Client trait ────────────────────────────────────────────────────

/// Cursor Cloud 客户端抽象。
///
/// 真实实现（生产）使用 reqwest 调 Cursor Cloud REST；测试实现使用
/// `FakeCursorCloudClient` 内存脚本。execute path 只与 trait 交互。
#[async_trait::async_trait]
pub trait CursorCloudClient: Send + Sync {
    async fn create_agent(&self, opts: &AgentOptions) -> Result<CloudAgent, CloudError>;
    async fn resume_agent(
        &self,
        agent_id: &str,
        opts: &AgentOptions,
    ) -> Result<CloudAgent, CloudError>;
    async fn get_run(
        &self,
        run_id: &str,
        opts: &RunFetchOptions,
    ) -> Result<Option<CloudRun>, CloudError>;
    async fn send_prompt(
        &self,
        agent: &CloudAgent,
        prompt: &str,
        opts: &SendOptions,
    ) -> Result<CloudRun, CloudError>;
    async fn stream_messages(
        &self,
        run: &CloudRun,
        sink: &mut (dyn FnMut(SdkTransportMessage) + Send),
    ) -> Result<(), CloudError>;
    async fn wait_for_run(&self, run: &CloudRun) -> Result<CloudRun, CloudError>;
}

/// Boxed client alias（execute path 持有）。
pub type DynClient = Arc<dyn CursorCloudClient>;

// ─── Scripted Fake Client ────────────────────────────────────────────

/// 单次脚本步骤 —— `When` × `Then` 配对。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ScriptedResponse {
    CreateAgent {
        agent_id: String,
    },
    ResumeAgent {
        agent_id: String,
    },
    GetRun {
        run: Option<CloudRun>,
    },
    SendPrompt {
        run: CloudRun,
    },
    Stream {
        messages: Vec<SdkTransportMessage>,
    },
    WaitFinal {
        final_run: CloudRun,
    },
    Error {
        message: String,
        code: Option<String>,
    },
}

/// Scripted 客户端 —— 严格按 FIFO 顺序消费脚本（run/test 一次完整剧）。
#[derive(Debug, Default)]
pub struct FakeCursorCloudClient {
    pub script: Mutex<Vec<ScriptedResponse>>,
    pub calls: Mutex<Vec<String>>,
    pub recorded_options: Mutex<Vec<AgentOptions>>,
    pub recorded_sends: Mutex<Vec<(String, String, Option<String>)>>, // (agent_id, prompt, model)
    pub recorded_run_fetches: Mutex<Vec<(String, String)>>,           // (run_id, agent_id)
    pub next_id: Mutex<u64>,
}

impl FakeCursorCloudClient {
    pub fn new() -> Self {
        Self::default()
    }

    /// 用脚本初始化 fake client。
    pub fn with_script(script: Vec<ScriptedResponse>) -> Self {
        Self {
            script: Mutex::new(script),
            calls: Mutex::new(Vec::new()),
            recorded_options: Mutex::new(Vec::new()),
            recorded_sends: Mutex::new(Vec::new()),
            recorded_run_fetches: Mutex::new(Vec::new()),
            next_id: Mutex::new(0),
        }
    }

    fn next_id(&self, prefix: &str) -> String {
        let mut g = self.next_id.lock().expect("id");
        *g += 1;
        format!("{prefix}-{}", *g)
    }

    fn pop(&self, call: &str) -> ScriptedResponse {
        self.calls.lock().expect("calls").push(call.to_owned());
        let mut script = self.script.lock().expect("script");
        script.remove(0)
    }

    fn record_options(&self, opts: &AgentOptions) {
        self.recorded_options
            .lock()
            .expect("opts")
            .push(opts.clone());
    }

    fn record_send(&self, agent_id: &str, prompt: &str, model: Option<&str>) {
        self.recorded_sends.lock().expect("sends").push((
            agent_id.to_owned(),
            prompt.to_owned(),
            model.map(str::to_owned),
        ));
    }

    fn record_run_fetch(&self, run_id: &str, agent_id: &str) {
        self.recorded_run_fetches
            .lock()
            .expect("fetches")
            .push((run_id.to_owned(), agent_id.to_owned()));
    }
}

#[async_trait::async_trait]
impl CursorCloudClient for FakeCursorCloudClient {
    async fn create_agent(&self, opts: &AgentOptions) -> Result<CloudAgent, CloudError> {
        self.record_options(opts);
        let id = self.next_id("cu");
        match self.pop("create_agent") {
            ScriptedResponse::CreateAgent { agent_id } => Ok(CloudAgent {
                agent_id,
                env_type: opts.env_type,
                env_name: opts.env_name.clone(),
                repos: opts.repos.clone(),
            }),
            ScriptedResponse::ResumeAgent { agent_id } => Ok(CloudAgent {
                agent_id,
                env_type: opts.env_type,
                env_name: opts.env_name.clone(),
                repos: opts.repos.clone(),
            }),
            ScriptedResponse::Error { message, code } => Err(match code {
                Some(c) => CloudError::new(message).with_code(c),
                None => CloudError::new(message),
            }),
            other => {
                let _ = id;
                Err(CloudError::new(format!(
                    "unexpected scripted response in create_agent: {:?}",
                    other
                )))
            }
        }
    }

    async fn resume_agent(
        &self,
        agent_id: &str,
        opts: &AgentOptions,
    ) -> Result<CloudAgent, CloudError> {
        self.record_options(opts);
        let _ = agent_id;
        let id = self.next_id("cu");
        match self.pop("resume_agent") {
            ScriptedResponse::ResumeAgent { agent_id } => Ok(CloudAgent {
                agent_id,
                env_type: opts.env_type,
                env_name: opts.env_name.clone(),
                repos: opts.repos.clone(),
            }),
            ScriptedResponse::CreateAgent { agent_id } => Ok(CloudAgent {
                agent_id,
                env_type: opts.env_type,
                env_name: opts.env_name.clone(),
                repos: opts.repos.clone(),
            }),
            ScriptedResponse::Error { message, code } => Err(match code {
                Some(c) => CloudError::new(message).with_code(c),
                None => CloudError::new(message),
            }),
            other => {
                let _ = id;
                Err(CloudError::new(format!(
                    "unexpected scripted response in resume_agent: {:?}",
                    other
                )))
            }
        }
    }

    async fn get_run(
        &self,
        run_id: &str,
        opts: &RunFetchOptions,
    ) -> Result<Option<CloudRun>, CloudError> {
        self.record_run_fetch(run_id, &opts.agent_id);
        match self.pop("get_run") {
            ScriptedResponse::GetRun { run } => Ok(run),
            ScriptedResponse::Error { message, code } => Err(match code {
                Some(c) => CloudError::new(message).with_code(c),
                None => CloudError::new(message),
            }),
            other => Err(CloudError::new(format!(
                "unexpected scripted response in get_run: {:?}",
                other
            ))),
        }
    }

    async fn send_prompt(
        &self,
        agent: &CloudAgent,
        prompt: &str,
        opts: &SendOptions,
    ) -> Result<CloudRun, CloudError> {
        self.record_send(&agent.agent_id, prompt, opts.model.as_deref());
        match self.pop("send_prompt") {
            ScriptedResponse::SendPrompt { run } => Ok(run),
            ScriptedResponse::Error { message, code } => Err(match code {
                Some(c) => CloudError::new(message).with_code(c),
                None => CloudError::new(message),
            }),
            other => Err(CloudError::new(format!(
                "unexpected scripted response in send_prompt: {:?}",
                other
            ))),
        }
    }

    async fn stream_messages(
        &self,
        _run: &CloudRun,
        sink: &mut (dyn FnMut(SdkTransportMessage) + Send),
    ) -> Result<(), CloudError> {
        match self.pop("stream_messages") {
            ScriptedResponse::Stream { messages } => {
                for m in messages {
                    sink(m);
                }
                Ok(())
            }
            ScriptedResponse::Error { message, code } => Err(match code {
                Some(c) => CloudError::new(message).with_code(c),
                None => CloudError::new(message),
            }),
            other => Err(CloudError::new(format!(
                "unexpected scripted response in stream_messages: {:?}",
                other
            ))),
        }
    }

    async fn wait_for_run(&self, run: &CloudRun) -> Result<CloudRun, CloudError> {
        match self.pop("wait_for_run") {
            ScriptedResponse::WaitFinal { final_run } => Ok(final_run),
            ScriptedResponse::GetRun { run: Some(r) } => Ok(r),
            ScriptedResponse::GetRun { run: None } => Ok(run.clone()),
            ScriptedResponse::Error { message, code } => Err(match code {
                Some(c) => CloudError::new(message).with_code(c),
                None => CloudError::new(message),
            }),
            other => Err(CloudError::new(format!(
                "unexpected scripted response in wait_for_run: {:?}",
                other
            ))),
        }
    }
}

// ─── Constructors / helpers ──────────────────────────────────────────

/// Quick agent options from a static shape (test fixtures).
pub fn simple_agent_opts(api_key: &str, repos: Vec<CursorCloudRepo>) -> AgentOptions {
    AgentOptions {
        api_key: api_key.to_owned(),
        name: format!("Paperclip test agent"),
        model: None,
        env_type: RuntimeEnvType::Cloud,
        env_name: None,
        repos,
        work_on_current_branch: false,
        auto_create_pr: false,
        skip_reviewer_request: false,
        env_vars: HashMap::new(),
    }
}

/// Construct a `CloudRun` fixture (test helper).
pub fn finished_run(id: &str, agent_id: &str, result: &str, model: Option<&str>) -> CloudRun {
    CloudRun {
        id: id.to_owned(),
        agent_id: agent_id.to_owned(),
        status: CloudRunStatus::Finished,
        model: model.map(str::to_owned),
        result: Some(result.to_owned()),
        duration_ms: Some(1234),
        git: None,
    }
}

/// Construct an errored `CloudRun` fixture.
pub fn errored_run(id: &str, agent_id: &str, error_msg: &str) -> CloudRun {
    CloudRun {
        id: id.to_owned(),
        agent_id: agent_id.to_owned(),
        status: CloudRunStatus::Error,
        model: None,
        result: Some(error_msg.to_owned()),
        duration_ms: None,
        git: None,
    }
}

/// Convenience: transport msg → JSON value (used by execute path's onLog).
pub fn transport_message_to_value(m: &SdkTransportMessage) -> Value {
    serde_json::to_value(m).unwrap_or(Value::Null)
}

/// Convenience: build a JSON for `agent_options_value` from an `AgentOptions`.
pub fn agent_options_value(opts: &AgentOptions) -> Value {
    json!({
        "apiKey": opts.api_key,
        "name": opts.name,
        "model": opts.model,
        "envType": opts.env_type.as_str(),
        "envName": opts.env_name,
        "repos": opts.repos,
        "workOnCurrentBranch": opts.work_on_current_branch,
        "autoCreatePR": opts.auto_create_pr,
        "skipReviewerRequest": opts.skip_reviewer_request,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_repos() -> Vec<CursorCloudRepo> {
        vec![CursorCloudRepo {
            url: "https://github.com/a/b".to_owned(),
            starting_ref: Some("main".to_owned()),
            pr_url: None,
        }]
    }

    #[tokio::test]
    async fn fake_create_agent_returns_scripted_id() {
        let client = FakeCursorCloudClient::with_script(vec![ScriptedResponse::CreateAgent {
            agent_id: "cu-fixed".to_owned(),
        }]);
        let opts = simple_agent_opts("ck-1", test_repos());
        let agent = client.create_agent(&opts).await.unwrap();
        assert_eq!(agent.agent_id, "cu-fixed");
        assert_eq!(agent.env_type, RuntimeEnvType::Cloud);
        assert_eq!(agent.repos.len(), 1);
    }

    #[tokio::test]
    async fn fake_resume_agent_returns_scripted_id() {
        let client = FakeCursorCloudClient::with_script(vec![ScriptedResponse::ResumeAgent {
            agent_id: "cu-resumed".to_owned(),
        }]);
        let opts = simple_agent_opts("ck-1", test_repos());
        let agent = client.resume_agent("ignored-id", &opts).await.unwrap();
        assert_eq!(agent.agent_id, "cu-resumed");
    }

    #[tokio::test]
    async fn fake_get_run_returns_none_when_not_attached() {
        let client =
            FakeCursorCloudClient::with_script(vec![ScriptedResponse::GetRun { run: None }]);
        let opts = RunFetchOptions {
            runtime: "cloud".to_owned(),
            agent_id: "cu-1".to_owned(),
            api_key: "ck-1".to_owned(),
        };
        let run = client.get_run("r-1", &opts).await.unwrap();
        assert!(run.is_none());
    }

    #[tokio::test]
    async fn fake_get_run_returns_some_when_attached() {
        let run = finished_run("r-1", "cu-1", "in-progress", Some("gpt-4"));
        let client = FakeCursorCloudClient::with_script(vec![ScriptedResponse::GetRun {
            run: Some(run.clone()),
        }]);
        let opts = RunFetchOptions {
            runtime: "cloud".to_owned(),
            agent_id: "cu-1".to_owned(),
            api_key: "ck-1".to_owned(),
        };
        let fetched = client.get_run("r-1", &opts).await.unwrap().unwrap();
        assert_eq!(fetched.id, "r-1");
        assert_eq!(fetched.status, CloudRunStatus::Finished);
    }

    #[tokio::test]
    async fn fake_send_prompt_records_call_and_returns_run() {
        let run = finished_run("r-2", "cu-1", "Hello", Some("gpt-4"));
        let client = FakeCursorCloudClient::with_script(vec![ScriptedResponse::SendPrompt {
            run: run.clone(),
        }]);
        let agent = CloudAgent {
            agent_id: "cu-1".to_owned(),
            env_type: RuntimeEnvType::Cloud,
            env_name: None,
            repos: test_repos(),
        };
        let sent = client
            .send_prompt(&agent, "Hello world", &SendOptions { model: None })
            .await
            .unwrap();
        assert_eq!(sent.id, "r-2");
        let recorded = client.recorded_sends.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].0, "cu-1");
        assert_eq!(recorded[0].1, "Hello world");
    }

    #[tokio::test]
    async fn fake_stream_messages_invokes_sink_for_each_msg() {
        let messages = vec![
            SdkTransportMessage::Assistant {
                text: "hi".to_owned(),
            },
            SdkTransportMessage::Status {
                status: "running".to_owned(),
                message: Some("go".to_owned()),
            },
            SdkTransportMessage::ToolCall {
                name: "grep".to_owned(),
                status: "running".to_owned(),
                args: Some(json!({"q": "x"})),
                result: None,
            },
        ];
        let client = FakeCursorCloudClient::with_script(vec![ScriptedResponse::Stream {
            messages: messages.clone(),
        }]);
        let run = finished_run("r-1", "cu-1", "done", None);
        let mut collected: Vec<SdkTransportMessage> = Vec::new();
        client
            .stream_messages(&run, &mut |m| collected.push(m))
            .await
            .unwrap();
        assert_eq!(collected.len(), 3);
        assert!(matches!(
            collected[0],
            SdkTransportMessage::Assistant { .. }
        ));
        assert!(matches!(collected[1], SdkTransportMessage::Status { .. }));
        assert!(matches!(collected[2], SdkTransportMessage::ToolCall { .. }));
    }

    #[tokio::test]
    async fn fake_wait_for_run_returns_final_run() {
        let run = finished_run("r-3", "cu-1", "All good", Some("gpt-4"));
        let client = FakeCursorCloudClient::with_script(vec![ScriptedResponse::WaitFinal {
            final_run: run.clone(),
        }]);
        let result = client.wait_for_run(&run).await.unwrap();
        assert_eq!(result.id, "r-3");
        assert_eq!(result.result.as_deref(), Some("All good"));
    }

    #[tokio::test]
    async fn fake_error_response_propagates_as_cloud_error() {
        let client = FakeCursorCloudClient::with_script(vec![ScriptedResponse::Error {
            message: "boom".to_owned(),
            code: Some("UNAUTHORIZED".to_owned()),
        }]);
        let opts = simple_agent_opts("ck-1", test_repos());
        let err = client.create_agent(&opts).await.unwrap_err();
        assert_eq!(err.message, "boom");
        assert_eq!(err.gateway_code.as_deref(), Some("UNAUTHORIZED"));
    }

    #[tokio::test]
    async fn fake_call_log_records_invocation_order() {
        let client = FakeCursorCloudClient::with_script(vec![
            ScriptedResponse::CreateAgent {
                agent_id: "cu-1".to_owned(),
            },
            ScriptedResponse::ResumeAgent {
                agent_id: "cu-1".to_owned(),
            },
        ]);
        let opts = simple_agent_opts("ck-1", test_repos());
        let _ = client.create_agent(&opts).await.unwrap();
        let _ = client.resume_agent("cu-1", &opts).await.unwrap();
        let calls = client.calls.lock().unwrap();
        assert_eq!(calls.as_slice(), &["create_agent", "resume_agent"]);
    }

    #[tokio::test]
    async fn fake_records_agent_options_for_audit() {
        let client = FakeCursorCloudClient::with_script(vec![ScriptedResponse::CreateAgent {
            agent_id: "cu-1".to_owned(),
        }]);
        let mut opts = simple_agent_opts("ck-1", test_repos());
        opts.model = Some("claude-3.5-sonnet".to_owned());
        opts.work_on_current_branch = true;
        let _ = client.create_agent(&opts).await.unwrap();
        let recorded = client.recorded_options.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].model.as_deref(), Some("claude-3.5-sonnet"));
        assert!(recorded[0].work_on_current_branch);
    }

    #[tokio::test]
    async fn multi_step_script_executes_in_order() {
        let run1 = finished_run("r-1", "cu-1", "first", Some("gpt-4"));
        let run2 = finished_run("r-2", "cu-1", "second", Some("gpt-4"));
        let client = FakeCursorCloudClient::with_script(vec![
            ScriptedResponse::CreateAgent {
                agent_id: "cu-1".to_owned(),
            },
            ScriptedResponse::SendPrompt { run: run1.clone() },
            ScriptedResponse::WaitFinal {
                final_run: run2.clone(),
            },
        ]);
        let opts = simple_agent_opts("ck-1", test_repos());
        let agent = client.create_agent(&opts).await.unwrap();
        let sent = client
            .send_prompt(&agent, "hello", &SendOptions::default())
            .await
            .unwrap();
        assert_eq!(sent.id, "r-1");
        let final_run = client.wait_for_run(&sent).await.unwrap();
        assert_eq!(final_run.id, "r-2");
    }

    #[test]
    fn cloud_error_displays_message() {
        let e = CloudError::new("oops").with_code("INVALID_REQUEST");
        assert_eq!(format!("{e}"), "oops");
        assert_eq!(e.gateway_code.as_deref(), Some("INVALID_REQUEST"));
    }

    #[test]
    fn cloud_error_with_details_serializes() {
        let e = CloudError::new("oops").with_details(json!({"k": "v"}));
        assert_eq!(e.details.unwrap(), json!({"k": "v"}));
    }

    #[test]
    fn finished_run_status_is_finished() {
        let r = finished_run("r-1", "cu-1", "done", Some("gpt-4"));
        assert_eq!(r.status, CloudRunStatus::Finished);
        assert_eq!(r.result.as_deref(), Some("done"));
    }

    #[test]
    fn errored_run_status_is_error() {
        let r = errored_run("r-1", "cu-1", "fail");
        assert_eq!(r.status, CloudRunStatus::Error);
        assert_eq!(r.result.as_deref(), Some("fail"));
    }

    #[test]
    fn agent_options_value_serializes_canonical_fields() {
        let opts = simple_agent_opts("ck-1", test_repos());
        let v = agent_options_value(&opts);
        assert_eq!(v["apiKey"], "ck-1");
        assert_eq!(v["envType"], "cloud");
        assert_eq!(v["repos"][0]["url"], "https://github.com/a/b");
    }

    #[test]
    fn transport_message_assistant_serializes_type_tag() {
        let m = SdkTransportMessage::Assistant {
            text: "hi".to_owned(),
        };
        let v = transport_message_to_value(&m);
        assert_eq!(v["type"], "assistant");
        assert_eq!(v["text"], "hi");
    }

    #[test]
    fn transport_message_tool_call_serializes_with_args() {
        let m = SdkTransportMessage::ToolCall {
            name: "grep".to_owned(),
            status: "running".to_owned(),
            args: Some(json!({"q": "x"})),
            result: None,
        };
        let v = transport_message_to_value(&m);
        assert_eq!(v["type"], "tool_call");
        assert_eq!(v["name"], "grep");
        assert_eq!(v["status"], "running");
        assert_eq!(v["args"]["q"], "x");
    }
}
