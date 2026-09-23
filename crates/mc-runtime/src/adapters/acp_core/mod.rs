//! ACP（Agent Client Protocol）共享骨架 —— M3-8 批 2 的 6 个 ACP provider 共用。
//!
//! # 为什么单独抽一层
//!
//! 批 2 / 批 3 的 **12 个** provider 说 **同一套线协议**（ACP/JSON-RPC 2.0 over
//! stdio）：kimi、kiro、qoder、qoderclicn、traecli、grok（批 2）+ hermes、reasonix、
//! dim、mcode、zeroclaw、qwenpaw（批 3）。它们的差别只有八处：
//!
//! | 差异点 | 取值 |
//! |---|---|
//! | argv（命令 + 非交互开关） | [`AcpProvider::build_args`] |
//! | 会话恢复方法 | [`AcpResume`]（`session/resume` 或 `session/load`） |
//! | `initialize` 后是否先 `authenticate` | [`AcpAuth`]（只有 grok） |
//! | `session/prompt` 的块字段名 | [`AcpPromptFields`]（kiro 额外带 `content`） |
//! | 推理等级怎么下发 | [`AcpFlavor::thinking_config`]（kimi / hermes / reasonix / dim） |
//! | 模型怎么选 | [`AcpFlavor::model_selection`]（`set_model` / 会话参数 / 不支持） |
//! | 会话参数要不要带 `_meta` | [`AcpFlavor::session_meta_key`]（只有 qwenpaw） |
//! | 建会话后要不要先下发固定配置 | [`AcpFlavor::session_configs`]（只有 dim） |
//!
//! 另有 [`AcpFlavor::resume_params`]：恢复会话的 params 形状（zeroclaw 只发
//! `{sessionId}`，其余发 `{cwd,sessionId,mcpServers}`）。
//!
//! 于是"握手 → 建会话 → 选模型 → 发 prompt → 收通知 → 收终态"这条状态机只写
//! **一份**（[`client::AcpDecoder`]），每个 provider 只提供一张
//! [`AcpFlavor`] 表 + 一个 `build_args`。这与批 1 的
//! [`super::cli_core`] 是同一手法：provider 模块只描述"是什么"，"怎么说话"由共享
//! 核心负责。
//!
//! # 与上游的对应
//!
//! 上游把这套客户端做成了 `server/pkg/agent/hermes.go` 的 `hermesClient`
//! （3322 行，含 Hermes 自己的工具名归一化、终态嗅探与终端能力）。kimi.go /
//! kiro.go / qoder.go / traecli.go / grok.go 都复用它，各自只覆盖 argv、
//! `clientCapabilities`、resume 方法与模型下发点。本 crate 只移植**外部 ACP
//! provider 实际会走到的那条路**，逐项取舍记在 `docs/33-M3-ADAPTERS.md` §6。
//!
//! # 有意不做的三件事（`docs/33` §6 有表）
//!
//! 1. **不宣告终端能力**（`clientCapabilities: {}`）。上游只有 kimi 传
//!    `{"terminal": true}`，其余传 `{}`；`terminal/*` 一族请求本片一律
//!    fail-closed 回 `-32601`（[`TERMINAL_NOT_ENABLED`]）。少一套跨进程终端
//!    协议，也少一处"半落地"的状态。
//! 2. **不做用法合并的上游精算**：`acp_usage.go` 把"输入是否已含缓存读"这类
//!    歧义按字段出现顺序消解；本片统一成"按模型逐桶取最大值"（单调、与到达
//!    顺序无关）。
//! 3. **不做握手/静默超时**：上游的 `acpNotificationQuietTime`、
//!    `grokReaderDrainGrace` 这类"空闲看门狗"归 M3-3 的看门狗切片，与
//!    `codex/stream.rs` 的记法一致。

pub mod client;
mod decode;

#[cfg(test)]
mod tests;

pub use client::AcpDecoder;

use super::cli_core::{
    CliCapabilities, CliCoreConfig, CliDecoder, CliProvider, CliSpec, PromptTransport,
};
use crate::adapter::{LaunchRequest, ProtocolFamily};
use crate::catalog::AgentType;

