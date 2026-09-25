//! GitHub webhook 载荷的分类与解码 —— 上游 `handler/github.go` 的事件分派
//! （`HandleGitHubWebhook` / `handleInstallationEvent` / `handlePullRequestEvent` /
//! `triggerPRRefreshFromCIEvent`）+ 三个 wire 结构（`ghInstallationPayload` /
//! `ghPullRequestPayload` / `ghCIEventPayload`）。
//!
//! GitHub 用 `X-GitHub-Event` 头做事件分类（`installation` / `pull_request` /
//! `check_suite`），本文件把三族收成一个枚举；`pull_request` 与 `check_suite` 的
//! **原始载荷**解码也在这里（`webhook.rs` 只做验签与分发）。
//!
//! # 相对 anchor 暂定形状的两处修订（登记 `docs/32` §9.12）
//!
//! | 条目 | anchor 暂定 | 本片（= 上游） | 依据 |
//! | --- | --- | --- | --- |
//! | `ping` | 归入 `Other` | **独立的 `Ping`** 变体 | `HandleGitHubWebhook` 对 `ping` 走 `writeJSON(200, {"ok":"pong"})` 并 `return`，**不**是「确认后忽略」的 202 路径 |
//! | 载荷形状 | 扁平的「最小载荷」（`number`/`repo_owner`/… 平铺） | **逐字照抄上游的嵌套 wire 形**（`pull_request.*` / `repository.owner.login` / `installation.id`） | 扁平形状无法承载 `body`（关闭关键词的唯一来源）与 `changes.base.ref.from`（`derivePRMergeableState` 的第三个输入） |
//!
//! # 解码纪律（与 Go 的 `json.Unmarshal` 对齐）
//!
//! 上游把缺字段解成**零值**而不是报错（`json.Unmarshal` 的默认行为），所以每个结构都带
//! `#[serde(default)]`：GitHub 少发一个字段、某个事件族不带 `changes`、`installation.id`
//! 缺席（`check_suite` 的 status 事件）都必须继续走下去，由**语义层**（`handlePullRequestEvent`
//! 的 `installation_id == 0` 短路）而不是解码层来拒绝。字段名逐字用 GitHub 的
//! `snake_case`（`html_url` / `mergeable_state` / `head_sha`），与上游 struct tag 一一对应。
//!
//! ⚠️ 本文件**不**做任何业务判定：没有非空校验、没有 identifier 抽取、没有状态归一化
//! （那些分别在 `mirror.rs` / `links.rs`），这样才能在纯函数层逐条钉住解码语义。

use serde::{Deserialize, Serialize};

/// `X-GitHub-Event` 的分类结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GithubEventKind {
    /// `ping` —— GitHub 的连通性探测：**只**回 `{"ok":"pong"}` + **200**。
    Ping,
    Installation,
    PullRequest,
    /// `check_suite` / `check_run` / `status` —— 三族 CI 事件。载荷**只**用来定位要刷新的
    /// PR（Plan C：CI 事件是纯触发器，载荷数据从不用于展示，上游注释逐字）。
    CheckSuite,
    /// 上游不建模的事件（`push` / `issues` / …）—— 确认（202）后忽略。
    Other,
}

impl GithubEventKind {
    /// 从 `X-GitHub-Event` 头值分类（上游 `switch event` 逐字，**大小写敏感**：
    /// GitHub 只发小写事件名，上游也没有 `strings.ToLower`）。
    pub fn classify(event_name: &str) -> Self {
        match event_name {
            "ping" => Self::Ping,
            "installation" => Self::Installation,
            "pull_request" => Self::PullRequest,
            "check_suite" | "check_run" | "status" => Self::CheckSuite,
            // 上游 `default:` 分支：acknowledge every event，但不动作。
            _ => Self::Other,
        }
    }

    /// 本事件是否走 202 的「确认」路径（`ping` 是唯一的 200 特例）。
    pub fn acknowledges_with_accepted(self) -> bool {
        self != Self::Ping
    }
}

