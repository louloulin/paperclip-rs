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

use async_trait::async_trait;
use http::HeaderMap;
use mc_core::vcs::VcsProviderKind;
use serde::Deserialize;

use crate::events::{Account, CIStatusEvent, EventKind, PullRequestEvent};
use crate::forgejo::normalize_instance_url;
use crate::provider::{Provider, VcsError};
use crate::registry::Registry;
use crate::signature::verify_plaintext_token;

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

// ---------------------------------------------------------------------------
// 时间戳归一化（等价于 Go `time.Parse(layout).UTC().Format(time.RFC3339Nano)`）
// ---------------------------------------------------------------------------

/// 上游 `normalizeGitLabTime`：把 GitLab 的四种方言（以及 RFC3339 自身）统一成
/// **UTC 的 `RFC3339Nano`**；认不出的输入 ⇒ `None`（上游返回 `""`，handler 随之回落到
/// 摄入时间）。
///
/// 接受的上游 layout（逐条来自上游的 `layout` 列表）：
/// - `2006-01-02T15:04:05Z07:00`（RFC3339，含小数秒）
/// - `2006-01-02 15:04:05 MST`（GitLab 实际发的 `"2017-09-20 08:31:45 UTC"`）
/// - `2006-01-02 15:04:05 -0700`
/// - `2006-01-02 15:04:05.999999 MST`
///
/// ⚠️ 与上游的差异：命名时区只认 `UTC` / `GMT`（上游的 Go `time.Parse` 认整张 zoneinfo
/// 表）。GitLab 只会发 `UTC` 或数字偏移，而把整张 zoneinfo 表搬进本仓是没有收益的
/// 体积 ⇒ 其余命名时区返回 `None`（= 回落摄入时间，与上游的"解析失败"同路）。
pub fn normalize_gitlab_time(raw: &str) -> Option<String> {
    let parts = split_timestamp(raw)?;
    let seconds = parts.epoch_seconds()?;
    Some(format_rfc3339_nano(seconds, parts.nanos))
}

/// 拆出来的时间戳字段（全部 `i64`：公历算法里没有一处需要窄类型，`u32` 只会引来
/// `as` 截断警告 —— clippy 的 `cast_possible_truncation` 在 `-D warnings` 下是硬失败）。
struct TimestampParts {
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
    nanos: i64,
    /// 相对 UTC 的偏移秒数（东为正）。
    offset_seconds: i64,
}

impl TimestampParts {
    fn epoch_seconds(&self) -> Option<i64> {
        if !(1..=12).contains(&self.month) || !(1..=31).contains(&self.day) {
            return None;
        }
        if self.hour > 23 || self.minute > 59 || self.second > 60 {
            return None;
        }
        Some(
            days_from_civil(self.year, self.month, self.day) * 86_400
                + self.hour * 3_600
                + self.minute * 60
                + self.second
                - self.offset_seconds,
        )
    }
}

/// 手写的语法解析（`YYYY-MM-DD` + `T`/空格 + `HH:MM:SS` + 可选小数秒 + 可选时区）。
fn split_timestamp(raw: &str) -> Option<TimestampParts> {
    let raw = raw.trim();
    let (date, rest) = raw.split_at_checked(10)?;
    let rest = rest.strip_prefix(['T', ' '])?;

    let (year, month, day) = parse_date(date)?;
    let (hour, minute, second, nanos, zone_text) = parse_time(rest)?;
    let offset_seconds = parse_offset(zone_text)?;

    Some(TimestampParts {
        year,
        month,
        day,
        hour,
        minute,
        second,
        nanos,
        offset_seconds,
    })
}

fn parse_date(date: &str) -> Option<(i64, i64, i64)> {
    let bytes = date.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    Some((
        parse_digits(&date[0..4])?,
        parse_digits(&date[5..7])?,
        parse_digits(&date[8..10])?,
    ))
}

