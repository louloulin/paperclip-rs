//! GitHub 面的响应 DTO（上游 `githubInstallationToResponse` / `githubPullRequestToResponse`，
//! `github.go` L54–L330）。
//!
//! ⚠️ 本文件的写者是 **M8-1**，**M8-4 只读**（`docs/61` §1.2 的写者/读者两列）：两片共用
//! 这一份响应映射，不得各造一份。
//!
//! # write-only / 脱敏
//!
//! DTO 里**不得**出现 installation token、App 私钥、webhook secret
//! （`docs/61` §2.4 的四条判据）。`installation_id` 是 GitHub 的公开数字标识，但对
//! **非 admin 成员**要按上游口径**整个字段缺席**（它是 Connect/Disconnect 的管理手柄）⇒
//! 用 `Option<i64>` + `skip_serializing_if`，不是 `null`。
//!
//! # 相对 anchor 暂定形状的三处修订（`docs/32` §9.12 有登记）
//!
//! anchor（M8-0）落的是「完整形状，切片只填函数体」，M8-1 逐条对齐上游**字面字段名**：
//!
//! | 条目 | anchor 暂定 | 本片（= 上游） | 依据 |
//! | --- | --- | --- | --- |
//! | connect 响应 | `install_url: Option<String>` | `url: String` | `GitHubConnectResponse{URL,Configured}`（`github.go:170`） |
//! | installation 响应 | 无 `workspace_id`、有 `connected_by_id`、`installation_id: i64` | 有 `workspace_id`、无 `connected_by_id`、`installation_id: Option<i64>` | `github.go:54-70` + 角色门（`github.go:742`） |
//! | repository 响应 | `name` / `owner` / `default_branch: Option` | `clone_url` / `archived` / `default_branch: String`（**无** `name`/`owner`） | `github.go:175-184` |
//!
//! `GithubPullRequestResponse` **保持 anchor 的暂定形状**（只覆盖 `github_pull_request`
//! 的基础列）：上游那个形状的其余字段（snapshot / checks / additions）要读 091/092/222
//! 三张迁移加出来的列与 `mc_core::github::GitHubPullRequest`（anchor 冻结），属 **M8-4 的
//! 填充面** —— M8-1 不越界，登记为 M8-4 的延伸点。

use serde::{Deserialize, Serialize};

use mc_repos::github::installation::GithubInstallationRow;

/// `GET /api/workspaces/{id}/github/installations` 的单条（上游 `githubInstallationToResponse`）。
///
/// `installation_id` **按角色缺席**：admin/owner 拿到管理手柄，其余角色拿到
/// `null`（序列化时整个字段缺席），前端据此隐藏 Connect/Disconnect。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubInstallationResponse {
    pub id: String,
    pub workspace_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installation_id: Option<i64>,
    pub account_login: String,
    pub account_type: String,
    pub account_avatar_url: Option<String>,
    pub created_at: String,
}

impl GithubInstallationResponse {
    /// 上游 `githubInstallationToResponse`（**含** `installation_id`）。
    pub fn from_row(row: &GithubInstallationRow) -> Self {
        Self {
            id: row.id.to_string(),
            workspace_id: row.workspace_id.to_string(),
            installation_id: Some(row.installation_id),
            account_login: row.account_login.clone(),
            account_type: row.account_type.clone(),
            account_avatar_url: row.account_avatar_url.clone(),
            created_at: row.created_at.to_rfc3339(),
        }
    }

    /// 非 admin 成员的视图：`installation_id` 整段缺席。
    #[must_use]
    pub fn without_installation_id(mut self) -> Self {
        self.installation_id = None;
        self
    }
}

/// `GET /api/workspaces/{id}/github/installations` 的整体响应（上游 `github.go:736-741`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubInstallationsResponse {
    pub installations: Vec<GithubInstallationResponse>,
    /// 「能连接」判据（App slug + webhook secret）—— 与 `can_manage` 正交。
    pub configured: bool,
    /// 「能浏览仓库」判据（App id + 私钥）—— **独立**于 `configured`。
    pub repository_browse_configured: bool,
    /// 调用者是否 owner/admin（管理手柄在不在本轮响应里）。
    pub can_manage: bool,
}

/// `GET /api/workspaces/{id}/github/connect` 的响应（上游 `GitHubConnectResponse`）。
///
/// 未配置 ⇒ **200 + `configured:false` + `url:""`**（不是 403/503：前端据此隐藏按钮，
/// `docs/61` §2.5 的 connect 行）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubConnectResponse {
    /// 可直接跳转的安装引导 URL；未配置时是**空串**（上游零值）。
    pub url: String,
    pub configured: bool,
}

