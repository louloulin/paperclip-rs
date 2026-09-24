//! job 注册与分发（M5-8）。
//!
//! - **写者**：M5-8（`jobs/**` 整组）。
//! - **上游**：`scheduler/jobs_autopilot.go`448（449）+ `scheduler/jobs_issue_wakeup.go`21（22）。
//! - **依赖方向**：M5-8 ← M5-7 的内核 + M5-4 的 dispatch + M5-6 的 wakeup
//!   （`docs/44` §4.3 的串行边）⇒ 本组只能在内核与两个面**合并之后**动。
//! - **注册点**：`apps/mc-server/src/main.rs` 的 spawn 块由 M5-7 写、M5-8 往注册表加 2 行
//!   （串行边，不是并发写）。
//!
//! # 两个 job 与上游的对应
//!
//! | job | 上游 | 本片文件 |
//! | --- | --- | --- |
//! | autopilot 调度派发 | `jobs_autopilot.go`448 | [`autopilot`] |
//! | issue wakeup 派发 | `jobs_issue_wakeup.go`21 | [`issue_wakeup`] |
//!
//! # 为什么 `register_all` 需要一个端口包
//!
//! `docs/48` §7.3 的承诺是「M5-8 只需加两行 `manager.register(job()?)?`」。**本片做不到**
//! 那个形状，原因不是设计偏好而是硬约束（登记为偏差 D1）：
//!
//! 上游两个 job 的 handler 直接拿 `*db.Queries` 打 SQL（列/读 trigger、推进展示列、
//! 建队列行、写收据）。本地 `mc-scheduler` 的依赖表里**没有 `sqlx`**（M5-0 冻结，本片不得加），
//! 连 `PgPool` / `PgConnection` 这两个类型名都写不出来 ⇒ 数据面必须由**外面**注进来，
//! 于是 `job()` 一定带参数、`register_all` 一定带一个端口包。两行 `register` 仍然只有两行。
//!
//! ⇒ 端口包 [`JobPorts`] 的构造方（`main.rs` 的 spawn 块）负责提供实现；
//! **生产实现目前缺位**（`apps/mc-server/Cargo.toml` 还没有 `mc-scheduler` 依赖边 ⇒
//! 接线仍待 P0，见 `docs/48` §7.1）。
//!
//! # 本 crate 没有 `serde_json`
//!
//! [`JsonObject`] 是这里唯一的 JSON 出口（`HandlerResult.result_json` 要求合法 JSON）。
//! 转义按 RFC 8259 做全；值都是 uuid / 状态字面量 / 计数，没有任何用户输入。

pub mod autopilot;
pub mod issue_wakeup;
pub mod plugin_hook;

use std::fmt::Write as _;
use std::pin::Pin;
use std::sync::Arc;

use crate::error::SchedulerResult;
use crate::manager::Manager;

use autopilot::{AutopilotSchedulePort, ScheduleDispatch};
use issue_wakeup::WakeupDispatchPort;
use plugin_hook::PluginHookPort;

/// 端口的 future：借用 `&self`（**不是** `'static`）—— 这样桩实现与真实现都不必把自身
/// 塞进 `Arc<Self>`，也不需要 `async_trait`（M5-0 的依赖表已冻结，加不了）。
pub type PortFuture<'a, T> = Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;

/// 四个 job 需要的全部**数据面端口**（生产接线方构造，见模块文档的 D1）。
///
/// 端口分开是因为它们的成熟度与来源不同：
///
/// * [`ScheduleDispatch`]：**已有真实现** —— `impl ScheduleDispatch for AutopilotDispatcher`
///   就在 [`autopilot`] 里（M5-4 的派发面，编译期受检）；
/// * [`AutopilotSchedulePort`]：trigger 的读面 + 展示列写面 —— 生产实现**待 P0 接线**
///   （4 条 SQL 的清单写在 trait 文档里）；
/// * [`WakeupDispatchPort`]：wakeup 的取行 + 单行派发 + 收尾 —— 生产实现**待 P0 接线**
///   （7 步事务顺序写在 trait 文档里，与 M5-6 `dispatch.rs` 模块头的清单逐条对应）；
/// * [`PluginHookPort`]：M6-8 的 hook job 数据面 —— 生产实现在
///   `apps/mc-server/src/scheduler/hook_port.rs`（它把活交给 `mc-http` 的 hook 引擎）。
///
/// **为什么 `plugin_hook` 是必填而不是 `Option`**：`Option` + 默认 `None` 会让「忘了接线」
/// 退化成「job 静默不注册」，而那正是本波最忌讳的假绿（issue `LUM-1673` 的「不要走
/// `Option<Arc<dyn …>>` + 默认 `None` 的回避路」）。必填的实际代价只有一处：
/// `crates/mc-scheduler/tests/jobs_issue_wakeup.rs`（M5 的用例）多传一个实参（已在本片登记）。
#[derive(Clone)]
pub struct JobPorts {
    /// autopilot job 的 trigger 读面 + 展示列写面。
    pub autopilot_catalog: Arc<dyn AutopilotSchedulePort>,
    /// autopilot job 的派发面（生产用 `AutopilotDispatcher`）。
    pub autopilot_dispatch: Arc<dyn ScheduleDispatch>,
    /// issue wakeup job 的数据面。
    pub wakeup: Arc<dyn WakeupDispatchPort>,
    /// M6-8 的 plugin hook 计划投递数据面。
    pub plugin_hook: Arc<dyn PluginHookPort>,
}

