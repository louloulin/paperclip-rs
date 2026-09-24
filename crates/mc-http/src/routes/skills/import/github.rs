//! github.com / skills.sh 的取件面（`routes/skills/import.rs` 的兄弟文件）。
//!
//! - **写者**：M6-3（`docs/57` §3.2；拆分登记在 `docs/32` §9 的文件→写者表）。
//! - **上游**：`internal/handler/skill.go` 的 `fetchFromGitHub` / `fetchFromSkillsSh` /
//!   `resolveGitHubRefAndPath` / `fetchGitHubTree` 一族。
//! - **布局**：本文件 = api.github.com 面 + 两条入口；`github/tree.rs` = 递归 tree 面上的
//!   「skill 目录解析 + 支持文件并发下载」。上游这一段 ~1000 行，单文件会撞门 ⑩ 的 800 行硬限。
//! - **本仓约定**：一切 HTTP 都走 `super::fetch` 的 `SourceEndpoints`（可被测试覆写成假服务器）；
//!   上限（1 MiB / 8 MiB / 256 条）来自 `mc_skill::archive` 的常量，**不在这里再写一份数字**。
//!
//! ## 有意偏离（`docs/32` §9.6）
//!
//! 上游在「tree 拿不到 / tree 被 GitHub 截断」时会回落到**逐目录 contents API 爬取**
//! （`addSupportingFilesViaCrawl` + `listGitHubSkillMdPaths`），必要时还会把一个「看起来
//! 合法但少了支持文件」的包当成成功落库。本仓**不移植这些回落**：tree 失败或截断 ⇒ **503 可重试**
//! （与上游 `errImportSourceUnavailable` 分支同义 —— 上游自己的注释就说「宁可可重试失败，
//! 也不要存错误的包」，且爬取本身受同一波限流影响）。代价是「超大仓库 + 无限流的 GitHub」
//! 从「可能侥幸成功」变成「明确 503」；换到的是「任何一次成功导入都带完整支持文件」。
//! W8（真 GitHub 客户端）可以补回爬取。

use mc_skill::archive::ImportError;
use mc_skill::frontmatter::parse_skill_frontmatter;
use mc_skill::source::{parse_github_url, parse_skills_sh_parts, GitHubSpec};

use super::fetch::{
    build_raw_github_url, fetch_raw_file, raw_github_prefix, FetchedSkill, SourceEndpoints,
};
use crate::routes::skills::helpers::path_escape;

mod tree;

use tree::add_supporting_files_from_tree;

#[cfg(test)]
mod tests;

// ---------------------------------------------------------------------------
// api.github.com 的 wire 类型（上游 `githubRepoInfo` / `githubTreeResponse` / `githubTreeEntry`）
// ---------------------------------------------------------------------------

/// 上游 `githubRepoInfo`。
#[derive(Debug, Default, serde::Deserialize)]
struct GithubRepoInfo {
    #[serde(default)]
    default_branch: String,
}

/// 上游 `githubTreeResponse`。
#[derive(Debug, Default, serde::Deserialize)]
struct GithubTreeResponse {
    #[serde(default)]
    tree: Vec<GithubTreeEntry>,
    #[serde(default)]
    truncated: bool,
}

/// 上游 `githubTreeEntry`。
#[derive(Debug, Clone, Default, serde::Deserialize)]
struct GithubTreeEntry {
    #[serde(default)]
    path: String,
    /// `"blob"` / `"tree"`。
    #[serde(default, rename = "type")]
    kind: String,
    /// blob 字节数（tree 条目缺省 / 0）。
    #[serde(default)]
    size: i64,
}

impl GithubTreeEntry {
    /// 上游 `githubTreeEntry.size()`：负数按 0 算（畸形 tree 响应不能把算术上限算成负数）。
    fn size(&self) -> i64 {
        if self.size < 0 {
            0
        } else {
            self.size
        }
    }
}

// ---------------------------------------------------------------------------
// api.github.com 调用
// ---------------------------------------------------------------------------

