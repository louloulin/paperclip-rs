//! Forgejo/Gitea（wire 同一套）的 provider 适配器 —— **M8-2 已落地（`LUM-1799`）**
//! （`docs/61-M8-PLAN.md` §3.3）。
//!
//! 上游对应物是 `internal/integrations/vcs/forgejo.go`（257 行）：
//! `X-Gitea-Signature` 的 HMAC-SHA256 验签、PR / commit-status 载荷解析、
//! `/api/v1/user` 的 token 校验。本文件还承接上游同 package 的 `shared helpers` 段
//! （[`normalize_instance_url`] / `derive_pr_state` / `coalesce`）—— 上游把它们放在
//! `forgejo.go` 的文件尾，本仓照抄同一方位（`gitlab.rs` 与路由层都引用它们）。
//!
//! # 一个结构体、两个 kind（上游的 `forgejoProvider{kind}`）
//!
//! Forgejo 与它的上游 Gitea 在 wire 上**逐字相同**（同一套 `/api/v1`、同一个
//! `X-Gitea-Signature` HMAC、同一份 `pull_request` / `status` 载荷）⇒ 上游用一个
//! 结构体带 `kind` 字段，注册成**两个** key。本仓同形：[`ForgejoProvider::forgejo`] /
//! [`ForgejoProvider::gitea`]，各自进 registry（`VcsProviderKind` 的三个值都有实现）。
//!
//! # 验签（`docs/61` §2.7 第 3 条）
//!
//! `X-Gitea-Signature` 是**裸十六进制** HMAC-SHA256（`sha256=` 前缀是 GitHub 的约定，
//! 上游「容忍它」⇒ 本仓也剥前缀）。比较走 [`crate::signature::verify_hmac_sha256_hex`]
//! （`hmac::Mac::verify_slice`，内部常量时间）。
//! 空 secret 直接 `false`：HMAC 的空密钥是**可伪造**的（上游注释逐字），不是理论问题。

use std::sync::OnceLock;
use std::time::Duration;

use async_trait::async_trait;
use http::HeaderMap;
use mc_core::vcs::VcsProviderKind;
use serde::Deserialize;

use crate::events::{Account, CIStatusEvent, EventKind, PullRequestEvent};
use crate::provider::{Provider, VcsError};
use crate::registry::Registry;
use crate::signature::verify_hmac_sha256_hex;

/// Forgejo（以及 wire 兼容的 Gitea）适配器。
///
/// 带 `kind` 字段而不是两个零尺寸类型：上游就是一个 `forgejoProvider{kind}`，
/// 两份实现会把「wire 相同」这件事拆成两处需要同步的代码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForgejoProvider {
    kind: VcsProviderKind,
}

impl ForgejoProvider {
    /// 注册成 `forgejo` 的那一份。
    pub const fn forgejo() -> Self {
        Self {
            kind: VcsProviderKind::Forgejo,
        }
    }

    /// 注册成 `gitea` 的那一份（与 `forgejo` **同一份 wire 实现**）。
    pub const fn gitea() -> Self {
        Self {
            kind: VcsProviderKind::Gitea,
        }
    }
}

impl Default for ForgejoProvider {
    /// 缺省 = `forgejo`（上游 `init()` 先注册的那个）。
    fn default() -> Self {
        Self::forgejo()
    }
}

#[async_trait]
impl Provider for ForgejoProvider {
    fn kind(&self) -> VcsProviderKind {
        self.kind
    }

    /// 上游 `EventKind`：`X-Gitea-Event`，缺省回落到 `X-GitHub-Event`（Gitea 会同时发这个头）。
    /// 不建模的一律 [`EventKind::Other`]（**确认但忽略**，不是错误）。
    fn event_kind(&self, headers: &HeaderMap) -> EventKind {
        let event = header_str(headers, "x-gitea-event")
            .or_else(|| header_str(headers, "x-github-event"))
            .unwrap_or_default();
        match event {
            "pull_request" => EventKind::PullRequest,
            "status" => EventKind::CIStatus,
            _ => EventKind::Other,
        }
    }

