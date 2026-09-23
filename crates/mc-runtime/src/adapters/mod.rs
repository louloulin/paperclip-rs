//! 各 provider 的 adapter 实现。
//!
//! M3-2 落了第一个**真** adapter：[`pi_local`]（上游 `server/pkg/agent/pi.go`）。
//! M3-8 批次 1 补齐 CLI + JSON 事件流的 7 个官方类型，批次 2 再补 8 个
//! （5 个 ACP：[`kimi`] / [`kiro`] / [`qoder`] / [`qoderclicn`] / [`traecli`]，
//! 以及 `stream-json` 的 [`cursor`] / [`antigravity`] 与 ACP 的 [`grok`]），
//! 并按 `docs/18-M3-RUNTIME-ADAPTER.md` 的 6 步流程接入一致性套件 ——
//! 每个模块末尾都有一行 `crate::adapter_conformance!(YourAdapter);`，
//! 套件因此自动扩到同一套覆盖度（16 个 adapter × 8 项检查）。
//!
//! # 布局
//!
//! ```text
//! cli_core/            共享运行骨架（spawn / stdout 泵 / 终态归因 / 取消 / 版本探测）
//!   args.rs            通用 extra_args 过滤（屏蔽表 + 取值模式 + 位置参数剔除）
//!   decoder.rs         解码器累加器（正文 / 会话 / 用量 / 首个错误胜出）
//!   run.rs             单次 run 的读写循环与终态归因
//! claude_family.rs     claude 系（claude / codebuddy）的 stream-json 解码器
//! opencode_family.rs   opencode 系（opencode / codearts / deveco）的 NDJSON 解码器
//! qoder_family.rs      qoder 系（qoder / qoderclicn）的 argv 与封锁表
//! acp_core/            ACP（JSON-RPC over stdio）共享骨架 + 6 个 provider 的差异表
//! claude/  codebuddy/  codex/  copilot/  opencode/  codearts/  deveco/
//! cursor/  antigravity/  kimi/  kiro/  qoder/  qoderclicn/  traecli/  grok/
//!                     各 provider 的 argv + spec + 一致性脚本（解码器大的自带
//!                     `stream.rs` 子模块，如 cursor）
//! ```
//!
//! `pi_local` **不**改：它自带一套已验证的 spawn/流解析实现（含 pi 专有的
//! `TextDrain` 消毒与 session 文件锁）。两套 CLI 核心并存是有意的取舍 ——
//! 见 `docs/33` 的"与 `docs/18` §4 的偏离"一节。
//!
//! 已实现的 adapter 由 [`builtin_adapters`] 统一列出（注册表只吃这一份）。

pub mod acp_core;
pub mod antigravity;
pub mod claude;
pub mod claude_family;
pub mod cli_core;
pub mod codearts;
pub mod codebuddy;
pub mod codex;
pub mod copilot;
pub mod cursor;
pub mod deveco;
pub mod grok;
pub mod kimi;
pub mod kiro;
pub mod opencode;
pub mod opencode_family;
pub mod pi_local;
pub mod qoder;
pub mod qoder_family;
pub mod qoderclicn;
pub mod traecli;

pub use antigravity::Antigravity;
pub use claude::Claude;
pub use codearts::Codearts;
pub use codebuddy::Codebuddy;
pub use codex::Codex;
pub use copilot::Copilot;
pub use cursor::Cursor;
pub use deveco::Deveco;
pub use grok::Grok;
pub use kimi::Kimi;
pub use kiro::Kiro;
pub use opencode::Opencode;
pub use pi_local::{PiDecoder, PiLocal, PiLocalConfig};
pub use qoder::Qoder;
pub use qoderclicn::QoderCliCn;
pub use traecli::TraeCli;

use std::sync::Arc;

use crate::adapter::RuntimeAdapter;

/// 内置 adapter 列表（按上游白名单顺序）。
///
/// M3-8 **批 1**：`pi`（M3-2 已落地）+ 批 1 的 7 项；**批 2**（本片）再补 8 项
/// （5 个 ACP + `cursor` / `antigravity` 两个 `StreamJson`）。
/// 剩下的 9 项（`ACP` 6 + `StreamJson` 1（`qwen`）+ 未归类 2）由批 3 在这里继续
/// 追加；列表顺序 = 白名单顺序，因此加项不会改变已有顺序。
///
/// 构造是**纯构造**：不探测、不 spawn 任何 CLI，机器上有没有装都不影响装配。
/// 注册表（[`crate::registry::AdapterRegistry::with_builtin_adapters`]）直接吃这份
/// 列表，因此“能注册的 provider”与“已实现的 provider”不会两处分叉。
pub fn builtin_adapters() -> Vec<Arc<dyn RuntimeAdapter>> {
    vec![
        Arc::new(Claude::default()),
        Arc::new(Codebuddy::default()),
        Arc::new(Codex::default()),
        Arc::new(Copilot::default()),
        Arc::new(Opencode::default()),
        Arc::new(Codearts::default()),
        Arc::new(Deveco::default()),
        Arc::new(PiLocal::default()),
        Arc::new(Cursor::default()),
        Arc::new(Kimi::default()),
        Arc::new(Kiro::default()),
        Arc::new(Antigravity::default()),
        Arc::new(Qoder::default()),
        Arc::new(QoderCliCn::default()),
        Arc::new(TraeCli::default()),
        Arc::new(Grok::default()),
    ]
}
