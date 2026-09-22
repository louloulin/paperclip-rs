//! 两个核心 trait：[`EventDecoder`]（可测的协议解码缝）与 [`RuntimeAdapter`]。
//!
//! 拆成子模块的唯一原因是 `adapter.rs` 要守住 gate ⑩（`scripts/file_size_check.py`）
//! 的 800 行上限；`pub use` 在 `adapter.rs` 里，对外面不变。

use async_trait::async_trait;

use super::{
    AdapterCapabilities, AdapterError, CancelOutcome, LaunchRequest, RunHandle, RunId,
    RuntimeEvent, VersionProbe,
};
use crate::catalog::AgentType;

/// 协议解码器：把 stdout 的一行喂进来，吐出 0..n 个 [`RuntimeEvent`]。
///
/// 这是"stream"关注点的**可测缝**：不启进程就能回放真实 transcript，
/// 因此一致性套件能对 25 个 adapter 用同一套断言，而不用 25 份进程级脚本。
pub trait EventDecoder: Send {
    /// 喂入一行（不含行尾换行）。未知/坏行返回空 `Vec`（协议容错：pi 的 stdout
    /// 里混日志行是常态，不能因此中断 run）。
    fn push_line(&mut self, line: &str) -> Vec<RuntimeEvent>;

    /// stdout 关闭后的冲刷（未闭合的文本增量在这里吐出）。
    fn finish(&mut self) -> Vec<RuntimeEvent>;
}

/// 一个 CLI backend 的适配器。
///
/// 实现者必须满足：
/// - `Send + Sync + 'static`（注册进 [`crate::AdapterRegistry`] 后跨 task 共享）；
/// - `launch` **不阻塞**在 run 结束上（它只负责 spawn + 返回句柄）；
/// - `cancel` 幂等（对已结束的 run 返回 [`CancelOutcome::NotRunning`]）；
/// - `launch` 返回的句柄遵守 [`RunHandle`] 的时序契约；
/// - `capabilities()` 自述的能力与实际行为一致 —— 一致性套件会按 `capabilities()`
///   的开关断言实际事件（例如 `usage_reporting = true` 就要求终态带用量）。
#[async_trait]
pub trait RuntimeAdapter: Send + Sync + 'static {
    /// 本 adapter 对应的官方类型（决定 registry 的键）。
    fn kind(&self) -> AgentType;

    /// 自述能力。
    fn capabilities(&self) -> AdapterCapabilities;

    /// 启动一次 run。失败 = **没能启动**（可执行文件缺失、会话忙），
    /// 启动之后的失败一律走 [`RunOutcome`]。
    async fn launch(&self, request: LaunchRequest) -> Result<RunHandle, AdapterError>;

    /// 取消一次 run（幂等）。
    async fn cancel(&self, run_id: &RunId) -> Result<CancelOutcome, AdapterError>;

    /// 探测 CLI 版本。
    async fn probe_version(&self) -> Result<VersionProbe, AdapterError>;

    /// 造一个协议解码器（每次 run 一个独立实例）。
    fn decoder(&self) -> Box<dyn EventDecoder>;
}
