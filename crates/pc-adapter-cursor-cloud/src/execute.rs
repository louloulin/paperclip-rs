//! Cursor Cloud execute path —— 完整 Adapter execute path。
//!
//! 流程（对齐 Node `execute.ts::execute`）：
//! 1. 校验配置（`evaluate_readiness`）
//! 2. 解析 config 字段
//! 3. 决策：reuse existing agent vs create new
//! 4. 拼接 prompt (instructions + bootstrap + wake + env_note + prompt + handoff)
//! 5. buildWakeEnv
//! 6. cloudClient.create_agent / resume_agent
//! 7. cloudClient.send_prompt
//! 8. stream_messages → onLog (collect-then-emit)
//! 9. wait_for_run → final result
//! 10. result_builder::build_success → AdapterExecutionResult

#![allow(dead_code)]

use std::sync::Arc;

use async_trait::async_trait;
use pc_adapter_api::{
    Adapter, AdapterDescriptor, AdapterError, AdapterEvent, AdapterEventSink,
    AdapterExecutionContext, AdapterExecutionResult,
};
use serde_json::{json, Map, Value};

use crate::cloud_client::{
    finished_run, AgentOptions, CloudAgent, CloudError, CloudRun, CloudRunStatus,
    CursorCloudClient, DynClient, ScriptedResponse, SdkTransportMessage, SendOptions,
};
use crate::constants::{ADAPTER_LABEL, ADAPTER_TYPE, BILLER, BILLING_TYPE, PROVIDER};
use crate::event_codec::{
    event_line, init_event, message_event, result_event, status_event, CursorCloudEvent,
};
use crate::prompt_render::{
    assemble_with_handoff, env_note_from_wake_env, PromptParts, TemplateContext,
};
use crate::result_builder::{build_success, to_adapter_outcome, ResultBuilderInput, SdkRunResult};
use crate::session_codec::{
    deserialize_session, display_id, serialize_session, session_matches, CursorCloudRepo,
    CursorCloudSession, RuntimeEnvType,
};
use crate::wake_env::{build_wake_env, WakeEnvInput};

#[derive(Debug, Clone)]
pub enum ExecuteError {
    NotReady(String),
    MissingRepoUrl,
    Cloud(String),
}

impl std::fmt::Display for ExecuteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecuteError::NotReady(m) => write!(f, "not ready: {m}"),
            ExecuteError::MissingRepoUrl => write!(f, "missing repoUrl"),
            ExecuteError::Cloud(m) => write!(f, "cloud error: {m}"),
        }
    }
}

impl std::error::Error for ExecuteError {}

#[derive(Debug, Clone)]
pub struct ExecutionPlan {
    pub session: Option<CursorCloudSession>,
    pub can_resume: bool,
    pub final_prompt: String,
    pub agent_opts: AgentOptions,
    pub env_type: RuntimeEnvType,
    pub env_name: Option<String>,
    pub repo: CursorCloudRepo,
}

/// Parse env bindings into flat string map (handles plain + secret_ref + legacy flat).
pub fn parse_env_bindings(v: Option<&Value>) -> Map<String, Value> {
    let Some(obj) = v.and_then(|x| x.as_object()) else {
        return Map::new();
    };
    let mut out = Map::new();
    for (k, raw) in obj {
        if let Some(rec) = raw.as_object() {
            let kind = rec.get("type").and_then(|v| v.as_str()).unwrap_or("");
            if kind == "plain" {
                if let Some(val) = rec.get("value").and_then(|v| v.as_str()) {
                    out.insert(k.clone(), Value::String(val.to_owned()));
                }
            } else if kind == "secret_ref" {
                if let Some(sid) = rec.get("secretId").and_then(|v| v.as_str()) {
                    out.insert(k.clone(), json!({"secretRef": sid}));
                }
            }
        } else if let Some(s) = raw.as_str() {
            out.insert(k.clone(), Value::String(s.to_owned()));
        }
    }
    out
}

