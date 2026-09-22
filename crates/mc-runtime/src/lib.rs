//! M3 anchor scaffold（LUM-1406）：runtime 抽象 crate —— **占位，无实现**。
//!
//! 由 M3-2（`feat/multica-rs-m3a-runtime-adapter`）填充：
//!
//! - `RuntimeAdapter` trait（launch / stream / cancel / probe-version / capabilities）；
//! - `AdapterRegistry` —— **替换** `crates/mc-http/src/state.rs::AdapterRegistryStub`
//!   及其调用点（`AdapterRegistryStub` 本片不动）；
//! - adapter 元数据表：白名单以 `server/pkg/agent/agent.go::SupportedTypes` 的
//!   **25 项**为准（docs/15 §9.3），launch header 从同文件的 `launchHeaders` 逐条抄；
//! - 一致性测试套件（宏 `adapter_conformance!(PiLocal)`），本片只做 `pi-local` 一个 adapter。
//!
//! 范围限制：不复制第 2 个 adapter；不做配额/计费；不做 Windows 分支（Linux-first）。
//! M3-4 / M3-8 会消费本 crate（profile 台账 / adapters 分批），因此依赖已在本文件预声明。
//!
//! 超 800 行按 `adapters/` 拆（docs/15 §7.7 的 R7）。
//!
//! scaffold 阶段本文件只有文档注释：一个类型都不定义。