/// `GITHUB_TOKEN`（上游 `os.Getenv("GITHUB_TOKEN")` + `TrimSpace`）。
pub(super) fn github_token() -> Option<String> {
    let token = std::env::var("GITHUB_TOKEN").unwrap_or_default();
    let token = token.trim().to_string();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

/// 上游 `addGitHubAuthHeader`：`GITHUB_TOKEN`（非空）作为 Bearer 附在 api.github.com 调用上
/// （未认证的 60 次/小时在共享自托管上是必然耗尽的）。
fn add_github_auth_header(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    match github_token() {
        Some(token) => request.bearer_auth(token),
        None => request,
    }
}

/// 上游 `doGitHubAPIGet`（可选 `Accept` 头）。
async fn github_api_get(
    client: &reqwest::Client,
    url: &str,
    accept: Option<&str>,
) -> Result<reqwest::Response, ImportError> {
    let mut request = add_github_auth_header(client.get(url));
    if let Some(accept) = accept {
        request = request.header(reqwest::header::ACCEPT, accept);
    }
    request
        .send()
        .await
        .map_err(|error| ImportError::invalid(format!("failed to reach GitHub: {error}")))
}

/// 上游 `errGitHubAPIBlocked`：探针被限流/认证拒（401/403/429），与「真的不存在」区分开。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefProbeError {
    /// 401/403/429：无法判断 ref 是否存在，调用方可以退回乐观拆分。
    Blocked,
    /// 其它非 200：上游原样上抛（网络 / 服务端故障）。
    Other,
}

/// 上游 `fetchGitHubDefaultBranch`：拿不到就回落 `"main"`（永不失败）。
pub(super) async fn fetch_github_default_branch(
    client: &reqwest::Client,
    endpoints: &SourceEndpoints,
    owner: &str,
    repo: &str,
) -> String {
    let url = format!(
        "{}/repos/{}/{}",
        endpoints.github_api,
        path_escape(owner),
        path_escape(repo)
    );
    let Ok(response) = github_api_get(client, &url, None).await else {
        return "main".to_string();
    };
    if response.status() != reqwest::StatusCode::OK {
        return "main".to_string();
    }
    let info = response.json::<GithubRepoInfo>().await.unwrap_or_default();
    if info.default_branch.is_empty() {
        "main".to_string()
    } else {
        info.default_branch
    }
}

/// 上游 `githubRefExists`：`commits/{ref}` 一次调用同时认分支 / 标签 / SHA。
///
/// 200 ⇒ 存在；404 / 422 ⇒ 不存在；401 / 403 / 429 ⇒ `Blocked`（调用方退回乐观拆分）；
/// 其余 ⇒ `Other`。
async fn github_ref_exists(
    client: &reqwest::Client,
    endpoints: &SourceEndpoints,
    owner: &str,
    repo: &str,
    reference: &str,
) -> Result<bool, RefProbeError> {
    let url = format!(
        "{}/repos/{}/{}/commits/{}",
        endpoints.github_api,
        path_escape(owner),
        path_escape(repo),
        escape_ref_path(reference)
    );
    // GitHub 文档：`Accept: application/vnd.github.v3.sha` 只回 SHA，是最省的探针。
    let response = github_api_get(client, &url, Some("application/vnd.github.v3.sha"))
        .await
        .map_err(|_| RefProbeError::Other)?;
    match response.status().as_u16() {
        200 => Ok(true),
        404 | 422 => Ok(false),
        401 | 403 | 429 => Err(RefProbeError::Blocked),
        _ => Err(RefProbeError::Other),
    }
}

/// 上游 `resolveGitHubRefAndPath`：`/tree/release/v2/skills/foo` 在 (ref=release, path=v2/…)
/// 与 (ref=release/v2, path=…) 之间有歧义，从**最长**前缀开始向 API 求证。
async fn resolve_github_ref_and_path(
    client: &reqwest::Client,
    endpoints: &SourceEndpoints,
    spec: &mut GitHubSpec,
) -> Result<(), ImportError> {
    if spec.ref_segments.is_empty() {
        return Ok(());
    }
    let mut tried: Vec<String> = Vec::with_capacity(spec.ref_segments.len());
    let mut blocked = false;
    for count in (1..=spec.ref_segments.len()).rev() {
        let candidate = spec.ref_segments[..count].join("/");
        tried.push(candidate.clone());
        match github_ref_exists(client, endpoints, &spec.owner, &spec.repo, &candidate).await {
            Err(RefProbeError::Blocked) => {
                // 「探不出来」不等于「不存在」：记一笔后继续试更短的前缀，别让一次 403
                // 把常见的单段 ref 情形也判死。
                blocked = true;
            }
            Err(RefProbeError::Other) => {
                return Err(ImportError::invalid(format!(
                    "validating ref {candidate:?}: github import: ref probe failed"
                )));
            }
            // 该前缀不是 ref：继续试更短的。
            Ok(false) => {}
            Ok(true) => {
                if count == spec.ref_segments.len() {
                    spec.skill_dir = String::new();
                } else {
                    spec.skill_dir = spec.ref_segments[count..].join("/");
                }
                spec.ref_ = candidate;
                return Ok(());
            }
        }
    }
    if blocked {
        // 全部探针要么确认 404、要么被限流 ⇒ 退回 `parse_github_url` 的乐观单段拆分。
        // 猜错时下游 raw 取件会给出更清楚的「SKILL.md not found」。
        tracing::warn!(
            owner = %spec.owner,
            repo = %spec.repo,
            tried = ?tried,
            "github import: ref resolution blocked by GitHub API (rate limit or auth); falling back to the optimistic single-segment ref. Set GITHUB_TOKEN to enable disambiguation of slash-bearing refs."
        );
        return Ok(());
    }
    Err(ImportError::invalid(format!(
        "could not resolve ref in github.com/{}/{} URL — tried: {}. Make sure the branch, tag, or commit exists and that the URL is the canonical /tree/{{ref}}/{{path}} or /blob/{{ref}}/{{path}}/SKILL.md form",
        spec.owner,
        spec.repo,
        tried.join(", ")
    )))
}

