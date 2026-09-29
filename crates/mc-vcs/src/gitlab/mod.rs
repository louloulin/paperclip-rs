//! GitLab 的 provider 适配器 —— **M8-2 已落地（`LUM-1799`）**
//! （`docs/61-M8-PLAN.md` §3.3）。
//!
//! 上游对应物是 `internal/integrations/vcs/gitlab.go`（248 行）：GitLab 在**每个轴**
//! 上都与 Forgejo/Gitea 不同 —— `/api/v4` + `PRIVATE-TOKEN` 头、`X-Gitlab-Token` 的
//! **明文 token 比较**（没有 HMAC）、`X-Gitlab-Event` 头、"merge request" 术语、
//! CI 走 **pipeline** 事件。归一化后的 [`PullRequestEvent`] / [`CIStatusEvent`]
//! 把这些差异全部挡在 handler 之外。
//!
//! # 与上游的两处刻意收紧（登记 `docs/32` §9.12）
//!
//! 1. **常量时间比较**：上游用 `subtle.ConstantTimeCompare`（Go 的 `==` 会提前返回），
//!    本仓同样走 [`crate::signature::verify_plaintext_token`]（`docs/61` §2.7 第 3 条）；
//! 2. **时间戳归一化不引入 `chrono`**：`mc-vcs` 的依赖边由 M8-0 anchor 一次冻结
//!    （`crates/mc-vcs/Cargo.toml` 注释逐字「此后 M8-2 的写者不得再新增三方依赖」），
//!    而 `events.rs` 的契约是「RFC3339 或空串」⇒ [`normalize_gitlab_time`] 用手写的
//!    公历算法完成 Go `time.Parse(...).UTC().Format(time.RFC3339Nano)` 的等价变换
//!    （**不**把 GitLab 方言泄进共享解析层，与上游注释的意图一致）。
//!
//! # 时间戳为什么必须归一化（上游注释的教训）
//!
//! GitLab 的 webhook 时间戳是 `"2017-09-20 08:31:45 UTC"` 这种**非 RFC3339** 形态。不归一化时
//! 每个事件的时间都解析失败、被静默替换成摄入时间，于是 PR 的 upsert 守卫与 commit-status 的
//! 单调守卫对 GitLab **整体失效**。归一化放在 provider 内，是因为这是 **provider 方言**。
//!
//! # 文件拆分（门 ⑩）
//!
//! 本文件与 [`time`]/`tests` 是同一份代码的**纯移动**（单文件 800 行上限，
//! `scripts/file_size_check.py`）：时间戳归一化的公历算法搬到 `gitlab/time.rs`，
//! 单元用例搬到 `gitlab/tests.rs`，[`normalize_gitlab_time`] 经 `pub use` 保持原路径。
//! 先例 = `docs/32` §30 的 **D10**（`routes/cloud/subscriptions/tests/{support,db}.rs`）。

use async_trait::async_trait;
use http::HeaderMap;
use mc_core::vcs::VcsProviderKind;
use serde::Deserialize;

use crate::events::{Account, CIStatusEvent, EventKind, PullRequestEvent};
use crate::forgejo::normalize_instance_url;
use crate::provider::{Provider, VcsError};
use crate::registry::Registry;
use crate::signature::verify_plaintext_token;

mod time;

/// 时间戳归一化（等价于 Go `time.Parse(layout).UTC().Format(time.RFC3339Nano)`）——
/// 实现住在 [`time`]，这里只做再导出，公开 API 的路径 `gitlab::normalize_gitlab_time` 不变。
pub use time::normalize_gitlab_time;

/// GitLab 适配器（无状态、零尺寸）。
#[derive(Debug, Clone, Copy, Default)]
pub struct GitLabProvider;

#[async_trait]
impl Provider for GitLabProvider {
    fn kind(&self) -> VcsProviderKind {
        VcsProviderKind::GitLab
    }