/// `HH:MM:SS[.fff][zulu]` → 各字段 + 时区文本。
fn parse_time(rest: &str) -> Option<(i64, i64, i64, i64, &str)> {
    if rest.len() < 8 || rest.as_bytes()[2] != b':' || rest.as_bytes()[5] != b':' {
        return None;
    }
    let hour = parse_digits(&rest[0..2])?;
    let minute = parse_digits(&rest[3..5])?;
    let second = parse_digits(&rest[6..8])?;

    let mut tail = &rest[8..];
    let mut nanos = 0i64;
    if let Some(fraction) = tail.strip_prefix('.') {
        let digits: String = fraction.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            return None;
        }
        // 只看前 9 位（纳秒）；多出来的位数按 Go 的 layout 规则是**非法**的，这里截断
        // 到纳秒即可（不改变 UTC 时刻的排序语义，且不会丢整秒）。
        let kept: String = digits.chars().take(9).collect();
        let padded = format!("{kept:0<9}");
        nanos = padded.parse().ok()?;
        tail = &fraction[digits.len()..];
    }
    Some((hour, minute, second, nanos, tail.trim()))
}

/// 时区文本 → 相对 UTC 的秒数。`Z` / `UTC` / `GMT` / 空串都按 0（上游的 `MST` 分支
/// 在 GitLab 的实际载荷上就是 `UTC`）。
fn parse_offset(zone: &str) -> Option<i64> {
    if zone.is_empty() || zone == "Z" || zone == "z" || zone == "UTC" || zone == "GMT" {
        return Some(0);
    }
    let bytes = zone.as_bytes();
    let sign = match bytes.first()? {
        b'+' => 1i64,
        b'-' => -1i64,
        _ => return None,
    };
    let digits: String = zone[1..].chars().filter(|c| *c != ':').collect();
    if digits.len() != 4 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let hours = parse_digits(&digits[0..2])?;
    let minutes = parse_digits(&digits[2..4])?;
    // 上游 `time.Parse` 会拒绝对不存在的偏移（`+25:00`）⇒ 这里同样拒绝，"解析失败" = `None`。
    if hours > 23 || minutes > 59 {
        return None;
    }
    Some(sign * (hours * 3_600 + minutes * 60))
}

fn parse_digits(raw: &str) -> Option<i64> {
    if raw.is_empty() || !raw.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    raw.parse().ok()
}

/// 上游 `time.RFC3339Nano`：小数秒**去掉尾随零**，零则整个省略。
fn format_rfc3339_nano(epoch_seconds: i64, nanos: i64) -> String {
    let days = epoch_seconds.div_euclid(86_400);
    let seconds_of_day = epoch_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    let mut out = format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}");
    if nanos > 0 {
        let fraction = format!("{nanos:09}");
        let trimmed = fraction.trim_end_matches('0');
        out.push('.');
        out.push_str(trimmed);
    }
    out.push('Z');
    out
}

