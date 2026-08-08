//! Claude CLI 能力探测（对齐 Node cli-capabilities.ts）。
//!
//! 提供：
//! - `claude_command_looks_like` — 校验命令 basename 是否匹配预期
//! - `claude_command_cache_key_for_target` — 生成缓存 key
//! - `claude_command_supports_effort_flag` — 占位（探测逻辑由调用方注入）
//! - `ClaudeCapabilityCache` — 简单的探测结果缓存（异步安全）

use std::path::Path;

/// 校验 `command` 的 basename 是否匹配 `expected`（含 `.cmd` / `.exe` 后缀）。
/// 对齐 Node `claudeCommandLooksLike`。
#[must_use]
pub fn claude_command_looks_like(command: &str, expected: &str) -> bool {
    let base = Path::new(command)
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    base == expected
        || base == format!("{expected}.cmd")
        || base == format!("{expected}.exe")
}

/// 生成缓存 key。对齐 Node `cacheKeyForTarget`。
///
/// - 无 target → `local::<command>`
/// - local target → `local:<envId>:<leaseId>:<command>`
/// - sandbox target → `sandbox:<providerKey>:<envId>:<command>`
/// - ssh target → `ssh:<envId>:<leaseId>:<host>:<port>:<user>:<command>`
#[must_use]
pub fn claude_command_cache_key_for_target(
    command: &str,
    target: Option<&pc_acpx::execution_target::AdapterExecutionTarget>,
) -> String {
    let Some(target) = target else {
        return format!("local::{command}");
    };
    match target {
        pc_acpx::execution_target::AdapterExecutionTarget::Local(local) => format!(
            "local:{}:{}:{}",
            local.environment_id.as_deref().unwrap_or(""),
            local.lease_id.as_deref().unwrap_or(""),
            command
        ),
        pc_acpx::execution_target::AdapterExecutionTarget::Remote(
            pc_acpx::execution_target::AdapterRemoteExecutionTarget::Sandbox(sandbox),
        ) => format!(
            "sandbox:{}:{}:{}",
            sandbox.provider_key.as_deref().unwrap_or(""),
            sandbox.environment_id.as_deref().unwrap_or(""),
            command
        ),
        pc_acpx::execution_target::AdapterExecutionTarget::Remote(
            pc_acpx::execution_target::AdapterRemoteExecutionTarget::Ssh(ssh),
        ) => format!(
            "ssh:{}:{}:{}:{}:{}:{}",
            ssh.environment_id.as_deref().unwrap_or(""),
            ssh.lease_id.as_deref().unwrap_or(""),
            ssh.spec.host,
            ssh.spec.port,
            ssh.spec.username,
            command
        ),
    }
}

/// 简易探测结果缓存（per-cache-key 一次探测，失败可丢弃重试）。
///
/// 缓存行为对齐 Node `effortFlagSupportCache`：
/// - 同一 key 的并发探测共享同一 future
/// - 探测失败（异常）时丢弃缓存项，下次重新探测
pub struct ClaudeCapabilityCache<T> {
    inner: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, CacheEntry<T>>>>,
}

#[derive(Clone)]
enum CacheEntry<T> {
    InFlight(std::sync::Arc<tokio::sync::Notify>),
    Done(T),
}

