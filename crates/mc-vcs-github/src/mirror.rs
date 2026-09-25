//! PR 镜像的**决策核** —— 上游 `mirrorPullRequestForWorkspace`（`github.go:1535-1739`）
//! 与其三个纯 helper（`derivePRState` / `derivePRMergeableState` / `baseRefChanged`）。
//!
//! 一次镜像要做四件事（M8-4 的 `DoD`，`docs/61` §6.5）：写 `github_pull_request` 行 →
//! 自动关联 issue（[`crate::links`]）→ 自动关闭决策（[`crate::closepolicy`]）→ 入队快照刷新
//! （[`crate::port::PrRefreshPort`]）。**幂等**：同一事件重投只插 1 行。
//!
//! # 本文件为什么是**纯函数**（相对 anchor 形状的修订，登记 `docs/32` §9.12）
//!
//! anchor 落的是 `pub async fn mirror_pull_request(request: MirrorRequest) -> Result<(), GithubError>`
//! —— 那个签名要求本 crate 能自己持有 `Db` / 仓储 / `state.realtime`。但本 crate 的依赖边是
//! anchor **一次接好并冻结**的（`Cargo.toml` 注释逐字「此后 M8-1/4/5 的写者不得再新增三方依赖」），
//! 其中**没有** `mc-db` / `sqlx`，也没有 `mc-realtime`。所以本片把边界这样切：
//!
//! | 层 | 谁 | 内容 |
//! | --- | --- | --- |
//! | **决策**（本文件） | `mc-vcs-github` | 载荷 → [`MirrorPlan`]：哪些 issue 建/删关联、`close_intent` 写什么、要不要再跑推进闸门。零 I/O，可纯函数逐条钉住 |
//! | **I/O 编排** | `mc-http::routes::github::webhook`（本片写集） | 读库（解析 identifier）→ 调 [`MirrorRequest::plan`] → 写库 → 广播 → 入队刷新 |
//!
//! 这条切法与 M8-1 的判例同向：M8-1 把 `github_installation:created` 的**广播**留在
//! HTTP 层（`routes/github/install.rs`），因为 `state.realtime` 只在那里。
//!
//! ⚠️ 门 ⑩ 预飞把本文件排在 600–800 行（`docs/61` §6.3）⇒ 关联/关闭逻辑必须分到
//! `links.rs` / `closepolicy.rs`，**不得**全塞这里。
//!
//! # 三条逐字对齐上游的语义（容易被「顺手简化」掉的）
//!
//! 1. **「提到的」与「主张的」是两回事**：只有 title / 分支名里的引用、或 body 里的关闭
//!    关键词才算 **claim**；body 里的裸提及（`Related MUL-1`）**不建关联账**，而且在 PR 仍
//!    可编辑时还会**删掉**早先 claim 建过的那一行（MUL-3739 / MUL-7072）。
//! 2. **关联账是「以本次解析为准」**：`unlink` 之后**仍然**把这个 issue 放进再评估集合 ——
//!    删掉一条关联本身可能就是让 issue 能前进的原因。
//! 3. **推进闸门读的是持久化后的聚合**，不是「本次载荷有没有关闭词」：一个 `Closes MUL-1`
//!    的 PR 先合并、只带链接的兄弟 PR 后关闭时，仍然是 MUL-1 前进（见调用方的聚合查询）。

use std::collections::HashMap;

use mc_core::id::Id;

use crate::closepolicy::{preserve_close_intent, CloseIntentPolicy};
use crate::links::{extract_closing_identifiers, extract_identifiers};
use crate::payload::{PrChanges, PullRequestEventPayload};
use crate::port::{PrRefreshRequest, RefreshReason};

