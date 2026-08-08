//! Pi-local execute 助手函数。
//!
//! 完整复刻 Node `packages/adapters/pi-local/src/server/execute.ts` 中
//! 与 session 解析、模型拆分、biller 解析、resume 决策、session path 构造、
//! 重试规划相关的纯函数。这些都是高 ROI、可独立测试、与 fs / runtime
//! 解耦的小函数。

use std::collections::BTreeMap;
use std::time::SystemTime;

pub use pc_acpx::paths::{cwds_match, normalize_cwd};
use pc_acpx::{billing::infer_openai_compatible_biller, paths};

use crate::pi_stream_json::is_pi_unknown_session_error;

/// 解析 "provider/model" 形式的 provider 前缀（pi-local 兼容性 re-export）。
///
/// Node 等价：`parseModelProvider`（pi-local）。权威实现在 `pc_acpx::model_id`。
pub fn model_provider(model: Option<&str>) -> Option<String> {
    pc_acpx::model_id::parse_model_provider(model)
}

/// 解析 "provider/model" 形式的 model id（pi-local 兼容性 re-export）。
///
/// Node 等价：`parseModelId`（pi-local）。权威实现在 `pc_acpx::model_id`。
pub fn model_id(model: Option<&str>) -> Option<String> {
    pc_acpx::model_id::parse_model_id(model)
}

