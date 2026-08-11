//! Cursor Cloud result builder — 把 SDK Run / Error 转为 Paperclip
//! `AdapterExecutionResult` 形状（对齐 Node `execute.ts::execute`
//! 成功/失败两路径的末尾逻辑）。

#![allow(dead_code)]

use serde_json::{json, Value};

use crate::constants::{BILLER, BILLING_TYPE, PROVIDER};
use crate::session_codec::{CursorCloudRepo, CursorCloudSession};

/// SDK Run result 模拟（来自 cloud_client.rs 的 trait output 形状）。
#[derive(Debug, Clone, PartialEq)]
pub struct SdkRunResult {
    pub id: String,
    pub status: String, // "finished" | "error" | "running"
    pub result: Option<String>,
    pub model: Option<String>,
    pub duration_ms: Option<u64>,
    pub git: Option<Value>,
    pub agent_id: String,
}

/// Input — 完整 execute 成功路径构造 `Value` (跟 Node `resultJson` 对齐)。
#[derive(Debug, Clone)]
pub struct ResultBuilderInput<'a> {
    pub run: &'a SdkRunResult,
    pub env_type: &'a str,
    pub env_name: Option<&'a str>,
    pub repos: &'a [CursorCloudRepo],
    pub stream_error: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BuiltResult {
    pub status: String,
    pub exit_code: i32,
    pub is_error: bool,
    pub summary: Option<String>,
    pub next_session: CursorCloudSession,
    pub result_json: Value,
    pub model_id: Option<String>,
    pub error_message: Option<String>,
}

/// 成功路径构造（Node `execute` 末尾 `return { ... finished }` 路径）。
pub fn build_success(input: &ResultBuilderInput<'_>) -> BuiltResult {
    let is_error = input.run.status != "finished";
    let summary = to_summary(&input.run.result);
    let model_id = input
        .run
        .model
        .clone()
        .or_else(|| input.run.model.clone())
        .filter(|s| !s.trim().is_empty());

    let mut obj = serde_json::Map::new();
    obj.insert("status".into(), json!(input.run.status.clone()));
    obj.insert("cursorAgentId".into(), json!(input.run.agent_id.clone()));
    obj.insert("cursorRunId".into(), json!(input.run.id.clone()));
    obj.insert("envType".into(), json!(input.env_type));
    if let Some(n) = input.env_name {
        obj.insert("envName".into(), json!(n));
    }
    if !input.repos.is_empty() {
        obj.insert(
            "repos".into(),
            Value::Array(input.repos.iter().map(|r| r.to_json()).collect()),
        );
    }
    if let Some(r) = &input.run.result {
        obj.insert("result".into(), json!(r));
    }
    if let Some(g) = &input.run.git {
        obj.insert("git".into(), g.clone());
    }
    if let Some(d) = input.run.duration_ms {
        obj.insert("durationMs".into(), json!(d));
    }
    if let Some(se) = input.stream_error {
        obj.insert("streamError".into(), json!(se));
    }

    let next_session = CursorCloudSession {
        cursor_agent_id: input.run.agent_id.clone(),
        latest_run_id: Some(input.run.id.clone()),
        runtime: "cloud",
        env_type: Some(match input.env_type {
            "pool" => crate::session_codec::RuntimeEnvType::Pool,
            "machine" => crate::session_codec::RuntimeEnvType::Machine,
            _ => crate::session_codec::RuntimeEnvType::Cloud,
        }),
        env_name: input.env_name.map(str::to_owned),
        repos: input.repos.to_vec(),
    };

    BuiltResult {
        status: input.run.status.clone(),
        exit_code: if is_error { 1 } else { 0 },
        is_error,
        summary,
        next_session,
        result_json: Value::Object(obj),
        model_id,
        error_message: if is_error {
            Some(input.run.result.clone().unwrap_or_else(|| {
                input
                    .stream_error
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("Cursor run {}", input.run.status))
            }))
        } else {
            None
        },
    }
}

/// 失败路径构造（Node `execute` `catch` 块）。
#[derive(Debug, Clone)]
pub struct FailureInput<'a> {
    pub session: Option<&'a CursorCloudSession>,
    pub run_id: Option<&'a str>,
    pub error_message: &'a str,
}