// ---------------------------------------------------------------------------
// 小工具（上游 `github.go` 底部三个 helper，逐字）
// ---------------------------------------------------------------------------

/// 上游 `coalesce(a, fallback)`：**只有 trim 后为空**才取 fallback，返回的是**原值**
/// （不 trim）—— 与「先 trim 再回填」不是同一语义，这里逐字照抄。
pub fn coalesce(value: &str, fallback: &str) -> String {
    if value.trim().is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

/// 上游 `strPtrOrNil(s)`：**空串**（不 trim）⇒ `None`。
pub fn str_ptr_or_nil(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// 上游 `parseGHTime(s)`：空的 / 非 RFC3339 ⇒ `None`（`pgtype.Timestamptz{}` 的零值）。
///
/// 本仓用 `chrono::DateTime<Utc>` 承载「有值」那一半；`None` 就是 Go 的
/// `pgtype.Timestamptz{Valid:false}`。
pub fn parse_gh_time(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    if value.is_empty() {
        return None;
    }
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|parsed| parsed.with_timezone(&chrono::Utc))
}

/// 上游 `parseGHTimeRequired(s)`：解析不出（含空串）⇒ **取当前时刻**
/// （上游 `time.Now().UTC()`；绝不写 NULL —— `pr_created_at` / `pr_updated_at` 是 `NOT NULL`）。
pub fn parse_gh_time_required(value: &str) -> chrono::DateTime<chrono::Utc> {
    parse_gh_time(value).unwrap_or_else(chrono::Utc::now)
}

// ---------------------------------------------------------------------------
// installation
// ---------------------------------------------------------------------------

/// 上游 `ghInstallationPayload`。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InstallationEventPayload {
    pub action: String,
    pub installation: InstallationObject,
}

/// `installation` 对象（只看 id 与 account）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InstallationObject {
    pub id: i64,
    pub account: InstallationAccount,
}

/// `installation.account`。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InstallationAccount {
    pub login: String,
    /// `User` / `Organization`（wire 名就是 `type` —— Rust 关键字 ⇒ 改名 + `rename`）。
    #[serde(rename = "type")]
    pub account_type: String,
    pub avatar_url: String,
}

/// 上游 `githubInstallationAccountFromPayload`：login 缺失 ⇒ `None`（调用方打 warn 后返回）。
///
/// 返回 `(login, account_type, avatar)`：`account_type` 走 `coalesce(…, "User")`，
/// `avatar` 走 `strPtrOrNil`（空串 ⇒ `None`）。
pub fn installation_account_from_payload(
    payload: &InstallationEventPayload,
) -> Option<(String, String, Option<String>)> {
    let login = payload.installation.account.login.trim();
    if login.is_empty() {
        return None;
    }
    Some((
        login.to_string(),
        coalesce(&payload.installation.account.account_type, "User"),
        str_ptr_or_nil(&payload.installation.account.avatar_url),
    ))
}

/// `installation` 事件里会**删除全部绑定**的两个 action（上游逐字：`"deleted", "suspend"`）。
pub const INSTALLATION_ACTIONS_DELETING: [&str; 2] = ["deleted", "suspend"];

/// `installation` 事件里会**刷新账号元数据 / 建 pending** 的三个 action
/// （上游逐字：`"created", "new_permissions_accepted", "unsuspend"`）。
pub const INSTALLATION_ACTIONS_UPSERTING: [&str; 3] =
    ["created", "new_permissions_accepted", "unsuspend"];

// ---------------------------------------------------------------------------
// pull_request
// ---------------------------------------------------------------------------

/// 上游 `ghPullRequestPayload`。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PullRequestEventPayload {
    pub action: String,
    pub pull_request: PullRequestObject,
    /// 只有 `pull_request.edited` 这类事件才带（本片只读 `base.ref.from`）。
    pub changes: Option<PrChanges>,
    pub repository: RepositoryObject,
    pub installation: InstallationId,
}

