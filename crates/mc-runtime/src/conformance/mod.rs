//! adapter 一致性套件（宏 + 假 CLI 测试台）。
//!
//! # 为什么要做成宏
//!
//! M3-8 要一次补齐 25 个 adapter。如果每个 adapter 各写一遍"启动、流式、非零退出、
//! 超时、取消"的测试，既会重复 25 遍，也会立刻退化成互相抄的散装断言 —— 某天有人
//! 少断言一条终态映射，没人会发现。
//!
//! 这里把断言集中在**一组** `check_*` 函数里，adapter 作者只需要实现
//! [`TestableAdapter`] 的两个方法，然后在自己的测试模块里写一行：
//!
//! ```ignore
//! crate::adapter_conformance!(MyAdapter);
//! ```
//!
//! 宏会展开出 8 个 `#[tokio::test]`（见 [`adapter_conformance`]），因此新增 adapter
//! 的测试成本是 0，而覆盖度是与所有 adapter **完全一致**的。
//!
//! # 不碰真 CLI
//!
//! 套件用 [`FakeCli`] 现场生成一个 `#!/bin/sh` 假 CLI（回放一段事件流、或睡死、
//! 或非零退出），因此：
//!
//! - CI 不需要装 pi/claude/codex；
//! - 断言的是**本 crate 的契约**（事件顺序、终态映射、prompt 走 stdin），
//!   而不是"某台机器上 CLI 的行为"。
//!
//! # 每个 adapter 必须满足的契约（套件逐条断言）
//!
//! 1. `kind()` 在白名单 `AgentType::ALL` 里，且 `as_str()` 往返成立；
//! 2. `capabilities().launch_header` 与白名单表里的启动骨架一致；
//! 3. `probe_version()` 能从 `--version` 输出里认出三段版本号，认不出时是
//!    `Ok(version: None)` 而**不是**错误；
//! 4. 一次正常 run：`Started` 是第一条事件、文本事件拼起来等于终态 `output`、
//!    终态 `Completed` + 无 `failure_reason` + `exit_code == Some(0)`；
//! 5. prompt 走 stdin（`argv` 里不得出现 prompt 正文）；
//! 6. 进程非零退出 → `Failed` + `AgentError`，且 stderr 尾部带上诊断文本；
//! 7. 超时 → `Timeout`；
//! 8. `cancel` 幂等 → 终态 `Cancelled` + `Manual`；
//! 9. 解码器容忍非 JSON / 未知事件类型，不会因此中断 run。

use std::path::Path;
use std::time::Duration;

use crate::adapter::{
    AdapterCapabilities, CancelOutcome, LaunchRequest, ProtocolFamily, RunStatus, RuntimeAdapter,
    RuntimeEvent,
};
use crate::catalog::AgentType;

mod fake_cli;

pub use fake_cli::FakeCli;

/// adapter 作者提供的一致性套件配置。
#[derive(Debug, Clone)]
pub struct ConformanceScript {
    /// `--version` 的 stdout。
    pub version_stdout: String,
    /// 期望从 `version_stdout` 里解析出的版本（`None` = 该输出里没有版本号）。
    pub expected_version: Option<String>,
    /// 一次正常 run 的 stdout（该 adapter 的协议格式）。
    pub success_stdout: String,
    /// 正常 run 结束后 `RunOutcome::output` 的期望值。
    pub expected_output: String,
    /// 正常 run 结束后的总 token 数期望（`None` = 该 adapter 不上报用量）。
    pub expected_usage_tokens: Option<u64>,
    /// 一段混入脏数据的 stdout（非 JSON 行 + 未知事件类型）。
    pub junk_stdout: String,
    /// 脏数据流里应当被解析出来的正文。
    pub expected_junk_output: String,
    /// 非零退出时写到 stderr 的诊断文本。
    pub expected_error: String,
    /// prompt 是否必须出现在 stdin（CLI 类 adapter 都是 `true`）。
    pub prompt_via_stdin: bool,
}

/// adapter 作者为了接入一致性套件要实现的两个方法。
///
/// 之所以要 `with_conformance_env` 而不是让套件直接用 `Default`：adapter 可能
/// 自带落盘目录（例如 pi 的会话文件），套件必须保证它们落在临时目录里，
/// 不能污染开发者/CI 的 `$HOME`。
pub trait TestableAdapter: RuntimeAdapter + Sized {
    /// 用假 CLI 与专用工作目录构造实例。
    fn with_conformance_env(executable: &Path, workdir: &Path) -> Self;