/// 一次镜像的输入（上游 `mirrorPullRequestForWorkspace` 的入参，去掉 DB / 广播句柄）。
#[derive(Debug, Clone)]
pub struct MirrorRequest<'a> {
    /// 本趟镜像归属的 workspace（上游按 installation 的绑定扇出，每个绑定一趟）。
    pub workspace_id: Id,
    /// 投递来源的 installation id（只用于快照入队的定位）。
    pub installation_id: i64,
    pub event: &'a PullRequestEventPayload,
    /// **投递级**的关闭裁决（所有绑定趟共用同一份，见 `closepolicy`）。
    pub close_policy: &'a CloseIntentPolicy,
}

/// 一条关联账的写入决策。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkDecision {
    pub issue_id: Id,
    /// 写进 `issue_pull_request.close_intent` 的值（`preserve` 为真时写库侧保持旧值）。
    pub close_intent: bool,
}

/// 一次镜像的决策结果（上游那趟循环的「意图」，不含任何写）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MirrorPlan {
    /// 上游 `preserveCloseIntent`：真 ⇒ 关联账的 `close_intent` 保持库里的值。
    pub preserve_close_intent: bool,
    /// 要 upsert 的关联账（顺序 = 标识符出现顺序）。
    pub links: Vec<LinkDecision>,
    /// 要**删除**的关联账（裸提及在可编辑期出现的「掉链」）。
    pub unlinks: Vec<Id>,
    /// 关联/掉链**之后**要重跑推进闸门的 issue（含掉链的那些，见模块头第 2 条）。
    pub reeval: Vec<Id>,
    /// PR 是否已达终态（`merged` / `closed`）—— 终态才跑「推进到 done」那一段。
    pub should_advance: bool,
}

impl MirrorPlan {
    /// 本次真正建/改的关联账所指向的 issue（上游广播里的 `linked_issue_ids`）。
    pub fn linked_issue_ids(&self) -> Vec<Id> {
        self.links.iter().map(|link| link.issue_id).collect()
    }
}