pub fn build_failure(input: &FailureInput<'_>) -> BuiltResult {
    let cursor_agent_id = input.session.map(|s| s.cursor_agent_id.clone());
    let mut obj = serde_json::Map::new();
    obj.insert("status".into(), json!("error"));
    if let Some(rid) = input.run_id {
        obj.insert("cursorRunId".into(), json!(rid));
    }
    if let Some(aid) = &cursor_agent_id {
        obj.insert("cursorAgentId".into(), json!(aid));
    }
    obj.insert("error".into(), json!(input.error_message));
    BuiltResult {
        status: "error".to_owned(),
        exit_code: 1,
        is_error: true,
        summary: None,
        next_session: input
            .session
            .cloned()
            .unwrap_or_else(|| CursorCloudSession::new_minimal("")),
        result_json: Value::Object(obj),
        model_id: None,
        error_message: Some(input.error_message.to_owned()),
    }
}

impl CursorCloudRepo {
    fn to_json(&self) -> Value {
        let mut o = serde_json::Map::new();
        o.insert("url".into(), json!(self.url));
        if let Some(sr) = &self.starting_ref {
            o.insert("startingRef".into(), json!(sr));
        }
        if let Some(pr) = &self.pr_url {
            o.insert("prUrl".into(), json!(pr));
        }
        Value::Object(o)
    }
}

fn trim_nonempty(v: Option<&Value>) -> Option<String> {
    let s = v.and_then(|x| x.as_str())?;
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_owned())
    }
}

/// `toSummary` — 取 result 文本第一行非空行作 summary。
pub fn to_summary(result: &Option<String>) -> Option<String> {
    let text = result.as_deref()?;
    for line in text.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }
    None
}