/// `initialize` 里自报的客户端名（上游 `acpClientName` 同值）。
pub const CLIENT_NAME: &str = "multica-agent-sdk";
/// 客户端版本（上游同值；与 `codex/stream.rs` 共用同一串）。
pub const CLIENT_VERSION: &str = "0.2.0";
/// ACP 协议版本（上游 `protocolVersion: 1`）。
pub const PROTOCOL_VERSION: i64 = 1;

/// 固定帧 id：握手各步的序号（解码器靠它把应答对回状态机，不靠到达顺序）。
pub const ID_INITIALIZE: i64 = 1;
/// 第二帧：`authenticate`（只有 [`AcpAuth::XaiApiKey`] 的 provider 会发）。
pub const ID_AUTHENTICATE: i64 = 2;
/// 第三帧：`session/new` / `session/resume` / `session/load`。
pub const ID_SESSION: i64 = 3;
/// 第四帧：`session/set_model`（请求带模型时才发）。
pub const ID_SET_MODEL: i64 = 4;
/// 第五帧：`session/set_config_option`（只有带 [`AcpFlavor::thinking_config`] 的 provider）。
pub const ID_SET_CONFIG: i64 = 5;
/// 第六帧：`session/prompt`。
pub const ID_PROMPT: i64 = 6;

/// **固定配置链**的帧 id 起点（`session/set_config_option`）。
///
/// 链上第 `i` 步的 id = `ID_SESSION_CONFIG_BASE + i`。用 50 起跳是为了与
/// 1..=6 的握手序号明显分开：抓到 `"id":50` 就知道这是配置链而不是握手。
pub const ID_SESSION_CONFIG_BASE: i64 = 50;

/// 固定配置链上第 `index` 步的帧 id。
pub const fn id_session_config(index: usize) -> i64 {
    // 静态配置链最多几条（`dim` 2 条）；`usize` 转 `i64` 在这里不可能溢出。
    #[allow(clippy::cast_possible_wrap)]
    let id = ID_SESSION_CONFIG_BASE + index as i64;
    id
}

/// `terminal/*` 一族请求的拒绝理由（fail-closed；上游 `hermes.go` L1152 同串）。
pub const TERMINAL_NOT_ENABLED: &str = "terminal capability is not enabled";
/// 没有任何"可安全自动选中"的权限选项时的拒绝理由（上游 L1368 同串）。
pub const NO_PERMISSION_OPTION: &str = "no auto-selectable permission option offered";

/// 会话恢复用哪个方法（上游逐 provider 实测：kimi/qoder/hermes/reasonix/zeroclaw 是
/// `session/resume`，kiro/traecli/grok/qwenpaw/dim/mcode 是 `session/load`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpResume {
    /// `session/resume`（kimi / qoder / qoderclicn / hermes / reasonix / zeroclaw）。
    Resume,
    /// `session/load`（kiro / traecli / grok / qwenpaw / dim / mcode）。
    Load,
}

impl AcpResume {
    /// 线上的方法名。
    pub const fn method(self) -> &'static str {
        match self {
            Self::Resume => "session/resume",
            Self::Load => "session/load",
        }
    }
}

/// `initialize` 之后要不要先 `authenticate`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpAuth {
    /// 不需要（除 grok 外的全部 ACP provider：kimi / kiro / qoder / qoderclicn /
    /// traecli / qwenpaw / hermes / reasonix / dim / mcode / zeroclaw）。
    None,
    /// 按 xAI 的规则从 `initialize` 的 `authMethods` 里挑一个（grok）。
    ///
    /// 规则（上游 `selectGrokAuthMethod`）：有 `XAI_API_KEY` 且对端提供了
    /// `xai.api_key` 就选它，否则选 `cached_token`，都没有则是**启动失败**。
    XaiApiKey,
}

