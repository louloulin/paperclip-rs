//! `mc-runtime`：M3 的**运行时适配层** —— 把各家 coding agent CLI 统一成一套
//! "启动 / 流式 / 取消 / 探版本 / 自述能力"的契约。
//!
//! # 它在整条链路上的位置
//!
//! M3-3（`mc-task`）负责"该不该跑、跑几次、失败怎么重试"，本 crate 负责"怎么把
//! 一次 run 跑起来并把过程与终态如实报回去"。两边通过 [`LaunchRequest`] /
//! [`RunOutcome`] 对齐，**本 crate 不碰数据库**（M3-2 的范围约束）。
//!
//! ```text
//! mc-task（M3-3，租约/重试）
//!    └── AdapterRegistry::get(AgentType::Pi) ──► RuntimeAdapter::launch(request)
//!                                                   ├── RunHandle::next_event()（流式）
//!                                                   └── RunHandle::outcome()（终态）
//! ```
//!
//! # 模块地图
//!
//! | 模块 | 内容 |
//! |---|---|
//! | [`adapter`] | 契约本身：trait、请求/终态、事件、错误 |
//! | [`catalog`] | 25 个官方 agent 类型的白名单表（含上游 `launchHeaders` 启动骨架） |
//! | [`registry`] | `AgentType → Arc<dyn RuntimeAdapter>` 注册表（替换 M0 的 `AdapterRegistryStub`） |
//! | [`adapters`] | 各 provider 的实现（M3-2 的 `pi_local` + M3-8 批 1 的 7 项 + 批 2 的 8 项） |
//! | [`conformance`] | adapter 一致性套件（宏 + 假 CLI），M3-8 批量补 adapter 靠它 |
//!
//! # 五条契约（写新 adapter 前必须认同）
//!
//! 1. **启动失败 vs 运行失败是两件事**：`launch` 返回 `Err` 只代表"没能启动"
//!    （可执行文件缺失、会话被占用）；启动之后的任何失败都在 [`RunOutcome`] 里，
//!    因为那时调用方已经有 `run_id`，需要的是终态而不是异常。
//! 2. **`Started` 必是第一条事件**，且事件通道在终态到达前关闭。
//! 3. **终态必须给出**：run 任务无论走哪条路径（超时、取消、写 stdin 失败、
//!    子进程被信号杀死）都要发一次 [`RunOutcome`]；只有 adapter 任务本身 panic
//!    才会变成 [`AdapterError::OutcomeLost`]。
//! 4. **事件通道是无界的**：run 阻塞在 `send` 上就看不到取消信号。调用方要么持续
//!    消费事件，要么用 `outcome()`（它会 drop 接收端，让发送端立刻停发）。
//! 5. **白名单是硬边界**：[`AgentType`] 的取值必须与上游 `SupportedTypes` 一致，
//!    新增取值等于新增一个后端，属于产品决策而不是本 crate 的自由。
//!
//! # 与上游守护进程的关系
//!
//! 白名单、启动骨架、pi 的事件流解析与终态归因都是**逐条对齐**上游
//! `server/pkg/agent`（Go）的结果，见 [`adapters::pi_local`] 的模块文档与
//! `docs/18-M3-RUNTIME-ADAPTER.md`。有意偏离上游的地方（pi 的会话锁由 `flock`
//! 改成进程内锁等）都在那里记了原因。

pub mod adapter;
pub mod adapters;
pub mod catalog;
#[cfg(unix)]
pub mod conformance;
pub mod registry;

