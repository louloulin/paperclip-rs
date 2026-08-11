//! Environment execution target 决策层。
//!
//! 对应 Node `server/src/services/environment-execution-target.ts`（259 行）1:1 复刻。
//! （原 `pc-environment-execution-target` crate 已下沉到 `pc-environment::execution_target`）。


use std::collections::HashMap;

/// 默认 sandbox remote cwd —— 与 Node `DEFAULT_SANDBOX_REMOTE_CWD` 一致。
pub const DEFAULT_SANDBOX_REMOTE_CWD: &str = "/tmp";

/// `Environment.driver` 字面量。
///
/// 与 Node `Environment["driver"]` 1:1 对齐。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EnvironmentDriver {
    Local,
    Sandbox,
    Ssh,
}

impl EnvironmentDriver {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Sandbox => "sandbox",
            Self::Ssh => "ssh",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "local" => Some(Self::Local),
            "sandbox" => Some(Self::Sandbox),
            "ssh" => Some(Self::Ssh),
            _ => None,
        }
    }
}

/// 解析后的 sandbox driver config（与 `resolveEnvironmentDriverConfigForRuntime`
/// 沙箱分支的返回结构对齐）。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxDriverConfig {
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// 其它未知字段，原样保留。
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// 解析后的 ssh driver config。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshDriverConfig {
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    pub username: String,
    pub remote_workspace_path: String,
    #[serde(default)]
    pub private_key: Option<String>,
    #[serde(default)]
    pub known_hosts: Option<String>,
    #[serde(default)]
    pub strict_host_key_checking: Option<bool>,
    /// 其它未知字段。
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

fn default_ssh_port() -> u16 {
    22
}

/// 解析后的 driver config —— discriminated union。
#[derive(Debug, Clone)]
pub enum ResolvedDriverConfig {
    Sandbox(SandboxDriverConfig),
    Ssh(SshDriverConfig),
}

impl ResolvedDriverConfig {
    pub fn driver(&self) -> EnvironmentDriver {
        match self {
            Self::Sandbox(_) => EnvironmentDriver::Sandbox,
            Self::Ssh(_) => EnvironmentDriver::Ssh,
        }
    }
}

/// 简化版 sandbox provider key → family 映射。
///
/// 完整映射在 Node `@paperclipai/adapter-utils`；这里实现闭集子集以保证 1:1 行为。
pub mod provider_family {
    pub const E2B: &str = "e2b";
    pub const DAYTONA: &str = "daytona";
    pub const MODAL: &str = "modal";
    pub const RUNSCOPE: &str = "runscope";
    pub const SHELLY: &str = "shelly";
    pub const VERCEL: &str = "vercel";
    pub const WORKSPACE: &str = "workspace";
    pub const PLUGIN: &str = "plugin";
    pub const UNKNOWN: &str = "unknown";
}

/// 把 sandbox provider key 归一化为低基数 family 字符串。
///
/// 与 Node `normalizeProviderFamily` 1:1 对齐：
/// - 已知闭集 → 闭集内字符串
/// - 其它非空 key → `"plugin"`
/// - 空 / 缺失 → `"unknown"`
pub fn normalize_provider_family(provider: Option<&str>) -> &'static str {
    let Some(p) = provider else {
        return provider_family::UNKNOWN;
    };
    let p = p.trim();
    if p.is_empty() {
        return provider_family::UNKNOWN;
    }
    match p {
        "e2b" => provider_family::E2B,
        "daytona" => provider_family::DAYTONA,
        "modal" => provider_family::MODAL,
        "runscope" => provider_family::RUNSCOPE,
        "shelly" => provider_family::SHELLY,
        "vercel" => provider_family::VERCEL,
        "workspace" => provider_family::WORKSPACE,
        _ => provider_family::PLUGIN,
    }
}

/// Adapter registry 能力查询 trait —— 用于 `adapterSupportsRemoteManagedEnvironments` 闸门。
///
/// 在真实部署中由 `pc-adapter-registry` 或类似 crate 实现；
/// 测试中用 in-memory 实现。
pub trait AdapterCapabilities: Send + Sync {
    fn supports_remote_managed_environments(&self, adapter_type: &str) -> bool;
}

/// In-memory 实现：用 `HashSet<String>` 表示支持 remote managed environments 的 adapter。
#[derive(Debug, Clone, Default)]
pub struct InMemoryAdapterCapabilities {
    pub supported: std::collections::HashSet<String>,
}