/// `GET .../installations/{installationId}/repositories` 的单条（上游 `GitHubRepositoryResponse`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubRepositoryResponse {
    pub id: i64,
    pub full_name: String,
    pub html_url: String,
    pub clone_url: String,
    pub description: Option<String>,
    pub private: bool,
    pub archived: bool,
    pub default_branch: String,
}

/// `GET .../repositories` 的分页信封（上游 `GitHubRepositoriesResponse`）。
///
/// `next_page` **没有** `omitempty`（上游是 `*int` + 无 omitempty）⇒ 无下一页时是 `null`，
/// 不是缺席。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubRepositoriesResponse {
    pub repositories: Vec<GithubRepositoryResponse>,
    pub total_count: i64,
    pub next_page: Option<u32>,
}

/// `GET /api/issues/{id}/pull-requests` 的单条（上游 `githubPullRequestToResponse` 的
/// **基础列子集**，见模块头的边界说明）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GithubPullRequestResponse {
    pub id: String,
    pub repo_owner: String,
    pub repo_name: String,
    pub pr_number: i32,
    pub title: String,
    pub state: String,
    pub html_url: String,
    pub branch: Option<String>,
    pub author_login: Option<String>,
    pub author_avatar_url: Option<String>,
    pub merged_at: Option<String>,
    pub closed_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 分页参数解析的**失败**（上游 `writeError(400, "invalid "+name)`）。
///
/// `Display` 逐字给出上游的文案（`invalid page` / `invalid per_page`），路由层直接把它
/// 当 400 的 message —— 不另起一套措辞。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GithubPageParamError {
    #[error("invalid page")]
    Page,
    #[error("invalid per_page")]
    PerPage,
}

/// 分页参数解析的结果（上游 `parseGitHubPageParam` 的边界，M8-1 的 `DoD` 点名）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GithubPageParam {
    /// 已归一化的页号（≥1）。
    pub page: u32,
    /// 已归一化的每页条数（≤100）。
    pub per_page: u32,
}

impl GithubPageParam {
    /// 上游 `page` 的默认值 / 上下界（`1, 1, 100000`）。
    pub const DEFAULT_PAGE: u32 = 1;
    pub const MIN_PAGE: u32 = 1;
    pub const MAX_PAGE: u32 = 100_000;
    /// 上游 `per_page` 的默认值 / 上下界（`100, 1, 100`）。
    pub const DEFAULT_PER_PAGE: u32 = 100;
    pub const MIN_PER_PAGE: u32 = 1;
    pub const MAX_PER_PAGE: u32 = 100;

    /// 从 query 的两个可选字符串解析（上游 `parseGitHubPageParam(w, r, name, default, min, max)`）。
    ///
    /// 逐字语义：
    /// - **trim 之后为空**（含未传）⇒ 取默认值（`page=1` / `per_page=100`）；
    /// - 非十进制整数、或落在 `[min, max]` 之外 ⇒ **400**（`invalid page` / `invalid per_page`），
    ///   **不是**「归一到安全值」—— 上游对这两种情形一律 400，静默归一会让客户端的
    ///   分页 bug 变成沉默的错页。
    /// - 解析的是**整数**（`strconv.Atoi`）：`"1.5"` / `"1e3"` / `" 7 "`（trim 后 `7`）分别
    ///   是 400 / 400 / 7。
    ///
    /// # Errors
    ///
    /// 非十进制整数或越界 ⇒ [`GithubPageParamError`]。
    pub fn parse(page: Option<&str>, per_page: Option<&str>) -> Result<Self, GithubPageParamError> {
        Ok(Self {
            page: Self::parse_one(
                page,
                Self::DEFAULT_PAGE,
                Self::MIN_PAGE,
                Self::MAX_PAGE,
                GithubPageParamError::Page,
            )?,
            per_page: Self::parse_one(
                per_page,
                Self::DEFAULT_PER_PAGE,
                Self::MIN_PER_PAGE,
                Self::MAX_PER_PAGE,
                GithubPageParamError::PerPage,
            )?,
        })
    }