/// 上游 `fetchGitHubTree`：一次调用取整棵递归 tree（每目录一次 contents 调用会把大型
/// monorepo 的导入拖过网关超时）。
async fn fetch_github_tree(
    client: &reqwest::Client,
    endpoints: &SourceEndpoints,
    owner: &str,
    repo: &str,
    reference: &str,
) -> Result<(Vec<GithubTreeEntry>, bool), ImportError> {
    let url = format!(
        "{}/repos/{}/{}/git/trees/{}?recursive=1",
        endpoints.github_api,
        path_escape(owner),
        path_escape(repo),
        path_escape(reference)
    );
    let response = github_api_get(client, &url, None).await?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(ImportError::invalid(format!(
            "HTTP {}",
            response.status().as_u16()
        )));
    }
    let tree = response
        .json::<GithubTreeResponse>()
        .await
        .map_err(|error| ImportError::invalid(format!("github import: {error}")))?;
    Ok((tree.tree, tree.truncated))
}

// ---------------------------------------------------------------------------
// 两条入口：github.com 与 skills.sh
// ---------------------------------------------------------------------------

/// 上游 `escapeRefPath`：逐段 percent-encode 后**保留** `/`（GitHub 的 commits / raw 端点
/// 不接受 `release%2Fv2`）。
pub(super) fn escape_ref_path(reference: &str) -> String {
    reference
        .split('/')
        .map(path_escape)
        .collect::<Vec<_>>()
        .join("/")
}