impl AdapterCapabilities for InMemoryAdapterCapabilities {
    fn supports_remote_managed_environments(&self, adapter_type: &str) -> bool {
        self.supported.contains(adapter_type)
    }
}

/// Sandbox runner trait —— 把命令分发到 sandbox runtime。
///
/// 完整实现会调用 `EnvironmentRuntimeService.execute(...)`；本 trait 只暴露
/// 决策层需要的 seam（execute / supportsSync / getRunnerHooks）。
pub trait SandboxRunner: Send + Sync {
    fn execute(
        &self,
        command: &str,
        args: &[String],
        cwd: &str,
        env: Option<&HashMap<String, String>>,
        stdin: Option<&str>,
        timeout_ms: Option<u64>,
    ) -> SandboxExecResult;
}

/// Sandbox exec 返回结构。
#[derive(Debug, Clone, Default)]
pub struct SandboxExecResult {
    pub exit_code: i32,
    pub signal: Option<String>,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: Option<u64>,
    pub get_duration_ms: Option<u64>,
}

/// 决策层需要的执行 seam —— 把 core decision 与具体执行解耦。
#[derive(Default)]
pub struct NullSandboxRunner;

impl SandboxRunner for NullSandboxRunner {
    fn execute(
        &self,
        _command: &str,
        _args: &[String],
        _cwd: &str,
        _env: Option<&HashMap<String, String>>,
        _stdin: Option<&str>,
        _timeout_ms: Option<u64>,
    ) -> SandboxExecResult {
        SandboxExecResult::default()
    }
}

/// 启动期 tracer span 的最小接口。
pub trait ExecSpan: Send + Sync {
    fn set_attribute(&self, key: &str, value: ExecAttrValue);
    fn end(&self);
}

/// Span 属性值 —— 限制为 `string | number | boolean`（与 OTel 语义对齐）。
#[derive(Debug, Clone)]
pub enum ExecAttrValue {
    Str(String),
    Num(f64),
    Bool(bool),
}

/// Tracer 接口 —— `startSpan(name) -> ExecSpan`。
pub trait ExecTracer: Send + Sync {
    fn start_span(&self, name: &str) -> Box<dyn ExecSpan>;
}

/// 默认 no-op tracer —— 所有 span 调用都是 no-op。
#[derive(Default)]
pub struct NoopExecTracer;

impl ExecTracer for NoopExecTracer {
    fn start_span(&self, _name: &str) -> Box<dyn ExecSpan> {
        Box::new(NoopSpan)
    }
}

#[derive(Default)]
struct NoopSpan;

impl ExecSpan for NoopSpan {
    fn set_attribute(&self, _key: &str, _value: ExecAttrValue) {}
    fn end(&self) {}
}

/// Recording tracer —— 收集 span 属性用于测试。
#[derive(Debug)]
pub struct RecordingTracer {
    pub spans: std::sync::Arc<std::sync::Mutex<Vec<RecordedSpan>>>,
}

impl Default for RecordingTracer {
    fn default() -> Self {
        Self { spans: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())) }
    }
}

impl Clone for RecordingTracer {
    fn clone(&self) -> Self {
        Self { spans: self.spans.clone() }
    }
}

#[derive(Debug, Clone)]
pub struct RecordedSpan {
    pub name: String,
    pub attributes: Vec<(String, ExecAttrValue)>,
    pub ended: bool,
}

impl ExecTracer for RecordingTracer {
    fn start_span(&self, name: &str) -> Box<dyn ExecSpan> {
        let span = RecordedSpan {
            name: name.to_string(),
            attributes: Vec::new(),
            ended: false,
        };
        self.spans.lock().unwrap().push(span.clone());
        Box::new(RecordingSpan {
            tracer: self.clone(),
            name: name.to_string(),
        })
    }
}

struct RecordingSpan {
    tracer: RecordingTracer,
    name: String,
}

impl ExecSpan for RecordingSpan {
    fn set_attribute(&self, key: &str, value: ExecAttrValue) {
        let mut spans = self.tracer.spans.lock().unwrap();
        if let Some(span) = spans.iter_mut().find(|s| s.name == self.name && !s.ended) {
            span.attributes.push((key.to_string(), value));
        }
    }
    fn end(&self) {
        let mut spans = self.tracer.spans.lock().unwrap();
        if let Some(span) = spans.iter_mut().find(|s| s.name == self.name && !s.ended) {
            span.ended = true;
        }
    }
}

