//! `protocol_family` / `runtime_type` / `launch_header` 的派生 —— 上游 `pkg/agent`
//! 里那三张表的本地等价物。
//!
//! 为什么在 HTTP 层而不是仓储层：`mc-repos` 不依赖 `mc-runtime`（仓储层不引入领域
//! crate，见 `docs/39-M3-4-RUNTIME-PROFILES.md` §3），而 `runtime_profile.protocol_family` 的取值必须来自
//! `mc-runtime::catalog::AgentType` 的 25 项白名单，不能是本仓自造的目录。

use mc_runtime::catalog::AgentType;

/// 上游 `BuiltinRuntimes`（`pkg/agent/builtin_runtimes.go:82`）**只有一条**：
/// `{ID: "omp", ProtocolFamily: "pi", LaunchHeader: "omp (json mode)"}`。
/// `omp`（oh-my-pi）是复用 `pi` 协议族的独立 CLI，因此不在 25 项 `SupportedTypes` 里。
const OMP: &str = "omp";
const OMP_PROTOCOL_FAMILY: &str = "pi";
const OMP_LAUNCH_HEADER: &str = "omp (json mode)";

/// upstream `RuntimeProtocolFamily(rt)` → `(family, supported)`。
///
/// 命中 `BuiltinRuntimes` 的 id 先走内建描述符（只有 `omp` → `pi`），否则
/// `family == rt`，可用性由 25 项白名单决定。
pub(crate) fn runtime_protocol_family(runtime_type: &str) -> (String, bool) {
    if runtime_type == OMP {
        return (OMP_PROTOCOL_FAMILY.to_string(), true);
    }
    (
        runtime_type.to_string(),
        AgentType::parse(runtime_type).is_some(),
    )
}

/// upstream `ProfileRuntimeType(rt, pf)`：`rt` 非空取 `rt`，否则回退到 `pf`
/// （兼容 `runtime_type` 列出现之前建的老 profile）。
pub(crate) fn profile_runtime_type(runtime_type: &str, protocol_family: &str) -> String {
    if runtime_type.trim().is_empty() {
        protocol_family.to_string()
    } else {
        runtime_type.to_string()
    }
}

/// upstream `agent.LaunchHeader(provider)`：白名单命中取描述符，否则内建 `omp`，
/// 都没有则空串（不是错误 —— 老 daemon 报上来的未知 provider 只该显示空）。
pub(crate) fn launch_header(provider: &str) -> String {
    if let Some(agent_type) = AgentType::parse(provider) {
        return agent_type.launch_header().to_string();
    }
    if provider == OMP {
        return OMP_LAUNCH_HEADER.to_string();
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omp_maps_to_the_pi_family() {
        assert_eq!(runtime_protocol_family(OMP), ("pi".to_string(), true));
        assert_eq!(launch_header(OMP), "omp (json mode)");
    }

    #[test]
    fn whitelist_members_are_supported_and_unknown_ones_are_not() {
        assert_eq!(
            runtime_protocol_family("claude"),
            ("claude".to_string(), true)
        );
        assert_eq!(
            runtime_protocol_family("not-a-runtime"),
            ("not-a-runtime".to_string(), false)
        );
        assert_eq!(launch_header("not-a-runtime"), "");
    }

    #[test]
    fn profile_runtime_type_falls_back_to_the_family() {
        assert_eq!(profile_runtime_type("omp", "pi"), "omp");
        assert_eq!(profile_runtime_type("  ", "pi"), "pi");
    }
}