fn bool_field(v: &Value, key: &str, default: bool) -> bool {
    v.get(key).and_then(|x| x.as_bool()).unwrap_or(default)
}

fn render_template_inline(template: &str, ctx: &Value) -> String {
    let mut out = template.to_owned();
    while let Some(start) = out.find("{{") {
        let Some(end_rel) = out[start + 2..].find("}}") else {
            break;
        };
        let end = start + 2 + end_rel;
        let path = out[start + 2..end].trim().to_owned();
        let val = resolve_template_path(&path, ctx);
        out.replace_range(start..end + 2, &val);
    }
    out
}

fn resolve_template_path(path: &str, ctx: &Value) -> String {
    let mut current = ctx.clone();
    for segment in path.split('.') {
        match current.get(segment) {
            Some(v) => current = v.clone(),
            None => return String::new(),
        }
    }
    match current {
        Value::String(s) => s,
        other => other.to_string(),
    }
}

fn render_wake_prompt(wake: &Value, resumed: bool) -> String {
    let kind = wake.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let note = if resumed {
        "You are being re-invoked against the existing Cursor cloud session."
    } else {
        "You are being invoked against a new Cursor cloud session."
    };
    let task_id = wake.get("taskId").and_then(|v| v.as_str()).unwrap_or("");
    let issue_id = wake.get("issueId").and_then(|v| v.as_str()).unwrap_or("");
    let reason = wake
        .get("wakeReason")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    format!("Wake: kind={kind} taskId={task_id} issueId={issue_id} reason={reason}\n{note}")
}