/// 把属性值写入 span —— 仅在值是有限数字时。
///
/// 与 Node `setFiniteNumberAttr` 1:1 对齐。
pub fn set_finite_number_attr(span: &dyn ExecSpan, key: &str, value: ExecAttrValue) {
    if let ExecAttrValue::Num(n) = value {
        if n.is_finite() {
            span.set_attribute(key, ExecAttrValue::Num(n));
        }
    }
}

/// 把 leaseMetadata 中的 remoteCwd 解析为合法字符串，否则返回 fallback。
///
/// 与 Node 内联解析逻辑 1:1 对齐：
/// - 字符串且非空（trim 后） → trim 后的字符串
/// - 其它 → `fallback`
pub fn resolve_lease_remote_cwd(
    lease_metadata: Option<&serde_json::Value>,
    fallback: &str,
) -> String {
    let Some(meta) = lease_metadata else {
        return fallback.to_string();
    };
    let Some(meta_obj) = meta.as_object() else {
        return fallback.to_string();
    };
    let Some(remote_cwd) = meta_obj.get("remoteCwd") else {
        return fallback.to_string();
    };
    let Some(s) = remote_cwd.as_str() else {
        return fallback.to_string();
    };
    let trimmed = s.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

/// 解析 shellCommand —— 只接受 `"bash"` 或 `"sh"`。
///
/// 与 Node 内联校验逻辑 1:1 对齐。
pub fn resolve_shell_command(lease_metadata: Option<&serde_json::Value>) -> Option<&'static str> {
    let Some(meta) = lease_metadata else {
        return None;
    };
    let Some(meta_obj) = meta.as_object() else {
        return None;
    };
    let Some(shell) = meta_obj.get("shellCommand") else {
        return None;
    };
    let Some(s) = shell.as_str() else {
        return None;
    };
    match s {
        "bash" => Some("bash"),
        "sh" => Some("sh"),
        _ => None,
    }
}

/// AdapterExecutionTarget 的本地等价物（kebab-cased JSON）。
///
/// 用 enum 而非 struct 以保证 1:1 与 Node discriminated union 对齐。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
#[serde(rename_all_fields = "camelCase")]
pub enum AdapterExecutionTarget {
    Local {
        #[serde(skip_serializing_if = "Option::is_none")]
        environment_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        lease_id: Option<String>,
    },
    Remote {
        transport: RemoteTransport,
        #[serde(skip_serializing_if = "Option::is_none")]
        provider_key: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        shell_command: Option<&'static str>,
        remote_cwd: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        environment_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        lease_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        timeout_ms: Option<u64>,
        /// SSH 专用 spec；sandbox 时为 `None`。
        #[serde(skip_serializing_if = "Option::is_none")]
        spec: Option<SshRemoteSpec>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RemoteTransport {
    Sandbox,
    Ssh,
}

/// SSH 远程 spec —— 1:1 对应 Node `AdapterExecutionTarget.spec`。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshRemoteSpec {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub remote_workspace_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub known_hosts: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict_host_key_checking: Option<bool>,
    pub remote_cwd: String,
}

/// `resolveEnvironmentExecutionTarget` 的纯函数版本 —— 不依赖 runtime/tracer。
///
/// 用于在 Rust 端测试决策层逻辑。
///
/// 返回 `AdapterExecutionTarget` 或 `None`（表示该 driver 不被 adapter 支持）。
pub fn resolve_execution_target_pure(input: ResolveInput) -> Option<AdapterExecutionTarget> {
    let driver = EnvironmentDriver::from_str(&input.environment_driver)
        .unwrap_or(EnvironmentDriver::Local);

    match driver {
        EnvironmentDriver::Local => Some(AdapterExecutionTarget::Local {
            environment_id: input.environment_id.clone(),
            lease_id: input.lease_id.clone(),
        }),
        EnvironmentDriver::Sandbox => {
            if !input
                .adapter_capabilities
                .supports_remote_managed_environments(&input.adapter_type)
            {
                return None;
            }
            let sandbox_config = input.sandbox_config?;
            let remote_cwd =
                resolve_lease_remote_cwd(input.lease_metadata.as_ref(), DEFAULT_SANDBOX_REMOTE_CWD);
            let timeout_ms = sandbox_config.timeout_ms;
            let shell_command = resolve_shell_command(input.lease_metadata.as_ref());
            Some(AdapterExecutionTarget::Remote {
                transport: RemoteTransport::Sandbox,
                provider_key: Some(sandbox_config.provider),
                shell_command,
                remote_cwd,
                environment_id: input.environment_id.clone(),
                lease_id: input.lease_id.clone(),
                timeout_ms,
                spec: None,
            })
        }
        EnvironmentDriver::Ssh => {
            if !input
                .adapter_capabilities
                .supports_remote_managed_environments(&input.adapter_type)
            {
                return None;
            }
            let ssh_config = input.ssh_config?;
            let remote_cwd = resolve_lease_remote_cwd(
                input.lease_metadata.as_ref(),
                &ssh_config.remote_workspace_path,
            );
            Some(AdapterExecutionTarget::Remote {
                transport: RemoteTransport::Ssh,
                provider_key: None,
                shell_command: None,
                remote_cwd: remote_cwd.clone(),
                environment_id: input.environment_id.clone(),
                lease_id: input.lease_id.clone(),
                timeout_ms: None,
                spec: Some(SshRemoteSpec {
                    host: ssh_config.host,
                    port: ssh_config.port,
                    username: ssh_config.username,
                    remote_workspace_path: ssh_config.remote_workspace_path,
                    private_key: ssh_config.private_key,
                    known_hosts: ssh_config.known_hosts,
                    strict_host_key_checking: ssh_config.strict_host_key_checking,
                    remote_cwd,
                }),
            })
        }
    }
}

/// `resolve_execution_target_pure` 的输入 —— 不含 runtime/tracer。
#[derive(Clone)]
pub struct ResolveInput {
    pub adapter_type: String,
    pub environment_driver: String,
    pub environment_id: Option<String>,
    pub lease_id: Option<String>,
    pub lease_metadata: Option<serde_json::Value>,
    pub sandbox_config: Option<SandboxDriverConfig>,
    pub ssh_config: Option<SshDriverConfig>,
    pub adapter_capabilities: std::sync::Arc<dyn AdapterCapabilities>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn caps_with(supported: &[&str]) -> std::sync::Arc<dyn AdapterCapabilities> {
        let mut set = std::collections::HashSet::new();
        for s in supported {
            set.insert(s.to_string());
        }
        std::sync::Arc::new(InMemoryAdapterCapabilities { supported: set })
    }