    /// `X-Gitea-Signature`（裸十六进制；容忍 `sha256=` 前缀）。
    fn verify_signature(&self, secret: &str, headers: &HeaderMap, body: &[u8]) -> bool {
        // 空密钥的 HMAC 是**可伪造**的 ⇒ 直接拒（上游逐字注释；密钥总是 32 随机字节，
        // 所以这条今天不可达，但鉴权边界不该依赖"上游还会继续这么做"）。
        if secret.is_empty() {
            return false;
        }
        let Some(raw) = header_str(headers, "x-gitea-signature") else {
            return false;
        };
        verify_hmac_sha256_hex(secret, body, raw.trim())
    }

    fn parse_pull_request(&self, body: &[u8]) -> Result<PullRequestEvent, VcsError> {
        let decoded: ForgejoPullRequestPayload = decode_payload(body)?;
        let pull = decoded.pull_request;
        let repository = decoded.repository;

        // owner：`owner.username` → `owner.login` → `full_name` 的前半段（上游三段回落）。
        let mut owner = coalesce(repository.owner.username, repository.owner.login);
        if owner.is_empty() {
            if let Some(index) = repository.full_name.find('/') {
                if index > 0 {
                    owner = repository.full_name[..index].to_string();
                }
            }
        }

        Ok(PullRequestEvent {
            action: decoded.action,
            repo_owner: owner,
            repo_name: repository.name,
            number: pull.number,
            title: pull.title,
            body: pull.body,
            state: derive_pr_state(&pull.state, pull.draft, pull.merged),
            html_url: pull.html_url,
            branch: non_empty(pull.head.ref_),
            head_sha: pull.head.sha,
            author_login: non_empty(coalesce(pull.user.username, pull.user.login)),
            author_avatar_url: non_empty(pull.user.avatar_url),
            additions: pull.additions,
            deletions: pull.deletions,
            changed_files: pull.changed_files,
            merged_at: non_empty(pull.merged_at),
            closed_at: non_empty(pull.closed_at),
            created_at: non_empty(pull.created_at),
            updated_at: non_empty(pull.updated_at),
        })
    }

    fn parse_ci_status(&self, body: &[u8]) -> Result<CIStatusEvent, VcsError> {
        let decoded: ForgejoStatusPayload = decode_payload(body)?;
        // 优先 status 自己的 `updated_at`（RFC3339），回落 `created_at`，再回落空串
        // （handler 用摄入时间）—— 这三段让 commit-status 的**单调守卫**是真的。
        let updated_at = if decoded.updated_at.is_empty() {
            decoded.created_at
        } else {
            decoded.updated_at
        };
        Ok(CIStatusEvent {
            sha: decoded.sha,
            context: decoded.context,
            state: normalize_forgejo_state(&decoded.state),
            target_url: non_empty(decoded.target_url),
            description: non_empty(decoded.description),
            updated_at: non_empty(updated_at),
        })
    }

    /// `GET {instance}/api/v1/user`，`Authorization: token <token>`。
    /// 401/403 ⇒ [`VcsError::Unauthorized`]（唯一的哨兵，让调用侧把"token 被拒"与
    /// "实例不可达"分开）；其它非 2xx 与解码失败 ⇒ `Instance` / `Malformed`。
    async fn validate_token(&self, instance_url: &str, token: &str) -> Result<Account, VcsError> {
        let endpoint = format!("{}/api/v1/user", normalize_instance_url(instance_url));
        let response = shared_http_client()
            .get(&endpoint)
            .header(reqwest::header::AUTHORIZATION, format!("token {token}"))
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| VcsError::Instance(format!("forgejo: request: {e}")))?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            // 上游在这里把状态码 + body 片段写进 slog（便于区分 401 = token 坏 /
            // 403 = scope 不够）。本仓**不打 body**：它可能回显 token 相关的服务端提示，
            // 而 `docs/61` §2.4 的纪律是「错误路径不回显凭据」⇒ 只留状态码。
            tracing::warn!(endpoint = %endpoint, status = status.as_u16(), "vcs: forgejo token rejected");
            return Err(VcsError::Unauthorized);
        }
        if !status.is_success() {
            return Err(VcsError::Instance(format!(
                "forgejo: GET /user: status {}",
                status.as_u16()
            )));
        }

        let user: ForgejoUser = response
            .json()
            .await
            .map_err(|e| VcsError::Malformed(format!("forgejo: decode user: {e}")))?;
        let login = coalesce(user.login, user.username);
        if login.is_empty() {
            return Err(VcsError::Malformed(
                "forgejo: user response missing login".into(),
            ));
        }
        Ok(Account {
            login,
            kind: self.kind,
        })
    }
}

