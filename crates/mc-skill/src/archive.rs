//! zip 归档的**解包与上限**（`POST /api/skills/import` 的 multipart 包 / 预览解析）。
//!
//! - **写者**：M6-3（`docs/57` §3.2：`mc-skill/src/{archive,git}.rs` | M6-3 写）。
//! - **上游**：`internal/handler/skill_import_archive.go`。
//! - **两条硬上限（照抄，不要「宽松一点」）**：单包文件数上限与整包字节上限。超限必须
//!   **整包失败**（上游 `errImportCapExceeded` ⇒ 413），不能截断 —— 截断会产出一个「看起来
//!   合法」的不完整 skill（上游注释就是这么写的）。
//! - **本仓约定**：`zip` 的版本停在 workspace 的 `2.x` 且只开 `deflate` 特征（见根
//!   `Cargo.toml` 的注释：6/8 的 MSRV 高于本仓 `rust-version`）；解包**只在内存里**做
//!   （本 crate 不写磁盘、不建临时目录）；路径先过 `reserved` / 二进制判定再过调用方。
//! - **不做什么**：不做 zip 加密包（我们的包都是自己产的，`aes-crypto` 特征故意没开）；
//!   不落库（`skill_file` 行的写入归 M6-3 的 `routes/skills/import.rs` + `mc-repos`）。
//!
//! **状态：M6-3 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 180 行以内（解包 + 两条上限 + 错误映射 + 用例）。