    fn caps_none() -> std::sync::Arc<dyn AdapterCapabilities> {
        std::sync::Arc::new(InMemoryAdapterCapabilities::default())
    }

    fn default_local_input() -> ResolveInput {
        ResolveInput {
            adapter_type: "codex".to_string(),
            environment_driver: "local".to_string(),
            environment_id: Some("env-1".to_string()),
            lease_id: Some("lease-1".to_string()),
            lease_metadata: None,
            sandbox_config: None,
            ssh_config: None,
            adapter_capabilities: caps_none(),
        }
    }

    #[test]
    fn r702_local_driver_returns_local_target() {
        let input = default_local_input();
        let target = resolve_execution_target_pure(input).unwrap();
        match target {
            AdapterExecutionTarget::Local {
                environment_id,
                lease_id,
            } => {
                assert_eq!(environment_id.as_deref(), Some("env-1"));
                assert_eq!(lease_id.as_deref(), Some("lease-1"));
            }
            _ => panic!("expected Local"),
        }
    }

    #[test]
    fn r702_local_driver_does_not_require_capability() {
        // 即使 adapter 不支持 remote，local 也应该返回 Local target
        let mut input = default_local_input();
        input.adapter_capabilities = caps_none();
        assert!(matches!(
            resolve_execution_target_pure(input).unwrap(),
            AdapterExecutionTarget::Local { .. }
        ));
    }

    #[test]
    fn r702_local_driver_preserves_null_ids() {
        let mut input = default_local_input();
        input.environment_id = None;
        input.lease_id = None;
        let target = resolve_execution_target_pure(input).unwrap();
        match target {
            AdapterExecutionTarget::Local {
                environment_id,
                lease_id,
            } => {
                assert!(environment_id.is_none());
                assert!(lease_id.is_none());
            }
            _ => panic!("expected Local"),
        }
    }

    #[test]
    fn r702_sandbox_requires_capability() {
        let mut input = default_local_input();
        input.environment_driver = "sandbox".to_string();
        input.adapter_type = "codex".to_string();
        // adapter 不支持 remote
        input.adapter_capabilities = caps_none();
        assert!(resolve_execution_target_pure(input).is_none());
    }