/// `pull_request` 对象（逐字对齐上游 struct tag）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PullRequestObject {
    pub number: i32,
    pub html_url: String,
    pub title: String,
    /// **关闭关键词的唯一来源**（`extractClosingIdentifiers(title, body)`）。
    pub body: String,
    /// `open` / `closed`（GitHub 的 REST 值，**不是**本仓的归一化四态）。
    pub state: String,
    pub draft: bool,
    pub merged: bool,
    pub merged_at: String,
    pub closed_at: String,
    pub created_at: String,
    pub updated_at: String,
    /// GitHub REST 的 `mergeable_state`（本片只把它原样入库，见 `derive_pr_mergeable_state`）。
    pub mergeable_state: String,
    pub additions: i32,
    pub deletions: i32,
    pub changed_files: i32,
    pub head: PrHead,
    pub user: PrUser,
}

/// `pull_request.head`（`ref` 是 Rust 关键字 ⇒ 字段改名 + `rename`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrHead {
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub sha: String,
}

/// `pull_request.user`。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrUser {
    pub login: String,
    pub avatar_url: String,
}

/// 上游 `ghPRChanges`：只取 `base.ref.from`（一次 base 分支切换）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrChanges {
    pub base: Option<PrBaseChange>,
}

/// `changes.base`。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrBaseChange {
    #[serde(rename = "ref")]
    pub ref_change: Option<PrBaseRefFrom>,
}

/// `changes.base.ref`（只看 `from`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrBaseRefFrom {
    pub from: String,
}

/// 上游 `repository` 对象（`name` + `owner.login`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RepositoryObject {
    pub name: String,
    pub owner: OwnerObject,
}

/// `repository.owner`。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OwnerObject {
    pub login: String,
}

/// `installation` 对象里我们只读 `id` 的那一半。
///
/// ⚠️ 与 [`InstallationObject`] 分开是有意的：`check_suite` / `status` 载荷里的
/// `installation` 只有 `id`，上游也是两个不同的匿名结构。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InstallationId {
    pub id: i64,
}

// ---------------------------------------------------------------------------
// check_suite / check_run / status（CI 事件：纯触发器）
// ---------------------------------------------------------------------------

/// 上游 `ghCIEventPayload`：三族 CI 事件的公共形状。
///
/// Plan C 下这些事件的载荷**从不**用于展示 —— 只用来回答「要刷新哪个 PR」：
/// `check_suite`/`check_run` 直接带 `pull_requests[].number`；`status` 只带 commit SHA，
/// 需要回查 `head_sha`（`mirror.rs` 之外的 DB 查询，落在 `mc_repos::github::check_suite`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CiEventPayload {
    pub installation: InstallationId,
    pub repository: RepositoryObject,
    /// `status` 事件的顶层 commit SHA（`check_suite`/`check_run` 没有）。
    pub sha: String,
    pub check_suite: CiCheckSuite,
    pub check_run: CiCheckRun,
}

/// `check_suite`（本片只读 `head_sha` 与 `pull_requests`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CiCheckSuite {
    pub head_sha: String,
    pub pull_requests: Vec<PrNumberRef>,
}

/// `check_run`（本片只读 `pull_requests` 与内嵌 `check_suite.head_sha`）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CiCheckRun {
    pub pull_requests: Vec<PrNumberRef>,
    pub check_suite: CiCheckSuiteHead,
}

/// `check_run.check_suite` 的**仅 `head_sha`** 投影。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CiCheckSuiteHead {
    pub head_sha: String,
}

/// `pull_requests[]` 里我们只读 `number` 的那一项。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrNumberRef {
    pub number: i32,
}

