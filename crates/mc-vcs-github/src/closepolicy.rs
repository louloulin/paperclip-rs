//! PR 自动关闭的**三态策略**与**投递级裁决** —— 上游 `closeIntentPolicy` 及其
//! `permits` / `resolveCloseIntentPolicy`（`github.go` L1325–L1466），外加
//! `preserveCloseIntent` 的冻结规则（`mirrorPullRequestForWorkspace` 内）。
//!
//! # 两个不同的东西（不要混）
//!
//! | 名词 | 上游 | 回答的问题 |
//! | --- | --- | --- |
//! | [`CloseIntent`] | `mirrorPullRequestForWorkspace` 里的 `closeIntent` 布尔表达式 | 这条关联账上的 `close_intent` 该写什么 |
//! | [`CloseIntentPolicy`] | `type closeIntentPolicy struct` | 这次投递里**哪个 workspace** 才允许对某个关闭关键词动手 |
//!
//! 前者的三态（[`CloseIntent::Close`] / [`CloseIntent::KeepOpen`] /
//! [`CloseIntent::Undecided`]）把「终态 + 有词」「终态 + 无词」「未终态」分开；后者的
//! **allowlist** 语义（缺席即拒绝、零值什么都不允许）解决的是「一个 GitHub App installation
//! 绑了多个 workspace 时，`Closes ABC-100` 在两个 workspace 里都能解析成真实 issue」这个
//! 歧义（上游 #6804）。
//!
//! # 决策必须在终态事件时**冻结**
//!
//! `preserve_close_intent` 的判据逐字是上游的
//! `p.Action != "closed" && (state == "merged" || state == "closed")`：
//! PR 还在可编辑期时 `close_intent` 跟 title/body 走；一旦 GitHub 投递了终态事件，后续的
//! `edited` / `synchronize` **不得**改写 merge 时刻的关闭裁决。见 [`preserve_close_intent`]。
//!
//! # 为什么 allowlist 而不是 denylist（上游注释逐字）
//!
//! 「misjudging *unique* as *ambiguous* costs an auto-complete a human can perform, while
//! misjudging *ambiguous* as *unique* silently closes someone else's issue.」⇒ 判不准就
//! **不**动手。这条倾向决定了本文件所有失败分支的走向（见 [`CloseIntentPolicy::from_resolvers`]）。

use std::collections::{BTreeMap, BTreeSet, HashMap};

/// 关闭意图三态（关联账 `close_intent` 的裁决输入）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CloseIntent {
    /// 明示关闭关键词（`fixes` / `closes` / …）+ PR 已到终态 ⇒ `close_intent = true`。
    Close,
    /// 到终态但没有关闭关键词（裸提及 / 分支名引用）⇒ 保持打开。
    KeepOpen,
    /// 尚未决：PR 未到终态，或载荷信息不足 ⇒ 不改写既有裁决（[`preserve_close_intent`] 为真时也走这条）。
    Undecided,
}

/// 依 PR 载荷与 issue 侧关键词算关闭意图 —— 上游 `closeIntent := declared && !preserveCloseIntent`
/// 的三态化。
///
/// - `closing_keywords`：本次投递里**该 issue 的**关闭关键词（已过 [`crate::links::extract_closing_identifiers`]
///   与 `closePolicy.permits` 两道闸；空 ⇒ 没有声明）；
/// - `is_terminal`：PR 是否已达终态（`state ∈ {merged, closed}`）。
pub fn close_intent_policy(closing_keywords: &[String], is_terminal: bool) -> CloseIntent {
    if !is_terminal {
        // 未终态：裁决尚未冻结，`close_intent` 跟 title/body 走（由 preserve=false 表达），
        // 这里给出「未决」而不给出「关」——避免调用方在 PR 还在飞时冻结一个意图。
        return CloseIntent::Undecided;
    }
    if closing_keywords.is_empty() {
        CloseIntent::KeepOpen
    } else {
        CloseIntent::Close
    }
}

