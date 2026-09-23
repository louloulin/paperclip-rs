//! 插件包的**入口白名单**与体积上限校验（zip 条目级）。
//!
//! - **写者**：M6-1（`docs/57` §3.2）。M6-5 的 `packages.rs` 只读。
//! - **上游**：`pkg/plugincontract/bundle.go` —— `BundleFile`(51) / `Bundle`(57) /
//!   `surfaceModuleSyntaxVisitor`(282)。
//! - **两条硬纪律（照抄，不要放宽）**：
//!   1. **条目白名单**：包里的每个条目都要过允许列表/路径规范化（拒 `..`、拒绝对路径、
//!      拒符号链接式越界）。这不是「防御性编程」，是插件包不可信的**唯一**边界；
//!   2. **体积上限**：单文件与整包两个上限（`docs/57` §4.2 M6-1 的 `MaxBundleSize`），
//!      超限整包失败而不是截断。
//! - **本仓约定**：`zip` 版本停在 workspace 的 `2.x` 且只开 `deflate`（见根 `Cargo.toml`
//!   注释）；解包在内存里做（不落临时目录）；`sha256` 用纯 hex（`plugin_package_version.digest`
//!   有 `char_length = 64` 的 CHECK，带 `sha256:` 前缀会直接撞约束）。
//! - **不做什么**：不做 zip 加密包（`aes-crypto` 特征故意没开）；不做签名/验签（上游这一代没有）。
//!
//! **状态：M6-1 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 240 行以内。