    /// 套件要回放的事件流与期望值。
    fn conformance_script() -> ConformanceScript;

    /// 覆盖"怎么推假 CLI 回放"（默认按 [`ProtocolFamily`] 猜，见 [`LivePlan`]）。
    ///
    /// 只有协议族不足以描述传输形态的 adapter 才需要实现（例如 `dsh`：协议族是
    /// `JsonLine`，但 stdin 常开、帧由 outbox 驱动）。
    fn conformance_live_plan() -> Option<LivePlan> {
        None
    }
}

fn build<A: TestableAdapter>(fake: &FakeCli) -> A {
    A::with_conformance_env(&fake.path(), fake.dir())
}

/// adapter 自报的协议族（用一个只打印版本的假 CLI 构造一次）。
fn protocol_of<A: TestableAdapter>() -> ProtocolFamily {
    let fake = FakeCli::version("x\n");
    build::<A>(&fake).capabilities().protocol
}

/// 长连接 stdin（JSON-RPC 类）协议的假 CLI 回放计划。
///
/// 四个变体对应四种"谁在推着回放往前走"：
///
/// * `Eof`：普通前台协议，`cat > stdin.txt` 读到 EOF 就放（prompt 写完即关 stdin）；
/// * `Gate`：单闸门协议，先确认 adapter 已把第一帧写进 stdin 再一次性回放，退出前
///   再等带 prompt 的那一帧落盘（见 [`FakeCli::live_replaying`]）；
/// * `Frames`：固定帧 id 的请求/应答协议，一帧一帧推（见 [`FakeCli::live_scripted`]）；
/// * `GateOnce`：长连接但**客户端只发一帧**的协议（`dsh`）：等那一帧落盘再整段回放，
///   不做第二道退出门（后面根本没有第二帧可等）。
///
/// 挂错计划的后果都是**挂死**而不是误判：长连接协议若用 `Eof`，双方的"等对方先说话"
/// 会一直顶到 run 超时（这是本片修掉的真实故障）；`Frames` 若整段一次性回放，
/// 相位不对的应答被解码器丢掉，流永远起不来。
pub enum LivePlan {
    /// 靠 stdin EOF 推进。
    Eof,
    /// `(回放前要等到的子串, 退出前要等到的子串)`。
    Gate(&'static str, &'static str),
    /// `(客户端帧里的子串, 该帧到达后回放的文本)`。
    Frames(Vec<(String, String)>),
    /// 等这一帧落盘 → 整段回放 → 0 退出（单帧长连接协议）。
    GateOnce(&'static str),
}

/// 按协议族挑回放计划。
///
/// adapter 可以先用 [`TestableAdapter::conformance_live_plan`] 覆盖：协议族
/// （[`ProtocolFamily`]）不足以确定回放方式 —— `dsh` 的协议族是 `JsonLine`，
/// 但它是**长连接**（stdin 常开、帧由解码器 outbox 驱动），套件默认的 `Eof`
/// 会与它互等。
fn live_plan<A: TestableAdapter>(script: &ConformanceScript) -> LivePlan {
    if let Some(plan) = A::conformance_live_plan() {
        return plan;
    }
    match protocol_of::<A>() {
        ProtocolFamily::AppServer => LivePlan::Gate("thread/start", "turn/start"),
        ProtocolFamily::Acp => LivePlan::Frames(response_frame_groups(&script.success_stdout)),
        _ => LivePlan::Eof,
    }
}

/// 请求/应答协议的"逐帧回放"分帧：把回放文本按**应答帧**切成组，组 k 的触发条件是
/// 客户端帧里出现 `"id":k,`。
///
/// 依据（`acp_core::client` 的固定帧 id 约定，见 `docs/33` §6.3）：
///
/// 1. 客户端帧是 `serde_json` 紧凑序列化 ⇒ 帧里一定有 `"id":<n>,` 子串，而本 crate
///    的 id 是固定序号（`ID_INITIALIZE`..`ID_PROMPT`，以及固定配置链的 `50+i`），
///    与应答 id 一一对应；
/// 2. **尾逗号不是装饰**：`"id":5` 是 `"id":50` 的前缀，不加逗号的话"等第 5 帧"
///    会被第 50 帧推走（dim 的配置链恰好同时出现 5 与 50）；
/// 3. 应答是"带 `id` 且带 `result`/`error` 的对象"，通知是"带 `method`、不带 `id`
///    的对象" ⇒ 按应答切组即可；
/// 4. 组内的非应答行（正文通知）留在**它前面**那个组里。
///    [`crate::adapters::acp_core::conformance_success_stdout`] 正是把
///    `session/update` 通知写在 `session/prompt` 应答**之前**的，因此这条通知随
///    `session` 组先落地 —— 取消用例拿到第一个正文事件时，会话 id 已经就位，
///    `session/cancel` 带得上 `sessionId`。
///
/// 通知略早到达不影响解析（解码器按行驱动，通知不依赖握手阶段），所以这个分帧只决定
/// "哪道闸门推它"，不决定语义。
///
/// 公开给 `tests/` 下的集成用例：crate 内的一致性套件与 crate 外的端到端用例必须
/// 用**同一套**分帧规则，否则会出现"套件绿、集成红"这种只能靠猜的偏差。
pub fn response_frame_groups(transcript: &str) -> Vec<(String, String)> {
    let mut groups: Vec<(String, String)> = Vec::new();
    // 第一道应答之前的杂行（横幅）并进第一组，不单独丢弃。
    let mut prefix = String::new();
    for line in transcript.lines() {
        match response_frame_id(line) {
            Some(id) => {
                let mut payload = std::mem::take(&mut prefix);
                payload.push_str(line);
                payload.push('\n');
                groups.push((format!("\"id\":{id},"), payload));
            }
            None => {
                if let Some(last) = groups.last_mut() {
                    last.1.push_str(line);
                    last.1.push('\n');
                } else {
                    prefix.push_str(line);
                    prefix.push('\n');
                }
            }
        }
    }
    assert!(
        groups.len() >= 2,
        "逐帧回放至少要有两道应答帧（握手 + 建会话），实际 {} 组：{transcript}",
        groups.len()
    );
    groups
}

/// 一行是不是"应答帧"：带 `id`，且带 `result` 或 `error`（通知只带 `method`）。
fn response_frame_id(line: &str) -> Option<i64> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let object = value.as_object()?;
    let id = object.get("id")?.as_i64()?;
    if object.contains_key("result") || object.contains_key("error") {
        Some(id)
    } else {
        None
    }
}

