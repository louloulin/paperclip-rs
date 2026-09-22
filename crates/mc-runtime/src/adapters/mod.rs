//! 各 provider 的 adapter 实现。
//!
//! M3-2 只落一个**真** adapter：[`pi_local`]（上游 `server/pkg/agent/pi.go`）。
//! 其余 24 个官方类型由 M3-8 按 `docs/18-M3-RUNTIME-ADAPTER.md` 的 6 步流程补齐 ——
//! 每补完一个，只在它的模块里加一行 `crate::adapter_conformance!(YourAdapter);`，
//! 一致性套件自动扩容到同一套覆盖度。

pub mod pi_local;

pub use pi_local::{PiDecoder, PiLocal, PiLocalConfig};