/// 工具名后处理表（上游在 `onMessage` 里对已映射的 `MessageToolUse.Tool` 再过一遍）。
///
/// ACP 的 `title` 是给人看的标签（`"Read file: /x"` / `"Run command: ls"`），
/// hermes 那张表只认小写冒号前缀，所以每家还要再归一一次：kimi/qoder/
/// qoderclicn/traecli/grok **以及批 3 的 6 家**共用 `kimiToolNameFromTitle`，
/// kiro 自己一张（多 `"code"` 与 `"todo list"` 两个别名）。两张表都对没认出来的名字做
/// "小写 + 空格转下划线"，所以对已归一的名字是幂等的。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpToolAliases {
    /// kimi / qoder / qoderclicn / traecli / grok 与批 3 六家的表。
    Kimi,
    /// kiro 的表。
    Kiro,
}

/// `session/prompt` 里 prompt 块的字段名。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpPromptFields {
    /// 只有 `prompt`（除 kiro 外的全部 ACP provider）。
    Prompt,
    /// `prompt` + `content` 各一份（kiro 两种键都读，上游两个都发）。
    PromptAndContent,
}

/// 模型怎么下发给 ACP 对端。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpModelSelection {
    /// 建会话后发 `session/set_model`（失败**致命**）。
    ///
    /// kimi / kiro / qoder / qoderclicn / traecli / grok / dim / reasonix。
    SetModel,
    /// 把模型塞进 `session/new` 的 params，**从不**发 `set_model`（hermes）。
    ///
    /// 上游 `hermes.go` 两者都做（`buildHermesSessionParams` 带 `model`，另发一帧
    /// `set_model`，但带一个"当前模型已等价就跳过"的闸门）。本 crate 不解析
    /// 会话的 `configOptions` / 当前模型 ⇒ 那道闸门永远跳过 ⇒ 只保留会话参数这条
    /// 下发路径（恢复会话因此保持原模型，见 `docs/33` §11 的偏离表）。
    SessionParam,
    /// 完全不支持选模型（qwenpaw / mcode / zeroclaw）。
    ///
    /// 既不发 `set_model`，也不把模型塞进会话参数；用量归属回落到 provider label
    /// （上游这三家同样是 `"unknown"`）。
    Unsupported,
}

/// 恢复会话时 `session/resume` / `session/load` 的 params 形状。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpResumeParams {
    /// `{cwd, sessionId, mcpServers}`（除 zeroclaw 外的全部 provider）。
    SessionAndCwd,
    /// 只有 `{sessionId}`（上游 `zeroclaw.go` 的 resume 原样如此）。
    SessionOnly,
}

/// 一个 ACP provider 的差异点（`static` 一张表 + 一个泛型客户端）。
#[derive(Debug)]
pub struct AcpFlavor {
    /// 官方类型（registry 的键）。
    pub kind: AgentType,
    /// 日志 / 错误串里的名字。
    pub label: &'static str,
    /// 恢复会话用哪个方法。
    pub resume: AcpResume,
    /// 要不要先认证。
    pub auth: AcpAuth,
    /// prompt 块字段名。
    pub prompt_fields: AcpPromptFields,
    /// 推理等级走 `session/set_config_option` 时用的 `configId`（`None` = 不支持）。
    ///
    /// kimi 是 `"thinking"`（上游硬编码）；dim / hermes 是 `"thought_level"`；
    /// reasonix 是 `"effort"`。上游 `applyACPEffortOption` 是**发现式**的（从会话
    /// 应答的 `configOptions` 里找 category 为 `thought_level`/`effort` 的那一项），
    /// 本 crate 不解析那份清单，改用静态 id —— 与批 2 kimi 同一取舍（`docs/33` §6.2）。
    pub thinking_config: Option<&'static str>,
    /// 工具名后处理用哪张表。
    pub tool_aliases: AcpToolAliases,
    /// 模型怎么选（见 [`AcpModelSelection`]）。
    pub model_selection: AcpModelSelection,
    /// 会话帧（`session/new` / `session/load`）params 里要不要带一个 `_meta` 键。
    ///
    /// 只有 qwenpaw 有：`"qwenpaw.coding_project_dir"`，值是 cwd，且**仅在
    /// `cwd != "."` 时**才带（上游 `qwenpaw.go` 的门）；其余 provider 是 `None`。
    pub session_meta_key: Option<&'static str>,
    /// 建完会话、选完模型之后要**依次**下发的固定配置（`session/set_config_option`）。
    ///
    /// 只有 dim 有：`[("permission", "full-access"), ("mode", "agent")]`（上游的
    /// 只读预设 → 放开；新会话与恢复会话都要重下发，因为恢复不保留）。链上任一步
    /// 失败都是**致命**的（与 `thinking_config` 的"失败仅告警"不同）。
    pub session_configs: &'static [(&'static str, &'static str)],
    /// 恢复会话的 params 形状（见 [`AcpResumeParams`]）。
    pub resume_params: AcpResumeParams,
}