/// 正常路径（回放到尾、0 退出）的假 CLI。
fn success_fake<A: TestableAdapter>(script: &ConformanceScript) -> FakeCli {
    match live_plan::<A>(script) {
        LivePlan::Eof => FakeCli::replaying(&script.success_stdout),
        LivePlan::Gate(after, until) => {
            FakeCli::live_replaying(&script.success_stdout, after, until)
        }
        LivePlan::GateOnce(after) => FakeCli::live_gated(&script.success_stdout, after),
        LivePlan::Frames(frames) => FakeCli::live_scripted(&frames, "exit 0\n"),
    }
}

/// 非零退出路径的假 CLI。
fn failing_fake<A: TestableAdapter>(script: &ConformanceScript) -> FakeCli {
    match live_plan::<A>(script) {
        LivePlan::Eof => FakeCli::failing(&script.success_stdout, 3, &script.expected_error),
        LivePlan::Gate(after, _) | LivePlan::GateOnce(after) => {
            FakeCli::live_failing(&script.success_stdout, 3, &script.expected_error, after)
        }
        LivePlan::Frames(mut frames) => {
            frames.pop();
            FakeCli::live_scripted_failing(&frames, 3, &script.expected_error)
        }
    }
}

/// 取消路径的假 CLI（回放完就睡死）。
fn cancelling_fake<A: TestableAdapter>(script: &ConformanceScript) -> FakeCli {
    match live_plan::<A>(script) {
        LivePlan::Eof => FakeCli::replaying_then_sleeping(&script.success_stdout, 30),
        LivePlan::Gate(after, _) | LivePlan::GateOnce(after) => {
            FakeCli::live_replaying_then_sleeping(&script.success_stdout, 30, after)
        }
        LivePlan::Frames(mut frames) => {
            // 末组（`session/prompt` 应答）**不**回放：终态只能来自取消，不与
            // "turn 已经跑完了再取消"抢时序（那会让用例变成掷骰子）。
            frames.pop();
            FakeCli::live_scripted(&frames, "exec sleep 30\n")
        }
    }
}