/// 解析 biller：env 中 OpenAI 兼容 hint 优先，否则 fallback 到 provider，最后 "unknown"。
///
/// Node 等价：`resolvePiBiller`。
pub fn resolve_pi_biller(
    env: &BTreeMap<String, String>,
    provider: Option<&str>,
) -> String {
    infer_openai_compatible_biller(env, None)
        .or_else(|| provider.map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

/// 读取 session 文件第一行 JSON 头里的 cwd（用于 resume 决策）。
///
/// Node 等价：`readSessionHeaderCwd`。第一行必须是 `{ "type": "session", "cwd": "..." }`。
pub fn parse_session_header_cwd(raw: &str) -> Option<String> {
    let header_line = raw
        .split('\n')
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let value: serde_json::Value = serde_json::from_str(header_line).ok()?;
    if value.get("type").and_then(serde_json::Value::as_str) != Some("session") {
        return None;
    }
    let cwd = value.get("cwd").and_then(serde_json::Value::as_str)?;
    let trimmed = cwd.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// 决策：stdout/stderr 是否触发 clear_session。
///
/// 等价 Node 的 `isPiUnknownSessionError(stdout, stderr)` 调用。
pub fn should_clear_session(stdout: &str, stderr: &str) -> bool {
    is_pi_unknown_session_error(stdout, stderr)
}

/// 决策：是否可 resume（saved_cwd 与 current_cwd 匹配且 saved_cwd 非空）。
///
/// Node 等价：saved cwd 与 current cwd 比较后置 `canResumeSession = true`。
pub fn should_resume(saved_cwd: Option<&str>, current_cwd: &str) -> bool {
    match saved_cwd {
        Some(saved) if !saved.trim().is_empty() => paths::cwds_match(saved, current_cwd),
        _ => false,
    }
}

/// 生成 `~/.pi/paperclips/<safe_timestamp>-<agent_id>.jsonl` 形式的本地 session 路径。
///
/// Node 等价：`buildSessionPath`。`:` 与 `.` 替换为 `-`，避免文件名在 Windows
/// 与某些 sandbox 文件系统上非法。
pub fn build_session_path(sessions_dir: &str, agent_id: &str, timestamp: &str) -> String {
    let safe_timestamp = sanitize_session_timestamp(timestamp);
    format!("{sessions_dir}/{safe_timestamp}-{agent_id}.jsonl")
}

/// 生成 `<runtime_root>/sessions/<safe_timestamp>-<agent_id>.jsonl` 形式的远程 session 路径。
///
/// Node 等价：`buildRemoteSessionPath`。无论本地 OS，路径都用 POSIX 分隔符。
pub fn build_remote_session_path(runtime_root_dir: &str, agent_id: &str, timestamp: &str) -> String {
    let safe_timestamp = sanitize_session_timestamp(timestamp);
    format!("{runtime_root_dir}/sessions/{safe_timestamp}-{agent_id}.jsonl")
}

fn sanitize_session_timestamp(timestamp: &str) -> String {
    timestamp.replace([':', '.'], "-")
}

/// 整合 Node `execute.ts` 中 `canResumeSession` 的判定逻辑（cwd / session params
/// / runtime 远程会话身份）。
///
/// `saved_session_cwd` 来自 session 文件头；`runtime_session_cwd` 来自
/// `runtime.sessionParams.cwd`；`effective_cwd` 是当前执行目录。
///
/// 任一字段缺失时按照 Node 行为：`runtimeSessionId` 为空 → 不可 resume；
/// `runtimeSessionCwd` 为空 → 跳过该维度检查；`savedSessionCwd` 为空 → 跳过该维度。
pub fn decide_resume(input: DecideResumeInput<'_>) -> bool {
    if input.runtime_session_id.trim().is_empty() {
        return false;
    }
    if !input.session_target_matches {
        return false;
    }
    if !input.runtime_session_cwd.trim().is_empty()
        && !should_resume(Some(input.runtime_session_cwd), input.effective_cwd)
    {
        return false;
    }
    if !input.saved_session_cwd.trim().is_empty()
        && !should_resume(Some(input.saved_session_cwd), input.effective_cwd)
    {
        return false;
    }
    true
}

/// `decide_resume` 的输入。
#[derive(Debug, Clone)]
pub struct DecideResumeInput<'a> {
    pub runtime_session_id: &'a str,
    pub runtime_session_cwd: &'a str,
    pub saved_session_cwd: &'a str,
    pub effective_cwd: &'a str,
    pub session_target_matches: bool,
}

/// 复刻 Node `execute.ts` 的 retry 决策：仅当首轮失败 + 是未知 session 错误
/// 时才触发一次重试。返回重试所需的 session 路径与 `clear_session` 标记。
///
/// Node 等价：
/// ```ts
/// if (canResumeSession && initialFailed && isPiUnknownSessionError(stdout, rawStderr)) {
///   return runAttempt(newSessionPath);
/// }
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryDecision {
    /// `true` 时表示上层应基于新 session path 再跑一次 attempt。
    pub should_retry: bool,
    /// 重试时建议的新 session 文件路径（仅在 `should_retry=true` 时有意义）。
    pub new_session_path: Option<String>,
    /// 重试成功后最终 `result.clearSession` 应置为 `true`（丢弃前一个 session id）。
    pub clear_session_on_retry: bool,
}

/// 根据当前 attempt 输出和 Node 的 retry 规则返回是否要重试。
///
/// `current_session_path` 是上一次 attempt 使用的 session 文件路径，仅用于
/// 日志；`build_new_session_path` 在需要重试时被调用以生成新路径。
pub fn retry_after_unknown_session<F>(
    input: RetryAfterUnknownInput<'_>,
    build_new_session_path: F,
) -> RetryDecision
where
    F: FnOnce() -> String,
{
    let initial_failed = !input.timed_out
        && (input.exit_code.unwrap_or(0) != 0 || !input.parsed_errors.is_empty());
    if !input.can_resume_session
        || !initial_failed
        || !is_pi_unknown_session_error(input.stdout, input.stderr)
    {
        return RetryDecision {
            should_retry: false,
            new_session_path: None,
            clear_session_on_retry: false,
        };
    }
    RetryDecision {
        should_retry: true,
        new_session_path: Some(build_new_session_path()),
        clear_session_on_retry: true,
    }
}

/// `retry_after_unknown_session` 的输入快照。
#[derive(Debug, Clone)]
pub struct RetryAfterUnknownInput<'a> {
    pub can_resume_session: bool,
    pub timed_out: bool,
    pub exit_code: Option<i32>,
    pub parsed_errors: &'a [String],
    pub stdout: &'a str,
    pub stderr: &'a str,
}

/// 给出默认 session 目录 `~/.pi/paperclips/`（与 Node `PAPERCLIP_SESSIONS_DIR` 对齐）。
///
/// 暴露成独立函数以便测试 override。
pub fn paperclip_sessions_dir() -> String {
    if let Ok(value) = std::env::var("PAPERCLIP_PI_SESSIONS_DIR") {
        return value;
    }
    if let Ok(home) = std::env::var("HOME") {
        return format!("{home}/.pi/paperclips");
    }
    "/tmp/.pi/paperclips".to_owned()
}

/// 给出"当前时间"的 ISO 8601 字符串用于 `build_session_path`。
///
/// 暴露成独立函数以便测试注入固定时间。
pub fn current_iso_timestamp() -> String {
    // 不依赖 chrono：使用 SystemTime + UNIX_EPOCH 推算。
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    format!("1970-01-01T00:00:{now:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_from(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn model_provider_拆分() {
        assert_eq!(
            model_provider(Some("anthropic/claude-sonnet-4")),
            Some("anthropic".to_owned())
        );
        assert_eq!(
            model_provider(Some("openai / gpt-5")),
            Some("openai".to_owned())
        );
    }

    #[test]
    fn model_provider_无斜杠返回None() {
        assert_eq!(model_provider(Some("claude-sonnet-4")), None);
        assert_eq!(model_provider(None), None);
        assert_eq!(model_provider(Some("")), None);
        assert_eq!(model_provider(Some("   ")), None);
    }

    #[test]
    fn model_provider_空前缀返回None() {
        // "/model" — provider 部分为空
        assert_eq!(model_provider(Some("/claude")), None);
    }

    #[test]
    fn model_id_拆分() {
        assert_eq!(
            model_id(Some("anthropic/claude-sonnet-4")),
            Some("claude-sonnet-4".to_owned())
        );
    }

    #[test]
    fn model_id_无斜杠返回整串() {
        assert_eq!(
            model_id(Some("claude-sonnet-4")),
            Some("claude-sonnet-4".to_owned())
        );
    }

    #[test]
    fn model_id_空后缀返回None() {
        assert_eq!(model_id(Some("anthropic/")), None);
    }

    #[test]
    fn model_id_空白输入返回None() {
        assert_eq!(model_id(None), None);
        assert_eq!(model_id(Some("")), None);
        assert_eq!(model_id(Some("   ")), None);
    }

    #[test]
    fn resolve_pi_biller_默认unknown() {
        let env = env_from(&[]);
        assert_eq!(resolve_pi_biller(&env, None), "unknown");
        assert_eq!(resolve_pi_biller(&env, Some("anthropic")), "anthropic");
    }

    #[test]
    fn resolve_pi_biller_openrouter_env优先() {
        let env = env_from(&[("OPENROUTER_API_KEY", "sk-or-test")]);
        assert_eq!(resolve_pi_biller(&env, Some("anthropic")), "openrouter");
    }

    #[test]
    fn resolve_pi_biller_provider作为fallback() {
        let env = env_from(&[]);
        assert_eq!(resolve_pi_biller(&env, Some("google")), "google");
    }

    #[test]
    fn parse_session_header_cwd_合法() {
        let raw = "{\"type\":\"session\",\"cwd\":\"/home/u/proj\",\"timestamp\":\"2026-08-08T00:00:00Z\"}\n";
        assert_eq!(parse_session_header_cwd(raw).as_deref(), Some("/home/u/proj"));
    }

    #[test]
    fn parse_session_header_cwd_非session_type() {
        let raw = "{\"type\":\"message\",\"cwd\":\"/x\"}\n";
        assert_eq!(parse_session_header_cwd(raw), None);
    }

    #[test]
    fn parse_session_header_cwd_损坏JSON返回None() {
        assert_eq!(parse_session_header_cwd("not-json\n"), None);
    }

    #[test]
    fn parse_session_header_cwd_空cwd返回None() {
        let raw = "{\"type\":\"session\",\"cwd\":\"\"}\n";
        assert_eq!(parse_session_header_cwd(raw), None);
    }

    #[test]
    fn parse_session_header_cwd_空输入返回None() {
        assert_eq!(parse_session_header_cwd(""), None);
        assert_eq!(parse_session_header_cwd("\n\n\n"), None);
    }

    #[test]
    fn parse_session_header_cwd_忽略前导空行() {
        let raw = "\n\n  {\"type\":\"session\",\"cwd\":\"/a\"}  \n";
        assert_eq!(parse_session_header_cwd(raw).as_deref(), Some("/a"));
    }

    #[test]
    fn should_clear_session_触发() {
        assert!(should_clear_session("", "unknown session id: abc"));
        assert!(should_clear_session("", "Session not found"));
        assert!(should_clear_session("stdout x", "no session"));
        assert!(should_clear_session("session abc not found", ""));
    }

    #[test]
    fn should_clear_session_正常文本() {
        assert!(!should_clear_session("ok", ""));
        assert!(!should_clear_session("", ""));
    }

    #[test]
    fn should_resume_cwd匹配() {
        assert!(should_resume(Some("/home/u/proj"), "/home/u/proj"));
        assert!(should_resume(Some("/home/u/proj/."), "/home/u/proj"));
        assert!(!should_resume(Some("/home/u/proj/sub"), "/home/u/proj/sub/.."));
    }

    #[test]
    fn should_resume_cwd不匹配() {
        assert!(!should_resume(Some("/home/u/proj"), "/home/u/other"));
        assert!(!should_resume(Some("/home/u/proj"), "/home/u/proj/sub"));
    }

    #[test]
    fn should_resume_saved_cwd为空() {
        assert!(!should_resume(None, "/any"));
        assert!(!should_resume(Some(""), "/any"));
        assert!(!should_resume(Some("   "), "/any"));
    }

    // build_session_path / build_remote_session_path

    #[test]
    fn build_session_path_替换不安全字符() {
        let path = build_session_path("/home/u/.pi/paperclips", "agent-1", "2026:08:08T10.00.00Z");
        assert_eq!(path, "/home/u/.pi/paperclips/2026-08-08T10-00-00Z-agent-1.jsonl");
    }

    #[test]
    fn build_session_path_保持安全字符() {
        let path = build_session_path("/tmp/s", "agent_2", "20260808T100000Z");
        assert_eq!(path, "/tmp/s/20260808T100000Z-agent_2.jsonl");
    }

    #[test]
    fn build_remote_session_path_posix分隔() {
        let path = build_remote_session_path("/srv/runtime", "agent-3", "2026:08:08T10.00.00Z");
        assert_eq!(path, "/srv/runtime/sessions/2026-08-08T10-00-00Z-agent-3.jsonl");
    }

    // decide_resume

    fn base_resume() -> DecideResumeInput<'static> {
        DecideResumeInput {
            runtime_session_id: "session-1",
            runtime_session_cwd: "/home/u/proj",
            saved_session_cwd: "/home/u/proj",
            effective_cwd: "/home/u/proj",
            session_target_matches: true,
        }
    }

    #[test]
    fn decide_resume_全部匹配() {
        assert!(decide_resume(base_resume()));
    }

    #[test]
    fn decide_resume_session_id为空_返回false() {
        let mut input = base_resume();
        input.runtime_session_id = "";
        assert!(!decide_resume(input));
    }

    #[test]
    fn decide_resume_target不匹配() {
        let mut input = base_resume();
        input.session_target_matches = false;
        assert!(!decide_resume(input));
    }

    #[test]
    fn decide_resume_runtime_cwd不匹配() {
        let mut input = base_resume();
        input.runtime_session_cwd = "/home/u/other";
        assert!(!decide_resume(input));
    }

    #[test]
    fn decide_resume_saved_cwd不匹配() {
        let mut input = base_resume();
        input.saved_session_cwd = "/home/u/other";
        assert!(!decide_resume(input));
    }

    #[test]
    fn decide_resume_跳过空白维度() {
        // runtime_session_cwd 与 saved_session_cwd 都为空 → 仅依据 session_id + target_matches。
        let mut input = base_resume();
        input.runtime_session_cwd = "";
        input.saved_session_cwd = "";
        assert!(decide_resume(input));
    }

    // retry_after_unknown_session

    fn base_retry(can_resume: bool) -> RetryAfterUnknownInput<'static> {
        RetryAfterUnknownInput {
            can_resume_session: can_resume,
            timed_out: false,
            exit_code: Some(1),
            parsed_errors: &[],
            stdout: "",
            stderr: "",
        }
    }

    #[test]
    fn retry_after_unknown_session_不满足条件() {
        // can_resume = false：永远不重试。
        let decision = retry_after_unknown_session(base_retry(false), || "x".to_owned());
        assert_eq!(decision.should_retry, false);
    }

    #[test]
    fn retry_after_unknown_session_首轮成功() {
        let mut input = base_retry(true);
        input.exit_code = Some(0);
        let decision = retry_after_unknown_session(input, || "x".to_owned());
        assert_eq!(decision.should_retry, false);
    }

    #[test]
    fn retry_after_unknown_session_parsed_error() {
        let mut input = base_retry(true);
        let errors: Vec<String> = vec!["oops".to_owned()];
        input.parsed_errors = errors.as_slice();
        input.exit_code = Some(0);
        let decision = retry_after_unknown_session(input, || "/tmp/new.jsonl".to_owned());
        assert_eq!(decision.should_retry, false);
    }

    #[test]
    fn retry_after_unknown_session_timed_out不重试() {
        let mut input = base_retry(true);
        input.timed_out = true;
        input.exit_code = None;
        input.stderr = "unknown session";
        let decision = retry_after_unknown_session(input, || "x".to_owned());
        assert_eq!(decision.should_retry, false);
    }

    #[test]
    fn retry_after_unknown_session_未知错误触发重试() {
        let mut input = base_retry(true);
        input.stderr = "unknown session id: abc";
        let decision = retry_after_unknown_session(input, || "/tmp/new.jsonl".to_owned());
        assert_eq!(decision.should_retry, true);
        assert_eq!(
            decision.new_session_path.as_deref(),
            Some("/tmp/new.jsonl")
        );
        assert_eq!(decision.clear_session_on_retry, true);
    }
}
