//! 技能来源的取件面：**可覆写的端点** + 原始文件下载 + `ClawHub` 取件 + `SkillSourceFetcher` 端口。
//!
//! - **写者**：M6-3（`docs/57` §3.2；拆分登记在 `docs/32` §9 的文件→写者表）。
//! - **上游**：`internal/handler/skill.go` 的 `clawHubAPIBase` / `fetchFromClawHub` /
//!   `fetchRawFile` / `newRawFileRequest` / `buildRawGitHubURL`。
//! - **为什么端点要可覆写**：上游测试把包级变量 `clawHubAPIBase` 换成 `httptest` 服务器地址
//!   （`skill_import_duplicate_test.go` / `skill_refresh_test.go`）来离线跑真实导入链路。
//!   Rust 没有包级可变静态，本仓改为 `SourceEndpoints` + `source_endpoints()` 读取一处
//!   `OnceLock<RwLock<Option<..>>>` 覆写（`#[cfg(feature = "test-util")]` 暴露 setter）。
//!   生产路径永远读到 `Default`，行为与上游的默认常量逐字一致。
//! - **`GITHUB_TOKEN` 的出站闸门**：同一个下载函数也服务 clawhub.ai / skills.sh，**只有**
//!   `raw.githubusercontent.com`（或其覆写端点）才带 Bearer —— 否则会把 token 泄给第三方技能站。

use std::sync::{OnceLock, RwLock};

use mc_skill::archive::{ImportError, ImportFailure, ImportedSkill, MAX_IMPORT_FILE_SIZE};
use mc_skill::source::{parse_clawhub_slug, ImportSource};
use serde::Deserialize;

use super::github::{escape_ref_path, fetch_from_github, fetch_from_skills_sh, github_token};
use crate::routes::skills::helpers::{path_escape, query_escape};

/// 上游 `clawHubAPIBase` 的默认值（`helpers.rs` 里同值的 `CLAWHUB_API_BASE` 是 M6-2 的**只读**
/// 常量，不能用来做覆写，故此处置一份同值默认）。
pub(super) const CLAWHUB_API_BASE: &str = "https://clawhub.ai/api/v1";
/// 上游 `api.github.com`。
pub(super) const GITHUB_API_BASE: &str = "https://api.github.com";
/// 上游 `rawGitHubContentHost`（带 scheme 的形态）。
pub(super) const GITHUB_RAW_BASE: &str = "https://raw.githubusercontent.com";
/// 上游 `importFetchTimeout`：单次导入取件的总预算（45s）。
pub(super) const IMPORT_FETCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);
/// 上游 `&http.Client{Timeout: 30 * time.Second}`：**单次**出站请求的上限。
pub(super) const SOURCE_HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// 上游 `importFetchErrorResponse` 的 504 文案（逐字）。
pub(super) const IMPORT_TIMEOUT_MESSAGE: &str =
    "skill import timed out fetching source files; the skill may be too large or the source too slow";

/// 三个上游端点。默认值与上游常量逐字一致；测试可整体覆写成假服务器。
///
/// `pub` 是因为 `#[cfg(feature = "test-util")]` 的 e2e 要构造它（`import.rs` 里再导出）；
/// 生产代码只会读 `Default`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceEndpoints {
    /// `ClawHub` API 根（上游 `clawHubAPIBase`）。
    pub clawhub_api: String,
    /// `api.github.com` 根。
    pub github_api: String,
    /// `raw.githubusercontent.com` 根。
    pub github_raw: String,
}

impl Default for SourceEndpoints {
    fn default() -> Self {
        Self {
            clawhub_api: CLAWHUB_API_BASE.to_string(),
            github_api: GITHUB_API_BASE.to_string(),
            github_raw: GITHUB_RAW_BASE.to_string(),
        }
    }
}

/// 覆写槽。`Option` 为 `None` ⇒ 用默认端点（生产路径）。
static SOURCE_ENDPOINT_OVERRIDE: OnceLock<RwLock<Option<SourceEndpoints>>> = OnceLock::new();

fn override_slot() -> &'static RwLock<Option<SourceEndpoints>> {
    SOURCE_ENDPOINT_OVERRIDE.get_or_init(|| RwLock::new(None))
}

/// 当前生效的端点。
pub(super) fn source_endpoints() -> SourceEndpoints {
    let guard = override_slot()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.clone().unwrap_or_default()
}