/// 断言进程已被回收（Linux 下 `/proc/<pid>` 消失；其它平台跳过）。
fn assert_process_reaped(pid: Option<u32>) {
    let Some(pid) = pid else {
        panic!("没拿到子进程 pid");
    };
    #[cfg(target_os = "linux")]
    assert!(
        !Path::new(&format!("/proc/{pid}")).exists(),
        "/proc/{pid} 还在：子进程没被回收"
    );
    #[cfg(not(target_os = "linux"))]
    let _ = pid;
}

/// 套件 1：`kind()` 是白名单里的取值，且字符串往返成立。
pub fn check_kind_is_in_catalog<A: TestableAdapter>() {
    let fake = FakeCli::version("x\n");
    let adapter = build::<A>(&fake);
    let kind = adapter.kind();
    assert!(
        AgentType::ALL.contains(&kind),
        "{kind:?} 不在 SupportedTypes 白名单里"
    );
    assert!(!kind.launch_header().is_empty());
    assert_eq!(
        AgentType::parse(kind.as_str()),
        Some(kind),
        "as_str/parse 必须往返"
    );
}

/// 套件 2：`capabilities()` 自述与白名单、与自身其他能力不矛盾。
pub fn check_capabilities_are_self_consistent<A: TestableAdapter>() {
    let fake = FakeCli::version("x\n");
    let adapter = build::<A>(&fake);
    let caps: AdapterCapabilities = adapter.capabilities();
    let kind = adapter.kind();
    assert_eq!(
        caps.launch_header,
        kind.launch_header(),
        "启动骨架必须来自白名单表"
    );
    assert_ne!(
        caps.protocol,
        ProtocolFamily::Opaque,
        "M3 的 adapter 必须有结构化协议"
    );
    assert!(caps.version_probe, "套件会跑 --version，能力位必须自报");
    if caps.thinking {
        assert!(caps.streaming, "推理增量属于流式能力");
    }
    if caps.tool_events {
        assert!(caps.streaming, "工具事件属于流式能力");
    }
}

/// 套件 3：`--version` 探测。
pub async fn check_probe_version_parses_semver<A: TestableAdapter>() {
    let script = A::conformance_script();
    let fake = FakeCli::version(&script.version_stdout);
    let adapter = build::<A>(&fake);
    let probe = adapter.probe_version().await.expect("版本探测成功");
    assert_eq!(probe.kind, adapter.kind());
    assert_eq!(probe.executable, fake.path());
    assert_eq!(
        probe.version.as_ref().map(ToString::to_string),
        script.expected_version,
        "raw = {:?}",
        probe.raw
    );

    // 输出里没有版本号 ≠ 探测失败：探测成功、版本未知，这两件事必须分开。
    let odd = FakeCli::version("no version token here\n");
    let probe = build::<A>(&odd)
        .probe_version()
        .await
        .expect("无版本号输出仍是探测成功");
    assert_eq!(probe.version, None);
    assert_eq!(probe.raw, "no version token here");
}