/// 公历 → 从 1970-01-01 起的天数（Howard Hinnant 的 `days_from_civil`，公开算法）。
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400; // [0, 399]
    let month_prime = (month + 9) % 12; // [0, 11]
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1; // [0, 365]
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// 天数 → 公历（Howard Hinnant 的 `civil_from_days`，`days_from_civil` 的逆）。
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100); // [0, 365]
    let month_prime = (5 * day_of_year + 2) / 153; // [0, 11]
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1; // [1, 31]
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    const SECRET: &str = "gl-webhook-token";

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                http::HeaderName::from_bytes(name.as_bytes()).expect("header name"),
                HeaderValue::from_str(value).expect("header value"),
            );
        }
        map
    }

    /// 事件分类：两个建模事件 + 未建模。
    #[test]
    fn event_kind_reads_gitlab_event_header() {
        let p = GitLabProvider;
        assert_eq!(
            p.event_kind(&headers(&[("X-Gitlab-Event", "Merge Request Hook")])),
            EventKind::PullRequest
        );
        assert_eq!(
            p.event_kind(&headers(&[("X-Gitlab-Event", "Pipeline Hook")])),
            EventKind::CIStatus
        );
        assert_eq!(
            p.event_kind(&headers(&[("X-Gitlab-Event", "Push Hook")])),
            EventKind::Other
        );
        assert_eq!(p.event_kind(&HeaderMap::new()), EventKind::Other);
    }

    /// **明文 token 比较：正例 + 反例**（GitLab 方案）。
    #[test]
    fn plaintext_token_accepts_correct_and_rejects_others() {
        let p = GitLabProvider;
        // 正例。
        assert!(p.verify_signature(SECRET, &headers(&[("X-Gitlab-Token", SECRET)]), b"{}"));

        // 反例：差 1 位字符、长一截、空 token、缺头、空 secret。
        let mut flipped = SECRET.to_string();
        flipped.push('x');
        assert!(!p.verify_signature(SECRET, &headers(&[("X-Gitlab-Token", &flipped)]), b"{}"));
        assert!(!p.verify_signature(
            SECRET,
            &headers(&[("X-Gitlab-Token", "gl-webhook-toke")]),
            b"{}"
        ));
        assert!(!p.verify_signature(SECRET, &headers(&[("X-Gitlab-Token", "")]), b"{}"));
        assert!(!p.verify_signature(SECRET, &HeaderMap::new(), b"{}"));
        assert!(!p.verify_signature("", &headers(&[("X-Gitlab-Token", SECRET)]), b"{}"));
    }

    /// MR 载荷：owner/name 拆子组、draft 三态来源、state 归一化、时间戳归一到 RFC3339。
    #[test]
    fn parse_merge_request_maps_gitlab_shape() {
        let p = GitLabProvider;
        let body = br#"{
          "object_kind": "merge_request",
          "user": { "username": "author", "avatar_url": "https://a.test/x.png" },
          "project": { "path_with_namespace": "group/subgroup/repo" },
          "object_attributes": {
            "iid": 12, "title": "Draft: MUL-2", "description": "body",
            "state": "opened", "action": "open",
            "source_branch": "feat/y", "url": "https://gl.test/group/subgroup/repo/-/merge_requests/12",
            "created_at": "2017-09-20 08:31:45 UTC", "updated_at": "2017-09-20 08:32:45 UTC",
            "last_commit": { "id": "cafebabe" }
          }
        }"#;
        let event = p.parse_pull_request(body).expect("parse");
        assert_eq!(event.action, "open");
        assert_eq!(event.repo_owner, "group/subgroup");
        assert_eq!(event.repo_name, "repo");
        assert_eq!(event.number, 12);
        // 标题前缀 `Draft:` 也算草稿（上游的第三段判据）。
        assert_eq!(event.state, "draft");
        assert_eq!(event.branch.as_deref(), Some("feat/y"));
        assert_eq!(event.head_sha, "cafebabe");
        assert_eq!(event.author_login.as_deref(), Some("author"));
        // 时间戳已被 provider 归一化成 RFC3339（不是 GitLab 方言）。
        assert_eq!(event.created_at.as_deref(), Some("2017-09-20T08:31:45Z"));
        assert_eq!(event.updated_at.as_deref(), Some("2017-09-20T08:32:45Z"));
        assert!(!event.is_terminal());

        // `work_in_progress` 是第二段草稿判据；`locked` 读作 open。
        let wip = br#"{"object_attributes":{"state":"locked","work_in_progress":true}}"#;
        let event = p.parse_pull_request(wip).expect("parse");
        assert_eq!(event.state, "draft");
        let open_locked = br#"{"object_attributes":{"state":"locked"}}"#;
        assert_eq!(
            p.parse_pull_request(open_locked).expect("parse").state,
            "open"
        );
    }

    /// MR 终态：`merged` / `closed` 与 `action` 的终态集合。
    #[test]
    fn parse_merge_request_normalizes_terminal_states() {
        let p = GitLabProvider;
        let merged = br#"{"object_attributes":{"state":"merged","action":"merge"}}"#;
        let event = p.parse_pull_request(merged).expect("parse");
        assert_eq!(event.state, "merged");
        assert!(event.is_terminal());

        let closed = br#"{"object_attributes":{"state":"closed","action":"close"}}"#;
        let event = p.parse_pull_request(closed).expect("parse");
        assert_eq!(event.state, "closed");
        assert!(event.is_terminal());
    }

    /// pipeline 载荷：合成 context、状态三态、`finished_at` → RFC3339。
    #[test]
    fn parse_pipeline_normalizes_state_and_context() {
        let p = GitLabProvider;
        let body = br#"{
          "object_kind": "pipeline",
          "object_attributes": {
            "sha": "abc123", "status": "failed", "url": "https://gl.test/p/1",
            "created_at": "2026-09-01 00:00:00 UTC", "finished_at": "2026-09-01 00:05:00 UTC"
          }
        }"#;
        let event = p.parse_ci_status(body).expect("parse");
        assert_eq!(event.sha, "abc123");
        assert_eq!(event.context, "gitlab/pipeline");
        assert_eq!(event.state, "failed");
        assert_eq!(event.target_url.as_deref(), Some("https://gl.test/p/1"));
        assert_eq!(event.updated_at.as_deref(), Some("2026-09-01T00:05:00Z"));

        for (wire, normalized) in [
            ("success", "passed"),
            ("skipped", "passed"),
            ("failed", "failed"),
            ("canceled", "failed"),
            ("running", "pending"),
            ("manual", "pending"),
            ("wat", "pending"),
        ] {
            let body = format!(
                r#"{{"object_attributes":{{"sha":"s","status":"{wire}","created_at":"2026-09-01 00:00:00 UTC"}}}}"#
            );
            assert_eq!(
                p.parse_ci_status(body.as_bytes()).expect("parse").state,
                normalized,
                "wire status {wire}"
            );
        }
    }

    /// 非 JSON / 数组 / 类型不匹配 ⇒ `Malformed`（**不** panic）；`null` 是 no-op。
    #[test]
    fn parse_rejects_malformed_payloads() {
        let p = GitLabProvider;
        assert!(matches!(
            p.parse_pull_request(b"{oops"),
            Err(VcsError::Malformed(_))
        ));
        assert!(matches!(
            p.parse_ci_status(b"[]"),
            Err(VcsError::Malformed(_))
        ));
        assert!(matches!(
            p.parse_pull_request(b"[1]"),
            Err(VcsError::Malformed(_))
        ));
        assert!(matches!(
            p.parse_ci_status(br#"{"object_attributes":{"sha":123}}"#),
            Err(VcsError::Malformed(_))
        ));
        // `null` 放行（Go 的 no-op 语义）。
        let from_null = p.parse_ci_status(b"null").expect("null is a no-op");
        assert_eq!(from_null.sha, "");
    }

    /// 时间戳归一化的**逐条**对照（含 Go `RFC3339Nano` 的尾零裁剪与偏移换算）。
    #[test]
    fn normalize_gitlab_time_matches_go_rfc3339nano() {
        for (raw, expected) in [
            // GitLab 实际发的 MST 形态（UTC）。
            ("2017-09-20 08:31:45 UTC", "2017-09-20T08:31:45Z"),
            // 小数秒：尾零被裁掉（Go 的 RFC3339Nano 行为）。
            ("2017-09-20 08:31:45.123000 UTC", "2017-09-20T08:31:45.123Z"),
            // RFC3339 自身（`T` 分隔 + `Z`）。
            ("2026-09-01T00:00:00Z", "2026-09-01T00:00:00Z"),
            // 数字偏移：`-0700` 与 `+05:30` 都换算到 UTC。
            ("2026-09-01 00:00:00 -0700", "2026-09-01T07:00:00Z"),
            ("2026-09-01T05:30:00+05:30", "2026-09-01T00:00:00Z"),
            ("2026-01-01 00:30:00 -0100", "2026-01-01T01:30:00Z"),
            // 闰日与纪元下界（公历算法最容易错的两处）。
            ("2024-02-29 12:00:00 UTC", "2024-02-29T12:00:00Z"),
            ("1970-01-01 00:00:00 UTC", "1970-01-01T00:00:00Z"),
            ("1969-12-31 23:59:59 UTC", "1969-12-31T23:59:59Z"),
            // 纳秒精度保留（单调守卫靠它排序同一秒内的两个事件）。
            (
                "2026-09-01 00:00:00.000000123 UTC",
                "2026-09-01T00:00:00.000000123Z",
            ),
        ] {
            assert_eq!(
                normalize_gitlab_time(raw).as_deref(),
                Some(expected),
                "raw = {raw:?}"
            );
        }
    }

    /// 认不出的输入 ⇒ `None`（= 上游的 `""`，handler 回落摄入时间）。
    #[test]
    fn normalize_gitlab_time_rejects_unknown_layouts() {
        for raw in [
            "",
            "not a time",
            "2026-09-01",
            "2026-09-01 00:00:00 America/New_York",
            "2026-13-01 00:00:00 UTC",
            "2026-09-01 25:00:00 UTC",
            "2026-09-01 00:00:00 +25:00",
            "2026-09-01T00:00:00.",
        ] {
            assert_eq!(normalize_gitlab_time(raw), None, "raw = {raw:?}");
        }
    }

    /// `days_from_civil` / `civil_from_days` 互逆（含闰年前后与世纪边界）。
    #[test]
    fn civil_date_helpers_round_trip() {
        for days in [-100_000i64, -719_468, -1, 0, 1, 11_016, 20_000, 100_000] {
            let (year, month, day) = civil_from_days(days);
            assert_eq!(days_from_civil(year, month, day), days, "days = {days}");
        }
        // 锚点。
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(civil_from_days(0), (1970, 1, 1));
    }

    /// `split_namespace` 的三种形态。
    #[test]
    fn split_namespace_keeps_subgroups_in_owner() {
        assert_eq!(
            split_namespace("group/subgroup/repo"),
            ("group/subgroup".to_string(), "repo".to_string())
        );
        assert_eq!(
            split_namespace("group/repo"),
            ("group".to_string(), "repo".to_string())
        );
        assert_eq!(split_namespace("repo"), (String::new(), "repo".to_string()));
        assert_eq!(
            split_namespace("/repo/"),
            (String::new(), "repo".to_string())
        );
    }

    /// registry 的**三个检验**之二 + 之三：注册后 `gitlab` 可解析，且未注册的 kind
    /// 报**可区分**的错误（不是 panic、不是静默 `None`）。
    #[test]
    fn register_makes_gitlab_resolvable_and_unknown_kind_errors() {
        let mut registry = Registry::new();
        register(&mut registry);
        assert_eq!(registry.kinds(), vec![VcsProviderKind::GitLab]);
        assert_eq!(
            registry
                .get(VcsProviderKind::GitLab)
                .expect("gitlab")
                .kind(),
            VcsProviderKind::GitLab
        );

        // 只注册 gitlab ⇒ forgejo 未注册，`get` 必须给出 UnknownProvider（带 kind）。
        let err = registry
            .get(VcsProviderKind::Forgejo)
            .err()
            .expect("forgejo 未注册");
        assert!(matches!(
            err,
            crate::registry::RegistryError::UnknownProvider(VcsProviderKind::Forgejo)
        ));
        assert_eq!(
            err.to_string(),
            "vcs: no provider registered for kind `forgejo`"
        );
    }
}