    /// 上游 `EventKind`：`Merge Request Hook` / `Pipeline Hook`，其余 [`EventKind::Other`]。
    fn event_kind(&self, headers: &HeaderMap) -> EventKind {
        match header_str(headers, "x-gitlab-event") {
            Some("Merge Request Hook") => EventKind::PullRequest,
            Some("Pipeline Hook") => EventKind::CIStatus,
            _ => EventKind::Other,
        }
    }

    /// `X-Gitlab-Token` 与存储 secret 的**常量时间**明文比较。
    ///
    /// GitLab 不对 body 做 HMAC：这个共享 token 就是**全部**鉴权 ⇒ 空 secret 永不通过
    /// （上游逐字：`an empty stored secret never validates`）。
    fn verify_signature(&self, secret: &str, headers: &HeaderMap, _body: &[u8]) -> bool {
        if secret.is_empty() {
            return false;
        }
        header_str(headers, "x-gitlab-token")
            .is_some_and(|presented| verify_plaintext_token(secret, presented))
    }

    fn parse_pull_request(&self, body: &[u8]) -> Result<PullRequestEvent, VcsError> {
        let decoded: GitLabMergeRequestPayload = crate::forgejo::decode_payload(body)?;
        let attributes = decoded.object_attributes;
        let (owner, name) = split_namespace(&decoded.project.path_with_namespace);
        let draft = attributes.draft
            || attributes.work_in_progress
            || attributes.title.to_lowercase().starts_with("draft:");

        Ok(PullRequestEvent {
            action: attributes.action,
            repo_owner: owner,
            repo_name: name,
            number: attributes.iid,
            title: attributes.title,
            body: attributes.description,
            state: normalize_gitlab_mr_state(&attributes.state, draft),
            html_url: attributes.url,
            branch: non_empty(attributes.source_branch),
            head_sha: attributes.last_commit.id,
            author_login: non_empty(decoded.user.username),
            author_avatar_url: non_empty(decoded.user.avatar_url),
            // GitLab 的 MR 载荷没有增删行数（上游同样不填）。
            additions: 0,
            deletions: 0,
            changed_files: 0,
            merged_at: None,
            closed_at: None,
            created_at: normalize_gitlab_time(&attributes.created_at),
            updated_at: normalize_gitlab_time(&attributes.updated_at),
        })
    }

    fn parse_ci_status(&self, body: &[u8]) -> Result<CIStatusEvent, VcsError> {
        let decoded: GitLabPipelinePayload = crate::forgejo::decode_payload(body)?;
        let attributes = decoded.object_attributes;
        // 优先 pipeline 的 `finished_at`（我们记录的状态跃迁），回落 `created_at`。
        let updated_at = if attributes.finished_at.is_empty() {
            attributes.created_at
        } else {
            attributes.finished_at
        };
        Ok(CIStatusEvent {
            sha: attributes.sha,
            // GitLab 的 pipeline 是「每个 commit 一条状态」而不是「每个命名 check 一条」
            // ⇒ 一个稳定的合成 context 作为状态行的键（上游逐字；合并列车/多 pipeline
            // 的已知简化见上游注释，属有意等价而非缺口）。
            context: "gitlab/pipeline".into(),
            state: normalize_gitlab_pipeline_state(&attributes.status),
            target_url: non_empty(attributes.url),
            description: None,
            updated_at: normalize_gitlab_time(&updated_at),
        })
    }

    /// `GET {instance}/api/v4/user`，`PRIVATE-TOKEN: <token>`。
    async fn validate_token(&self, instance_url: &str, token: &str) -> Result<Account, VcsError> {
        let endpoint = format!("{}/api/v4/user", normalize_instance_url(instance_url));
        let response = shared_client()
            .get(&endpoint)
            .header("PRIVATE-TOKEN", token)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| VcsError::Instance(format!("gitlab: request: {e}")))?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            tracing::warn!(endpoint = %endpoint, status = status.as_u16(), "vcs: gitlab token rejected");
            return Err(VcsError::Unauthorized);
        }
        if !status.is_success() {
            return Err(VcsError::Instance(format!(
                "gitlab: GET /user: status {}",
                status.as_u16()
            )));
        }

        let user: GitLabUser = response
            .json()
            .await
            .map_err(|e| VcsError::Malformed(format!("gitlab: decode user: {e}")))?;
        if user.username.is_empty() {
            return Err(VcsError::Malformed(
                "gitlab: user response missing username".into(),
            ));
        }
        Ok(Account {
            login: user.username,
            kind: VcsProviderKind::GitLab,
        })
    }
}