/// 套件 4 + 5：正常 run 的完整生命周期，以及 prompt 走 stdin。
pub async fn check_launch_streams_and_completes<A: TestableAdapter>() {
    let script = A::conformance_script();
    let fake = success_fake::<A>(&script);
    let adapter = build::<A>(&fake);
    let caps = adapter.capabilities();
    let prompt = "conformance prompt：请只回一行";
    let handle = adapter
        .launch(LaunchRequest::new(prompt))
        .await
        .expect("launch 成功");
    let run_id = handle.run_id().clone();
    let (events, outcome) = handle.drain().await.expect("run 必须给出终态");

    assert_eq!(outcome.status, RunStatus::Completed, "{outcome:?}");
    assert_eq!(outcome.error, None);
    assert_eq!(outcome.failure_reason, None);
    assert_eq!(outcome.exit_code, Some(0));
    assert_eq!(outcome.run_id, run_id);
    assert_eq!(outcome.output, script.expected_output);
    assert!(outcome.session_id.is_some(), "adapter 必须回报会话 id");
    assert_eq!(outcome.stderr_tail, "");

    let Some(RuntimeEvent::Started { executable, pid }) = events.first() else {
        panic!("第一条事件必须是 Started，实际 {:?}", events.first());
    };
    assert_eq!(Path::new(executable), fake.path().as_path());
    assert!(pid.is_some(), "子进程必须有 pid");

    let text: String = events
        .iter()
        .filter_map(|event| match event {
            RuntimeEvent::Text { delta } => Some(delta.as_str()),
            _ => None,
        })
        .collect();
    if caps.streaming {
        assert_eq!(text, script.expected_output, "文本事件拼起来应等于终态正文");
    }
    if caps.usage_reporting {
        assert!(
            events
                .iter()
                .any(|event| matches!(event, RuntimeEvent::Usage { .. })),
            "自报用量就必须有 Usage 事件"
        );
        if let Some(expected) = script.expected_usage_tokens {
            let total: u64 = outcome
                .usage
                .iter()
                .map(|model| model.usage.total_tokens)
                .sum();
            assert_eq!(total, expected);
        }
    }

    let argv = fake.recorded_argv();
    assert!(!argv.trim().is_empty(), "argv 不该是空的");
    if script.prompt_via_stdin {
        assert!(
            fake.recorded_stdin().contains(prompt),
            "prompt 必须写到 stdin"
        );
        assert!(
            !argv.contains(prompt),
            "prompt 不得出现在 argv（会被 CLI 重新分词）：{argv}"
        );
    }
}

/// 套件 6：非零退出 → `Failed` / `AgentError`，并带上 stderr 诊断。
pub async fn check_nonzero_exit_maps_to_agent_error<A: TestableAdapter>() {
    let script = A::conformance_script();
    let fake = failing_fake::<A>(&script);
    let adapter = build::<A>(&fake);
    let outcome = adapter
        .launch(LaunchRequest::new("会失败的 run"))
        .await
        .expect("launch 成功（失败发生在启动之后）")
        .outcome()
        .await
        .expect("run 必须给出终态");

    assert_eq!(outcome.status, RunStatus::Failed);
    assert_eq!(
        outcome.failure_reason,
        Some(crate::adapter::FailureReason::AgentError)
    );
    assert_eq!(outcome.exit_code, Some(3));
    let error = outcome.error.expect("失败必须有错误串");
    assert!(error.contains("exited with error"), "{error}");
    assert!(
        outcome.stderr_tail.contains(&script.expected_error),
        "stderr 尾部应有诊断文本，实际 {:?}",
        outcome.stderr_tail
    );
}

/// 套件 7：超时 → `Timeout`（并且进程被真的杀掉了）。
pub async fn check_timeout_maps_to_timeout<A: TestableAdapter>() {
    let fake = FakeCli::sleeping(30);
    let adapter = build::<A>(&fake);
    let request = LaunchRequest::new("永远不返回的 run").with_timeout(Duration::from_millis(300));
    let handle = adapter.launch(request).await.expect("launch 成功");
    let (events, outcome) = handle.drain().await.expect("超时也必须给出终态");
    let pid = match events.first() {
        Some(RuntimeEvent::Started { pid, .. }) => *pid,
        _ => None,
    };

    assert_eq!(outcome.status, RunStatus::Timeout);
    assert_eq!(
        outcome.failure_reason,
        Some(crate::adapter::FailureReason::Timeout)
    );
    assert_eq!(outcome.exit_code, None, "被信号杀死时退出码未知");
    assert_process_reaped(pid);
}

/// 套件 8：`cancel` 幂等，终态是 `Cancelled` / `Manual`。
pub async fn check_cancel_is_idempotent<A: TestableAdapter>() {
    let script = A::conformance_script();
    let fake = cancelling_fake::<A>(&script);
    let adapter = build::<A>(&fake);
    let mut handle = adapter
        .launch(LaunchRequest::new("会被取消的 run"))
        .await
        .expect("launch 成功");
    let run_id = handle.run_id().clone();

    // 先看到正文事件，证明流确实起来了（而不是在 cancel 之后才失败）。
    let mut saw_text = false;
    let mut pid = None;
    while let Some(event) = handle.next_event().await {
        match event {
            RuntimeEvent::Started { pid: started, .. } => pid = started,
            RuntimeEvent::Text { .. } => {
                saw_text = true;
                break;
            }
            _ => {}
        }
    }
    assert!(saw_text, "取消前应先收到正文事件");

    let first = adapter.cancel(&run_id).await.expect("取消不该报错");
    assert_eq!(first, CancelOutcome::Signalled);
    // 幂等：第二次可以再报 `Signalled`（run 还没收尾）或 `NotRunning`（已收尾），但不能报错。
    let second = adapter.cancel(&run_id).await.expect("重复取消不该报错");
    assert!(
        matches!(second, CancelOutcome::Signalled | CancelOutcome::NotRunning),
        "意外的取消结果：{second:?}"
    );

    // 排干事件（通道关闭 = run 结束）再取终态。
    while handle.next_event().await.is_some() {}
    let outcome = handle.outcome().await.expect("run 必须给出终态");
    assert_eq!(outcome.status, RunStatus::Cancelled);
    assert_eq!(
        outcome.failure_reason,
        Some(crate::adapter::FailureReason::Manual)
    );
    assert_process_reaped(pid);
}

