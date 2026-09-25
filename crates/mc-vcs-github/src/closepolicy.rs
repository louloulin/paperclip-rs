//! PR 自动关闭的**三态策略** —— 上游 `closeIntentPolicy`（`github.go` L964–L1997 内）
//! （M8-0 anchor 建桩，**实现归 M8-4**）。
//!
//! 决策必须在 PR 的**终态事件**（merge/close）时**冻结**：之后 issue 侧的改动不得
//! 改变它（`docs/61` §6.5 的 M8-4 行「自动关闭的三态策略」）。

/// 关闭意图三态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CloseIntent {
    /// 明示关闭关键词（`fixes` / `closes` / …）⇒ 终态时关 issue。
    Close,
    /// 提及但无关闭关键词 ⇒ 保持打开。
    KeepOpen,
    /// 尚未决（PR 未到终态，或载荷信息不足）。
    Undecided,
}

/// 依 PR 载荷与 issue 侧关键词算关闭意图 —— **anchor 期是桩**，实现归 M8-4。
pub fn close_intent_policy(_closing_keywords: &[String], _is_terminal: bool) -> CloseIntent {
    todo!("M8-4：closeIntentPolicy 三态（docs/61 §4.1 的 M8-4 行）")
}
