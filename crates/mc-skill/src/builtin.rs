//! 内置 skill 的**物化**（列表 / 取件 / 与工作区行的对齐）。
//!
//! - **写者**：M6-4（`docs/57` §3.2：`mc-skill/src/builtin.rs + assets/**` | M6-4 写）。
//! - **上游**：`internal/skill` 的内置物 + `/api/agents/{id}/skills` 的供给面；本仓的
//!   解析/供给路由是同切片（M6-4）的 `routes/agents/skills.rs`。
//! - **语义**：内置 skill 是**进程内资源**（`assets/**`，编译期随 crate 进二进制），
//!   不是数据库行；供给时要么按需物化成 `skill` 行（`mc-repos` 的 skill 写侧），要么直接
//!   以 `SkillRef{source: Builtin}` 出现在 bundle 里。`skillbundle.Source*` 的三个取值
//!   （`workspace`/`builtin`/`plugin`）见 `mc-core::skill` 的 `SkillSource`。
//! - **bundle 哈希的既有实现**：`mc-http` 的 `routes/daemon/skills.rs`（M3-7 已落地，本波
//!   归 M6-4）已经在建 manifest ⇒ **本文件不要**再写一份哈希（口径见 `mc-core::skill`）。
//! - **本仓约定**：`assets/**` 用 `include_str!`/`include_bytes!` 引入，**不用**运行时读盘
//!   （部署环境没有源树）；新增资产要过门 ⑩（单文件 ≤800 行，`file_size_baseline.tsv` 只减不增）。
//! - **不做什么**：不实现「内置 skill 的自动升级 / 版本回填」。
//!
//! **状态：M6-4 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 220 行以内。