/// Construct an `ExecutionPlan` from `adapter_config` JSON (pure function).
///
/// `wake_payload` / `workspace` are optional runtime context (caller extracts from
/// `runtime_config` or `paperclipWake` JSON field).
pub fn plan_execution(
    adapter_config: &Value,
    wake_payload: Option<&Value>,
    workspace: Option<&Value>,
    session_params: Option<&Value>,
    session_id: Option<&str>,
    agent: &Value,
    run_id: &str,
) -> Result<ExecutionPlan, ExecuteError> {
    let env_map = parse_env_bindings(adapter_config.get("env"));

    // 1. readiness
    use crate::{evaluate_readiness, readiness_error_message};
    let workspace_value = workspace.cloned().unwrap_or(Value::Null);
    let readiness = evaluate_readiness(
        &Value::Object(env_map.clone()),
        adapter_config,
        &workspace_value,
    );
    if !readiness.ready {
        return Err(ExecuteError::NotReady(
            readiness_error_message(&readiness).unwrap_or_else(|| "not ready".to_owned()),
        ));
    }

    // 2. CURSOR_API_KEY
    let api_key = env_map
        .get("CURSOR_API_KEY")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ExecuteError::NotReady("CURSOR_API_KEY missing".to_owned()))?
        .to_owned();

    // 3. repoUrl (config or workspace fallback)
    let repo_url = adapter_config
        .get("repoUrl")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            workspace_value
                .get("repoUrl")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .ok_or(ExecuteError::MissingRepoUrl)?
        .to_owned();

    let repo_starting_ref = adapter_config
        .get("repoStartingRef")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            workspace_value
                .get("repoRef")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .map(str::to_owned);
    let repo_pr_url = adapter_config
        .get("repoPullRequestUrl")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);

    let env_type = RuntimeEnvType::from_value(adapter_config.get("runtimeEnvType"));
    let env_name = adapter_config
        .get("runtimeEnvName")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);

    let model_str = adapter_config
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");

    let work_on_current_branch = bool_field(adapter_config, "workOnCurrentBranch", false);
    let auto_create_pr = bool_field(adapter_config, "autoCreatePR", false);
    let skip_reviewer_request = bool_field(adapter_config, "skipReviewerRequest", false);

    // 4. session
    let session_decoded = session_params.and_then(deserialize_session);

    let repo = CursorCloudRepo {
        url: repo_url,
        starting_ref: repo_starting_ref,
        pr_url: repo_pr_url,
    };

    let can_resume = if let Some(s) = session_decoded.as_ref() {
        session_matches(
            Some(s),
            env_type,
            env_name.as_deref(),
            std::slice::from_ref(&repo),
        )
    } else {
        !session_id
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_none()
    };

    // 5. wake env (build for prompt env_note only)
    let _wake_env_output = {
        let input = WakeEnvInput {
            config_env: env_map.clone(),
            agent: agent.clone(),
            run_id,
            workspace: workspace_value.clone(),
            wake: wake_payload,
            context_extras: Value::Null,
            auth_token: None,
        };
        build_wake_env(&input)
    };

    // 6. prompt render
    let template_ctx = TemplateContext::from_agent_run(agent, run_id);
    let template_data = template_ctx.to_template_data();

    let prompt_template = adapter_config
        .get("promptTemplate")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let bootstrap_template = adapter_config
        .get("bootstrapPromptTemplate")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let recovery_wake = wake_payload
        .and_then(|v| v.get("type"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_lowercase().contains("recover"))
        .unwrap_or(false);

    let resume_for_prompt = !recovery_wake && can_resume;
    let wake_prompt = match wake_payload {
        Some(w) => render_wake_prompt(w, resume_for_prompt),
        None => String::new(),
    };
    let bootstrap_prompt = if !resume_for_prompt && !bootstrap_template.trim().is_empty() {
        render_template_inline(bootstrap_template, &template_data)
    } else {
        String::new()
    };
    let rendered_prompt = if !resume_for_prompt || wake_prompt.is_empty() {
        if prompt_template.trim().is_empty() {
            String::new()
        } else {
            render_template_inline(prompt_template, &template_data)
        }
    } else {
        String::new()
    };
    let env_note = env_note_from_wake_env(&_wake_env_output);
    let session_handoff = adapter_config
        .get("paperclipSessionHandoffMarkdown")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("")
        .to_owned();

    let parts = PromptParts {
        instructions_prefix: "",
        bootstrap_prompt: &bootstrap_prompt,
        wake_prompt: &wake_prompt,
        env_note: &env_note,
        rendered_prompt: &rendered_prompt,
        session_handoff: &session_handoff,
    };
    let final_prompt = assemble_with_handoff(&parts);

    // 7. agent options
    let agent_opts = AgentOptions {
        api_key,
        name: format!(
            "Paperclip {}",
            agent
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("agent")
        ),
        model: if model_str.is_empty() {
            None
        } else {
            Some(model_str.to_owned())
        },
        env_type,
        env_name: env_name.clone(),
        repos: vec![repo.clone()],
        work_on_current_branch,
        auto_create_pr,
        skip_reviewer_request,
        env_vars: env_map
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
            .collect(),
    };

    Ok(ExecutionPlan {
        session: session_decoded,
        can_resume,
        final_prompt,
        agent_opts,
        env_type,
        env_name,
        repo,
    })
}

async fn emit_event(events: &AdapterEventSink, evt: &CursorCloudEvent) {
    let line = event_line(evt);
    let _ = events.emit(AdapterEvent::stdout(line)).await;
}

/// Bridge: `AdapterExecutionContext` → plan_execution args.
///
/// Adapter 持有 `run_id: Uuid` / `agent_id: Uuid` / `env: BTreeMap<String,String>`，
/// 我们把所有 `paperclipWake` / workspace 信息从 `runtime_config` + `adapter_config`
/// 中取出（Paperclip 渲染层负责把 wake payload 注入到 runtime_config 顶层）。
pub fn plan_from_context(context: &AdapterExecutionContext) -> Result<ExecutionPlan, ExecuteError> {
    let cfg = &context.adapter_config;
    let runtime_cfg = &context.runtime_config;

    let wake = runtime_cfg.get("paperclipWake").cloned();
    let workspace = runtime_cfg.get("paperclipWorkspace").cloned();
    let session_params = context.session_params.as_ref();
    let session_id = context.session_id.as_deref();

    let agent = runtime_cfg.get("agent").cloned().unwrap_or(Value::Null);

    plan_execution(
        cfg,
        wake.as_ref(),
        workspace.as_ref(),
        session_params,
        session_id,
        &agent,
        &context.run_id.to_string(),
    )
}

