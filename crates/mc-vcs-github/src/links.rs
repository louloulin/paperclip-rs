//! issue ↔ PR 的自动关联与「closing keyword」抽取 —— 上游 `extractIdentifiers` /
//! `extractClosingIdentifiers`（`github.go` L964–L1997 内）
//! （M8-0 anchor 建桩，**实现归 M8-4**）。
//!
//! M8-4 的 `DoD` 点名了 **6 个边界**（`docs/61` §6.5）：大小写、`#` 前缀、跨行、代码块内、
//! `owner/repo#n` 形态、重复标识去重。

/// 抽取文中提到的全部 issue identifier（去重，保持出现顺序）。
///
/// **anchor 期是桩**，实现归 M8-4。
pub fn extract_identifiers(_text: &str) -> Vec<String> {
    todo!("M8-4：extractIdentifiers 的 6 个边界（docs/61 §6.5 的 M8-4 行）")
}

/// 只抽取**关闭语义**的 identifier（`fixes` / `closes` / `resolves` + 变体）。
///
/// **anchor 期是桩**，实现归 M8-4。
pub fn extract_closing_identifiers(_text: &str) -> Vec<String> {
    todo!("M8-4：extractClosingIdentifiers（docs/61 §6.5 的 M8-4 行）")
}
