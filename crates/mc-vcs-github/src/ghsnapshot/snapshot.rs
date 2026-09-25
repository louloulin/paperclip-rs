//! 那一条 GraphQL 查询的解析与归一化 —— 上游 `ghsnapshot/snapshot.go`（290 行）
//! （M8-0 anchor 建桩，**M8-5 落地**）。
//!
//! 契约（`docs/61` §4.2 的 M8-5 行）：把 `statusCheckRollup` 的 contexts 分页拿到后，
//! 归一化成扁平的逐 check 结果，并裁决「已决 / 未决」。**本文件是 PR 卡片 CI + 可合并性
//! 的唯一真值来源**：webhook 与页面访问只触发刷新，没有任何一处从载荷增量推断状态。
//!
//! # 三层落点（与上游逐字对应）
//!
//! | 上游 | 本文件 | 语义 |
//! | --- | --- | --- |
//! | `prSnapshotQuery`（`snapshot.go:27`） | [`PR_SNAPSHOT_QUERY`] | 一次往返拿全 `headRefOid` / `mergeable` / `mergeStateStatus` / `statusCheckRollup` |
//! | `FetchPRSnapshot`（`snapshot.go:159`） | [`fetch_pr_snapshot`] | **游标分页到完**（绝不假设 <100 个 context）+ 三道守卫 |
//! | `PRSnapshot.Decided`（`snapshot.go:78`） | [`PrSnapshot::decided`] | 已决 / CI 还在跑 **或** 可合并性未知 |
//! | `normalizeNode` / `normalizeRunStatus` / `normalizeStatusState` | [`normalize_node`] / [`normalize_run_status`] / [`normalize_status_state`] | GraphQL 联合体（`CheckRun` / `StatusContext`）→ 扁平三态 |
//!
//! # 三道分页守卫（上游逐字，各有一条用例）
//!
//! 1. **head 变了**（分页途中一次 `synchronize`）⇒ 整条响应作废
//!    （`ghsnapshot: pull request head changed during pagination`）；
//! 2. **rollup 从有变无**（第 >0 页拿到 `null`）⇒ 作废
//!    （`ghsnapshot: check rollup changed during pagination`）；
//! 3. **游标不前进 / 空游标**（病态替身）⇒ 作废
//!    （`ghsnapshot: invalid check-context pagination cursor`），外加
//!    [`MAX_SNAPSHOT_CONTEXT_PAGES`]（100 页 = 10k 个 context）的硬上限。
//!
//! 三条都是「检测到不一致就整条丢弃」，**不是**「尽力拼一个快照」—— 卡片宁可继续显示上一条
//! 陈旧但真实的快照，也不显示一条把新 head 的 context 标成旧 head 的假快照。
//!
//! # `statusCheckRollup == null` 的语义（`docs/61` §6.5 的 M8-5 `DoD`）
//!
//! `null` = 「这个 commit 还没有任何 check」⇒ [`PrSnapshot::has_checks`] `== false`。
//! 它**绝不**能被渲染成「通过」（上游 `snapshot.go:52-55` 的注释逐字）。它可以是「已决」的：
//! `mergeable` 已知时，`has_checks == false` 的 PR 照样已决（没有 CI 要等）。
//!
//! # 与 anchor 桩的一处**签名修订**（登记 `docs/32` §21.2 的 D1）
//!
//! anchor 的桩是 `parse_pr_snapshot(&Value) -> Result<PullRequestSnapshot, GithubError>`，
//! 而 `mc_core::github::PullRequestSnapshot` 需要的 `workspace_id` / `repo_owner` / `repo_name` /
//! `pr_number` / `fetched_at` **在 GraphQL 载荷里都不存在**，且它不携带迁移 `222` 要写的
//! `api_mergeable` / `api_merge_state_status` / `checks_rollup_state` 三列 ⇒ 那个签名**无法实现**。
//! 修订为返回本文件的上游同形快照 [`PrSnapshot`]（名字保留，`ghsnapshot/mod.rs` 的
//! `pub use snapshot::parse_pr_snapshot;` 逐字不动）；`parse_pr_snapshot` 在锚点期**零调用方**
//! （`grep -rn parse_pr_snapshot` 实测只有桩与 `pub use` 两处）⇒ 无消费者受影响。

use serde::Deserialize;
use serde_json::{json, Value};

use crate::ghsnapshot::Client;
use crate::rest::GithubError;