fn to_sdk_run(run: &CloudRun) -> SdkRunResult {
    SdkRunResult {
        id: run.id.clone(),
        status: match run.status {
            CloudRunStatus::Finished => "finished".to_owned(),
            CloudRunStatus::Error => "error".to_owned(),
            CloudRunStatus::Running => "running".to_owned(),
        },
        result: run.result.clone(),
        model: run.model.clone(),
        duration_ms: run.duration_ms,
        git: run.git.clone(),
        agent_id: run.agent_id.clone(),
    }
}

/// Execute the adapter flow against a (mockable) cloud client.
pub async fn execute_with_client(
    client: DynClient,
    context: AdapterExecutionContext,
    events: AdapterEventSink,
) -> Result<AdapterExecutionResult, AdapterError> {
    let plan = plan_from_context(&context).map_err(|e| AdapterError::Process(format!("{e}")))?;

    let env_type = plan.env_type;
    let env_name = plan.env_name.clone();
    let repo = plan.repo.clone();

    // 1. Resume or create
    let agent: CloudAgent = if plan.can_resume {
        let existing_id = plan
            .session
            .as_ref()
            .map(|s| s.cursor_agent_id.clone())
            .or_else(|| {
                context
                    .session_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
            })
            .ok_or_else(|| AdapterError::Process("missing agent id for resume".to_owned()))?;
        emit_event(
            &events,
            &status_event("running", Some(&format!("Resuming agent {existing_id}"))),
        )
        .await;
        client
            .resume_agent(&existing_id, &plan.agent_opts)
            .await
            .map_err(|e| AdapterError::Process(e.message.clone()))?
    } else {
        emit_event(
            &events,
            &status_event("running", Some("Creating new agent")),
        )
        .await;
        client
            .create_agent(&plan.agent_opts)
            .await
            .map_err(|e| AdapterError::Process(e.message.clone()))?
    };

    // 2. Send prompt
    let run = client
        .send_prompt(
            &agent,
            &plan.final_prompt,
            &SendOptions {
                model: plan.agent_opts.model.clone(),
            },
        )
        .await
        .map_err(|e| AdapterError::Process(e.message.clone()))?;

    emit_event(
        &events,
        &init_event(
            &run.agent_id,
            &run.agent_id,
            Some(&run.id),
            run.model.as_deref(),
        ),
    )
    .await;
    emit_event(
        &events,
        &status_event("running", Some(&format!("Started run {}", run.id))),
    )
    .await;
    let _ = events
        .emit(AdapterEvent::Session {
            session_id: Some(run.agent_id.clone()),
            session_params: None,
            display_id: Some(run.agent_id.clone()),
            at: chrono::Utc::now(),
        })
        .await;

    // 3. Stream (collect-then-emit)
    let mut collected: Vec<SdkTransportMessage> = Vec::new();
    let mut stream_error: Option<String> = None;
    if let Err(e) = client
        .stream_messages(&run, &mut |m| collected.push(m))
        .await
    {
        stream_error = Some(e.message);
    }
    use crate::cloud_client::transport_message_to_value;
    for m in &collected {
        let v = transport_message_to_value(m);
        emit_event(&events, &message_event(v)).await;
    }

    // 4. Wait final
    let final_run = client
        .wait_for_run(&run)
        .await
        .map_err(|e| AdapterError::Process(e.message.clone()))?;

    // 5. Build result
    let sdk_run = to_sdk_run(&final_run);
    let input = ResultBuilderInput {
        run: &sdk_run,
        env_type: env_type.as_str(),
        env_name: env_name.as_deref(),
        repos: std::slice::from_ref(&repo),
        stream_error: stream_error.as_deref(),
    };
    let built = build_success(&input);
    emit_event(
        &events,
        &result_event(
            &built.status,
            built.summary.as_deref(),
            built.model_id.as_deref(),
            sdk_run.duration_ms,
            sdk_run.git.clone(),
            built.error_message.as_deref(),
        ),
    )
    .await;

    let outcome = to_adapter_outcome(&built, Some(&built.next_session));
    let next_session_value = serialize_session(&built.next_session);
    let wrapped_params = json!({"next_session": next_session_value});

    Ok(AdapterExecutionResult {
        provider: Some(PROVIDER.to_owned()),
        exit_code: Some(built.exit_code),
        signal: None,
        timed_out: false,
        error_message: built.error_message.clone(),
        error_code: None,
        usage: None,
        session_id: outcome.session_id.clone(),
        session_params: Some(wrapped_params),
        session_display_id: outcome.session_display_id.clone(),
        model: outcome.model.clone(),
        billing_type: Some(BILLING_TYPE.to_owned()),
        cost_usd: None::<f64>,
        summary: outcome.summary.clone(),
        result_json: outcome.result_json.clone(),
        clear_session: outcome.clear_session,
    })
}