// ---------------------------------------------------------------------------
// 载荷形状（上游 `fjPullRequestPayload` / `fjStatusPayload`，逐字段同名）
// ---------------------------------------------------------------------------

/// 上游 `fjPullRequestPayload`。
///
/// ⚠️ **容器级** `#[serde(default)]`（每个结构体一条）：上游的 `json.Unmarshal` 对**缺字段**不报错，
/// 而 serde 默认要求字段存在 ⇒ 不加 default 会把上游能解析的载荷判成 `Malformed`
/// （那是**收窄**，不是照搬）。类型不匹配仍然报错，与上游一致。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ForgejoPullRequestPayload {
    action: String,
    pull_request: ForgejoPullRequest,
    repository: ForgejoRepository,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ForgejoPullRequest {
    number: i32,
    title: String,
    body: String,
    state: String,
    merged: bool,
    draft: bool,
    html_url: String,
    additions: i32,
    deletions: i32,
    changed_files: i32,
    merged_at: String,
    closed_at: String,
    created_at: String,
    updated_at: String,
    user: ForgejoUserWithAvatar,
    head: ForgejoHead,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ForgejoUser {
    login: String,
    username: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ForgejoUserWithAvatar {
    login: String,
    username: String,
    avatar_url: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ForgejoHead {
    #[serde(default, rename = "ref")]
    ref_: String,
    sha: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ForgejoRepository {
    name: String,
    full_name: String,
    owner: ForgejoOwner,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ForgejoOwner {
    login: String,
    username: String,
}

/// 上游 `fjStatusPayload`。
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ForgejoStatusPayload {
    sha: String,
    context: String,
    state: String,
    target_url: String,
    description: String,
    created_at: String,
    updated_at: String,
}

// ---------------------------------------------------------------------------
// 上游同 package 的 shared helpers（`forgejo.go` 文件尾）
// ---------------------------------------------------------------------------

/// 上游 `NormalizeInstanceURL`：trim 空白 + 去尾斜杠，让存储值与派生的 webhook URL
/// 与输入形态无关。
///
/// 位置照抄上游（`forgejo.go` 的 shared 段）：`gitlab.rs` 与路由层都用它，
/// 但它是 provider 无关的 ⇒ **不要**在别处再写一份。
pub fn normalize_instance_url(raw: &str) -> String {
    raw.trim().trim_end_matches('/').to_string()
}

/// 上游 `derivePRState`：把 `(state, draft, merged)` 收成归一化四值。
///
/// 顺序是承重的：`merged` 先于 `state == "closed"`（合并的 PR 在 wire 上是 `closed`）。
fn derive_pr_state(state: &str, draft: bool, merged: bool) -> String {
    if merged {
        return "merged".into();
    }
    if state == "closed" {
        return "closed".into();
    }
    if draft {
        return "draft".into();
    }
    "open".into()
}

/// 上游 `normalizeForgejoState`：`warning` 算**通过**（它不阻塞），与 GitHub 把
/// neutral/skipped 算 passed 同判。
fn normalize_forgejo_state(state: &str) -> String {
    match state {
        "success" | "warning" => "passed".into(),
        "failure" | "error" => "failed".into(),
        // pending + 一切未知值。
        _ => "pending".into(),
    }
}

/// 上游 `coalesce(a, b)`：取第一个非空。
fn coalesce(first: String, second: String) -> String {
    if first.is_empty() {
        second
    } else {
        first
    }
}

/// Go 的零值是 `""`，本仓的领域类型用 `Option<String>` 表达"无" —— 这一层把它翻过去。
fn non_empty(value: String) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

/// 取头（`HeaderMap` 的键是小写）。
fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// 载荷解码的**形状前置**：JSON 对象（或 `null`）才可解。
///
/// 为什么不能只靠 serde：两个 adapter 的载荷结构体都带**容器级** `#[serde(default)]`
/// （上游 Go 的 `json.Unmarshal` 对缺字段不报错，serde 默认却要求字段存在）。那个属性有一个
/// 副作用：serde 为结构体派生的 `visit_seq` 在容器级 default 下不再要求元素个数 ⇒ `[]`
/// 会被解成 `Default::default()`。**Go 对数组进结构体是报错的** ⇒ 这里显式拦回。
///
/// `null` 是 **no-op**（Go 的 `json.Unmarshal(body, &struct)` 语义）⇒ 解成 `Default`。
pub(crate) fn decode_payload<T>(body: &[u8]) -> Result<T, VcsError>
where
    T: serde::de::DeserializeOwned + Default,
{
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| VcsError::Malformed(e.to_string()))?;
    if value.is_null() {
        // serde 的派生实现会报 `invalid type: null` ⇒ 这一支手动对齐 Go。
        return Ok(T::default());
    }
    if !value.is_object() {
        return Err(VcsError::Malformed("payload is not a JSON object".into()));
    }
    serde_json::from_value(value).map_err(|e| VcsError::Malformed(e.to_string()))
}

/// 上游 `var httpClient = &http.Client{Timeout: 15 * time.Second}`（package 级，建一次）。
///
/// `pub(crate)`：`gitlab.rs` 复用同一个实例（上游两个 adapter 也共用一个 package 级 client）。
pub(crate) fn shared_http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

/// 把 Forgejo 适配器注册进 registry。
///
/// **两个 kind 一份 wire 实现**（上游 `init()` 逐字）：`forgejo` 与 `gitea` 各注一份，
/// 于是"provider 标签不同、行为相同"。M8-2 已落地本函数（anchor 期是空体）。
pub fn register(registry: &mut Registry) {
    registry.register(std::sync::Arc::new(ForgejoProvider::forgejo()));
    registry.register(std::sync::Arc::new(ForgejoProvider::gitea()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signature::hmac_sha256_hex;
    use http::HeaderValue;

    const SECRET: &str = "fj-webhook-secret";

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

    fn provider() -> ForgejoProvider {
        ForgejoProvider::forgejo()
    }

    /// Forgejo/Gitea 的三种事件分类 + 回落头 + 未建模事件。
    #[test]
    fn event_kind_reads_gitea_then_github_header() {
        let p = provider();
        assert_eq!(
            p.event_kind(&headers(&[("X-Gitea-Event", "pull_request")])),
            EventKind::PullRequest
        );
        assert_eq!(
            p.event_kind(&headers(&[("X-Gitea-Event", "status")])),
            EventKind::CIStatus
        );
        // Gitea 也会发 X-GitHub-Event（上游逐字）。
        assert_eq!(
            p.event_kind(&headers(&[("X-GitHub-Event", "pull_request")])),
            EventKind::PullRequest
        );
        // 未建模 ⇒ 确认但忽略。
        assert_eq!(
            p.event_kind(&headers(&[("X-Gitea-Event", "issue_comment")])),
            EventKind::Other
        );
        assert_eq!(p.event_kind(&HeaderMap::new()), EventKind::Other);
    }

    /// **签名正例 + 反例**（`docs/61` §6.5 的 M8-2 行：每方案各 1 正 1 反）。
    #[test]
    fn hmac_signature_accepts_correct_and_rejects_tampered() {
        let p = provider();
        let body = br#"{"action":"opened"}"#;
        let good = hmac_sha256_hex(SECRET, body);

        // 正例：裸十六进制。
        assert!(p.verify_signature(SECRET, &headers(&[("X-Gitea-Signature", &good)]), body));
        // 上游"容忍" sha256= 前缀（那是 GitHub 的约定）。
        assert!(p.verify_signature(
            SECRET,
            &headers(&[("X-Gitea-Signature", &format!("sha256={good}"))]),
            body
        ));

        // 反例一：签名差 1 位。
        let mut flipped = good.clone();
        let tail = flipped.pop().expect("hex");
        flipped.push(if tail == '0' { '1' } else { '0' });
        assert!(!p.verify_signature(SECRET, &headers(&[("X-Gitea-Signature", &flipped)]), body));
        // 反例二：body 差 1 字节（签名仍然格式合法）。
        let mut other = body.to_vec();
        other.push(b' ');
        assert!(!p.verify_signature(SECRET, &headers(&[("X-Gitea-Signature", &good)]), &other));
        // 反例三：换密钥。
        assert!(!p.verify_signature(
            "other-secret",
            &headers(&[("X-Gitea-Signature", &good)]),
            body
        ));
        // 反例四：缺头 / 空 secret / 非法十六进制 ⇒ 一律 false（**不** panic）。
        assert!(!p.verify_signature(SECRET, &HeaderMap::new(), body));
        assert!(!p.verify_signature("", &headers(&[("X-Gitea-Signature", &good)]), body));
        assert!(!p.verify_signature(SECRET, &headers(&[("X-Gitea-Signature", "zzzz")]), body));
    }

    /// PR 载荷：三段 owner 回落 + `username` 优先 + draft/merged 的 state 归一化。
    #[test]
    fn parse_pull_request_maps_forgejo_shape() {
        let p = provider();
        let body = br#"{
          "action": "opened",
          "pull_request": {
            "number": 7, "title": "LUM-1 fix", "body": "Closes LUM-1",
            "state": "open", "draft": true, "merged": false,
            "html_url": "https://git.test/acme/repo/pulls/7",
            "additions": 3, "deletions": 1, "changed_files": 2,
            "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-01T00:01:00Z",
            "user": { "login": "fallback", "username": "author", "avatar_url": "https://a.test/x.png" },
            "head": { "ref": "feat/x", "sha": "deadbeef" }
          },
          "repository": {
            "name": "repo", "full_name": "acme/repo",
            "owner": { "login": "acme-org", "username": "acme" }
          }
        }"#;
        let event = p.parse_pull_request(body).expect("parse");
        assert_eq!(event.action, "opened");
        assert_eq!(event.repo_owner, "acme");
        assert_eq!(event.repo_name, "repo");
        assert_eq!(event.number, 7);
        assert_eq!(event.state, "draft");
        assert_eq!(event.branch.as_deref(), Some("feat/x"));
        assert_eq!(event.head_sha, "deadbeef");
        assert_eq!(event.author_login.as_deref(), Some("author"));
        assert_eq!(event.changed_files, 2);
        assert!(!event.is_terminal());

        // owner 两段回落：没有 username 时用 login。
        let login_only = br#"{"repository":{"name":"r","owner":{"login":"acme-login"}}}"#;
        assert_eq!(
            p.parse_pull_request(login_only).expect("parse").repo_owner,
            "acme-login"
        );
        // owner 全缺 ⇒ full_name 的前半段。
        let full_name_only = br#"{"repository":{"name":"r","full_name":"acme/full"}}"#;
        assert_eq!(
            p.parse_pull_request(full_name_only)
                .expect("parse")
                .repo_owner,
            "acme"
        );
    }

    /// `merged=true` 的 wire state 是 `closed`，但归一化必须是 `merged`
    /// （`derivePRState` 的**顺序**是承重的）。
    #[test]
    fn parse_pull_request_normalizes_terminal_states() {
        let p = provider();
        let merged = br#"{"action":"closed","pull_request":{"state":"closed","merged":true}}"#;
        let event = p.parse_pull_request(merged).expect("parse");
        assert_eq!(event.state, "merged");
        assert!(event.is_terminal());

        let closed = br#"{"action":"closed","pull_request":{"state":"closed","merged":false}}"#;
        assert_eq!(p.parse_pull_request(closed).expect("parse").state, "closed");

        let open = br#"{"action":"opened","pull_request":{"state":"open"}}"#;
        assert_eq!(p.parse_pull_request(open).expect("parse").state, "open");
    }

    /// 非 JSON 载荷 / 非对象载荷 ⇒ [`VcsError::Malformed`]（**不** panic、**不**静默 None）。
    #[test]
    fn parse_rejects_malformed_payloads() {
        let p = provider();
        assert!(matches!(
            p.parse_pull_request(b"not json"),
            Err(VcsError::Malformed(_))
        ));
        // 类型不匹配也算 Malformed（上游 json.Unmarshal 同样报错）。
        assert!(matches!(
            p.parse_pull_request(br#"{"pull_request":{"number":"seven"}}"#),
            Err(VcsError::Malformed(_))
        ));
        assert!(matches!(
            p.parse_ci_status(b"["),
            Err(VcsError::Malformed(_))
        ));
        // 数组进结构体：Go 的 `json.Unmarshal` 报错 ⇒ 本仓也必须报错（容器级 default 会把它
        // 解成默认值，所以 `decode_payload` 里有显式的对象形状前置）。
        assert!(matches!(
            p.parse_pull_request(b"[]"),
            Err(VcsError::Malformed(_))
        ));
        assert!(matches!(
            p.parse_ci_status(b"[1,2]"),
            Err(VcsError::Malformed(_))
        ));
        // `null` 放行（Go 对 null 是 no-op ⇒ 全零值载荷）：repo owner / number 全空，
        // 由 handler 的身份守卫丢掉。
        let from_null = p.parse_pull_request(b"null").expect("null is a no-op");
        assert_eq!(from_null.repo_owner, "");
        assert_eq!(from_null.number, 0);
    }

    /// commit-status：五个 wire 状态 → 三态；`updated_at` 的两级回落。
    #[test]
    fn parse_ci_status_normalizes_state_and_time() {
        let p = provider();
        let cases = [
            ("success", "passed"),
            ("warning", "passed"),
            ("failure", "failed"),
            ("error", "failed"),
            ("pending", "pending"),
            ("wat", "pending"),
        ];
        for (wire, normalized) in cases {
            let body = format!(
                r#"{{"sha":"abc","context":"ci","state":"{wire}","updated_at":"2026-09-01T00:00:00Z"}}"#
            );
            let event = p.parse_ci_status(body.as_bytes()).expect("parse");
            assert_eq!(event.state, normalized, "wire state {wire}");
            assert_eq!(event.sha, "abc");
            assert_eq!(event.context, "ci");
            assert_eq!(event.updated_at.as_deref(), Some("2026-09-01T00:00:00Z"));
        }

        // updated_at 缺 ⇒ 回落 created_at；两者都缺 ⇒ None（handler 用摄入时间）。
        let fallback = br#"{"sha":"abc","state":"success","created_at":"2026-09-02T00:00:00Z"}"#;
        assert_eq!(
            p.parse_ci_status(fallback)
                .expect("parse")
                .updated_at
                .as_deref(),
            Some("2026-09-02T00:00:00Z")
        );
        let none = br#"{"sha":"abc","state":"success"}"#;
        assert!(p.parse_ci_status(none).expect("parse").updated_at.is_none());
    }

    /// `normalize_instance_url`：trim + 去尾斜杠（`gitlab.rs` 与路由层共用）。
    #[test]
    fn normalize_instance_url_trims_and_strips_trailing_slashes() {
        assert_eq!(
            normalize_instance_url("  https://git.test/  "),
            "https://git.test"
        );
        assert_eq!(
            normalize_instance_url("https://git.test"),
            "https://git.test"
        );
        assert_eq!(normalize_instance_url(""), "");
    }

    /// registry 的**三个检验**之一：forgejo/gitea 两个实现拿到的是同一份 wire 行为。
    #[test]
    fn register_populates_both_forgejo_and_gitea() {
        let mut registry = Registry::new();
        register(&mut registry);
        assert_eq!(registry.len(), 2);
        assert_eq!(
            registry.kinds(),
            vec![VcsProviderKind::Forgejo, VcsProviderKind::Gitea]
        );
        let forgejo = registry.get(VcsProviderKind::Forgejo).expect("forgejo");
        let gitea = registry.get(VcsProviderKind::Gitea).expect("gitea");
        assert_eq!(forgejo.kind(), VcsProviderKind::Forgejo);
        assert_eq!(gitea.kind(), VcsProviderKind::Gitea);
        // wire 行为相同：同一个 body 的分类与解析结果一致。
        let body = br#"{"action":"opened","pull_request":{"number":1,"state":"open"}}"#;
        assert_eq!(
            forgejo.parse_pull_request(body).expect("fj"),
            gitea.parse_pull_request(body).expect("gt")
        );
    }
}