/// 「一条查询拿全 PR 卡片所需的一切」—— 上游 `prSnapshotQuery`（`snapshot.go:27`）逐字。
///
/// `$cursor` 分页 `statusCheckRollup.contexts`；调用方循环到 `hasNextPage == false`
/// （验收判据 2：**绝不**假设 <100 个 context）。
pub const PR_SNAPSHOT_QUERY: &str = r"query($owner:String!,$repo:String!,$number:Int!,$cursor:String){
  repository(owner:$owner,name:$repo){
    pullRequest(number:$number){
      headRefOid
      mergeable
      mergeStateStatus
      commits(last:1){nodes{commit{
        statusCheckRollup{
          state
          contexts(first:100,after:$cursor){
            pageInfo{hasNextPage endCursor}
            nodes{
              __typename
              ... on CheckRun{name status conclusion detailsUrl}
              ... on StatusContext{context state targetUrl}
            }
          }
        }
      }}}
    }
  }
}";

/// contexts 分页的硬上限（上游 `maxSnapshotContextPages`）：100 页 = 10k 个 context，
/// 远超任何真实 PR。病态游标循环在这里被截断。
pub const MAX_SNAPSHOT_CONTEXT_PAGES: usize = 100;

/// 归一化后的单个 check —— 上游 `CheckContext`（`snapshot.go:57`）。
///
/// GraphQL 的 `CheckRun` 与 `StatusContext` 两种节点都折叠成这一形状，于是下游的存储与聚合
/// 不需要分支。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotCheck {
    pub name: String,
    /// 归一化生命周期：`queued` / `in_progress` / `completed`（上游三值）。
    pub status: String,
    /// 归一化结论：`success` / `failure` / `neutral` / `cancelled` / `skipped` /
    /// `timed_out` / `action_required` / `startup_failure` / `stale` / `error` / `failure`…
    /// **`None` = 还在跑**（上游用空串表示；落库时两者都写 `NULL`，见迁移 `222` 的列注释）。
    pub conclusion: Option<String>,
    /// 上游 `DetailsURL` / `TargetURL`（空串 ⇒ `None`，落库写 `NULL`）。
    pub details_url: Option<String>,
    /// `StatusContext`（legacy commit status）为 `true`，`CheckRun` 为 `false`。
    /// 保留给展示与排障（迁移 `222` 的 `is_status_context` 列注释逐字）。
    pub is_status_context: bool,
}

/// 一次抓取写出的原子快照 —— 上游 `PRSnapshot`（`snapshot.go:66`）。
///
/// 它**逐字**镜像 API 返回的东西，不做任何增量推断。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrSnapshot {
    /// `headRefOid`：快照描述的那个 head（钉住防陈旧写）。
    pub head_sha: String,
    /// `mergeable`：`MERGEABLE` / `CONFLICTING` / `UNKNOWN`（**原始枚举**，落库前不再改大小写
    /// —— 上游 `lowerTextPtr` 在响应层做小写化）。`None` = 载荷缺这一项。
    pub mergeable: Option<String>,
    /// `mergeStateStatus`：`CLEAN` / `DIRTY` / `BLOCKED` / `BEHIND` / `UNSTABLE` …（原始枚举）。
    /// 「Ready to merge」**只**由 `CLEAN` 派生。
    pub merge_state_status: Option<String>,
    /// `statusCheckRollup.state`：`SUCCESS` / `FAILURE` / `PENDING` / `ERROR` / `EXPECTED`。
    /// **仅**当 [`PrSnapshot::has_checks`] 为 `false` 时才是 `None`。
    pub rollup_state: Option<String>,
    /// `statusCheckRollup` 为 `null` ⇒ `false`（上游：GitHub 报告「这个 commit 还没有 check」）。
    /// **绝不**能被渲染成「通过」。
    pub has_checks: bool,
    pub checks: Vec<SnapshotCheck>,
}

impl PrSnapshot {
    /// 快照是否**已决** —— 上游 `PRSnapshot.Decided`（`snapshot.go:78`）。
    ///
    /// 三条判据（逐字）：
    /// 1. `mergeable` 未知（`UNKNOWN` 或缺失）⇒ 未决；
    /// 2. **有 checks 且** rollup 还是 `PENDING` / `EXPECTED` / 空 ⇒ 未决；
    /// 3. 任一条 context 尚未 `completed` ⇒ 未决。
    ///
    /// 注意判据 3 **不**受 `has_checks` 约束（上游同款）：`has_checks == false` 时 `checks`
    /// 必为空，该条自然为真 ⇒ 「无检查但可合并性已知」= **已决**（`docs/61` §6.5 的 M8-5
    /// `DoD` 三态里的「无检查」那一态）。
    #[must_use]
    pub fn decided(&self) -> bool {
        match self.mergeable.as_deref() {
            None | Some("" | "UNKNOWN") => return false,
            Some(_) => {}
        }
        if self.has_checks {
            match self.rollup_state.as_deref() {
                None | Some("" | "PENDING" | "EXPECTED") => return false,
                Some(_) => {}
            }
        }
        self.checks.iter().all(|c| c.status == "completed")
    }

