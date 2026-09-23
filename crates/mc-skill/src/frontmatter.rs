//! `SKILL.md` 的 YAML frontmatter 解析与序列化。
//!
//! - **写者**：M6-2（`docs/57` §3.2：`mc-skill/src/{frontmatter,binary,reserved}.rs` | M6-2 写）。
//! - **上游**：`internal/skill/frontmatter.go`（+ `internal/handler/skill.go` 的
//!   `CreateSkillRequest` 校验段）—— 结构化 skill 的 `name` / `description` 允许从
//!   `SKILL.md` 顶部的 `---` 围栏里取，正文才是 `skill.content`。
//! - **本仓约定**：返回 `Result<_, SkillParseError>`（crate 级错误，`thiserror`），
//!   **不要** `unwrap`；围栏缺失 = `Ok(None)`（「没有 frontmatter」不是错误，上游同样容忍），
//!   围栏存在但 YAML 非法 = `Err`（这时调用方返回 400，不能退化成「没有 frontmatter」，
//!   否则用户看着自己的文件被静默丢弃）。
//! - **不做什么**：不做 `name` 的**唯一性**判定（那是 `skill` 表的 `UNIQUE(workspace_id,name)`
//!   约束 + M6-2 的 409 映射）；不解析正文里的 `{{ }}` 模板语法（上游没有这一步）。
//!
//! **状态：M6-2 待落地**（本文件由 M6-0 anchor 建为 doc-only 桩）。
//!
//! 行预算（门 ⑩）：预计 120 行以内（纯函数 + 单元测试）。