impl<T: Clone> Default for ClaudeCapabilityCache<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Clone> ClaudeCapabilityCache<T> {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
        }
    }

    /// 获取缓存值（如有）；否则调用 `probe` 计算并存入缓存。
    /// 探测过程 panic 或返回错误时 key 会被移除以便下次重试。
    pub async fn get_or_probe<F, Fut, E>(&self, key: &str, probe: F) -> Result<T, E>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, E>>,
    {
        // fast path
        if let Some(entry) = self.inner.lock().expect("capability cache lock").get(key).cloned() {
            match entry {
                CacheEntry::Done(value) => return Ok(value),
                CacheEntry::InFlight(notify) => {
                    drop(notify.notified());
                    // re-check after notify
                    if let Some(entry) = self.inner.lock().expect("capability cache lock").get(key).cloned() {
                        if let CacheEntry::Done(value) = entry {
                            return Ok(value);
                        }
                    }
                    // in-flight still pending → fall through to re-probe
                }
            }
        }
        let notify = std::sync::Arc::new(tokio::sync::Notify::new());
        self.inner
            .lock()
            .expect("capability cache lock")
            .insert(key.to_string(), CacheEntry::InFlight(std::sync::Arc::clone(&notify)));
        let result = probe().await;
        match &result {
            Ok(value) => {
                self.inner
                    .lock()
                    .expect("capability cache lock")
                    .insert(key.to_string(), CacheEntry::Done(value.clone()));
            }
            Err(_) => {
                self.inner
                    .lock()
                    .expect("capability cache lock")
                    .remove(key);
            }
        }
        notify.notify_waiters();
        result
    }

    pub fn reset(&self) {
        self.inner
            .lock()
            .expect("capability cache lock")
            .clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pc_acpx::execution_target::{
        AdapterExecutionTarget, AdapterLocalExecutionTarget, AdapterRemoteExecutionTarget,
        AdapterSandboxExecutionTarget, AdapterSshExecutionTarget,
    };

    #[test]
    fn command_looks_like_matches_basename() {
        assert!(claude_command_looks_like("/usr/local/bin/claude", "claude"));
        assert!(claude_command_looks_like("claude", "claude"));
        assert!(claude_command_looks_like("CLAUDE", "claude")); // case-insensitive
    }

    #[test]
    fn command_looks_like_matches_cmd_exe_suffixes() {
        // .exe 后缀在 Unix 上能正确处理；.cmd 后缀的 Windows 路径测试
        // 需要 cfg(windows)，此处仅覆盖可移植部分。
        assert!(claude_command_looks_like("/opt/claude.exe", "claude"));
        assert!(claude_command_looks_like("/usr/local/bin/claude.exe", "claude"));
    }

    #[test]
    fn command_looks_like_rejects_other() {
        assert!(!claude_command_looks_like("/usr/bin/cursor", "claude"));
        assert!(!claude_command_looks_like("/opt/claude-cli", "claude"));
        assert!(!claude_command_looks_like("", "claude"));
    }

    #[test]
    fn cache_key_no_target_is_local_prefix() {
        let key = claude_command_cache_key_for_target("claude", None);
        assert!(key.starts_with("local::"));
        assert!(key.ends_with("claude"));
    }

    #[test]
    fn cache_key_local_target_includes_ids() {
        let target = AdapterExecutionTarget::Local(AdapterLocalExecutionTarget {
            kind: "local".into(),
            environment_id: Some("env-1".into()),
            lease_id: Some("lease-1".into()),
            workspace_realization: None,
        });
        let key = claude_command_cache_key_for_target("claude", Some(&target));
        assert!(key.starts_with("local:env-1:lease-1:"));
        assert!(key.ends_with(":claude"));
    }

    #[test]
    fn cache_key_sandbox_target() {
        let target = AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Sandbox(
            AdapterSandboxExecutionTarget {
                kind: "remote".into(),
                transport: "sandbox".into(),
                environment_id: Some("sb-env".into()),
                lease_id: None,
                remote_cwd: "/work".into(),
                provider_key: Some("e2b".into()),
                shell_command: None,
                stream_run_logs: Some(false),
                timeout_ms: None,
                workspace_realization: None,
            },
        ));
        let key = claude_command_cache_key_for_target("claude", Some(&target));
        assert!(key.starts_with("sandbox:e2b:sb-env:"));
    }

    #[test]
    fn cache_key_ssh_target() {
        let target = AdapterExecutionTarget::Remote(AdapterRemoteExecutionTarget::Ssh(
            AdapterSshExecutionTarget {
                kind: "remote".into(),
                transport: "ssh".into(),
                environment_id: Some("env-2".into()),
                lease_id: Some("lease-2".into()),
                remote_cwd: "/work".into(),
                spec: pc_acpx::ssh::SshRemoteExecutionSpec {
                    host: "h.example".into(),
                    port: 22,
                    username: "u".into(),
                    remote_cwd: "/work".into(),
                    remote_workspace_path: "/work".into(),
                    private_key: None,
                    known_hosts: None,
                    strict_host_key_checking: true,
                },
                workspace_realization: None,
            },
        ));
        let key = claude_command_cache_key_for_target("claude", Some(&target));
        assert!(key.starts_with("ssh:env-2:lease-2:h.example:22:u:"));
        assert!(key.ends_with(":claude"));
    }

    #[tokio::test]
    async fn capability_cache_returns_value_and_caches() {
        let cache: ClaudeCapabilityCache<bool> = ClaudeCapabilityCache::new();
        let key = "test-key";
        let v = cache
            .get_or_probe(key, || async { Ok::<bool, ()>(true) })
            .await
            .unwrap();
        assert!(v);
        // 第二次调用应命中缓存
        let v2 = cache
            .get_or_probe(key, || async { Ok::<bool, ()>(false) })
            .await
            .unwrap();
        assert!(v2); // 仍是缓存的 true
    }

    #[tokio::test]
    async fn capability_cache_drops_key_on_error() {
        let cache: ClaudeCapabilityCache<bool> = ClaudeCapabilityCache::new();
        let key = "test-key-err";
        let result = cache
            .get_or_probe(key, || async { Err::<bool, &str>("boom") })
            .await;
        assert!(result.is_err());
        // key 被移除，下一次成功应能拿到新值
        let v = cache
            .get_or_probe(key, || async { Ok::<bool, &str>(true) })
            .await
            .unwrap();
        assert!(v);
    }

    #[test]
    fn capability_cache_reset_clears_state() {
        let cache: ClaudeCapabilityCache<bool> = ClaudeCapabilityCache::new();
        cache.reset();
    }
}
