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

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::adapter::{
    AdapterCapabilities, CancelOutcome, LaunchRequest, ProtocolFamily, RunStatus, RuntimeAdapter,
    RuntimeEvent,
};
use crate::catalog::AgentType;

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
}

/// 现场生成的假 CLI（`#!/bin/sh` 脚本）。析构时删掉整个临时目录。
pub struct FakeCli {
    dir: PathBuf,
    executable: PathBuf,
}

impl FakeCli {
    fn allocate(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "mc-runtime-conformance-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).expect("建临时目录");
        Self {
            executable: dir.join(format!("{tag}.sh")),
            dir,
        }
    }

    /// 假 CLI 的绝对路径（交给 adapter 当 executable）。
    pub fn path(&self) -> PathBuf {
        self.executable.clone()
    }

    /// 工作目录（adapter 的落盘目录都在这里，析构时一起删）。
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 脚本捕获到的 stdin。
    pub fn recorded_stdin(&self) -> String {
        std::fs::read_to_string(self.dir.join("stdin.txt")).unwrap_or_default()
    }

    /// 脚本捕获到的 argv（一行一个）。
    pub fn recorded_argv(&self) -> String {
        std::fs::read_to_string(self.dir.join("argv.txt")).unwrap_or_default()
    }

    /// 只打印一行版本号的假 CLI。
    pub fn version(stdout: &str) -> Self {
        let fake = Self::allocate("version");
        let payload = fake.payload("version.txt", stdout);
        fake.install(&format!("#!/bin/sh\ncat {}\n", quoted(&payload)));
        fake
    }

    /// 回放一段事件流、以 0 退出的假 CLI（同时捕获 stdin/argv）。
    pub fn replaying(transcript: &str) -> Self {
        let fake = Self::allocate("replay");
        let payload = fake.payload("transcript.txt", transcript);
        fake.install(&fake.script(&format!(
            "while IFS= read -r line; do\n  printf '%s\\n' \"$line\"\ndone < {}\nexit 0\n",
            quoted(&payload)
        )));
        fake
    }

    /// 回放一段事件流、写 stderr、以 `exit_code` 退出的假 CLI。
    pub fn failing(transcript: &str, exit_code: i32, stderr: &str) -> Self {
        let fake = Self::allocate("failing");
        let payload = fake.payload("transcript.txt", transcript);
        let error = fake.payload("stderr.txt", &format!("{stderr}\n"));
        fake.install(&fake.script(&format!(
            "while IFS= read -r line; do\n  printf '%s\\n' \"$line\"\ndone < {transcript}\ncat {error} >&2\nexit {exit_code}\n",
            transcript = quoted(&payload),
            error = quoted(&error),
        )));
        fake
    }

    /// 什么都不输出、睡死（用 `exec` 保证杀的就是 sleep 本体，不留孤儿）的假 CLI。
    pub fn sleeping(seconds: u32) -> Self {
        let fake = Self::allocate("sleeping");
        fake.install(&fake.script(&format!("exec sleep {seconds}\n")));
        fake
    }

    /// 先回放一段事件流、再睡死（用于"流已经起来了再取消"）。
    pub fn replaying_then_sleeping(transcript: &str, seconds: u32) -> Self {
        let fake = Self::allocate("replay-sleep");
        let payload = fake.payload("transcript.txt", transcript);
        fake.install(&fake.script(&format!(
            "while IFS= read -r line; do\n  printf '%s\\n' \"$line\"\ndone < {}\nexec sleep {seconds}\n",
            quoted(&payload)
        )));
        fake
    }

    fn payload(&self, name: &str, content: &str) -> PathBuf {
        let path = self.dir.join(name);
        std::fs::write(&path, content).expect("写 payload");
        path
    }

    fn install(&self, body: &str) {
        std::fs::write(&self.executable, body).expect("写脚本");
        std::fs::set_permissions(&self.executable, std::fs::Permissions::from_mode(0o755))
            .expect("chmod +x");
    }

    /// 公共前缀：把 argv 与 stdin 落到临时目录，便于"prompt 必须走 stdin"的断言。
    fn script(&self, tail: &str) -> String {
        format!(
            "#!/bin/sh\nd={dir}\nprintf '%s\\n' \"$@\" > \"$d/argv.txt\"\ncat > \"$d/stdin.txt\"\n{tail}",
            dir = quoted(&self.dir),
        )
    }
}

impl Drop for FakeCli {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// 把路径写成 shell 单引号字面量（临时目录路径里出现单引号才会出问题，直接拦掉）。
fn quoted(path: &Path) -> String {
    let text = path.display().to_string();
    assert!(!text.contains('\''), "临时目录路径不能含单引号：{text}");
    format!("'{text}'")
}

fn build<A: TestableAdapter>(fake: &FakeCli) -> A {
    A::with_conformance_env(&fake.path(), fake.dir())
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
    let fake = FakeCli::replaying(&script.success_stdout);
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
    let fake = FakeCli::failing(&script.success_stdout, 3, &script.expected_error);
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
    let fake = FakeCli::replaying_then_sleeping(&script.success_stdout, 30);
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