    #[test]
    fn r702_sandbox_supported_returns_remote() {
        let mut input = default_local_input();
        input.environment_driver = "sandbox".to_string();
        input.adapter_type = "claude-local".to_string();
        input.adapter_capabilities = caps_with(&["claude-local"]);
        input.sandbox_config = Some(SandboxDriverConfig {
            provider: "e2b".to_string(),
            timeout_ms: Some(30_000),
            extra: HashMap::new(),
        });
        let target = resolve_execution_target_pure(input).unwrap();
        match target {
            AdapterExecutionTarget::Remote {
                transport,
                provider_key,
                timeout_ms,
                remote_cwd,
                ..
            } => {
                assert_eq!(transport, RemoteTransport::Sandbox);
                assert_eq!(provider_key.as_deref(), Some("e2b"));
                assert_eq!(timeout_ms, Some(30_000));
                assert_eq!(remote_cwd, DEFAULT_SANDBOX_REMOTE_CWD);
            }
            _ => panic!("expected Remote"),
        }
    }

    #[test]
    fn r702_sandbox_remote_cwd_from_lease_metadata() {
        let mut input = default_local_input();
        input.environment_driver = "sandbox".to_string();
        input.adapter_type = "claude-local".to_string();
        input.adapter_capabilities = caps_with(&["claude-local"]);
        input.sandbox_config = Some(SandboxDriverConfig {
            provider: "e2b".to_string(),
            timeout_ms: None,
            extra: HashMap::new(),
        });
        input.lease_metadata = Some(json!({"remoteCwd": "  /workspace/run  "}));
        let target = resolve_execution_target_pure(input).unwrap();
        match target {
            AdapterExecutionTarget::Remote { remote_cwd, .. } => {
                assert_eq!(remote_cwd, "/workspace/run");
            }
            _ => panic!("expected Remote"),
        }
    }

    #[test]
    fn r702_sandbox_remote_cwd_empty_falls_back_to_default() {
        let mut input = default_local_input();
        input.environment_driver = "sandbox".to_string();
        input.adapter_type = "claude-local".to_string();
        input.adapter_capabilities = caps_with(&["claude-local"]);
        input.sandbox_config = Some(SandboxDriverConfig {
            provider: "e2b".to_string(),
            timeout_ms: None,
            extra: HashMap::new(),
        });
        // 空字符串 / 纯空白
        for cwd in ["", "   ", "\t\n"] {
            let mut input = input.clone();
            input.lease_metadata = Some(json!({"remoteCwd": cwd}));
            let target = resolve_execution_target_pure(input).unwrap();
            if let AdapterExecutionTarget::Remote { remote_cwd, .. } = target {
                assert_eq!(remote_cwd, DEFAULT_SANDBOX_REMOTE_CWD);
            } else {
                panic!("expected Remote");
            }
        }
    }

    #[test]
    fn r702_sandbox_remote_cwd_non_string_falls_back() {
        let mut input = default_local_input();
        input.environment_driver = "sandbox".to_string();
        input.adapter_type = "claude-local".to_string();
        input.adapter_capabilities = caps_with(&["claude-local"]);
        input.sandbox_config = Some(SandboxDriverConfig {
            provider: "e2b".to_string(),
            timeout_ms: None,
            extra: HashMap::new(),
        });
        input.lease_metadata = Some(json!({"remoteCwd": 42}));
        let target = resolve_execution_target_pure(input).unwrap();
        if let AdapterExecutionTarget::Remote { remote_cwd, .. } = target {
            assert_eq!(remote_cwd, DEFAULT_SANDBOX_REMOTE_CWD);
        } else {
            panic!("expected Remote");
        }
    }

    #[test]
    fn r702_sandbox_shell_command_valid_values() {
        let mut input = default_local_input();
        input.environment_driver = "sandbox".to_string();
        input.adapter_type = "claude-local".to_string();
        input.adapter_capabilities = caps_with(&["claude-local"]);
        input.sandbox_config = Some(SandboxDriverConfig {
            provider: "e2b".to_string(),
            timeout_ms: None,
            extra: HashMap::new(),
        });
        for shell in ["bash", "sh"] {
            let mut input = input.clone();
            input.lease_metadata = Some(json!({"shellCommand": shell}));
            let target = resolve_execution_target_pure(input).unwrap();
            if let AdapterExecutionTarget::Remote { shell_command, .. } = target {
                assert_eq!(shell_command, Some(shell));
            } else {
                panic!("expected Remote");
            }
        }
    }