impl AcpFlavor {
    /// 用量归属的回落标签。
    ///
    /// 不支持选模型的 provider（[`AcpModelSelection::Unsupported`]）统一落到
    /// `"unknown"` —— 上游 mcode / qwenpaw / zeroclaw 从不给 `opts.Model`，
    /// 用量归因就是字面量 `"unknown"`。
    pub const fn usage_label(&self) -> &'static str {
        match self.model_selection {
            AcpModelSelection::Unsupported => "unknown",
            _ => self.label,
        }
    }
}

/// 一个 ACP provider 需要提供的三件事（其余全由 [`AcpDecoder`] 负责）。
///
/// 之所以 [`AcpProvider::flavor`] 返回 `&'static`：provider 的差异表是编译期常量，
/// 解码器按 run 构造一次、只借它不克隆；同时 [`AcpProvider::build_args`] 保持成
/// **无 self 的关联函数**，才能塞进 [`CliSpec::build_args`] 那个函数指针。
pub trait AcpProvider: Send + Sync + 'static {
    /// 本 provider 的差异表。
    fn flavor() -> &'static AcpFlavor;

    /// 组装 argv（含 `extra_args` 过滤）。
    fn build_args(request: &LaunchRequest) -> Vec<String>;

    /// 运行期配置。
    fn config(&self) -> &CliCoreConfig;
}

/// 把 [`AcpProvider::build_args`] 转成 [`CliSpec::build_args`] 需要的函数指针。
fn acp_build_args<P: AcpProvider>(request: &LaunchRequest) -> Vec<String> {
    P::build_args(request)
}

impl<P: AcpProvider> CliProvider for P {
    fn kind(&self) -> AgentType {
        P::flavor().kind
    }

    fn spec(&self) -> CliSpec {
        CliSpec {
            label: P::flavor().label,
            // ACP 的 stdin 是**长连接**：帧由解码器的 outbox 驱动，不能写完即关。
            transport: PromptTransport::JsonRpc,
            capabilities: CliCapabilities {
                protocol: ProtocolFamily::Acp,
                streaming: true,
                // 协议层就有 `agent_thought_chunk`（逐 provider 是否真的发，见 `docs/33` §6）。
                thinking: true,
                tool_events: true,
                usage_reporting: true,
                resume: true,
            },
            // 对端先退出导致的 EPIPE 是常态，退出码才权威（与 codex 同款）。
            prompt_write_is_fatal: false,
            build_args: acp_build_args::<P>,
        }
    }

    fn config(&self) -> &CliCoreConfig {
        // 显式限定：`CliProvider` 与 `AcpProvider` 同名方法，不限定会歧义。
        AcpProvider::config(self)
    }

    fn decoder_for(&self, request: &LaunchRequest) -> Box<dyn CliDecoder> {
        Box::new(AcpDecoder::new(P::flavor(), request))
    }
}

/// 一行一帧地往 `out` 追加 JSON（`serde_json::to_string` 不产生裸换行）。
#[allow(clippy::needless_pass_by_value)] // 调用方就是 `push_json(&mut out, json!({…}))`。
pub(crate) fn push_json(out: &mut String, value: serde_json::Value) {
    out.push_str(&value.to_string());
    out.push('\n');
}