    fn parse_one(
        raw: Option<&str>,
        default: u32,
        min: u32,
        max: u32,
        on_error: GithubPageParamError,
    ) -> Result<u32, GithubPageParamError> {
        let raw = raw.map_or("", str::trim);
        if raw.is_empty() {
            return Ok(default);
        }
        // `strconv.Atoi`：只收可选的 `+`/`-` 前缀 + 十进制数字；`-1` 解析成功但越界 ⇒ 400。
        let value: i64 = raw.parse().map_err(|_| on_error.clone())?;
        if value < i64::from(min) || value > i64::from(max) {
            return Err(on_error);
        }
        u32::try_from(value).map_err(|_| on_error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc_core::{Id, Timestamp};

    fn row() -> GithubInstallationRow {
        GithubInstallationRow {
            id: Id::new().0,
            workspace_id: Id::new().0,
            installation_id: 42,
            account_login: "acme".into(),
            account_type: "Organization".into(),
            account_avatar_url: Some("https://avatars.example/acme.png".into()),
            connected_by_id: None,
            created_at: Timestamp::now().as_datetime(),
            updated_at: Timestamp::now().as_datetime(),
        }
    }

    #[test]
    fn installation_response_hides_the_management_handle_when_asked() {
        let full = GithubInstallationResponse::from_row(&row());
        let json = serde_json::to_value(&full).unwrap();
        assert_eq!(json["installation_id"], 42);
        assert!(json.get("workspace_id").is_some());
        assert!(
            json.get("connected_by_id").is_none(),
            "上游没有这个响应字段"
        );

        let stripped = full.without_installation_id();
        let json = serde_json::to_value(&stripped).unwrap();
        assert!(
            json.get("installation_id").is_none(),
            "非 admin 视图里该字段必须**缺席**（不是 null）"
        );
        assert_eq!(json["account_login"], "acme");
    }

    #[test]
    fn connect_response_is_url_plus_configured() {
        let unconfigured = GithubConnectResponse {
            url: String::new(),
            configured: false,
        };
        assert_eq!(
            serde_json::to_value(&unconfigured).unwrap(),
            serde_json::json!({"url": "", "configured": false})
        );
    }

    #[test]
    fn repositories_envelope_keeps_next_page_as_null_when_absent() {
        let body = GithubRepositoriesResponse {
            repositories: vec![],
            total_count: 0,
            next_page: None,
        };
        let json = serde_json::to_value(&body).unwrap();
        assert!(
            json.get("next_page").is_some(),
            "上游无 omitempty ⇒ null 必须在"
        );
        assert!(json["next_page"].is_null());
    }

    #[test]
    fn page_param_defaults_and_boundaries() {
        // 缺失 / 空 / 纯空白 ⇒ 默认值。
        assert_eq!(
            GithubPageParam::parse(None, None).unwrap(),
            GithubPageParam {
                page: 1,
                per_page: 100
            }
        );
        assert_eq!(
            GithubPageParam::parse(Some(""), Some("   ")).unwrap(),
            GithubPageParam {
                page: 1,
                per_page: 100
            }
        );
        // trim 后是整数。
        assert_eq!(
            GithubPageParam::parse(Some(" 7 "), Some("25"))
                .unwrap()
                .page,
            7
        );
        // 边界：page ∈ [1, 100000]、per_page ∈ [1, 100]。
        assert_eq!(
            GithubPageParam::parse(Some("1"), Some("1"))
                .unwrap()
                .per_page,
            1
        );
        assert_eq!(
            GithubPageParam::parse(Some("100000"), Some("100"))
                .unwrap()
                .page,
            100_000
        );
        // 越界（含下界与上界的两侧）。
        assert_eq!(
            GithubPageParam::parse(Some("0"), None),
            Err(GithubPageParamError::Page)
        );
        assert_eq!(
            GithubPageParam::parse(Some("100001"), None),
            Err(GithubPageParamError::Page)
        );
        assert_eq!(
            GithubPageParam::parse(Some("-1"), None),
            Err(GithubPageParamError::Page)
        );
        assert_eq!(
            GithubPageParam::parse(None, Some("0")),
            Err(GithubPageParamError::PerPage)
        );
        assert_eq!(
            GithubPageParam::parse(None, Some("101")),
            Err(GithubPageParamError::PerPage)
        );
        // 非整数：`strconv.Atoi` 拒收（`1.5` / `1e3` / `一`）。
        assert_eq!(
            GithubPageParam::parse(Some("1.5"), None),
            Err(GithubPageParamError::Page)
        );
        assert_eq!(
            GithubPageParam::parse(Some("1e3"), None),
            Err(GithubPageParamError::Page)
        );
        // 两个都错时先报 page（上游按调用顺序短路）。
        assert_eq!(
            GithubPageParam::parse(Some("x"), Some("y")),
            Err(GithubPageParamError::Page)
        );
        // 错误文案逐字是上游那两句。
        assert_eq!(GithubPageParamError::Page.to_string(), "invalid page");
        assert_eq!(
            GithubPageParamError::PerPage.to_string(),
            "invalid per_page"
        );
    }
}