    #[test]
    fn r702_sandbox_shell_command_invalid_returns_null() {
        let mut input = default_local_input();
        input.environment_driver = "sandbox".to_string();
        input.adapter_type = "claude-local".to_string();
        input.adapter_capabilities = caps_with(&["claude-local"]);
        input.sandbox_config = Some(SandboxDriverConfig {
            provider: "e2b".to_string(),
            timeout_ms: None,
            extra: HashMap::new(),
        });
        for invalid in ["zsh", "fish", "powershell", "", "BASH"] {
            let mut input = input.clone();
            input.lease_metadata = Some(json!({"shellCommand": invalid}));
            let target = resolve_execution_target_pure(input).unwrap();
            if let AdapterExecutionTarget::Remote { shell_command, .. } = target {
                assert_eq!(shell_command, None, "shell '{invalid}' should be rejected");
            } else {
                panic!("expected Remote");
            }
        }
        // 非 string 类型
        input.lease_metadata = Some(json!({"shellCommand": 42}));
        let target = resolve_execution_target_pure(input).unwrap();
        if let AdapterExecutionTarget::Remote { shell_command, .. } = target {
            assert_eq!(shell_command, None);
        } else {
            panic!("expected Remote");
        }
    }

    #[test]
    fn r702_sandbox_timeout_ms_extracted() {
        let mut input = default_local_input();
        input.environment_driver = "sandbox".to_string();
        input.adapter_type = "claude-local".to_string();
        input.adapter_capabilities = caps_with(&["claude-local"]);
        input.sandbox_config = Some(SandboxDriverConfig {
            provider: "e2b".to_string(),
            timeout_ms: Some(123_456),
            extra: HashMap::new(),
        });
        let target = resolve_execution_target_pure(input).unwrap();
        if let AdapterExecutionTarget::Remote { timeout_ms, .. } = target {
            assert_eq!(timeout_ms, Some(123_456));
        } else {
            panic!("expected Remote");
        }
    }

    #[test]
    fn r702_sandbox_missing_config_returns_none() {
        // sandbox_config = None 视为解析失败 → None
        let mut input = default_local_input();
        input.environment_driver = "sandbox".to_string();
        input.adapter_type = "claude-local".to_string();
        input.adapter_capabilities = caps_with(&["claude-local"]);
        input.sandbox_config = None;
        assert!(resolve_execution_target_pure(input).is_none());
    }

    #[test]
    fn r702_ssh_requires_capability() {
        let mut input = default_local_input();
        input.environment_driver = "ssh".to_string();
        input.adapter_type = "codex".to_string();
        input.adapter_capabilities = caps_none();
        input.ssh_config = Some(SshDriverConfig {
            host: "h.example.com".to_string(),
            port: 22,
            username: "u".to_string(),
            remote_workspace_path: "/home/u".to_string(),
            private_key: None,
            known_hosts: None,
            strict_host_key_checking: None,
            extra: HashMap::new(),
        });
        assert!(resolve_execution_target_pure(input).is_none());
    }

    #[test]
    fn r702_ssh_supported_returns_remote() {
        let mut input = default_local_input();
        input.environment_driver = "ssh".to_string();
        input.adapter_type = "claude-local".to_string();
        input.adapter_capabilities = caps_with(&["claude-local"]);
        input.ssh_config = Some(SshDriverConfig {
            host: "h.example.com".to_string(),
            port: 2222,
            username: "deploy".to_string(),
            remote_workspace_path: "/srv/app".to_string(),
            private_key: Some("KEY".to_string()),
            known_hosts: Some("KNOWN".to_string()),
            strict_host_key_checking: Some(true),
            extra: HashMap::new(),
        });
        let target = resolve_execution_target_pure(input).unwrap();
        match target {
            AdapterExecutionTarget::Remote {
                transport,
                spec,
                remote_cwd,
                ..
            } => {
                assert_eq!(transport, RemoteTransport::Ssh);
                let spec = spec.unwrap();
                assert_eq!(spec.host, "h.example.com");
                assert_eq!(spec.port, 2222);
                assert_eq!(spec.username, "deploy");
                assert_eq!(spec.remote_workspace_path, "/srv/app");
                assert_eq!(spec.private_key.as_deref(), Some("KEY"));
                assert_eq!(spec.known_hosts.as_deref(), Some("KNOWN"));
                assert_eq!(spec.strict_host_key_checking, Some(true));
                // remote_cwd 默认从 config.remoteWorkspacePath
                assert_eq!(remote_cwd, "/srv/app");
            }
            _ => panic!("expected Remote"),
        }
    }