impl MirrorRequest<'_> {
    /// 本次投递在**本 workspace** 里要解析的全部标识符（`title` + `body` + 分支名）。
    ///
    /// 调用方必须先按这个列表逐个解析成 issue（顺序即上游的 `extractIdentifiers` 顺序），
    /// 再交给 [`Self::plan`]。抽出来是为了让「解析」与「决策」用的是**同一份**列表 ——
    /// 两处各算一次 `extract_identifiers` 就会在 `MUL-007` / `MUL-7` 这类输入上分叉。
    pub fn identifiers(&self) -> Vec<String> {
        extract_identifiers(&[
            self.event.pull_request.title.as_str(),
            self.event.pull_request.body.as_str(),
            self.event.pull_request.head.ref_name.as_str(),
        ])
    }

    /// 归一化后的 PR 状态（上游 `derivePRState`）：`merged` > `closed` > `draft` > `open`。
    pub fn derived_state(&self) -> &'static str {
        derive_pr_state(
            &self.event.pull_request.state,
            self.event.pull_request.draft,
            self.event.pull_request.merged,
        )
    }

    /// `mergeable_state` 的写入决策（上游 `derivePRMergeableState`）：`(值, 是否强制置 NULL)`。
    pub fn mergeable_write(&self) -> (Option<String>, bool) {
        derive_pr_mergeable_state(
            &self.event.action,
            &self.event.pull_request.mergeable_state,
            base_ref_changed(self.event.changes.as_ref()),
        )
    }

    /// 快照刷新入队载荷（上游 `h.PRRefresh.Enqueue(...)`，本仓端口多带 `workspace_id` 与 `head_sha`）。
    pub fn refresh_request(&self) -> PrRefreshRequest {
        PrRefreshRequest {
            workspace_id: self.workspace_id,
            repo_owner: self.event.repository.owner.login.clone(),
            repo_name: self.event.repository.name.clone(),
            pr_number: self.event.pull_request.number,
            head_sha: crate::payload::str_ptr_or_nil(&self.event.pull_request.head.sha),
            reason: RefreshReason::Webhook,
        }
    }

    /// 算出这趟镜像的 [`MirrorPlan`]。
    ///
    /// `resolved`：标识符 → 本 workspace 里的 issue id（**只**含真的解析到的那部分；
    /// 缺席即「本 workspace 没有这个 issue」，上游 `lookupIssueByIdentifier` 的 `ok=false`）。
    /// `auto_link`：workspace 的自动关联开关（关掉时**整段跳过**，只保留 PR 行的 upsert）。
    /// `prefix`：本 workspace 的 issue 前缀（已经过 `issueNumberForPrefix` 的过滤，这里只用于
    /// 文档性断言 —— 见用例）。
    pub fn plan(
        &self,
        resolved: &HashMap<String, Id>,
        auto_link: bool,
        prefix: &str,
    ) -> MirrorPlan {
        let state = self.derived_state();
        let preserve = preserve_close_intent(&self.event.action, state);
        let mut plan = MirrorPlan {
            preserve_close_intent: preserve,
            should_advance: state == "merged" || state == "closed",
            ..MirrorPlan::default()
        };
        if !auto_link {
            // 上游：PR 行的 upsert 无条件跑（重开 GitHub 功能能恢复历史，不需要回填），
            // 但**关联账**是「新副作用」，由 workspace 的开关把门。
            return plan;
        }

        let body = self.event.pull_request.body.as_str();
        let title = self.event.pull_request.title.as_str();
        let branch = self.event.pull_request.head.ref_name.as_str();
        let closing: Vec<String> = extract_closing_identifiers(&[title, body]);
        // 主张集 = title / 分支名的引用 ∪ body 的关闭关键词（body 的裸提及不算主张）。
        let mut claimed: Vec<String> = extract_identifiers(&[title, branch]);
        for identifier in &closing {
            if !claimed.contains(identifier) {
                claimed.push(identifier.clone());
            }
        }

        for identifier in self.identifiers() {
            let Some(issue_id) = resolved.get(&identifier).copied() else {
                // 上游 `lookupIssueByIdentifier` 失败 ⇒ `continue`（连掉链都不做）。
                continue;
            };
            if !claimed.contains(&identifier) {
                // 一次**路过式的提及**：从不建关联；PR 还可编辑时还要删掉早先 claim 建的行，
                // 让列表跟着实时解析走。PR 已终态 ⇒ 冻结规则同样适用（merge 之后的编辑
                // 不得把做了事的 PR 拆链）。
                if preserve {
                    continue;
                }
                plan.unlinks.push(issue_id);
                // 删链本身可能就是让 issue 前进的原因 ⇒ 照样进再评估集合。
                plan.reeval.push(issue_id);
                continue;
            }
            let declared = closing.contains(&identifier)
                && self
                    .close_policy
                    .permits(&identifier, &self.workspace_id.0.to_string());
            plan.links.push(LinkDecision {
                issue_id,
                close_intent: declared && !preserve,
            });
            plan.reeval.push(issue_id);
        }
        let _ = prefix;
        plan
    }
}

/// 上游 `derivePRState`：`merged` > `closed` > `draft` > `open`（顺序即优先级）。
pub fn derive_pr_state(state: &str, draft: bool, merged: bool) -> &'static str {
    if merged {
        "merged"
    } else if state == "closed" {
        "closed"
    } else if draft {
        "draft"
    } else {
        "open"
    }
}

/// 上游 `baseRefChanged`：`changes.base.ref.from` 非空（一次 base 分支切换）。
pub fn base_ref_changed(changes: Option<&PrChanges>) -> bool {
    changes
        .and_then(|changes| changes.base.as_ref())
        .and_then(|base| base.ref_change.as_ref())
        .is_some_and(|from| !from.from.is_empty())
}