/// 覆写 / 清除端点（上游测试替换 `clawHubAPIBase` 的等价物）；`None` 恢复默认。
///
/// **只在测试里调用**：e2e 用 `--features mc-http/test-util` 跑，生产构建里这个函数不存在。
#[cfg(feature = "test-util")]
pub fn set_source_endpoints(endpoints: Option<SourceEndpoints>) {
    let mut guard = override_slot()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *guard = endpoints;
}

// ---------------------------------------------------------------------------
// URL 拼装（上游 `escapeRefPath` / `buildRawGitHubURL`）
// ---------------------------------------------------------------------------

/// 上游 `https://raw.githubusercontent.com/{owner}/{repo}/{ref}` 前缀。
pub(super) fn raw_github_prefix(
    endpoints: &SourceEndpoints,
    owner: &str,
    repo: &str,
    reference: &str,
) -> String {
    format!(
        "{}/{}/{}/{}",
        endpoints.github_raw,
        path_escape(owner),
        path_escape(repo),
        escape_ref_path(reference)
    )
}

/// 上游 `buildRawGitHubURL`：把仓库内路径逐段转义后接到前缀上（空路径 ⇒ 前缀本身）。
pub(super) fn build_raw_github_url(raw_prefix: &str, repo_path: &str) -> String {
    let escaped: Vec<String> = repo_path
        .trim_matches('/')
        .split('/')
        .filter(|part| !part.is_empty())
        .map(path_escape)
        .collect();
    if escaped.is_empty() {
        return raw_prefix.to_string();
    }
    format!("{raw_prefix}/{}", escaped.join("/"))
}

/// 上游 `req.URL.Hostname()`：取主机名（去 scheme / userinfo / 端口，小写）。
///
/// 只在 `GITHUB_TOKEN` 的出站闸门里用；本仓 `mc-http` 没有 `url` 依赖（M6-0 冻结了 manifest），
/// 所以这里用字符串切分而不是 `url::Url::parse`。它不是通用 URL 解析器：只认
/// `scheme://host[:port]/...` 这一种形态，够闸门用即可。
fn host_of(url: &str) -> String {
    let rest = match url.split_once("://") {
        Some((_, rest)) => rest,
        None => url,
    };
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .rsplit('@')
        .next()
        .unwrap_or_default();
    authority
        .split(':')
        .next()
        .unwrap_or_default()
        .to_lowercase()
}

// ---------------------------------------------------------------------------
// 原始文件下载（上游 `fetchRawFile` / `newRawFileRequest`）
// ---------------------------------------------------------------------------

/// 上游 `fetchRawFile`：流式下载并**带 1 MiB 上限**。
///
/// 用 `chunk()` 逐块累加而不是 `bytes()`：后者会先把超限响应整个读进内存，再判上限 ——
/// 对「1 MiB 上限」这种保护来说，等于保护失效。超过上限时返回 `Cap`，调用方按
/// `isCapError` 决定「致命」还是「跳过」。
pub(super) async fn fetch_raw_file(
    client: &reqwest::Client,
    endpoints: &SourceEndpoints,
    file_url: &str,
) -> Result<Vec<u8>, ImportError> {
    let mut request = client.get(file_url);
    // 上游 `newRawFileRequest`：只有 GitHub 的 raw 主机才附 token。
    if host_of(file_url) == host_of(&endpoints.github_raw) {
        if let Some(token) = github_token() {
            request = request.bearer_auth(token);
        }
    }
    let mut response = request
        .send()
        .await
        .map_err(|error| ImportError::invalid(format!("HTTP request failed: {error}")))?;
    if response.status() != reqwest::StatusCode::OK {
        return Err(ImportError::invalid(format!(
            "HTTP {}",
            response.status().as_u16()
        )));
    }
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| ImportError::invalid(format!("HTTP body read failed: {error}")))?
    {
        body.extend_from_slice(&chunk);
        if body.len() as u64 > MAX_IMPORT_FILE_SIZE {
            return Err(ImportError::cap(format!(
                "file exceeds {MAX_IMPORT_FILE_SIZE} byte limit"
            )));
        }
    }
    Ok(body)
}

// ---------------------------------------------------------------------------
// 取件端口
// ---------------------------------------------------------------------------

/// 上游 `http.Client{Timeout: 30 * time.Second}`（每个请求建一次）。
pub(super) fn source_http_client() -> Result<reqwest::Client, ImportError> {
    reqwest::Client::builder()
        .timeout(SOURCE_HTTP_TIMEOUT)
        .build()
        .map_err(|error| ImportError::invalid(format!("failed to build http client: {error}")))
}