/// 上游 `fetchFromGitHub`。
pub(super) async fn fetch_from_github(
    client: &reqwest::Client,
    endpoints: &SourceEndpoints,
    raw_url: &str,
) -> Result<FetchedSkill, ImportError> {
    let mut spec = parse_github_url(raw_url).map_err(|error| ImportError::invalid(error.0))?;
    if !spec.ref_segments.is_empty() {
        // 先向 API 求证带 `/` 的 ref（release/v2 …），再发任何 raw / contents 请求。
        resolve_github_ref_and_path(client, endpoints, &mut spec).await?;
    }
    if spec.ref_.is_empty() {
        spec.ref_ = fetch_github_default_branch(client, endpoints, &spec.owner, &spec.repo).await;
    }
    let raw_prefix = raw_github_prefix(endpoints, &spec.owner, &spec.repo, &spec.ref_);

    let skill_md_path = if spec.skill_dir.is_empty() {
        "SKILL.md".to_string()
    } else {
        format!("{}/SKILL.md", spec.skill_dir)
    };
    let skill_md_body = fetch_raw_file(
        client,
        endpoints,
        &build_raw_github_url(&raw_prefix, &skill_md_path),
    )
    .await
    .map_err(|error| {
        if spec.skill_dir.is_empty() {
            ImportError::invalid(format!(
                "SKILL.md not found at the root of {}/{}@{}. For multi-skill repositories, point to a specific directory using github.com/{}/{}/tree/{}/<skill-dir>",
                spec.owner, spec.repo, spec.ref_, spec.owner, spec.repo, spec.ref_
            ))
        } else {
            ImportError::invalid(format!(
                "SKILL.md not found at {skill_md_path} in {}/{}@{}: {error}",
                spec.owner, spec.repo, spec.ref_
            ))
        }
    })?;
    let content = String::from_utf8_lossy(&skill_md_body).into_owned();

    let frontmatter = parse_skill_frontmatter(&content);
    let mut name = frontmatter.name;
    if name.is_empty() {
        name = if spec.skill_dir.is_empty() {
            spec.repo.clone()
        } else {
            spec.skill_dir.rsplit('/').next().unwrap_or("").to_string()
        };
    }

    let mut imported =
        mc_skill::archive::ImportedSkill::new(name, frontmatter.description, content);
    let origin = serde_json::json!({
        "type": "github",
        "source_url": raw_url,
        "owner": spec.owner,
        "repo": spec.repo,
        "ref": spec.ref_,
        "path": spec.skill_dir,
    });

    // 上游在这里会在 tree 不可用/截断时退回逐目录爬取；本仓**改为 503 可重试**
    // （见文件头偏离说明）：宁可让用户重试，也不落一个少了支持文件的包。
    let (tree, truncated) = fetch_github_tree(client, endpoints, &spec.owner, &spec.repo, &spec.ref_)
        .await
        .map_err(|error| {
            ImportError::unavailable(format!(
                "could not read the {}/{} repository tree (usually GitHub API rate limiting — set GITHUB_TOKEN on the server or retry): {error}",
                spec.owner, spec.repo
            ))
        })?;
    if truncated {
        return Err(ImportError::unavailable(format!(
            "the {}/{} repository tree is too large for GitHub to return in one call (set GITHUB_TOKEN and retry)",
            spec.owner, spec.repo
        )));
    }
    add_supporting_files_from_tree(
        client,
        endpoints,
        &mut imported,
        &tree,
        &raw_prefix,
        &spec.skill_dir,
    )
    .await?;
    Ok(FetchedSkill {
        skill: imported,
        origin,
    })
}

/// 上游 `fetchFromSkillsSh`：skills.sh 的 URL 映射到 GitHub 仓库，目录解析与支持文件枚举
/// 都靠**一次**递归 tree 调用。
pub(super) async fn fetch_from_skills_sh(
    client: &reqwest::Client,
    endpoints: &SourceEndpoints,
    raw_url: &str,
) -> Result<FetchedSkill, ImportError> {
    let (owner, repo, skill_name) =
        parse_skills_sh_parts(raw_url).map_err(|error| ImportError::invalid(error.0))?;

    let default_branch = fetch_github_default_branch(client, endpoints, &owner, &repo).await;
    let raw_prefix = raw_github_prefix(endpoints, &owner, &repo, &default_branch);

    let (tree, truncated) = fetch_github_tree(client, endpoints, &owner, &repo, &default_branch)
        .await
        .map_err(|error| {
            // 与上游同义：没有 tree 就无法安全判断哪个目录才是 skill（raw 探针 + 根 SKILL.md
            // 回落会在 slug 与仓库名撞车时选中整仓根），故返回可重试的 503。
            tracing::warn!(owner = %owner, repo = %repo, error = %error, "skills.sh import: repository tree fetch failed");
            ImportError::unavailable(format!(
                "could not read the {owner}/{repo} repository tree (usually GitHub API rate limiting — set GITHUB_TOKEN on the server or retry): {error}"
            ))
        })?;
    if truncated {
        return Err(ImportError::unavailable(format!(
            "the {owner}/{repo} repository tree is too large for GitHub to return in one call (set GITHUB_TOKEN and retry)"
        )));
    }

    let (skill_dir, skill_md_body) = tree::resolve_skill_dir_from_tree(
        client,
        endpoints,
        &raw_prefix,
        &owner,
        &repo,
        &skill_name,
        &tree,
    )
    .await?;

    let frontmatter = parse_skill_frontmatter(&skill_md_body);
    let mut name = frontmatter.name;
    if name.is_empty() {
        name = skill_name.clone();
    }
    let mut imported =
        mc_skill::archive::ImportedSkill::new(name, frontmatter.description, skill_md_body);
    let origin = serde_json::json!({
        "type": "skills_sh",
        "source_url": raw_url,
        "owner": owner,
        "repo": repo,
        "skill": skill_name,
    });

    add_supporting_files_from_tree(
        client,
        endpoints,
        &mut imported,
        &tree,
        &raw_prefix,
        &skill_dir,
    )
    .await?;
    Ok(FetchedSkill {
        skill: imported,
        origin,
    })
}