/// 上游 `preserveCloseIntent`：`p.Action != "closed" && (state == "merged" || state == "closed")`。
///
/// 真 ⇒ 关联账的 `close_intent` 保持库里的值（终态事件之后的编辑不改写 merge 时刻的裁决）。
pub fn preserve_close_intent(action: &str, state: &str) -> bool {
    action != "closed" && (state == "merged" || state == "closed")
}

/// 投递级的关闭裁决：**哪个 workspace** 才允许对某个关闭关键词动手。
///
/// 零值（[`Self::withheld`]）什么都不允许 —— 这是上游刻意的 fail-closed 构造：
/// 「absence denies」。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CloseIntentPolicy {
    /// 单绑定情形：跨 workspace 歧义不可能存在，扫描一次读都不做。
    unrestricted: bool,
    /// 关闭标识符 → 被**证明**能解析它的那个 workspace。记「赢家」而不是裸的
    /// `allowed`，是为了同时关掉扫描与镜像两趟之间的时间窗。
    owner: HashMap<String, String>,
}

impl CloseIntentPolicy {
    /// 单绑定的无限制策略（上游 `closeIntentPolicy{unrestricted: true}`）。
    pub fn unrestricted() -> Self {
        Self {
            unrestricted: true,
            owner: HashMap::new(),
        }
    }

    /// 零值：什么都不允许（每次「读不完整」的返回都是它）。
    pub fn withheld() -> Self {
        Self::default()
    }

    /// `permits` 是否处于无限制模式（诊断 / 用例用）。
    pub fn is_unrestricted(&self) -> bool {
        self.unrestricted
    }

    /// 已记录的唯一 owner 数（诊断 / 用例用）。
    pub fn owner_count(&self) -> usize {
        self.owner.len()
    }

    /// 上游 `permits(identifier, workspaceID)`。
    pub fn permits(&self, identifier: &str, workspace_id: &str) -> bool {
        if self.unrestricted {
            return true;
        }
        self.owner
            .get(identifier)
            .is_some_and(|owner| owner == workspace_id)
    }

    /// 上游 `resolveCloseIntentPolicy` 的最后一段：把「每个标识符被哪些 workspace 解析到」
    /// 折成策略，并**报告被withheld 的歧义标识符**（上游对每一个打一条 warn，因为症状是
    /// 「issue 莫名其妙变 done」且无其它线索可循）。
    ///
    /// - 恰好一个解析者 ⇒ 记为该标识符的 owner；
    /// - 两个及以上 ⇒ **不放行**（记进返回的歧义列表供调用方打日志）；
    /// - 没有解析者 ⇒ 该标识符根本不进 map（缺席即拒绝）。
    pub fn from_resolvers(resolvers: BTreeMap<String, Vec<String>>) -> (Self, Vec<String>) {
        let mut owner = HashMap::new();
        let mut ambiguous = Vec::new();
        for (identifier, workspaces) in resolvers {
            // 用 BTreeSet 去重，保持确定顺序（同一 workspace 的重复解析不算歧义）。
            let unique: BTreeSet<String> = workspaces.into_iter().collect();
            if unique.len() == 1 {
                if let Some(only) = unique.into_iter().next() {
                    owner.insert(identifier, only);
                }
            } else {
                ambiguous.push(identifier);
            }
        }
        (
            Self {
                unrestricted: false,
                owner,
            },
            ambiguous,
        )
    }