/// 套件 9：解码器容忍非 JSON 行与未知事件类型。
pub fn check_decoder_tolerates_junk<A: TestableAdapter>() {
    let script = A::conformance_script();
    let fake = FakeCli::version("x\n");
    let adapter = build::<A>(&fake);
    let mut decoder = adapter.decoder();
    let mut text = String::new();
    for line in script.junk_stdout.lines() {
        for event in decoder.push_line(line) {
            if let RuntimeEvent::Text { delta } = event {
                text.push_str(&delta);
            }
        }
    }
    for event in decoder.finish() {
        if let RuntimeEvent::Text { delta } = event {
            text.push_str(&delta);
        }
    }
    assert_eq!(
        text, script.expected_junk_output,
        "脏数据流里仍然要解析出正文"
    );
}

/// 展开一组一致性测试（8 个 `#[tokio::test]`），一个 adapter 一行。
///
/// ```ignore
/// #[cfg(test)]
/// mod tests {
///     use super::*;
///
///     crate::adapter_conformance!(PiLocal);
/// }
/// ```
///
/// 断言全在 [`crate::conformance`] 里，因此**所有 adapter 的覆盖度完全一致**；
/// 新增 adapter 不需要写任何测试代码。仅在 unix 上编译（假 CLI 是可执行 shell 脚本）。
#[macro_export]
macro_rules! adapter_conformance {
    ($adapter:ty) => {
        #[cfg(unix)]
        mod conformance {
            // 调用点所在模块把 adapter 类型带进来（通常是 `use super::*;`）。
            use super::*;

            #[tokio::test]
            async fn conformance_kind_is_in_catalog() {
                $crate::conformance::check_kind_is_in_catalog::<$adapter>();
            }

            #[tokio::test]
            async fn conformance_capabilities_are_self_consistent() {
                $crate::conformance::check_capabilities_are_self_consistent::<$adapter>();
            }

            #[tokio::test]
            async fn conformance_probe_version_parses_semver() {
                $crate::conformance::check_probe_version_parses_semver::<$adapter>().await;
            }

            #[tokio::test]
            async fn conformance_launch_streams_and_completes() {
                $crate::conformance::check_launch_streams_and_completes::<$adapter>().await;
            }

            #[tokio::test]
            async fn conformance_nonzero_exit_maps_to_agent_error() {
                $crate::conformance::check_nonzero_exit_maps_to_agent_error::<$adapter>().await;
            }

            #[tokio::test]
            async fn conformance_timeout_maps_to_timeout() {
                $crate::conformance::check_timeout_maps_to_timeout::<$adapter>().await;
            }

            #[tokio::test]
            async fn conformance_cancel_is_idempotent() {
                $crate::conformance::check_cancel_is_idempotent::<$adapter>().await;
            }

            #[tokio::test]
            async fn conformance_decoder_tolerates_junk() {
                $crate::conformance::check_decoder_tolerates_junk::<$adapter>();
            }
        }
    };
}

#[cfg(all(test, unix))]
mod harness_tests {
    use super::*;
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::process::{Command, Stdio};