    #[test]
    fn r702_ssh_remote_cwd_from_lease_metadata() {
        let mut input = default_local_input();
        input.environment_driver = "ssh".to_string();
        input.adapter_type = "claude-local".to_string();
        input.adapter_capabilities = caps_with(&["claude-local"]);
        input.ssh_config = Some(SshDriverConfig {
            host: "h.example.com".to_string(),
            port: 22,
            username: "u".to_string(),
            remote_workspace_path: "/home/u".to_string(),
            private_key: None,
            known_hosts: None,
            strict_host_key_checking: None,
            extra: HashMap::new(),
        });
        input.lease_metadata = Some(json!({"remoteCwd": "/var/log"}));
        let target = resolve_execution_target_pure(input).unwrap();
        if let AdapterExecutionTarget::Remote { remote_cwd, spec, .. } = target {
            assert_eq!(remote_cwd, "/var/log");
            assert_eq!(spec.unwrap().remote_cwd, "/var/log");
        } else {
            panic!("expected Remote");
        }
    }

    #[test]
    fn r702_unknown_driver_returns_local_with_nulls() {
        // 非法 driver 字符串 fallback 到 local
        let mut input = default_local_input();
        input.environment_driver = "unknown-driver".to_string();
        let target = resolve_execution_target_pure(input).unwrap();
        match target {
            AdapterExecutionTarget::Local { environment_id, .. } => {
                assert_eq!(environment_id.as_deref(), Some("env-1"));
            }
            _ => panic!("expected Local"),
        }
    }

    #[test]
    fn r702_normalize_provider_family_known_keys() {
        assert_eq!(normalize_provider_family(Some("e2b")), provider_family::E2B);
        assert_eq!(normalize_provider_family(Some("daytona")), provider_family::DAYTONA);
        assert_eq!(normalize_provider_family(Some("modal")), provider_family::MODAL);
        assert_eq!(normalize_provider_family(Some("runscope")), provider_family::RUNSCOPE);
        assert_eq!(normalize_provider_family(Some("shelly")), provider_family::SHELLY);
        assert_eq!(normalize_provider_family(Some("vercel")), provider_family::VERCEL);
        assert_eq!(normalize_provider_family(Some("workspace")), provider_family::WORKSPACE);
    }

    #[test]
    fn r702_normalize_provider_family_plugin_for_unknown() {
        assert_eq!(
            normalize_provider_family(Some("custom-plugin-foo")),
            provider_family::PLUGIN
        );
    }

    #[test]
    fn r702_normalize_provider_family_unknown_for_empty() {
        assert_eq!(normalize_provider_family(None), provider_family::UNKNOWN);
        assert_eq!(normalize_provider_family(Some("")), provider_family::UNKNOWN);
        assert_eq!(normalize_provider_family(Some("   ")), provider_family::UNKNOWN);
    }

    #[test]
    fn r702_set_finite_number_attr_writes_finite() {
        let span = NoopSpan;
        set_finite_number_attr(&span, "k", ExecAttrValue::Num(42.0));
        set_finite_number_attr(&span, "k", ExecAttrValue::Num(0.0));
        set_finite_number_attr(&span, "k", ExecAttrValue::Num(-1.5));
        // 仅检查不 panic；属性读取通过 RecordingTracer 验证
    }

    #[test]
    fn r702_recording_tracer_captures_span_attributes() {
        let tracer = RecordingTracer::default();
        let span = tracer.start_span("provider.execute");
        span.set_attribute("provider", ExecAttrValue::Str("e2b".to_string()));
        set_finite_number_attr(&*span, "duration", ExecAttrValue::Num(100.5));
        // NaN / Infinity 应跳过
        set_finite_number_attr(&*span, "duration", ExecAttrValue::Num(f64::NAN));
        set_finite_number_attr(&*span, "duration", ExecAttrValue::Num(f64::INFINITY));
        span.end();
        let spans = tracer.spans.lock().unwrap();
        assert_eq!(spans.len(), 1);
        let s = &spans[0];
        assert_eq!(s.name, "provider.execute");
        assert!(s.ended);
        assert_eq!(s.attributes.len(), 2);
        assert_eq!(s.attributes[0].0, "provider");
        assert_eq!(s.attributes[1].0, "duration");
    }