/// Default `CursorCloudAdapter` using `FakeCursorCloudClient`.
///
/// Provide a stub for build / dev paths; production should swap in a real
/// `DynClient` implementation (e.g. `ReqwestCursorCloudClient` once added).
pub struct CursorCloudAdapter {
    client: DynClient,
}

impl CursorCloudAdapter {
    pub fn new(client: DynClient) -> Self {
        Self { client }
    }

    /// 生产 runtime 工厂：构造基于 `reqwest` 的真实 HTTP client。
    ///
    /// 真实生产环境应提供 `CURSOR_CLOUD_BASE_URL`（默认 `https://api.cursor.com`）。
    /// 若 `api_key` 缺失则回退到 `FakeCursorCloudClient`（仅用于本地启动）。
    pub fn for_runtime(base_url: impl Into<String>, api_key: impl Into<String>) -> Self {
        let url: String = base_url.into();
        let key: String = api_key.into();
        let key = key.trim().to_owned();
        if key.is_empty() {
            return Self::default();
        }
        let base = if url.trim().is_empty() {
            "https://api.cursor.com".to_owned()
        } else {
            url
        };
        Self::new(Arc::new(crate::http_client::ReqwestCursorCloudClient::new(
            base, key,
        )))
    }
}

impl Default for CursorCloudAdapter {
    fn default() -> Self {
        Self::new(Arc::new(crate::cloud_client::FakeCursorCloudClient::new()))
    }
}

#[async_trait]
impl Adapter for CursorCloudAdapter {
    fn descriptor(&self) -> AdapterDescriptor {
        AdapterDescriptor::builtin(ADAPTER_TYPE, ADAPTER_LABEL)
    }