/// 上游 `ctx, cancel := context.WithTimeout(r.Context(), importFetchTimeout)` 包住的
/// 「按源分发到三条 `fetchFrom*`」那一段。
///
/// 超时被折成 [`ImportError::timeout`]（⇒ route 层 504 与上游 `DeadlineExceeded` 分支同码同文案）；
/// 已经下好的文件随 future 一起丢弃 —— 上游同理，超时后的包**不落库**。
///
/// `import.rs` 与 `refresh.rs` **共用**这一个函数：两条路径的超时 / 分类口径必须一致。
// `pub`（而非 `pub(super)`）：`refresh.rs` 是 `import` 的兄弟模块，需要经 `import.rs`
// 再导出拿到它；`fetch` 模块本身是私有的，所以可达面没有任何扩大。
pub async fn fetch_imported(
    source: ImportSource,
    normalized: &str,
) -> Result<FetchedSkill, ImportError> {
    let fetcher = HttpSkillSourceFetcher::new(source_http_client()?);
    match tokio::time::timeout(
        IMPORT_FETCH_TIMEOUT,
        SkillSourceFetcher::fetch(&fetcher, source, normalized),
    )
    .await
    {
        Ok(result) => result,
        Err(_elapsed) => Err(ImportError::timeout(IMPORT_TIMEOUT_MESSAGE)),
    }
}

/// 上游 `importFetchErrorResponse`：取件失败 → `(状态码, 文案)`。
///
/// 上限 ⇒ 413；整轮超时 ⇒ 504；源暂时不可用 ⇒ 503（可重试）；其余 ⇒ 502。
/// 注意**没有 400**：slug / URL 的解析错误发生在 `fetchFrom*` **内部**，上游把它们也算 502
/// （`detectImportSource` 那一步的 400 在 handler 里，早于取件）。
pub fn import_fetch_error_response(error: &ImportError) -> (u16, String) {
    match error.failure {
        ImportFailure::Cap => (413, error.message.clone()),
        ImportFailure::Timeout => (504, error.message.clone()),
        ImportFailure::SourceUnavailable => (503, error.message.clone()),
        ImportFailure::Invalid => (502, error.message.clone()),
    }
}

/// 取到的包 + 它的 origin（上游 `importedSkill` 里的 `origin map[string]any`）。
#[derive(Debug, Clone)]
pub struct FetchedSkill {
    pub skill: ImportedSkill,
    pub origin: serde_json::Value,
}

/// 上游三条 `fetchFrom*` 的端口形态。
///
/// 端口放在 `mc-http` 而不是 `mc-skill`：`mc-skill` 没有 `reqwest` 依赖边（`docs/32` §9 的
/// 依赖方向是冻结的），而 W8 会提供真正的 GitHub 客户端实现。
#[async_trait::async_trait]
pub(super) trait SkillSourceFetcher: Send + Sync {
    async fn fetch(
        &self,
        source: ImportSource,
        normalized: &str,
    ) -> Result<FetchedSkill, ImportError>;
}

/// 生产实现：直连上游的三个端点。
pub(super) struct HttpSkillSourceFetcher {
    client: reqwest::Client,
    endpoints: SourceEndpoints,
}

impl HttpSkillSourceFetcher {
    pub(super) fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            endpoints: source_endpoints(),
        }
    }
}