/// `formatRunError` — Error → 字符串。
pub fn format_run_error(err: &str) -> String {
    let trimmed = err.trim();
    if trimmed.is_empty() {
        "unknown error".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Export fields — 给 `AdapterExecutionResult` 提供填充。
#[derive(Debug, Clone)]
pub struct AdapterExecutionOutcome {
    pub exit_code: i32,
    pub error_message: Option<String>,
    pub provider: &'static str,
    pub biller: &'static str,
    pub billing_type: &'static str,
    pub session_id: Option<String>,
    pub session_display_id: Option<String>,
    pub session_params: Option<CursorCloudSession>,
    pub model: Option<String>,
    pub summary: Option<String>,
    pub result_json: Option<Value>,
    pub clear_session: bool,
}

pub fn to_adapter_outcome(
    built: &BuiltResult,
    session: Option<&CursorCloudSession>,
) -> AdapterExecutionOutcome {
    AdapterExecutionOutcome {
        exit_code: built.exit_code,
        error_message: built.error_message.clone(),
        provider: PROVIDER,
        biller: BILLER,
        billing_type: BILLING_TYPE,
        session_id: session.map(|s| s.cursor_agent_id.clone()),
        session_display_id: session.map(|s| s.cursor_agent_id.clone()),
        session_params: Some(built.next_session.clone()),
        model: built.model_id.clone(),
        summary: built.summary.clone(),
        result_json: Some(built.result_json.clone()),
        clear_session: false,
    }
}

/// export — `AdapterReadinessReport` → JSON
pub fn _ignore_unused() {
    let _ = trim_nonempty;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fin_run(result: Option<&str>) -> SdkRunResult {
        SdkRunResult {
            id: "r-1".to_owned(),
            status: "finished".to_owned(),
            result: result.map(str::to_owned),
            model: Some("gpt-4".to_owned()),
            duration_ms: Some(1234),
            git: None,
            agent_id: "cu-1".to_owned(),
        }
    }

    #[test]
    fn build_success_finished_runs_yields_zero_exit_code() {
        let run = fin_run(Some("All good"));
        let input = ResultBuilderInput {
            run: &run,
            env_type: "cloud",
            env_name: None,
            repos: &[],
            stream_error: None,
        };
        let out = build_success(&input);
        assert_eq!(out.exit_code, 0);
        assert!(!out.is_error);
        assert_eq!(out.summary.as_deref(), Some("All good"));
        assert_eq!(out.next_session.cursor_agent_id, "cu-1");
        assert_eq!(out.next_session.latest_run_id.as_deref(), Some("r-1"));
        assert_eq!(
            out.next_session.env_type,
            Some(crate::session_codec::RuntimeEnvType::Cloud)
        );
        assert_eq!(out.result_json["status"], json!("finished"));
        assert_eq!(out.result_json["cursorRunId"], json!("r-1"));
        assert_eq!(out.result_json["durationMs"], json!(1234));
    }

    #[test]
    fn build_success_error_status_uses_fallback_message() {
        let mut run = fin_run(None);
        run.status = "error".to_owned();
        let input = ResultBuilderInput {
            run: &run,
            env_type: "cloud",
            env_name: None,
            repos: &[],
            stream_error: None,
        };
        let out = build_success(&input);
        assert_eq!(out.exit_code, 1);
        assert!(out.is_error);
        assert_eq!(out.error_message.as_deref(), Some("Cursor run error"));
    }

    #[test]
    fn build_success_error_uses_stream_error_when_result_missing() {
        let mut run = fin_run(None);
        run.status = "error".to_owned();
        let input = ResultBuilderInput {
            run: &run,
            env_type: "cloud",
            env_name: None,
            repos: &[],
            stream_error: Some("stream failure"),
        };
        let out = build_success(&input);
        assert_eq!(out.error_message.as_deref(), Some("stream failure"));
    }

    #[test]
    fn build_success_normalizes_env_type_pool() {
        let run = fin_run(None);
        let input = ResultBuilderInput {
            run: &run,
            env_type: "pool",
            env_name: Some("env-1"),
            repos: &[],
            stream_error: None,
        };
        let out = build_success(&input);
        assert_eq!(
            out.next_session.env_type,
            Some(crate::session_codec::RuntimeEnvType::Pool)
        );
        assert_eq!(out.next_session.env_name.as_deref(), Some("env-1"));
        assert_eq!(out.result_json["envName"], json!("env-1"));
    }

    #[test]
    fn build_success_normalizes_env_type_machine() {
        let run = fin_run(None);
        let input = ResultBuilderInput {
            run: &run,
            env_type: "machine",
            env_name: Some("mac-1"),
            repos: &[],
            stream_error: None,
        };
        let out = build_success(&input);
        assert_eq!(
            out.next_session.env_type,
            Some(crate::session_codec::RuntimeEnvType::Machine)
        );
    }

    #[test]
    fn build_success_includes_repos() {
        let run = fin_run(None);
        let repos = vec![CursorCloudRepo {
            url: "https://github.com/a/b".to_owned(),
            starting_ref: Some("main".to_owned()),
            pr_url: None,
        }];
        let input = ResultBuilderInput {
            run: &run,
            env_type: "cloud",
            env_name: None,
            repos: &repos,
            stream_error: None,
        };
        let out = build_success(&input);
        assert_eq!(out.result_json["repos"][0]["url"], "https://github.com/a/b");
        assert_eq!(out.next_session.repos.len(), 1);
    }

    #[test]
    fn build_failure_sets_exit_one_and_error_message() {
        let session = CursorCloudSession::new_minimal("cu-1");
        let input = FailureInput {
            session: Some(&session),
            run_id: Some("r-1"),
            error_message: "boom",
        };
        let out = build_failure(&input);
        assert_eq!(out.exit_code, 1);
        assert!(out.is_error);
        assert_eq!(out.error_message.as_deref(), Some("boom"));
        assert_eq!(out.result_json["status"], json!("error"));
        assert_eq!(out.result_json["error"], json!("boom"));
        assert_eq!(out.result_json["cursorRunId"], json!("r-1"));
        assert_eq!(out.result_json["cursorAgentId"], json!("cu-1"));
    }

    #[test]
    fn build_failure_without_run_id_omits_run_id_field() {
        let input = FailureInput {
            session: None,
            run_id: None,
            error_message: "kaboom",
        };
        let out = build_failure(&input);
        assert!(out.result_json.get("cursorRunId").is_none());
    }

    #[test]
    fn to_summary_picks_first_nonempty_line() {
        assert_eq!(to_summary(&Some("".to_owned())), None);
        assert_eq!(
            to_summary(&Some("\n\nhello\nworld".to_owned())).as_deref(),
            Some("hello")
        );
        assert_eq!(
            to_summary(&Some("   \nfirst".to_owned())).as_deref(),
            Some("first")
        );
    }

    #[test]
    fn to_summary_none_returns_none() {
        assert!(to_summary(&None).is_none());
    }

    #[test]
    fn format_run_error_returns_cleaned_string() {
        assert_eq!(format_run_error("boom"), "boom");
        assert_eq!(format_run_error("  boom  "), "boom");
        assert_eq!(format_run_error(""), "unknown error");
        assert_eq!(format_run_error("   "), "unknown error");
    }

    #[test]
    fn to_adapter_outcome_exposes_provider_metadata() {
        let run = fin_run(Some("Done"));
        let input = ResultBuilderInput {
            run: &run,
            env_type: "cloud",
            env_name: None,
            repos: &[],
            stream_error: None,
        };
        let built = build_success(&input);
        let outcome = to_adapter_outcome(&built, Some(&built.next_session));
        assert_eq!(outcome.provider, "cursor");
        assert_eq!(outcome.biller, "cursor");
        assert_eq!(outcome.billing_type, "api");
        assert_eq!(outcome.session_id.as_deref(), Some("cu-1"));
        assert_eq!(outcome.summary.as_deref(), Some("Done"));
        assert!(outcome.result_json.is_some());
        assert!(!outcome.clear_session);
    }
}