    /// 未决的一个**可展示原因**（诊断用，不是上游字段）：`None` = 已决。
    ///
    /// 判定顺序与 [`PrSnapshot::decided`] 的三条判据**逐条对应**（先可合并性、再 rollup、
    /// 最后逐 check），只用于日志与测试断言；卡片渲染**不**读它（上游也没有这个字段）。
    #[must_use]
    pub fn pending_reason(&self) -> Option<&'static str> {
        match self.mergeable.as_deref() {
            None | Some("" | "UNKNOWN") => return Some("mergeability_unknown"),
            Some(_) => {}
        }
        if self.has_checks {
            match self.rollup_state.as_deref() {
                None | Some("" | "PENDING" | "EXPECTED") => return Some("checks_rollup_pending"),
                Some(_) => {}
            }
        }
        if self.checks.iter().any(|check| check.status != "completed") {
            return Some("checks_running");
        }
        None
    }
}

// ---------------------------------------------------------------------------
// wire 结构（上游 `graphqlRollup` / `graphqlPullRequest` / `graphqlPRData`）
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GraphQlRollup {
    #[serde(default)]
    state: Option<String>,
    contexts: GraphQlContexts,
}

#[derive(Debug, Deserialize)]
struct GraphQlContexts {
    #[serde(rename = "pageInfo")]
    page_info: GraphQlPageInfo,
    #[serde(default)]
    nodes: Vec<Value>,
}