    async fn execute(
        &self,
        context: AdapterExecutionContext,
        events: AdapterEventSink,
    ) -> Result<AdapterExecutionResult, AdapterError> {
        execute_with_client(self.client.clone(), context, events).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_agent_config() -> Value {
        json!({
            "env": {"CURSOR_API_KEY": "ck-1"},
            "repoUrl": "https://github.com/a/b",
            "model": "gpt-4",
            "promptTemplate": "Hi {{ agentId }} from {{ runId }}"
        })
    }

    fn sample_agent() -> Value {
        json!({"id": "ag-1", "companyId": "co-1", "name": "Test"})
    }

    #[test]
    fn bool_field_default_when_missing() {
        let v = json!({});
        assert!(!bool_field(&v, "x", false));
        assert!(bool_field(&v, "x", true));
    }

    #[test]
    fn parse_env_bindings_handles_all_variants() {
        let v = json!({
            "env": {
                "FOO": {"type": "plain", "value": "bar"},
                "BAZ": {"type": "secret_ref", "secretId": "sec-1"},
                "FLAT": "qux",
                "BAD": 123,
            }
        });
        let m = parse_env_bindings(v.get("env"));
        assert_eq!(m.get("FOO").and_then(|v| v.as_str()), Some("bar"));
        assert_eq!(
            m.get("BAZ")
                .and_then(|v| v.get("secretRef"))
                .and_then(|v| v.as_str()),
            Some("sec-1")
        );
        assert_eq!(m.get("FLAT").and_then(|v| v.as_str()), Some("qux"));
        assert!(m.get("BAD").is_none());
    }

    #[test]
    fn parse_env_bindings_handles_missing_env() {
        let v = json!({});
        assert!(parse_env_bindings(v.get("env")).is_empty());
    }

    #[test]
    fn plan_execution_reports_not_ready_when_api_key_missing() {
        let cfg = json!({"repoUrl": "https://github.com/a/b"});
        let err =
            plan_execution(&cfg, None, None, None, None, &sample_agent(), "run-1").unwrap_err();
        assert!(matches!(err, ExecuteError::NotReady(_)));
    }

    #[test]
    fn plan_execution_reports_missing_repo_url_via_readiness() {
        let cfg = json!({"env": {"CURSOR_API_KEY": "ck-1"}});
        let err =
            plan_execution(&cfg, None, None, None, None, &sample_agent(), "run-1").unwrap_err();
        // readiness catches this first → NotReady (not MissingRepoUrl)
        assert!(matches!(err, ExecuteError::NotReady(_)));
    }

    #[test]
    fn plan_execution_reports_missing_repo_url_when_no_readiness_check() {
        // With sessionParams and apiKey in non-config env map, but config has no env map
        // — readiness will pass (since CURSOR_API_KEY comes from envMap), but plan_extraction
        // still needs repoUrl from workspace fallback.
        let cfg = json!({}); // no env binding
        let err =
            plan_execution(&cfg, None, None, None, None, &sample_agent(), "run-1").unwrap_err();
        // readiness returns NotReady (missing api_key + repo_url)
        assert!(matches!(err, ExecuteError::NotReady(_)));
    }

    #[test]
    fn plan_execution_full_config() {
        let plan = plan_execution(
            &sample_agent_config(),
            None,
            None,
            None,
            None,
            &sample_agent(),
            "run-1",
        )
        .unwrap();
        assert_eq!(plan.repo.url, "https://github.com/a/b");
        assert!(plan.final_prompt.contains("ag-1"));
        assert!(plan.final_prompt.contains("run-1"));
        assert_eq!(plan.agent_opts.model.as_deref(), Some("gpt-4"));
        assert!(!plan.can_resume);
    }

    #[test]
    fn plan_execution_can_resume_when_session_matches() {
        let mut session = CursorCloudSession::new_minimal("cu-existing");
        session.runtime = "cloud";
        // Must match repoURL in sample_agent_config for session_matches to succeed
        session.repos = vec![CursorCloudRepo {
            url: "https://github.com/a/b".to_owned(),
            starting_ref: None,
            pr_url: None,
        }];
        let session_params = serialize_session(&session);
        let plan = plan_execution(
            &sample_agent_config(),
            None,
            None,
            Some(&session_params),
            None,
            &sample_agent(),
            "run-1",
        )
        .unwrap();
        assert!(plan.can_resume);
    }

    #[test]
    fn plan_execution_no_resume_without_session() {
        let plan = plan_execution(
            &sample_agent_config(),
            None,
            None,
            None,
            None,
            &sample_agent(),
            "run-1",
        )
        .unwrap();
        assert!(!plan.can_resume);
    }

    #[tokio::test]
    async fn full_execute_create_branch() {
        let run = finished_run("r-1", "cu-1", "Hello world", Some("gpt-4"));
        let script = vec![
            ScriptedResponse::CreateAgent {
                agent_id: "cu-1".to_owned(),
            },
            ScriptedResponse::SendPrompt { run: run.clone() },
            ScriptedResponse::Stream {
                messages: vec![SdkTransportMessage::Assistant {
                    text: "hi".to_owned(),
                }],
            },
            ScriptedResponse::WaitFinal {
                final_run: run.clone(),
            },
        ];
        let client: DynClient = Arc::new(crate::cloud_client::FakeCursorCloudClient::with_script(
            script,
        ));
        let mut ctx =
            AdapterExecutionContext::new(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), "hello");
        ctx.adapter_config = json!({
            "env": {"CURSOR_API_KEY": "ck-1"},
            "repoUrl": "https://github.com/a/b",
        });
        ctx.runtime_config = json!({
            "agent": {"id": "ag-1", "companyId": "co-1", "name": "Test"},
        });
        let (_sink, _rx) = AdapterEventSink::channel(8);
        let result = execute_with_client(client, ctx, _sink)
            .await
            .expect("execute ok");
        assert_eq!(result.exit_code, Some(0));
        assert_eq!(result.session_id.as_deref(), Some("cu-1"));
    }

