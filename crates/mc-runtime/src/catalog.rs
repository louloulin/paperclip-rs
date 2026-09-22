//! adapter 元数据表：**25 项**支持类型白名单 + 用户可见的 launch header。
//!
//! 权威来源（逐条复制，不做改写）：
//!
//! - 白名单 = 上游 `server/pkg/agent/agent.go::SupportedTypes`（25 项，注释见该文件
//!   L333-349：它 "MUST stay in lockstep with the `runtime_profile.protocol_family`
//!   CHECK constraint"）；
//! - launch header = 同文件 `launchHeaders`（L502-528）。上游注释明确它"故意最小化：
//!   只有命令 + 子命令（没有子命令时给一个短模式标签）"，供 profile 选择器展示。
//!
//! ## 两个容易踩的坑（本片实测）
//!
//! 1. **数量是 25，不是 26**。`docs/15-M3-PLAN.md` §9.3 已更正；白名单与
//!    `crates/mc-core/src/runtime.rs::RuntimeProfile`（26 项，多一个 `omp`）**不是同一张表**。
//! 2. **`runtime_profile` 表/枚举不是 adapter 列表**。`RuntimeProfile` 是"runtime profile"
//!    这个领域概念的取值集（含 `Omp`，且命名是 kebab-case profile 名），而 adapter 列表是
//!    CLI backend 白名单（`kiro` / `claude` 这类 provider key）。两者只能按名字做**部分**映射，
//!    见 [`AgentType::runtime_profile`]。
//!
//! 本片只把 `pi` 接成真 adapter（[`crate::adapters::PiLocal`]），其余 24 项在 M3-8 分批落地；
//! 表本身在本片就位，所以 M3-8 不需要再动 [`AgentType`]。

use std::fmt;
use std::str::FromStr;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// 官方支持的 agent 类型（CLI backend provider key）。
///
/// 取值字符串 = 上游 `SupportedTypes` 的原文，**也是** 线上表示（手写 `Serialize` /
/// `Deserialize` 走 [`AgentType::as_str`] / [`AgentType::parse`]，避免 derive 出
/// `qoder_cli_cn` 这种与上游不一致的 `snake_case` 名）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AgentType {
    Claude,
    Codebuddy,
    Codex,
    Copilot,
    Opencode,
    Codearts,
    Deveco,
    Openclaw,
    Hermes,
    Pi,
    Cursor,
    Kimi,
    Reasonix,
    Dsh,
    Kiro,
    Antigravity,
    Qoder,
    QoderCliCn,
    TraeCli,
    Grok,
    Qwen,
    QwenPaw,
    Mcode,
    Dim,
    Zeroclaw,
}

impl AgentType {
    /// 全部 25 项，**按上游 `SupportedTypes` 的原始顺序**排列。
    pub const ALL: [Self; 25] = [
        Self::Claude,
        Self::Codebuddy,
        Self::Codex,
        Self::Copilot,
        Self::Opencode,
        Self::Codearts,
        Self::Deveco,
        Self::Openclaw,
        Self::Hermes,
        Self::Pi,
        Self::Cursor,
        Self::Kimi,
        Self::Reasonix,
        Self::Dsh,
        Self::Kiro,
        Self::Antigravity,
        Self::Qoder,
        Self::QoderCliCn,
        Self::TraeCli,
        Self::Grok,
        Self::Qwen,
        Self::QwenPaw,
        Self::Mcode,
        Self::Dim,
        Self::Zeroclaw,
    ];

