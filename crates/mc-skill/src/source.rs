//! 导入源判定与规范化（clawhub.ai / skills.sh / github.com）——**只判源，不取件**。
//!
//! - **写者**：M6-3（`docs/57` §3.2 的 M6-3 文件组）。
//! - **上游**：`internal/handler/skill.go` 的 `detectImportSource`（枚举 `sourceClawHub` /
//!   `sourceSkillsSh` / `sourceGitHub`）与 `parseClawHubSlug` / `parseGitHubURL` 等 slug 解析。
//! - **语义（逐条对齐）**：
//!   1. `TrimSpace` 后为空 ⇒ 400（`empty URL`）；
//!   2. 缺 scheme 时补 `https://`（`github.com/a/b` 这种裸写法合法）；
//!   3. 按 **hostname** 判源（`www.` 前缀等价），非法 host ⇒ 400（错误里带支持列表）；
//!   4. **裸 slug 默认 clawhub**：不含 `/` 或不含 `.` 时按 clawhub skill 名处理。
//! - **本仓约定**：返回 `(ImportSource, String /*normalized*/)`，无 IO、无 `reqwest` 依赖；
//!   取件（三个源的 HTTP 调用 + 45s 总超时 + 502/503/504 映射）归 M6-3 的
//!   `routes/skills/import.rs`（`mc-http` 有 reqwest 边，本 crate 故意没有）。
//! - **不做什么**：不解析 GitHub tree 递归（那需要出网）；不做 HTML 抓取。
//!
//! **状态：M6-3 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 140 行以内（枚举 + 判定 + slug 解析 + 用例）。