    #[tokio::test]
    async fn full_execute_resume_branch() {
        let run = finished_run("r-3", "cu-existing", "resumed", None);
        let script = vec![
            ScriptedResponse::ResumeAgent {
                agent_id: "cu-existing".to_owned(),
            },
            ScriptedResponse::SendPrompt { run: run.clone() },
            ScriptedResponse::Stream { messages: vec![] },
            ScriptedResponse::WaitFinal {
                final_run: run.clone(),
            },
        ];
        let client: DynClient = Arc::new(crate::cloud_client::FakeCursorCloudClient::with_script(
            script,
        ));
        let mut ctx =
            AdapterExecutionContext::new(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), "hello");
        ctx.adapter_config = json!({
            "env": {"CURSOR_API_KEY": "ck-1"},
            "repoUrl": "https://github.com/a/b",
        });
        ctx.runtime_config = json!({
            "agent": {"id": "ag-1", "companyId": "co-1", "name": "Test"},
        });
        let session = CursorCloudSession::new_minimal("cu-existing");
        ctx.session_params = Some(serialize_session(&session));
        let (_sink, _rx) = AdapterEventSink::channel(8);
        let result = execute_with_client(client, ctx, _sink)
            .await
            .expect("execute ok");
        assert_eq!(result.exit_code, Some(0));
    }

    #[tokio::test]
    async fn full_execute_error_branch() {
        let err_run = crate::cloud_client::errored_run("r-2", "cu-1", "boom");
        let script = vec![
            ScriptedResponse::CreateAgent {
                agent_id: "cu-1".to_owned(),
            },
            ScriptedResponse::SendPrompt {
                run: err_run.clone(),
            },
            ScriptedResponse::Stream { messages: vec![] },
            ScriptedResponse::WaitFinal {
                final_run: err_run.clone(),
            },
        ];
        let client: DynClient = Arc::new(crate::cloud_client::FakeCursorCloudClient::with_script(
            script,
        ));
        let mut ctx =
            AdapterExecutionContext::new(uuid::Uuid::new_v4(), uuid::Uuid::new_v4(), "hello");
        ctx.adapter_config = json!({
            "env": {"CURSOR_API_KEY": "ck-1"},
            "repoUrl": "https://github.com/a/b",
        });
        ctx.runtime_config = json!({
            "agent": {"id": "ag-1", "companyId": "co-1", "name": "Test"},
        });
        let (_sink, _rx) = AdapterEventSink::channel(8);
        let result = execute_with_client(client, ctx, _sink)
            .await
            .expect("execute ok");
        assert_eq!(result.exit_code, Some(1));
        assert!(result.error_message.is_some());
    }
}