    /// `live_*` 的闸门必须真的被 stdin 内容推开。
    ///
    /// 这是**测试台自己的测试**：闸门一坏（例如后台读 stdin 被 dash 接到
    /// `/dev/null`），长连接协议的 adapter 会退化成"10 秒后拿到空流"，
    /// 断言却指向 adapter —— 排查成本极高。
    #[test]
    fn live_fake_gates_replay_behind_stdin_content() {
        let fake = FakeCli::live_replaying("第一行\n第二行\n", "thread/start", "turn/start");
        let mut child = Command::new(fake.path())
            .arg("--listen")
            .arg("stdio://")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("起假 CLI");
        let mut stdin = child.stdin.take().expect("stdin 管道");
        let mut stdout = BufReader::new(child.stdout.take().expect("stdout 管道"));

        // 1) 写出第一道闸门：此时脚本才该开始回放。
        stdin.write_all(b"{\"method\":\"thread/start\"}\n").unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        stdout.read_line(&mut line).expect("回放第一行");
        assert_eq!(line, "第一行\n");
        line.clear();
        stdout.read_line(&mut line).expect("回放第二行");
        assert_eq!(line, "第二行\n");

        // 2) 第二道闸门没写之前不许退出（写方还没把 prompt 帧送完）。
        assert!(
            child.try_wait().expect("探活").is_none(),
            "第二道闸门之前不该退出"
        );
        stdin
            .write_all(b"{\"id\":3,\"method\":\"turn/start\"}\n")
            .unwrap();
        stdin.flush().unwrap();
        drop(stdin);
        let status = child.wait().expect("等假 CLI 退出");
        assert!(status.success(), "{status}");

        let recorded = fake.recorded_stdin();
        assert!(recorded.contains("thread/start"), "{recorded}");
        assert!(recorded.contains("turn/start"), "{recorded}");
        assert!(
            fake.recorded_argv().contains("stdio://"),
            "argv 要落盘：{:?}",
            fake.recorded_argv()
        );
    }

    /// 分帧：应答按 `id` 成组，正文通知留在**它前面**那组（取消用例靠它拿到正文）。
    #[test]
    fn response_frames_are_grouped_by_fixed_ids() {
        let transcript = concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"sessionId\":\"s\"}}\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{}}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":6,\"result\":{\"stopReason\":\"end_turn\"}}\n",
        );
        let groups = response_frame_groups(transcript);
        assert_eq!(
            groups
                .iter()
                .map(|(gate, _)| gate.as_str())
                .collect::<Vec<_>>(),
            vec!["\"id\":1,", "\"id\":3,", "\"id\":6,"],
            "闸门必须直接取自应答 id（客户端请求用同一批固定序号）"
        );
        assert!(
            groups[1].1.contains("session/update"),
            "正文通知要随 session 组一起先落地：{:?}",
            groups[1].1
        );
        assert!(!groups[2].1.contains("session/update"));
    }

    /// 逐帧回放的闸门必须真的挡住"提前回放"：写第二帧之前不得出现第二组。
    #[test]
    fn scripted_fake_replays_one_group_per_client_frame() {
        use std::sync::mpsc;

        let frames = vec![
            ("\"id\":1".to_owned(), "第一组\n第二行\n".to_owned()),
            ("\"id\":3".to_owned(), "第三组\n".to_owned()),
        ];
        let fake = FakeCli::live_scripted(&frames, "exit 0\n");
        let mut child = Command::new(fake.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("起逐帧假 CLI");
        let mut stdin = child.stdin.take().expect("stdin 管道");
        let stdout = child.stdout.take().expect("stdout 管道");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                if tx.send(line).is_err() {
                    break;
                }
            }
        });

        // 1) 第一帧到位才回放第一组。
        assert!(
            rx.recv_timeout(Duration::from_millis(500)).is_err(),
            "还没写任何帧就不该有回放"
        );
        stdin
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}\n")
            .unwrap();
        stdin.flush().unwrap();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).expect("第一行"),
            "第一组\n"
        );
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).expect("第二行"),
            "第二行\n"
        );

        // 2) 第二帧没写之前，第二组不许出现（这就是"整段回放"会挂掉的原因）。
        assert!(
            rx.recv_timeout(Duration::from_millis(500)).is_err(),
            "第二帧之前不该回放第二组"
        );
        stdin
            .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"session/new\"}\n")
            .unwrap();
        stdin.flush().unwrap();
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).expect("第三行"),
            "第三组\n"
        );

        drop(stdin);
        let status = child.wait().expect("等逐帧假 CLI 退出");
        assert!(status.success(), "{status}");
        let recorded = fake.recorded_stdin();
        assert!(recorded.contains("session/new"), "{recorded}");
    }
}