    #[test]
    fn r702_recording_tracer_skips_non_finite_numbers() {
        let tracer = RecordingTracer::default();
        let span = tracer.start_span("test");
        set_finite_number_attr(&*span, "k", ExecAttrValue::Num(f64::NAN));
        set_finite_number_attr(&*span, "k", ExecAttrValue::Num(f64::INFINITY));
        set_finite_number_attr(&*span, "k", ExecAttrValue::Num(f64::NEG_INFINITY));
        span.end();
        let spans = tracer.spans.lock().unwrap();
        assert_eq!(spans[0].attributes.len(), 0);
    }

    #[test]
    fn r702_local_serializes_with_correct_shape() {
        let target = AdapterExecutionTarget::Local {
            environment_id: Some("e".to_string()),
            lease_id: Some("l".to_string()),
        };
        let v = serde_json::to_value(&target).unwrap();
        assert_eq!(v["kind"], "local");
        assert_eq!(v["environmentId"], "e");
        assert_eq!(v["leaseId"], "l");
    }

    #[test]
    fn r702_remote_sandbox_serializes_with_correct_shape() {
        let target = AdapterExecutionTarget::Remote {
            transport: RemoteTransport::Sandbox,
            provider_key: Some("e2b".to_string()),
            shell_command: Some("bash"),
            remote_cwd: "/tmp".to_string(),
            environment_id: Some("e".to_string()),
            lease_id: Some("l".to_string()),
            timeout_ms: Some(30_000),
            spec: None,
        };
        let v = serde_json::to_value(&target).unwrap();
        assert_eq!(v["kind"], "remote");
        assert_eq!(v["transport"], "sandbox");
        assert_eq!(v["providerKey"], "e2b");
        assert_eq!(v["shellCommand"], "bash");
        assert_eq!(v["timeoutMs"], 30_000);
        assert!(v.get("spec").is_none());
    }

    #[test]
    fn r702_remote_ssh_serializes_with_correct_shape() {
        let target = AdapterExecutionTarget::Remote {
            transport: RemoteTransport::Ssh,
            provider_key: None,
            shell_command: None,
            remote_cwd: "/srv/app".to_string(),
            environment_id: Some("e".to_string()),
            lease_id: None,
            timeout_ms: None,
            spec: Some(SshRemoteSpec {
                host: "h".to_string(),
                port: 22,
                username: "u".to_string(),
                remote_workspace_path: "/srv/app".to_string(),
                private_key: None,
                known_hosts: None,
                strict_host_key_checking: None,
                remote_cwd: "/srv/app".to_string(),
            }),
        };
        let v = serde_json::to_value(&target).unwrap();
        assert_eq!(v["kind"], "remote");
        assert_eq!(v["transport"], "ssh");
        assert!(v.get("providerKey").is_none());
        assert!(v.get("shellCommand").is_none());
        assert!(v.get("timeoutMs").is_none());
        assert_eq!(v["spec"]["host"], "h");
        assert_eq!(v["spec"]["port"], 22);
        assert_eq!(v["spec"]["username"], "u");
        assert_eq!(v["spec"]["remoteWorkspacePath"], "/srv/app");
    }

    #[test]
    fn r702_environment_driver_round_trip() {
        for d in [
            EnvironmentDriver::Local,
            EnvironmentDriver::Sandbox,
            EnvironmentDriver::Ssh,
        ] {
            assert_eq!(EnvironmentDriver::from_str(d.as_str()), Some(d));
        }
        assert_eq!(EnvironmentDriver::from_str("unknown"), None);
    }

    #[test]
    fn r702_resolved_driver_config_driver_accessor() {
        let sandbox = ResolvedDriverConfig::Sandbox(SandboxDriverConfig {
            provider: "e2b".to_string(),
            timeout_ms: None,
            extra: HashMap::new(),
        });
        assert_eq!(sandbox.driver(), EnvironmentDriver::Sandbox);
        let ssh = ResolvedDriverConfig::Ssh(SshDriverConfig {
            host: "h".to_string(),
            port: 22,
            username: "u".to_string(),
            remote_workspace_path: "/".to_string(),
            private_key: None,
            known_hosts: None,
            strict_host_key_checking: None,
            extra: HashMap::new(),
        });
        assert_eq!(ssh.driver(), EnvironmentDriver::Ssh);
    }
}