impl CiEventPayload {
    /// 载荷里直接给出的 PR 号（去重，保持出现顺序：先 `check_suite` 再 `check_run`）。
    ///
    /// 上游用 `seen map[int32]struct{}` 去重后又判断 `len(seen) > 0`，所以**顺序无关**，
    /// 但去重语义必须一致（同一个 PR 在两个数组里各出现一次 ⇒ 只入队一次）。
    pub fn direct_pull_request_numbers(&self) -> Vec<i32> {
        let mut seen = Vec::new();
        for number in self
            .check_suite
            .pull_requests
            .iter()
            .chain(self.check_run.pull_requests.iter())
            .map(|pr| pr.number)
        {
            if number == 0 || seen.contains(&number) {
                continue;
            }
            seen.push(number);
        }
        seen
    }

    /// 没有 PR 号时要回查的 head SHA（上游 `coalesce(p.CheckSuite.HeadSHA, p.CheckRun.CheckSuite.HeadSHA)`
    /// 之外先看顶层 `sha`）。
    pub fn head_sha_for_lookup(&self) -> Option<String> {
        let sha = if self.sha.is_empty() {
            coalesce(
                &self.check_suite.head_sha,
                &self.check_run.check_suite.head_sha,
            )
        } else {
            self.sha.clone()
        };
        if sha.is_empty() {
            None
        } else {
            Some(sha)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_classification_matches_the_upstream_switch() {
        assert_eq!(GithubEventKind::classify("ping"), GithubEventKind::Ping);
        assert_eq!(
            GithubEventKind::classify("installation"),
            GithubEventKind::Installation
        );
        assert_eq!(
            GithubEventKind::classify("pull_request"),
            GithubEventKind::PullRequest
        );
        // 三族 CI 事件同判（上游 `case "check_suite", "check_run", "status"`）。
        for name in ["check_suite", "check_run", "status"] {
            assert_eq!(
                GithubEventKind::classify(name),
                GithubEventKind::CheckSuite,
                "{name}"
            );
        }
        assert_eq!(GithubEventKind::classify("push"), GithubEventKind::Other);
        // 大小写敏感：上游没有 ToLower。
        assert_eq!(GithubEventKind::classify("Ping"), GithubEventKind::Other);
        // `ping` 是唯一的 200 特例，其余全走 202。
        assert!(!GithubEventKind::Ping.acknowledges_with_accepted());
        assert!(GithubEventKind::Other.acknowledges_with_accepted());
    }

    #[test]
    fn pull_request_payload_decodes_the_nested_wire_shape() {
        let raw = serde_json::json!({
            "action": "closed",
            "pull_request": {
                "number": 7,
                "html_url": "https://github.com/acme/api/pull/7",
                "title": "fix: Closes MUL-42",
                "body": "also Closes: mul-43",
                "state": "closed",
                "draft": false,
                "merged": true,
                "merged_at": "2026-09-25T07:00:00Z",
                "closed_at": "",
                "created_at": "2026-09-20T07:00:00Z",
                "updated_at": "2026-09-25T07:00:00Z",
                "mergeable_state": "clean",
                "additions": 3, "deletions": 4, "changed_files": 5,
                "head": { "ref": "fix/login", "sha": "deadbeef" },
                "user": { "login": "dev", "avatar_url": "" }
            },
            "changes": { "base": { "ref": { "from": "main" } } },
            "repository": { "name": "api", "owner": { "login": "acme" } },
            "installation": { "id": 4242 }
        });
        let payload: PullRequestEventPayload =
            serde_json::from_value(raw).expect("decode pull_request payload");
        assert_eq!(payload.action, "closed");
        assert_eq!(payload.pull_request.number, 7);
        assert_eq!(payload.pull_request.body, "also Closes: mul-43");
        assert!(payload.pull_request.merged);
        assert_eq!(payload.pull_request.head.ref_name, "fix/login");
        assert_eq!(payload.repository.owner.login, "acme");
        assert_eq!(payload.repository.name, "api");
        assert_eq!(payload.installation.id, 4242);
        assert!(parse_gh_time(&payload.pull_request.merged_at).is_some());
        // 空串 ⇒ `None`（Go 的 `pgtype.Timestamptz{Valid:false}`）。
        assert!(parse_gh_time(&payload.pull_request.closed_at).is_none());
    }

    #[test]
    fn absent_fields_decode_to_zero_values_not_errors() {
        // 上游 `json.Unmarshal` 把缺字段解成零值；Go 的零值 `installation.id == 0` 由
        // **语义层**（`handlePullRequestEvent` 的第一条短路）拒绝，不是解码层。
        let payload: PullRequestEventPayload =
            serde_json::from_value(serde_json::json!({ "action": "opened" }))
                .expect("minimal payload decodes");
        assert_eq!(payload.installation.id, 0);
        assert_eq!(payload.pull_request.number, 0);
        assert!(payload.changes.is_none());
        // 显式 `null` 也走同一路（Go 的指针字段解成 nil）。
        let payload: PullRequestEventPayload =
            serde_json::from_value(serde_json::json!({ "changes": null })).expect("null changes");
        assert!(payload.changes.is_none());
    }

    #[test]
    fn installation_account_helpers_follow_coalesce_and_nil_semantics() {
        let payload: InstallationEventPayload = serde_json::from_value(serde_json::json!({
            "action": "created",
            "installation": { "id": 9, "account": { "login": "  acme  ", "type": "Organization", "avatar_url": "https://a/x.png" } }
        }))
        .expect("decode");
        let (login, kind, avatar) =
            installation_account_from_payload(&payload).expect("account present");
        // login 被 **trim**（上游 `strings.TrimSpace`），但 account_type 不 trim。
        assert_eq!(login, "acme");
        assert_eq!(kind, "Organization");
        assert_eq!(avatar.as_deref(), Some("https://a/x.png"));

        // 空白 login ⇒ None（不是空串）。
        let empty: InstallationEventPayload = serde_json::from_value(serde_json::json!({
            "installation": { "id": 9, "account": { "login": "   " } }
        }))
        .expect("decode");
        assert!(installation_account_from_payload(&empty).is_none());

        // 缺 `type` ⇒ `coalesce` 回填 `User`；空 avatar ⇒ None。
        let no_type: InstallationEventPayload = serde_json::from_value(serde_json::json!({
            "installation": { "id": 9, "account": { "login": "acme" } }
        }))
        .expect("decode");
        let (_, kind, avatar) = installation_account_from_payload(&no_type).expect("account");
        assert_eq!(kind, "User");
        assert!(avatar.is_none());
    }

    #[test]
    fn ci_payload_yields_direct_numbers_then_falls_back_to_head_sha() {
        let payload: CiEventPayload = serde_json::from_value(serde_json::json!({
            "installation": { "id": 3 },
            "repository": { "name": "api", "owner": { "login": "acme" } },
            "check_suite": {
                "head_sha": "aaa",
                "pull_requests": [{ "number": 5 }, { "number": 0 }, { "number": 5 }]
            },
            "check_run": { "pull_requests": [{ "number": 6 }], "check_suite": { "head_sha": "bbb" } }
        }))
        .expect("decode");
        // 去重 + 丢掉 0（上游 `enqueue` 的第一条短路）。
        assert_eq!(payload.direct_pull_request_numbers(), vec![5, 6]);
        // 有 PR 号时上游**不**做 head_sha 回查，但取值函数本身仍要给出答案。
        assert_eq!(payload.head_sha_for_lookup().as_deref(), Some("aaa"));

        // `status` 事件：顶层 sha、无 PR 号。
        let status: CiEventPayload = serde_json::from_value(serde_json::json!({
            "installation": { "id": 3 },
            "sha": "ccc",
            "check_suite": { "head_sha": "aaa" }
        }))
        .expect("decode");
        assert!(status.direct_pull_request_numbers().is_empty());
        assert_eq!(status.head_sha_for_lookup().as_deref(), Some("ccc"));

        // 什么都没有 ⇒ None（上游 `if sha == "" { return }`）。
        let bare: CiEventPayload = serde_json::from_value(serde_json::json!({})).expect("decode");
        assert!(bare.head_sha_for_lookup().is_none());
    }
}