#[async_trait::async_trait]
impl SkillSourceFetcher for HttpSkillSourceFetcher {
    async fn fetch(
        &self,
        source: ImportSource,
        normalized: &str,
    ) -> Result<FetchedSkill, ImportError> {
        match source {
            ImportSource::ClawHub => {
                fetch_from_clawhub(&self.client, &self.endpoints, normalized).await
            }
            ImportSource::SkillsSh => {
                fetch_from_skills_sh(&self.client, &self.endpoints, normalized).await
            }
            ImportSource::GitHub => {
                fetch_from_github(&self.client, &self.endpoints, normalized).await
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ClawHub（上游 `fetchFromClawHub`）
// ---------------------------------------------------------------------------

/// 上游 `clawhubSkill`。
///
/// ⚠️ 上游 `clawhubSkill` 还有 `Stats`，但**导入路径不读它**（只有 M6-2 的搜索结果补水读
/// `InstallsAllTime`）；本片不搬没用的字段，否则 Rust 侧会报 `dead_code`。
#[derive(Debug, Default, Deserialize)]
struct ClawhubSkill {
    #[serde(default, rename = "displayName")]
    display_name: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    tags: std::collections::BTreeMap<String, String>,
}

/// 上游 `clawhubLatestVersion`。
#[derive(Debug, Default, Deserialize)]
struct ClawhubLatestVersion {
    #[serde(default)]
    version: String,
}

/// 上游 `clawhubGetSkillResponse`。
#[derive(Debug, Default, Deserialize)]
struct ClawhubGetSkillResponse {
    #[serde(default)]
    skill: ClawhubSkill,
    #[serde(default, rename = "latestVersion")]
    latest_version: Option<ClawhubLatestVersion>,
}

/// 上游 `clawhubVersionDetailResponse`。
#[derive(Debug, Default, Deserialize)]
struct ClawhubVersionDetailResponse {
    #[serde(default)]
    version: ClawhubVersionDetail,
}

/// 上游 `clawhubVersionDetail`。
#[derive(Debug, Default, Deserialize)]
struct ClawhubVersionDetail {
    #[serde(default)]
    files: Vec<ClawhubFileEntry>,
}

/// 上游 `clawhubFileEntry`。
#[derive(Debug, Default, Deserialize)]
struct ClawhubFileEntry {
    #[serde(default)]
    path: String,
}

/// 上游 `fetchFromClawHub` 的第 ① 步：skill 元数据 + `latestVersion.version` 兜底值。
///
/// 上游把「连不上」「404」「非 200」都算普通错误（⇒ 502），只有包本身不合法才是别的分类。
/// 返回的第二个值是 `latestVersion.version`（清单为空时用它兜底取文件清单）。
async fn fetch_clawhub_skill(
    client: &reqwest::Client,
    api_base: &str,
    slug: &str,
) -> Result<(ClawhubSkill, String), ImportError> {
    let skill_url = format!("{api_base}/skills/{}", path_escape(slug));
    let response = client
        .get(&skill_url)
        .send()
        .await
        .map_err(|error| ImportError::invalid(format!("failed to reach ClawHub: {error}")))?;
    match response.status().as_u16() {
        200 => {}
        404 => {
            return Err(ImportError::invalid(format!(
                "skill not found on ClawHub: {slug}"
            )));
        }
        status => {
            return Err(ImportError::invalid(format!(
                "ClawHub returned status {status}"
            )));
        }
    }
    let detail = response
        .json::<ClawhubGetSkillResponse>()
        .await
        .map_err(|_| ImportError::invalid("failed to parse ClawHub response"))?;
    let fallback_version = detail
        .latest_version
        .map(|latest| latest.version)
        .unwrap_or_default();
    Ok((detail.skill, fallback_version))
}

/// 上游 `fetchFromClawHub` 的第 ② 步：该版本的文件清单。
///
/// 清单拿不到**不算错**（上游连版本详情请求失败都只记日志）：返回空清单，后面会因为
/// 「`SKILL.md` is empty or missing」失败 —— 与上游同一条兜底路径。
async fn fetch_clawhub_file_paths(
    client: &reqwest::Client,
    api_base: &str,
    slug: &str,
    latest_version: &str,
) -> Vec<String> {
    if latest_version.is_empty() {
        return Vec::new();
    }
    let version_url = format!(
        "{api_base}/skills/{}/versions/{}",
        path_escape(slug),
        path_escape(latest_version)
    );
    let Ok(response) = client.get(&version_url).send().await else {
        return Vec::new();
    };
    if response.status() != reqwest::StatusCode::OK {
        return Vec::new();
    }
    match response.json::<ClawhubVersionDetailResponse>().await {
        Ok(detail) => detail
            .version
            .files
            .into_iter()
            .map(|file| file.path)
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// 上游 `fetchFromClawHub` 的第 ③ 步：逐个下载。
///
/// **只有 `SKILL.md` 与上限违例是致命的**：静默丢文件会产出一个「看起来合法」的不完整包，
/// 而 `SKILL.md` 是承重件；其余单文件失败按旧循环的宽容跳过（记 warn 继续）。
async fn download_clawhub_files(
    client: &reqwest::Client,
    endpoints: &SourceEndpoints,
    api_base: &str,
    slug: &str,
    latest_version: &str,
    file_paths: Vec<String>,
    result: &mut ImportedSkill,
) -> Result<(), ImportError> {
    for file_path in file_paths {
        let mut file_url = format!(
            "{api_base}/skills/{}/file?path={}",
            path_escape(slug),
            query_escape(&file_path)
        );
        if !latest_version.is_empty() {
            file_url.push_str("&version=");
            file_url.push_str(&query_escape(latest_version));
        }
        match fetch_raw_file(client, endpoints, &file_url).await {
            Ok(body) => {
                let text = String::from_utf8_lossy(&body).into_owned();
                if file_path == "SKILL.md" {
                    result.content = text;
                } else {
                    result.add_file(file_path.as_str(), text)?;
                }
            }
            Err(error) => {
                if error.is_cap() || file_path == "SKILL.md" {
                    // 保留分类只换文案（等价于 Go 的 `%w`：`errors.Is(err, errImportCapExceeded)`
                    // 仍然成立，route 层才判得出 413）。
                    let message = format!("clawhub import: {file_path}: {error}");
                    return Err(ImportError {
                        failure: error.failure,
                        message,
                    });
                }
                tracing::warn!(path = %file_path, error = %error, "clawhub import: file download failed");
            }
        }
    }
    Ok(())
}

/// 上游 `fetchFromClawHub`：元数据 → 文件清单 → 逐个下载。
pub(super) async fn fetch_from_clawhub(
    client: &reqwest::Client,
    endpoints: &SourceEndpoints,
    raw_url: &str,
) -> Result<FetchedSkill, ImportError> {
    let slug = parse_clawhub_slug(raw_url).map_err(|error| ImportError::invalid(error.0))?;
    let api_base = endpoints.clawhub_api.as_str();
    let (ch_skill, fallback_version) = fetch_clawhub_skill(client, api_base, &slug).await?;

    // `tags["latest"]` **存在**就优先（哪怕是空串），否则才回落 `latestVersion.version`。
    let latest_version = match ch_skill.tags.get("latest") {
        Some(value) => value.clone(),
        None => fallback_version,
    };
    let file_paths = fetch_clawhub_file_paths(client, api_base, &slug, &latest_version).await;

    let mut name = ch_skill.display_name.clone();
    if name.is_empty() {
        name = slug.clone();
    }
    let mut result = ImportedSkill::new(name, ch_skill.summary.clone(), String::new());
    download_clawhub_files(
        client,
        endpoints,
        api_base,
        &slug,
        &latest_version,
        file_paths,
        &mut result,
    )
    .await?;
    if result.content.is_empty() {
        return Err(ImportError::invalid(format!(
            "clawhub import: SKILL.md is empty or missing for {slug}"
        )));
    }
    let origin = serde_json::json!({
        "type": "clawhub",
        "source_url": raw_url,
        "slug": slug,
    });
    Ok(FetchedSkill {
        skill: result,
        origin,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_of_matches_reqwest_hostname_semantics() {
        assert_eq!(
            host_of("https://raw.githubusercontent.com/a/b"),
            "raw.githubusercontent.com"
        );
        assert_eq!(
            host_of("https://raw.githubusercontent.com:443/a"),
            "raw.githubusercontent.com"
        );
        assert_eq!(host_of("http://127.0.0.1:41234/a/b"), "127.0.0.1");
        assert_eq!(host_of("https://clawhub.ai/api/v1/skills/x"), "clawhub.ai");
        // 大小写不敏感（Go 的 `URL.Hostname()` 保留原样，但闸门比较用 EqualFold）。
        assert_eq!(host_of("https://RAW.GitHub.COM/a"), "raw.github.com");
    }

    #[test]
    fn raw_urls_escape_per_segment_and_keep_ref_slashes() {
        let endpoints = SourceEndpoints::default();
        assert_eq!(
            raw_github_prefix(&endpoints, "acme", "repo", "release/v2"),
            "https://raw.githubusercontent.com/acme/repo/release/v2"
        );
        assert_eq!(
            build_raw_github_url(
                "https://raw.githubusercontent.com/acme/repo/main",
                "skills/my skill/SKILL.md"
            ),
            "https://raw.githubusercontent.com/acme/repo/main/skills/my%20skill/SKILL.md"
        );
        // 空路径 ⇒ 前缀本身（上游 `buildRawGitHubURL` 的空 list 分支）。
        assert_eq!(build_raw_github_url("https://x/y/z", "/"), "https://x/y/z");
    }

    #[test]
    fn default_endpoints_are_the_upstream_constants() {
        let endpoints = SourceEndpoints::default();
        assert_eq!(endpoints.clawhub_api, "https://clawhub.ai/api/v1");
        assert_eq!(endpoints.github_api, "https://api.github.com");
        assert_eq!(endpoints.github_raw, "https://raw.githubusercontent.com");
    }
}