/// 一致性套件用的 ACP 正常回放（6 个 provider 只差 `session_id` 与正文）。
///
/// 帧序（刻意把正文通知排在 `session/prompt` 应答**之前**：解码器收到第一个
/// `Text` 时就已经有会话 id，取消用例里 `session/cancel` 才带得上 `sessionId`）：
///
/// 1. `id=1` `initialize` 应答（`with_auth` 时带 `authMethods: [{id:
///    "cached_token"}]`，让 grok 的选认证分支**不依赖** `XAI_API_KEY` 是否在环境里）；
/// 2. `with_auth` 时补一条 `id=2` `authenticate` 应答；
/// 3. `id=3` 会话应答（`sessionId`）；
/// 4. 一条 `session/update` 正文通知；
/// 5. `id=6` `session/prompt` 应答（`stopReason: end_turn` + 一份 `usage`）。
pub fn conformance_success_stdout(session_id: &str, text: &str, with_auth: bool) -> String {
    use serde_json::json;

    let init = if with_auth {
        json!({"protocolVersion": PROTOCOL_VERSION, "authMethods": [{"id": "cached_token"}]})
    } else {
        json!({"protocolVersion": PROTOCOL_VERSION, "clientCapabilities": {}})
    };
    let mut out = String::new();
    push_json(
        &mut out,
        json!({"jsonrpc": "2.0", "id": ID_INITIALIZE, "result": init}),
    );
    if with_auth {
        push_json(
            &mut out,
            json!({"jsonrpc": "2.0", "id": ID_AUTHENTICATE, "result": {}}),
        );
    }
    push_json(
        &mut out,
        json!({"jsonrpc": "2.0", "id": ID_SESSION, "result": {"sessionId": session_id}}),
    );
    push_json(
        &mut out,
        json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": session_id,
                "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}},
            },
        }),
    );
    push_json(
        &mut out,
        json!({
            "jsonrpc": "2.0",
            "id": ID_PROMPT,
            "result": {
                "stopReason": "end_turn",
                "usage": {"inputTokens": 10, "outputTokens": 5, "totalTokens": 15},
            },
        }),
    );
    out
}

/// 一致性套件用的 ACP 回放，带**固定配置链**（dim 型）。
///
/// 帧序与 [`conformance_success_stdout`] 相同，只在 `id=3` 的会话应答之后插入
/// `session_configs.len()` 条 `id=50+i` 的 `session/set_config_option` 应答：
/// 逐帧回放是**按客户端实际发的帧**推进的，多一条应答就会死等，所以需要配置链的
/// provider 必须用这一份而不是通用那份。
pub fn conformance_config_stdout(session_id: &str, text: &str, session_configs: usize) -> String {
    use serde_json::json;

    let mut out = conformance_success_stdout(session_id, text, false);
    // 在 `id=3` 应答之后、正文通知之前插入配置链应答。
    let anchor = format!(
        "{}\n",
        json!({"jsonrpc": "2.0", "id": ID_SESSION, "result": {"sessionId": session_id}})
    );
    let mut injected = String::new();
    for index in 0..session_configs {
        push_json(
            &mut injected,
            json!({"jsonrpc": "2.0", "id": id_session_config(index), "result": {}}),
        );
    }
    out = out.replacen(&anchor, &format!("{anchor}{injected}"), 1);
    out
}

/// 一致性套件用的脏数据回放：非 JSON 横幅 + 未知 `sessionUpdate` + 一条正文通知。
///
/// 解码器是**按行**驱动的状态机（通知不依赖握手阶段），所以这段流单独喂给
/// `decoder()` 也能解出 `text`。`label` 只用来拼那句横幅（十二个 ACP provider 共用
/// 同一个回放，横幅带上自己的名字才像真的）。
pub fn conformance_junk_stdout(label: &str, text: &str) -> String {
    use serde_json::json;

    let mut out = format!("{label}: 无法解析的横幅\n");
    push_json(
        &mut out,
        json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {"sessionId": "s", "update": {"sessionUpdate": "future.chunk"}},
        }),
    );
    push_json(
        &mut out,
        json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {"sessionId": "s", "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": text}}},
        }),
    );
    out
}