// ---------------------------------------------------------------------------
// 载荷形状（上游 `glMergeRequestPayload` / `glPipelinePayload`）
// ---------------------------------------------------------------------------

/// 上游 `glMergeRequestPayload`。**容器级** `#[serde(default)]` —— 理由同 `forgejo.rs`
/// 的载荷结构（Go 的 `json.Unmarshal` 不要求字段存在；加 `default` 才是**同宽**）。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct GitLabMergeRequestPayload {
    project: GitLabProject,
    object_attributes: GitLabMergeRequestAttributes,
    user: GitLabUserWithAvatar,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct GitLabProject {
    path_with_namespace: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct GitLabMergeRequestAttributes {
    iid: i32,
    title: String,
    description: String,
    /// `opened | closed | merged | locked`
    state: String,
    action: String,
    source_branch: String,
    url: String,
    draft: bool,
    work_in_progress: bool,
    created_at: String,
    updated_at: String,
    last_commit: GitLabLastCommit,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct GitLabLastCommit {
    id: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct GitLabUser {
    username: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct GitLabUserWithAvatar {
    username: String,
    avatar_url: String,
}

/// 上游 `glPipelinePayload`。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct GitLabPipelinePayload {
    object_attributes: GitLabPipelineAttributes,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct GitLabPipelineAttributes {
    sha: String,
    status: String,
    url: String,
    created_at: String,
    finished_at: String,
}

// ---------------------------------------------------------------------------
// 归一化
// ---------------------------------------------------------------------------

/// 上游 `normalizeGitLabMRState`：`locked` 是临时的 open 子状态 ⇒ 读作 open。
fn normalize_gitlab_mr_state(state: &str, draft: bool) -> String {
    match state {
        "merged" => "merged".into(),
        "closed" => "closed".into(),
        // opened、locked、未知值。
        _ => {
            if draft {
                "draft".into()
            } else {
                "open".into()
            }
        }
    }
}

/// 上游 `normalizeGitLabPipelineState`：`skipped` 算通过（没有失败的东西）；
/// `canceled` 是失败类终态（与 GitHub 对 cancelled 的判法一致）。
fn normalize_gitlab_pipeline_state(state: &str) -> String {
    match state {
        "success" | "skipped" => "passed".into(),
        "failed" | "canceled" => "failed".into(),
        // created / waiting_for_resource / preparing / pending / running / manual / scheduled
        _ => "pending".into(),
    }
}

/// 上游 `splitNamespace`：`"group/subgroup/repo"` → `("group/subgroup", "repo")`。
/// 子组留在 owner 里，身份才不会撞。
fn split_namespace(path: &str) -> (String, String) {
    let trimmed = path.trim_matches('/');
    match trimmed.rfind('/') {
        Some(index) => (
            trimmed[..index].to_string(),
            trimmed[index + 1..].to_string(),
        ),
        None => (String::new(), trimmed.to_string()),
    }
}

/// Go 零值 `""` → 本仓领域类型的 `None`（与 `forgejo.rs` 同款）。
fn non_empty(value: String) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// 上游 package 级 `httpClient`（15s 超时）。与 `forgejo.rs` 共用同一个实例。
fn shared_client() -> &'static reqwest::Client {
    crate::forgejo::shared_http_client()
}

/// 把 GitLab 适配器注册进 registry（M8-2 已落地本函数；anchor 期是空体）。
pub fn register(registry: &mut Registry) {
    registry.register(std::sync::Arc::new(GitLabProvider));
}

#[cfg(test)]
mod tests;