impl JobPorts {
    /// 组装端口包（四个实参，**没有缺省**）。
    #[must_use]
    pub fn new(
        autopilot_catalog: Arc<dyn AutopilotSchedulePort>,
        autopilot_dispatch: Arc<dyn ScheduleDispatch>,
        wakeup: Arc<dyn WakeupDispatchPort>,
        plugin_hook: Arc<dyn PluginHookPort>,
    ) -> Self {
        Self {
            autopilot_catalog,
            autopilot_dispatch,
            wakeup,
            plugin_hook,
        }
    }
}

/// 把本波所有 job 注册进管理器（每片**恰好一行 `register`**）。
///
/// `register` 必须在 `spawn()` **之前**调用（`Manager::spawn` 消费 `self`）；
/// 注册完就是上游 `scheduler.New(...)` + `Start(ctx)` 的等价物。
///
/// 调用方：`apps/mc-server/src/scheduler/mod.rs` 的 `build`（M5-9 的接线点，M6-8 只往它
/// 多传一个端口）。**顺序有语义**：job 之间无依赖，但都必须在 `spawn` 前登记，否则第一轮
/// tick 不会带上它们；顺序也进了 `manager.jobs()`，用例按它断言登记表。
pub fn register_all(manager: &mut Manager, ports: &JobPorts) -> SchedulerResult<()> {
    manager.register(autopilot::job(
        ports.autopilot_catalog.clone(),
        ports.autopilot_dispatch.clone(),
    ))?;
    manager.register(issue_wakeup::job(ports.wakeup.clone()))?;
    // M6-8：端口是必填 ⇒ 这一行无条件注册（不能靠 `Option::is_some` 把注册藏起来）。
    manager.register(plugin_hook::job(ports.plugin_hook.clone()))?;
    Ok(())
}

/// 极小的 JSON 对象构造器。
///
/// `mc-scheduler` 的依赖表里没有 `serde_json`（M5-0 冻结）⇒ 手搓。之所以**不是** `format!`
/// 拼串：`HandlerResult.result_json` 是落 `jsonb` 列的合法 JSON 文本，转义漏一个字符就会让
/// 审计写入失败（`docs/48` §4 的 `result_json` 上限 16KB 校验也是在这里满足的）。
#[derive(Debug, Default)]
pub struct JsonObject {
    out: String,
    count: usize,
}

impl JsonObject {
    /// 空对象。
    #[must_use]
    pub fn new() -> Self {
        Self {
            out: String::from("{"),
            count: 0,
        }
    }

    /// 加一个字符串字段。
    pub fn text(&mut self, key: &str, value: &str) -> &mut Self {
        self.separator();
        self.out.push('"');
        push_json_escaped(&mut self.out, key);
        self.out.push_str("\":\"");
        push_json_escaped(&mut self.out, value);
        self.out.push('"');
        self
    }

    /// 加一个整数字段（计数）。
    pub fn number(&mut self, key: &str, value: i64) -> &mut Self {
        self.separator();
        self.out.push('"');
        push_json_escaped(&mut self.out, key);
        // 整数没有可转义的字符；`write!` 到 `String` 不会失败。
        let _ = write!(self.out, "\":{value}");
        self
    }

    /// 收尾成 JSON 文本。
    #[must_use]
    pub fn finish(mut self) -> String {
        self.out.push('}');
        self.out
    }

    fn separator(&mut self) {
        if self.count > 0 {
            self.out.push(',');
        }
        self.count += 1;
    }
}

/// RFC 8259 的字符串转义（`"` / `\` / 控制字符；其余按 UTF-8 原样）。
fn push_json_escaped(out: &mut String, raw: &str) {
    for ch in raw.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            control if control < '\u{20}' => {
                let _ = write!(out, "\\u{:04x}", u32::from(control));
            }
            other => out.push(other),
        }
    }
}

/// 把任意错误文本压成一行（日志 / 审计用）：换行 / 回车 / 制表 → 空格 + 去首尾空白。
///
/// `last_error` 的 500-rune 截断在 M5-6 的 `note_dispatch_failure` 里（本片不重复）。
#[must_use]
pub fn one_line(value: &str) -> String {
    let flattened: String = value
        .chars()
        .map(|ch| match ch {
            '\n' | '\r' | '\t' => ' ',
            other => other,
        })
        .collect();
    flattened.trim().to_owned()
}

/// 便捷：由 `(key, value)` 列表直接建对象（本 crate 的少量固定形状用它省几行）。
#[must_use]
pub fn json_object(pairs: &[(&str, &str)]) -> String {
    let mut json = JsonObject::new();
    for (key, value) in pairs {
        json.text(key, value);
    }
    json.finish()
}