pub use adapter::{
    AdapterCapabilities, AdapterError, CancelOutcome, EventDecoder, EventReceiver, EventSender,
    FailureReason, LaunchRequest, ModelUsage, ProtocolFamily, RunHandle, RunId, RunOutcome,
    RunStatus, RuntimeAdapter, RuntimeEvent, Semver, TokenUsage, VersionProbe, STDERR_TAIL_LIMIT,
};
pub use adapters::{
    builtin_adapters, Antigravity, Claude, Codearts, Codebuddy, Codex, Copilot, Cursor, Deveco,
    Dim, Dsh, Grok, Hermes, Kimi, Kiro, Mcode, Openclaw, Opencode, PiDecoder, PiLocal,
    PiLocalConfig, Qoder, QoderCliCn, Qwen, Qwenpaw, Reasonix, TraeCli, Zeroclaw,
};
pub use catalog::{AgentType, UnknownAgentType};
#[cfg(unix)]
pub use conformance::{ConformanceScript, FakeCli, TestableAdapter};
pub use registry::AdapterRegistry;

/// 本 crate 适配层的名字/版本，用于日志与 run 元数据。
pub const CRATE_NAME: &str = "mc-runtime";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn re_exports_are_complete_for_registry_consumers() {
        // mc-http 只 `use mc_runtime::AdapterRegistry;`，其余调用点靠这些 re-export。
        let registry = AdapterRegistry::default();
        assert!(registry.is_empty());
        assert_eq!(AgentType::ALL.len(), 25);
        assert_eq!(CRATE_NAME, "mc-runtime");
    }

    #[test]
    fn builtin_registry_installs_pi_local() {
        let registry = AdapterRegistry::with_builtin_adapters();
        let adapter = registry.get(AgentType::Pi).expect("pi-local 必须注册");
        assert_eq!(adapter.kind(), AgentType::Pi);
        assert_eq!(adapter.capabilities().protocol, ProtocolFamily::JsonLine);
    }

    #[test]
    fn builtin_registry_covers_every_m3_8_batch() {
        // M3-8 的整体交付面：批 1 的 7 项 + 批 2 的 8 项 + 批 3 的 9 项 + pi，
        // 每项的协议族与 `catalog` 的映射一致（两处独立声明同一事实，所以要对得上，
        // 而不是“差不多”）。把批 3 也列进来，是因为批 3 的收口口径就是
        // “ `AgentType::ALL` 全员都有 adapter ”。
        let registry = AdapterRegistry::with_builtin_adapters();
        let batches = [
            // 批 1。
            AgentType::Claude,
            AgentType::Codebuddy,
            AgentType::Codex,
            AgentType::Copilot,
            AgentType::Opencode,
            AgentType::Codearts,
            AgentType::Deveco,
            // 批 2。
            AgentType::Cursor,
            AgentType::Kimi,
            AgentType::Kiro,
            AgentType::Antigravity,
            AgentType::Qoder,
            AgentType::QoderCliCn,
            AgentType::TraeCli,
            AgentType::Grok,
            // 批 3（本片）。
            AgentType::Openclaw,
            AgentType::Hermes,
            AgentType::Reasonix,
            AgentType::Dsh,
            AgentType::Qwen,
            AgentType::QwenPaw,
            AgentType::Mcode,
            AgentType::Dim,
            AgentType::Zeroclaw,
        ];
        for kind in batches {
            let adapter = registry
                .get(kind)
                .unwrap_or_else(|| panic!("{kind} 未注册"));
            assert_eq!(adapter.kind(), kind);
            assert_eq!(
                adapter.capabilities().protocol,
                kind.protocol_family(),
                "{kind}：adapter 自报的协议族与 catalog 映射不一致"
            );
            assert_ne!(
                adapter.capabilities().protocol,
                ProtocolFamily::Opaque,
                "{kind}：已落地的 adapter 不该未归类"
            );
            assert_eq!(adapter.capabilities().launch_header, kind.launch_header());
            // 取解码器不该 panic（契约：每次 run 一个独立实例）。这里只断言"能
            // 安全喂一行脏数据"，**不**断言零事件 —— 纯文本回退型解码器
            // （如 `antigravity`，上游同款）会把解析不了的行当正文回显。
            let _ = adapter.decoder().push_line("not json");
        }
        assert_eq!(registry.len(), 25);
        assert_eq!(
            registry.len(),
            AgentType::ALL.len(),
            "M3-8 收口：白名单里不该还有没实现的类型"
        );
    }
}