    /// provider key（= 上游 `SupportedTypes` 里的字符串）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codebuddy => "codebuddy",
            Self::Codex => "codex",
            Self::Copilot => "copilot",
            Self::Opencode => "opencode",
            Self::Codearts => "codearts",
            Self::Deveco => "deveco",
            Self::Openclaw => "openclaw",
            Self::Hermes => "hermes",
            Self::Pi => "pi",
            Self::Cursor => "cursor",
            Self::Kimi => "kimi",
            Self::Reasonix => "reasonix",
            Self::Dsh => "dsh",
            Self::Kiro => "kiro",
            Self::Antigravity => "antigravity",
            Self::Qoder => "qoder",
            Self::QoderCliCn => "qoderclicn",
            Self::TraeCli => "traecli",
            Self::Grok => "grok",
            Self::Qwen => "qwen",
            Self::QwenPaw => "qwenpaw",
            Self::Mcode => "mcode",
            Self::Dim => "dim",
            Self::Zeroclaw => "zeroclaw",
        }
    }

    /// 解析 provider key；不在白名单里返回 `None`（不用默认值兜底，避免把拼错的
    /// 类型静默当成 `pi`）。
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|k| k.as_str() == value)
    }

    /// 用户可见的启动骨架（逐字复制上游 `launchHeaders`）。
    pub fn launch_header(self) -> &'static str {
        match self {
            Self::Antigravity => "agy -p (non-interactive)",
            Self::Claude => "claude (stream-json)",
            Self::Codebuddy => "codebuddy (stream-json)",
            Self::Codex => "codex app-server",
            Self::Copilot => "copilot (json)",
            Self::Cursor => "cursor-agent (stream-json)",
            Self::Codearts => "codearts run (json)",
            Self::Deveco => "deveco run (json)",
            Self::Hermes => "hermes acp",
            Self::Kimi => "kimi acp",
            Self::Reasonix => "reasonix acp",
            Self::Dsh => "dsh --profile multica (stdio)",
            Self::Kiro => "kiro-cli acp",
            Self::Openclaw => "openclaw agent (json)",
            Self::Opencode => "opencode run (json)",
            Self::Pi => "pi (json mode)",
            Self::Qoder => "qodercli --acp",
            Self::QoderCliCn => "qoderclicn --acp",
            Self::TraeCli => "traecli acp serve",
            Self::Grok => "grok agent stdio",
            Self::Qwen => "qwen -p (stream-json)",
            Self::QwenPaw => "qwenpaw acp",
            Self::Dim => "dim acp",
            Self::Mcode => "mcode acp",
            Self::Zeroclaw => "zeroclaw acp",
        }
    }

    /// 默认可执行文件名。
    ///
    /// 由 `launchHeaders` 的**第一个 token** 得出 —— 上游保证该字符串"只有命令 + 子命令"，
    /// 所以第一个 token 就是命令本身（`antigravity` → `agy`、`qoder` → `qodercli`、
    /// `kiro` → `kiro-cli`）。不另立一张表，避免与 header 漂移。
    pub fn cli_command(self) -> &'static str {
        self.launch_header().split(' ').next().unwrap_or("")
    }

    /// 按**名字**尝试映射到 `mc-core` 的 [`mc_core::runtime::RuntimeProfile`]。
    ///
    /// 这不是 1:1：两张表口径不同（白名单 25 项 provider key vs profile 26 项，
    /// profile 多一个 `omp`），且命名口径不同 —— 25 项里有 **3 项**匹配不上：
    /// `claude`/`claude-code`、`kiro`/`kiro-cli`、`qoder`（profile 只有 `qodercli`），
    /// 故返回 `None`。需要映射的消费方（M3-7 注册 runtime）
    /// 必须显式处理 `None`，不要 `unwrap_or_default`。
    pub fn runtime_profile(self) -> Option<mc_core::runtime::RuntimeProfile> {
        use mc_core::runtime::RuntimeProfile;
        RuntimeProfile::all()
            .into_iter()
            .find(|p| p.as_str() == self.as_str())
    }
}

impl fmt::Display for AgentType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AgentType {
    type Err = UnknownAgentType;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s).ok_or_else(|| UnknownAgentType(s.to_owned()))
    }
}

/// 解析白名单外的 agent 类型时的错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown agent type: {0}")]
pub struct UnknownAgentType(pub String);