/// 上游 `derivePRMergeableState` 的三态，返回 `(要写的新值, 是否强制置 NULL)`。
///
/// - `clear=true` ⇒ 写 `NULL`：状态**会变**的事件（`opened` / `synchronize` / `reopened`，
///   或 `edited` 换了 base 分支）必须抹掉旧裁决 —— GitHub 是异步重算可合并性的，载荷里的
///   值可能还是**上一个 head** 的结论，信它就是把陈旧裁决挂到新 head 上。
/// - `clear=false` + 新值非空 ⇒ 写新值。
/// - `clear=false` + 新值为空 ⇒ **保留旧值**：`labeled` / `assigned` / 不换 base 的 `edited`
///   这类元数据事件的载荷里没有可合并性，静默清空会丢掉 GitHub 懒得重算的结论。
pub fn derive_pr_mergeable_state(
    action: &str,
    payload: &str,
    base_ref_changed: bool,
) -> (Option<String>, bool) {
    if matches!(action, "opened" | "synchronize" | "reopened") {
        return (None, true);
    }
    if action == "edited" && base_ref_changed {
        return (None, true);
    }
    if payload.is_empty() {
        return (None, false);
    }
    (Some(payload.to_string()), false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::PullRequestEventPayload;

    fn payload(action: &str, patch: impl Into<serde_json::Value>) -> PullRequestEventPayload {
        let mut base = serde_json::json!({
            "action": action,
            "pull_request": {
                "number": 7,
                "title": "feat: nothing",
                "body": "",
                "state": "open",
                "draft": false,
                "merged": false,
                "head": { "ref": "feat/x", "sha": "abc" }
            },
            "repository": { "name": "api", "owner": { "login": "acme" } },
            "installation": { "id": 11 }
        });
        merge(&mut base, &patch.into());
        serde_json::from_value(base).expect("payload")
    }

    fn merge(target: &mut serde_json::Value, patch: &serde_json::Value) {
        let (Some(target), Some(patch)) = (target.as_object_mut(), patch.as_object()) else {
            return;
        };
        for (key, value) in patch {
            let replace =
                value.is_object() && target.get(key).is_some_and(serde_json::Value::is_object);
            if replace {
                let mut merged = target[key].clone();
                merge(&mut merged, value);
                target.insert(key.clone(), merged);
            } else {
                target.insert(key.clone(), value.clone());
            }
        }
    }

    fn request<'a>(
        event: &'a PullRequestEventPayload,
        policy: &'a CloseIntentPolicy,
    ) -> MirrorRequest<'a> {
        MirrorRequest {
            workspace_id: Id::new(),
            installation_id: event.installation.id,
            event,
            close_policy: policy,
        }
    }

    #[test]
    fn derived_state_priority_is_merged_closed_draft_open() {
        assert_eq!(derive_pr_state("open", false, false), "open");
        assert_eq!(derive_pr_state("open", true, false), "draft");
        assert_eq!(derive_pr_state("closed", false, false), "closed");
        assert_eq!(derive_pr_state("closed", true, false), "closed");
        // merged 压过一切（GitHub 的 closed 事件里 merged=true）。
        assert_eq!(derive_pr_state("closed", false, true), "merged");
        assert_eq!(derive_pr_state("open", true, true), "merged");
    }

    #[test]
    fn mergeable_state_three_state() {
        // 状态会变的三个 action ⇒ 强制 NULL，忽略载荷里的值。
        for action in ["opened", "synchronize", "reopened"] {
            assert_eq!(
                derive_pr_mergeable_state(action, "clean", false),
                (None, true),
                "{action}"
            );
        }
        // `edited` 换 base ⇒ 强制 NULL。
        assert_eq!(
            derive_pr_mergeable_state("edited", "clean", true),
            (None, true)
        );
        // `edited` 不换 base + 有值 ⇒ 写值。
        assert_eq!(
            derive_pr_mergeable_state("edited", "clean", false),
            (Some("clean".to_string()), false)
        );
        // 元数据事件 + 空载荷 ⇒ 保留旧值（两个 flag 都 false 语义）。
        assert_eq!(
            derive_pr_mergeable_state("labeled", "", false),
            (None, false)
        );
    }

    #[test]
    fn base_ref_changed_reads_only_the_from_field() {
        let json = serde_json::json!({ "base": { "ref": { "from": "main" } } });
        let changes: PrChanges = serde_json::from_value(json).expect("changes");
        assert!(base_ref_changed(Some(&changes)));
        let empty = serde_json::json!({ "base": { "ref": { "from": "" } } });
        let changes: PrChanges = serde_json::from_value(empty).expect("changes");
        assert!(!base_ref_changed(Some(&changes)));
        assert!(!base_ref_changed(None));
    }

    #[test]
    fn plan_links_only_claimed_identifiers_and_marks_close_intent() {
        // title 里有前缀 → claim；body 里 `Closes` 一个 → claim + close_intent。
        let event = payload(
            "closed",
            serde_json::json!({
                "pull_request": { "title": "MUL-1: do it", "body": "Closes MUL-2 and mentions MUL-3", "state": "closed", "merged": true }
            }),
        );
        let policy = CloseIntentPolicy::unrestricted();
        let request = request(&event, &policy);
        assert_eq!(
            request.identifiers(),
            vec![
                "MUL-1".to_string(),
                "MUL-2".to_string(),
                "MUL-3".to_string()
            ]
        );
        let mut resolved = HashMap::new();
        let (one, two, three) = (Id::new(), Id::new(), Id::new());
        resolved.insert("MUL-1".to_string(), one);
        resolved.insert("MUL-2".to_string(), two);
        resolved.insert("MUL-3".to_string(), three);
        let plan = request.plan(&resolved, true, "MUL");

        assert_eq!(request.derived_state(), "merged");
        assert!(plan.should_advance);
        // `closed` action 投递的**就是**终态事件本身 ⇒ `preserveCloseIntent` 为**假**
        //（上游逐字：`p.Action != "closed" && …`）。终态之后再来事件才冻结。
        assert!(!plan.preserve_close_intent);
        // title 前缀：claim、链接、但**没有**关闭意图。
        assert_eq!(
            plan.links,
            vec![
                LinkDecision {
                    issue_id: one,
                    close_intent: false
                },
                LinkDecision {
                    issue_id: two,
                    close_intent: true
                },
            ]
        );
        // body 里的裸提及：不建链，而且照样走「掉链」那一段（preserve 为假）。
        assert_eq!(plan.unlinks, vec![three]);
        assert_eq!(plan.reeval, vec![one, two, three]);
        assert_eq!(plan.linked_issue_ids(), vec![one, two]);
    }

    #[test]
    fn plan_unlinks_passing_mentions_while_the_pr_is_still_editable() {
        // PR 还在飞：裸提及要**删掉**早先 claim 建过的关联账。
        let event = payload(
            "edited",
            serde_json::json!({ "pull_request": { "body": "Related MUL-9" } }),
        );
        let policy = CloseIntentPolicy::unrestricted();
        let request = request(&event, &policy);
        let mut resolved = HashMap::new();
        let nine = Id::new();
        resolved.insert("MUL-9".to_string(), nine);
        let plan = request.plan(&resolved, true, "MUL");
        assert!(plan.links.is_empty());
        assert_eq!(plan.unlinks, vec![nine]);
        // 删链之后仍然要再评估（删链本身可能就是前进的原因）。
        assert_eq!(plan.reeval, vec![nine]);
        // 未终态 ⇒ 不跑推进那一段。
        assert!(!plan.should_advance);
        assert!(!plan.preserve_close_intent);
    }

    #[test]
    fn plan_auto_link_off_writes_no_link_rows_at_all() {
        let event = payload(
            "opened",
            serde_json::json!({ "pull_request": { "title": "MUL-1: do it", "body": "Closes MUL-2" } }),
        );
        let policy = CloseIntentPolicy::unrestricted();
        let request = request(&event, &policy);
        let mut resolved = HashMap::new();
        resolved.insert("MUL-1".to_string(), Id::new());
        resolved.insert("MUL-2".to_string(), Id::new());
        let plan = request.plan(&resolved, false, "MUL");
        assert!(plan.links.is_empty());
        assert!(plan.unlinks.is_empty());
        assert!(plan.reeval.is_empty());
        assert!(!plan.should_advance, "state=open");
    }

    #[test]
    fn plan_withheld_close_policy_keeps_the_link_but_drops_close_intent() {
        // 多绑定 + 歧义 ⇒ 策略不放行：**链接照建**，但不带 close_intent（绝不能推进 issue）。
        let event = payload(
            "closed",
            serde_json::json!({
                "pull_request": { "title": "work", "body": "Closes MUL-2", "state": "closed", "merged": true }
            }),
        );
        let policy = CloseIntentPolicy::withheld();
        let request = request(&event, &policy);
        let mut resolved = HashMap::new();
        let two = Id::new();
        resolved.insert("MUL-2".to_string(), two);
        let plan = request.plan(&resolved, true, "MUL");
        assert_eq!(
            plan.links,
            vec![LinkDecision {
                issue_id: two,
                close_intent: false
            }],
            "不放行 ⇒ close_intent=false（但关联账照建）"
        );
    }

    #[test]
    fn plan_preserves_close_intent_on_post_terminal_edits() {
        // 终态**之后**的编辑：close_intent 冻结（写库侧保留旧值），裸提及也不删链。
        let event = payload(
            "edited",
            serde_json::json!({
                "pull_request": { "title": "MUL-1: do it", "body": "Related MUL-9", "state": "closed", "merged": true }
            }),
        );
        let policy = CloseIntentPolicy::unrestricted();
        let request = request(&event, &policy);
        let mut resolved = HashMap::new();
        let (one, nine) = (Id::new(), Id::new());
        resolved.insert("MUL-1".to_string(), one);
        resolved.insert("MUL-9".to_string(), nine);
        let plan = request.plan(&resolved, true, "MUL");
        assert!(plan.preserve_close_intent);
        assert_eq!(
            plan.links,
            vec![LinkDecision {
                issue_id: one,
                close_intent: false
            }]
        );
        assert!(plan.unlinks.is_empty(), "终态之后不得拆链");
        assert!(plan.should_advance);
    }

    #[test]
    fn plan_skips_identifiers_that_do_not_resolve_in_this_workspace() {
        let event = payload(
            "opened",
            serde_json::json!({ "pull_request": { "title": "MUL-1 OTHER-2", "body": "" } }),
        );
        let policy = CloseIntentPolicy::unrestricted();
        let request = request(&event, &policy);
        // 只有 MUL-1 解析到了（OTHER-2 属于另一个 workspace 的前缀）。
        let mut resolved = HashMap::new();
        let one = Id::new();
        resolved.insert("MUL-1".to_string(), one);
        let plan = request.plan(&resolved, true, "MUL");
        assert_eq!(plan.links.len(), 1);
        assert_eq!(plan.links[0].issue_id, one);
    }

    #[test]
    fn refresh_request_carries_workspace_and_head_sha() {
        let event = payload("opened", serde_json::json!({}));
        let policy = CloseIntentPolicy::unrestricted();
        let request = MirrorRequest {
            workspace_id: Id::new(),
            installation_id: 11,
            event: &event,
            close_policy: &policy,
        };
        let want = request.refresh_request();
        assert_eq!(want.repo_owner, "acme");
        assert_eq!(want.repo_name, "api");
        assert_eq!(want.pr_number, 7);
        assert_eq!(want.head_sha.as_deref(), Some("abc"));
        assert_eq!(want.reason, RefreshReason::Webhook);
        assert_eq!(want.workspace_id, request.workspace_id);
    }
}