/// 一页的 `pageInfo`（`hasNextPage` / `endCursor`）—— 分页循环的输入。
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct GraphQlPageInfo {
    #[serde(rename = "hasNextPage", default)]
    has_next_page: bool,
    #[serde(rename = "endCursor", default)]
    end_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GraphQlPullRequest {
    #[serde(rename = "headRefOid", default)]
    head_ref_oid: Option<String>,
    #[serde(default)]
    mergeable: Option<String>,
    #[serde(rename = "mergeStateStatus", default)]
    merge_state_status: Option<String>,
    #[serde(default)]
    commits: GraphQlCommits,
}

#[derive(Debug, Default, Deserialize)]
struct GraphQlCommits {
    #[serde(default)]
    nodes: Vec<GraphQlCommitNode>,
}

#[derive(Debug, Deserialize)]
struct GraphQlCommitNode {
    commit: GraphQlCommit,
}

#[derive(Debug, Deserialize)]
struct GraphQlCommit {
    #[serde(rename = "statusCheckRollup", default)]
    status_check_rollup: Option<GraphQlRollup>,
}

impl GraphQlPullRequest {
    /// 上游 `graphqlPullRequest.rollup()`：只看 `commits(last:1).nodes[0]`。
    fn rollup(&self) -> Option<&GraphQlRollup> {
        self.commits
            .nodes
            .first()
            .and_then(|node| node.commit.status_check_rollup.as_ref())
    }
}

#[derive(Debug, Deserialize)]
struct GraphQlRepository {
    #[serde(rename = "pullRequest", default)]
    pull_request: Option<GraphQlPullRequest>,
}

#[derive(Debug, Deserialize)]
struct GraphQlPrData {
    repository: GraphQlRepository,
}

fn malformed(reason: &str) -> GithubError {
    GithubError::Malformed(format!("ghsnapshot: {reason}"))
}

/// 解析**一页** GraphQL `data` 对象 —— `graph_ql` 已经剥掉信封。
///
/// 返回 `(快照, page_info)`；`page_info` 由 [`fetch_pr_snapshot`] 用来决定是否继续分页。
/// `rollup == null`（或 `commits.nodes` 为空）⇒ `has_checks == false`，且
/// `page_info.has_next_page == false`（没有东西可再分页）。
///
/// # Errors
///
/// 载荷形状不符合预期（缺 `data.repository` / `pullRequest`）⇒ [`GithubError::Malformed`]。
pub(crate) fn parse_pr_snapshot_page(
    graphql_response: &Value,
) -> Result<(PrSnapshot, GraphQlPageInfo), GithubError> {
    let parsed: GraphQlPrData = serde_json::from_value(graphql_response.clone())
        .map_err(|_| malformed("malformed pull request data"))?;
    let pr = parsed
        .repository
        .pull_request
        .ok_or_else(|| malformed("pull request not found"))?;

    let mut snapshot = PrSnapshot {
        head_sha: pr.head_ref_oid.clone().unwrap_or_default(),
        mergeable: non_empty(pr.mergeable.clone()),
        merge_state_status: non_empty(pr.merge_state_status.clone()),
        ..PrSnapshot::default()
    };
    let Some(rollup) = pr.rollup() else {
        let page = GraphQlPageInfo {
            has_next_page: false,
            end_cursor: None,
        };
        return Ok((snapshot, page));
    };
    snapshot.has_checks = true;
    snapshot.rollup_state = non_empty(rollup.state.clone());
    for raw in &rollup.contexts.nodes {
        if let Some(check) = normalize_node(raw) {
            snapshot.checks.push(check);
        }
    }
    Ok((snapshot, rollup.contexts.page_info.clone()))
}

/// 解析**单页** GraphQL `data` 对象（不分页）—— anchor 桩名的保留形状。
///
/// 管道的真实入口是 [`fetch_pr_snapshot`]（分页到完）；本函数给「一次往返」的调用方与单测
/// 一个无 I/O 的解析口。签名相对 anchor 桩的修订理由见模块头。
///
/// # Errors
///
/// 同 [`parse_pr_snapshot_page`]。
pub fn parse_pr_snapshot(graphql_response: &Value) -> Result<PrSnapshot, GithubError> {
    parse_pr_snapshot_page(graphql_response).map(|(snapshot, _page)| snapshot)
}

/// 跑 `prSnapshotQuery`，把 `statusCheckRollup.contexts` 分页到完，返回归一化快照。
///
/// 上游 `FetchPRSnapshot`（`snapshot.go:159`）逐字。`now_unix` 由调用方注入
/// （[`crate::ghsnapshot::refresh::Manager`] 的 `Clock`），**不读系统时钟** —— App JWT 的
/// `iat` / `exp` 与 token 过期判定都可注入。
///
/// # Errors
///
/// - 客户端未配置（缺 App 私钥）⇒ [`GithubError::NotConfigured`]（上游 `client not configured`）；
/// - 传输 / 查询级错误 ⇒ `Client::graph_ql` 的错误（含 [`GithubError::RateLimited`]）；
/// - 载荷畸形 ⇒ 见 [`parse_pr_snapshot_page`]；
/// - 三道分页守卫之一触发、或超出 [`MAX_SNAPSHOT_CONTEXT_PAGES`] ⇒ [`GithubError::Malformed`]。
pub async fn fetch_pr_snapshot(
    client: &Client,
    installation_id: i64,
    owner: &str,
    repo: &str,
    number: i32,
    now_unix: i64,
) -> Result<PrSnapshot, GithubError> {
    if !client.enabled() {
        return Err(malformed("client not configured"));
    }
    let mut snapshot = PrSnapshot::default();
    let mut cursor: Option<String> = None;
    for page in 0..MAX_SNAPSHOT_CONTEXT_PAGES {
        let variables = json!({
            "owner": owner,
            "repo": repo,
            "number": number,
            "cursor": cursor.clone(),
        });
        let data = client
            .graph_ql(installation_id, PR_SNAPSHOT_QUERY, &variables, now_unix)
            .await?;
        let (page_snapshot, page_info) = parse_pr_snapshot_page(&data)?;
        if page == 0 {
            snapshot.head_sha.clone_from(&page_snapshot.head_sha);
            snapshot.mergeable.clone_from(&page_snapshot.mergeable);
            snapshot
                .merge_state_status
                .clone_from(&page_snapshot.merge_state_status);
        } else if page_snapshot.head_sha != snapshot.head_sha {
            // 每一页都会重读 PR 的最新 commit。若分页途中一个 synchronize 推进了 head，
            // 把两页混起来会把新 head 的 context 标成旧 head。
            return Err(malformed("pull request head changed during pagination"));
        }
        if !page_snapshot.has_checks {
            // `statusCheckRollup` 为 null ⇒ 还没有 check，没有可再分页的东西。
            if page > 0 {
                return Err(malformed("check rollup changed during pagination"));
            }
            return Ok(snapshot);
        }
        snapshot.has_checks = true;
        snapshot
            .rollup_state
            .clone_from(&page_snapshot.rollup_state);
        snapshot.checks.extend(page_snapshot.checks);
        if !page_info.has_next_page {
            return Ok(snapshot);
        }
        let next_cursor = page_info.end_cursor.clone().unwrap_or_default();
        if next_cursor.is_empty() || Some(next_cursor.as_str()) == cursor.as_deref() {
            return Err(malformed("invalid check-context pagination cursor"));
        }
        if page == MAX_SNAPSHOT_CONTEXT_PAGES - 1 {
            return Err(malformed("check-context pagination exceeds page limit"));
        }
        cursor = Some(next_cursor);
    }
    Err(malformed("check-context pagination exceeds page limit"))
}

/// 把一个 GraphQL 联合体节点（`CheckRun` / `StatusContext`）折叠成 [`SnapshotCheck`]。
///
/// 上游 `normalizeNode`（`snapshot.go:198`）：`__typename` 不认识、或节点不是对象
/// ⇒ `None`（**跳过**该节点，不报错 —— 上游返回 `ok=false`）。
///
/// `CheckRun`：`status` 走 [`normalize_run_status`]，`conclusion` 走**小写化**；
/// `StatusContext`：`state` 走 [`normalize_status_state`]，`is_status_context = true`。
/// 两者的 URL 字段分别落到 `detailsUrl` / `targetUrl`。
#[must_use]
pub fn normalize_node(raw: &Value) -> Option<SnapshotCheck> {
    let typename = raw.get("__typename").and_then(Value::as_str).unwrap_or("");
    match typename {
        "CheckRun" => {
            let name = raw.get("name").and_then(Value::as_str).unwrap_or("");
            let status = raw.get("status").and_then(Value::as_str).unwrap_or("");
            let conclusion = raw.get("conclusion").and_then(Value::as_str);
            Some(SnapshotCheck {
                name: name.to_string(),
                status: normalize_run_status(status).to_string(),
                conclusion: conclusion
                    .map(str::to_lowercase)
                    .and_then(|value| non_empty(Some(value))),
                details_url: non_empty(
                    raw.get("detailsUrl")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                ),
                is_status_context: false,
            })
        }
        "StatusContext" => {
            let name = raw.get("context").and_then(Value::as_str).unwrap_or("");
            let state = raw.get("state").and_then(Value::as_str).unwrap_or("");
            let (status, conclusion) = normalize_status_state(state);
            Some(SnapshotCheck {
                name: name.to_string(),
                status: status.to_string(),
                conclusion: non_empty(Some(conclusion.to_string())),
                details_url: non_empty(
                    raw.get("targetUrl")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                ),
                is_status_context: true,
            })
        }
        _ => None,
    }
}

/// 把 GraphQL `CheckRun.status` 枚举映射到生命周期 —— 上游 `normalizeRunStatus`
/// （`snapshot.go:264`）。
///
/// **只有 `COMPLETED` 是终态**；`QUEUED` / `IN_PROGRESS` / `WAITING` / `PENDING` /
/// `REQUESTED` 都还在跑（上游的 `default` 分支兜住后四种）。
#[must_use]
pub fn normalize_run_status(status: &str) -> &'static str {
    if status.eq_ignore_ascii_case("COMPLETED") {
        "completed"
    } else if status.eq_ignore_ascii_case("IN_PROGRESS") {
        "in_progress"
    } else {
        "queued"
    }
}

/// 把 legacy `StatusContext.state` 映射到 `(status, conclusion)` —— 上游
/// `normalizeStatusState`（`snapshot.go:277`）。
///
/// ```text
/// SUCCESS  → completed / success
/// FAILURE  → completed / failure
/// ERROR    → completed / error
/// PENDING  → in_progress / ""
/// EXPECTED（及任何未知） → queued / ""
/// ```
///
/// 结论为空串 = **还在跑**。`EXPECTED` 走 `default` 与未知状态同判（上游逐字）。
#[must_use]
pub fn normalize_status_state(state: &str) -> (&'static str, &'static str) {
    if state.eq_ignore_ascii_case("SUCCESS") {
        ("completed", "success")
    } else if state.eq_ignore_ascii_case("FAILURE") {
        ("completed", "failure")
    } else if state.eq_ignore_ascii_case("ERROR") {
        ("completed", "error")
    } else if state.eq_ignore_ascii_case("PENDING") {
        ("in_progress", "")
    } else {
        ("queued", "")
    }
}

/// 空串 ⇒ `None`（本仓的「无值」表示；落库时写 `NULL`，与上游 `textOrNull` 同判）。
fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests;