impl Serialize for AgentType {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AgentType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).ok_or_else(|| D::Error::custom(UnknownAgentType(raw)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 白名单逐项锁死（顺序 + 取值原文）。改这一行等于改上游协议，必须连带改 `docs/18`。
    const WHITELIST: [(&str, &str); 25] = [
        ("claude", "claude (stream-json)"),
        ("codebuddy", "codebuddy (stream-json)"),
        ("codex", "codex app-server"),
        ("copilot", "copilot (json)"),
        ("opencode", "opencode run (json)"),
        ("codearts", "codearts run (json)"),
        ("deveco", "deveco run (json)"),
        ("openclaw", "openclaw agent (json)"),
        ("hermes", "hermes acp"),
        ("pi", "pi (json mode)"),
        ("cursor", "cursor-agent (stream-json)"),
        ("kimi", "kimi acp"),
        ("reasonix", "reasonix acp"),
        ("dsh", "dsh --profile multica (stdio)"),
        ("kiro", "kiro-cli acp"),
        ("antigravity", "agy -p (non-interactive)"),
        ("qoder", "qodercli --acp"),
        ("qoderclicn", "qoderclicn --acp"),
        ("traecli", "traecli acp serve"),
        ("grok", "grok agent stdio"),
        ("qwen", "qwen -p (stream-json)"),
        ("qwenpaw", "qwenpaw acp"),
        ("mcode", "mcode acp"),
        ("dim", "dim acp"),
        ("zeroclaw", "zeroclaw acp"),
    ];

    #[test]
    fn whitelist_is_25_and_matches_upstream_verbatim() {
        assert_eq!(
            AgentType::ALL.len(),
            25,
            "SupportedTypes 是 25 项（不是 26）"
        );
        assert_eq!(AgentType::ALL.len(), WHITELIST.len());
        for (kind, (name, header)) in AgentType::ALL.iter().zip(WHITELIST) {
            assert_eq!(kind.as_str(), name);
            assert_eq!(kind.launch_header(), header);
        }
    }

    #[test]
    fn serde_roundtrip_uses_provider_key() {
        for kind in AgentType::ALL {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
            let back: AgentType = serde_json::from_str(&json).unwrap();
            assert_eq!(back, kind);
        }
        // `QoderCliCn` 是本表里唯一会被 derive 成 `qoder_cli_cn` 的取值 ——
        // 手写 serde 的意义就是让它保持上游的 `qoderclicn`。
        assert_eq!(
            serde_json::to_string(&AgentType::QoderCliCn).unwrap(),
            "\"qoderclicn\""
        );
    }

    #[test]
    fn parse_rejects_unknown_instead_of_defaulting() {
        assert_eq!(AgentType::parse("pi"), Some(AgentType::Pi));
        assert_eq!(AgentType::parse(""), None);
        assert_eq!(AgentType::parse("Pi"), None);
        assert_eq!(AgentType::parse("claude-code"), None);
        assert!("nope".parse::<AgentType>().is_err());
    }

    #[test]
    fn cli_command_is_first_token_of_launch_header() {
        for kind in AgentType::ALL {
            let cmd = kind.cli_command();
            assert!(!cmd.is_empty(), "{kind} 的 launch header 缺命令");
            assert!(!cmd.contains('('), "{kind} 的第一个 token 不该是模式标签");
            assert_eq!(kind.launch_header().split(' ').next(), Some(cmd));
        }
        // 三个「provider key ≠ 命令名」的例子（上游 launchHeaders 明写）。
        assert_eq!(AgentType::Kiro.cli_command(), "kiro-cli");
        assert_eq!(AgentType::Qoder.cli_command(), "qodercli");
        assert_eq!(AgentType::Antigravity.cli_command(), "agy");
        assert_eq!(AgentType::Pi.cli_command(), "pi");
    }

    #[test]
    fn runtime_profile_mapping_is_partial_and_documented() {
        // profile 表 26 项、白名单 25 项 —— 不是同一张表。
        assert_eq!(mc_core::runtime::RuntimeProfile::all().len(), 26);
        assert_eq!(
            AgentType::Pi.runtime_profile(),
            Some(mc_core::runtime::RuntimeProfile::Pi)
        );
        assert_eq!(
            AgentType::Qwen.runtime_profile(),
            Some(mc_core::runtime::RuntimeProfile::Qwen)
        );
        // 命名口径不同 → 匹配不上（不是 bug，是两张表的定义差异）。
        assert_eq!(AgentType::Claude.runtime_profile(), None); // profile 名是 "claude-code"
        assert_eq!(AgentType::Kiro.runtime_profile(), None); // profile 名是 "kiro-cli"
        assert_eq!(AgentType::Qoder.runtime_profile(), None); // profile 只有 "qodercli"
        let matched = AgentType::ALL
            .iter()
            .filter(|k| k.runtime_profile().is_some())
            .count();
        assert_eq!(matched, 22, "25 项里 22 项能按名字匹配上 profile");
    }
}