    /// 上游 `resolveCloseIntentPolicy` 的**第一段**：绑定数 < 2 ⇒ 无限制、零读。
    ///
    /// 单独抽出来是为了让「单绑定不做任何 DB 读」这条性能/正确性契约可被纯函数钉住。
    pub fn for_bindings(binding_count: usize) -> Self {
        if binding_count < 2 {
            Self::unrestricted()
        } else {
            Self::withheld()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_state_close_intent() {
        // 未终态 ⇒ 未决（不冻结任何意图）。
        assert_eq!(
            close_intent_policy(&["MUL-1".into()], false),
            CloseIntent::Undecided
        );
        assert_eq!(close_intent_policy(&[], false), CloseIntent::Undecided);
        // 终态 + 有关闭词 ⇒ 关。
        assert_eq!(
            close_intent_policy(&["MUL-1".into()], true),
            CloseIntent::Close
        );
        // 终态 + 无关闭词（裸提及 / 分支名引用）⇒ 保持打开。
        assert_eq!(close_intent_policy(&[], true), CloseIntent::KeepOpen);
    }

    #[test]
    fn preserve_close_intent_freezes_after_terminal_events() {
        // 终态由 `closed` action 投递 ⇒ **不**冻结（那正是 merge 时刻本身）。
        assert!(!preserve_close_intent("closed", "merged"));
        assert!(!preserve_close_intent("closed", "closed"));
        // 终态之后的编辑 / 同步 ⇒ 冻结。
        assert!(preserve_close_intent("edited", "merged"));
        assert!(preserve_close_intent("synchronize", "closed"));
        assert!(preserve_close_intent("labeled", "merged"));
        // PR 还在飞 ⇒ 不冻结（close_intent 跟 title/body 走）。
        assert!(!preserve_close_intent("opened", "open"));
        assert!(!preserve_close_intent("edited", "draft"));
    }

    #[test]
    fn policy_zero_value_denies_everything() {
        let withheld = CloseIntentPolicy::withheld();
        assert!(!withheld.permits("MUL-1", "ws-a"));
        assert_eq!(withheld.owner_count(), 0);
        assert!(!withheld.is_unrestricted());
    }

    #[test]
    fn policy_single_binding_is_unrestricted_and_does_no_reads() {
        let policy = CloseIntentPolicy::for_bindings(1);
        assert!(policy.is_unrestricted());
        assert!(policy.permits("ANY-1", "any-workspace"));
        // 零绑定（installation 没绑任何 workspace）也走无限制分支 —— 上游只判 `< 2`，
        // 而调用方在这种情形下根本不会走到镜像。
        assert!(CloseIntentPolicy::for_bindings(0).is_unrestricted());
        // 多绑定 ⇒ 零值起手（什么都不允许，等着扫描填）。
        let multi = CloseIntentPolicy::for_bindings(2);
        assert!(!multi.is_unrestricted());
        assert!(!multi.permits("MUL-1", "ws-a"));
    }

    #[test]
    fn policy_permits_only_the_recorded_owner() {
        let mut resolvers = BTreeMap::new();
        resolvers.insert("MUL-1".to_string(), vec!["ws-a".to_string()]);
        // 两个解析者 ⇒ 歧义，不放行。
        resolvers.insert(
            "MUL-2".to_string(),
            vec!["ws-a".to_string(), "ws-b".to_string()],
        );
        // 同一个 workspace 重复出现 **不**算歧义。
        resolvers.insert(
            "MUL-3".to_string(),
            vec!["ws-b".to_string(), "ws-b".to_string()],
        );
        let (policy, ambiguous) = CloseIntentPolicy::from_resolvers(resolvers);
        assert!(policy.permits("MUL-1", "ws-a"));
        assert!(!policy.permits("MUL-1", "ws-b"));
        assert!(!policy.permits("MUL-2", "ws-a"));
        assert!(!policy.permits("MUL-2", "ws-b"));
        assert!(policy.permits("MUL-3", "ws-b"));
        // 没被任何 workspace 解析到的标识符 ⇒ 缺席即拒绝。
        assert!(!policy.permits("MUL-9", "ws-a"));
        assert_eq!(ambiguous, vec!["MUL-2".to_string()]);
        assert_eq!(policy.owner_count(), 2);
    }
}
