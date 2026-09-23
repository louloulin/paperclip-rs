//! `mc-skill`：skill 前端件的**纯逻辑**层（frontmatter / 二进制判定 / 保留路径 / 归档 /
//! 导入源判定 / 内置物化）。
//!
//! **状态：M6-0 anchor 只落文件与边界**（`LUM-1665`）—— 本文件只有模块声明与下面的归属表；
//! 六个子模块都是 doc-only 桩，由 M6-2 / M6-3 / M6-4 各自填自己的文件（一个文件一个写者）。
//! **本 crate 现在没有任何公开类型**，所以它是可编译、可门禁、可独立验收的空壳。
//!
//! ## 为什么是独立 crate
//!
//! 这些逻辑有三个互不相邻的消费者（`docs/57` §2.2）：
//!
//! - M6-2 的 `POST/PUT/DELETE /api/skills*`：写库前要校验 frontmatter、要挡保留路径；
//! - M6-3 的 `POST /api/skills/import` + `refresh`：解归档、判导入源、应用每包体积上限；
//! - M6-4 的 `/api/agents/{id}/skills`：把内置 skill 物化成 `skill` 行（`builtin.rs` +
//!   `assets/**`）。
//!
//! 三者都在 `mc-http` 里，但**都不需要 `AppState`/`Router`**；放进 route 文件会让「同一段
//! 校验」被抄三份（M3 已经吃过「两处真值」的亏）。crate 边界同时把它挡在 `mc-http` 的
//! 800 行门之外。
//!
//! ## 上游与写者（`docs/57` §3.2 矩阵）
//!
//! | 文件 | 写者 | 上游 |
//! | --- | :-: | --- |
//! | `src/lib.rs` | M6-0（本 anchor） | —— |
//! | `src/frontmatter.rs` | M6-2 | `internal/skill/frontmatter.go` |
//! | `src/binary.rs` | M6-2 | `internal/skill/binary.go`（`IsLikelyBinaryFilePath` / `IsLikelyBinaryContent`） |
//! | `src/reserved.rs` | M6-2 | `internal/skill/reserved.go`（`IsReservedContentPath`） |
//! | `src/archive.rs` | M6-3 | `internal/handler/skill_import_archive.go`（zip 解包 + 每包上限） |
//! | `src/source.rs` | M6-3 | `internal/handler/skill.go` 的 `detectImportSource` / `parseClawHubSlug`（**只判源，不取件**） |
//! | `src/builtin.rs` + `assets/**` | M6-4 | 内置 skill 的物化（`docs/57` §4.2 M6-4） |
//!
//! ## 与 `mc-core::skill` 的分工（别把两处真值立起来）
//!
//! `mc-core::skill` 是**列投影**（`skill` / `skill_file` / `agent_skill` / `skill_to_label`
//! 四张表的字段、封闭词表、bundle-ref 的 egress 形态）。本 crate 是**解析与判定**：它
//! 产出/校验 `String` 与 `Vec<u8>`，不定义新的实体、不新增字段。两边都不重复对方的内容。
//!
//! ## 不做什么
//!
//! - 不出网：三条导入源（clawhub.ai / skills.sh / github.com）的**取件**是 HTTP 客户端，
//!   归 M6-3 的 `routes/skills/import.rs`（`mc-http` 已有 reqwest 边）；
//! - 不碰数据库：`skill`/`skill_file` 行的读写是 `mc-repos` 的 skill 模块；
//! - 不新增迁移：四张表在 `migrations/upstream/008` + `162` + `368` 里都已存在；
//! - 不重写 bundle 哈希：唯一实现点是 `mc-http` 的 `routes/daemon/skills.rs`（M3-7 已落地，
//!   归 M6-4），口径见 `mc-core::skill` 的 bundle 段。

pub mod archive;
pub mod binary;
pub mod builtin;
pub mod frontmatter;
pub mod reserved;
pub mod source;
