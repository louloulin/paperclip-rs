use super::*;

/// 上游 `clawHubAPIBase`。
pub(crate) const CLAWHUB_API_BASE: &str = "https://clawhub.ai/api/v1";
/// 上游 `clawHubSearchStatsLimit`：只给前 10 条补水安装量（每条一次出站请求）。
pub(crate) const CLAWHUB_STATS_LIMIT: usize = 10;
/// 上游 `http.Client{Timeout: 30 * time.Second}`。
pub(crate) const CLAWHUB_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Default, Deserialize)]
struct ClawhubSearchResponse {
    /// 缺省与 `null` 都当空表；**每个字段都必须可缺省** —— 线上响应不保证键齐全，
    /// 少一个键就整轮 502 是过度严格。
    #[serde(default)]
    results: Option<Vec<ClawhubSearchResult>>,
}

#[derive(Debug, Default, Deserialize)]
struct ClawhubSearchResult {
    #[serde(default)]
    slug: String,
    #[serde(default, rename = "displayName")]
    display_name: String,
    #[serde(default)]
    summary: String,
    #[serde(default, rename = "ownerHandle")]
    owner_handle: String,
}

#[derive(Debug, Default, Deserialize)]
struct ClawhubGetSkillResponse {
    #[serde(default)]
    skill: ClawhubSkill,
}

#[derive(Debug, Default, Deserialize)]
struct ClawhubSkill {
    #[serde(default)]
    stats: ClawhubSkillStats,
}

#[derive(Debug, Default, Deserialize)]
struct ClawhubSkillStats {
    #[serde(default, rename = "installsAllTime")]
    installs_all_time: i64,
    #[serde(default, rename = "installsCurrent")]
    installs_current: i64,
}

/// 上游 `searchClawHubSkills`：搜索 +（前 10 条）安装量补水。错误一律是**字符串**而不是
/// `Error` —— 调用方要把它塞进 502 的扁平体（形状与本仓标准错误体不同）。
pub(crate) async fn search_clawhub_skills(
    client: &reqwest::Client,
    query: &str,
) -> Result<Vec<SkillSearchCandidateDto>, String> {
    let url = format!("{CLAWHUB_API_BASE}/search?q={}", query_escape(query));
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("failed to reach ClawHub: {e}"))?;
    if resp.status() != reqwest::StatusCode::OK {
        return Err(format!(
            "ClawHub search returned status {}",
            resp.status().as_u16()
        ));
    }
    let body: ClawhubSearchResponse = resp
        .json()
        .await
        .map_err(|_| "failed to parse ClawHub search response".to_string())?;

    let results = body.results.unwrap_or_default();
    let mut candidates = Vec::with_capacity(results.len());
    for (index, result) in results.iter().enumerate() {
        // 空 slug 直接跳过；注意下标用的是**原始**下标（上游 `for i, r := range`），
        // 所以被跳过的条目仍占掉一个补水名额。
        if result.slug.is_empty() {
            continue;
        }
        let mut candidate = SkillSearchCandidateDto {
            name: if result.display_name.is_empty() {
                result.slug.clone()
            } else {
                result.display_name.clone()
            },
            url: clawhub_skill_url(&result.owner_handle, &result.slug),
            source: "clawhub.ai".to_string(),
            repo: None,
            install_count: None,
            github_stars: None,
            description: result.summary.clone(),
        };
        if index < CLAWHUB_STATS_LIMIT {
            if let Some(count) = fetch_clawhub_install_count(client, &result.slug).await {
                candidate.install_count = Some(count);
            }
        }
        candidates.push(candidate);
    }
    Ok(candidates)
}

/// 上游 `buildClawHubSkillURL`。
pub(crate) fn clawhub_skill_url(owner_handle: &str, slug: &str) -> String {
    if owner_handle.is_empty() {
        return format!("https://clawhub.ai/{}", path_escape(slug));
    }
    format!(
        "https://clawhub.ai/{}/{}",
        path_escape(owner_handle),
        path_escape(slug)
    )
}

/// 上游 `fetchClawHubInstallCount`：任何失败都只记 warn 并留 `None`（搜索不因补水失败）。
async fn fetch_clawhub_install_count(client: &reqwest::Client, slug: &str) -> Option<i64> {
    let url = format!("{CLAWHUB_API_BASE}/skills/{}", path_escape(slug));
    let resp = match client.get(&url).send().await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!(slug, error = %e, "clawhub search: failed to fetch skill details");
            return None;
        }
    };
    if resp.status() != reqwest::StatusCode::OK {
        tracing::warn!(
            slug,
            status = resp.status().as_u16(),
            "clawhub search: skill details returned non-200"
        );
        return None;
    }
    let detail: ClawhubGetSkillResponse = match resp.json().await {
        Ok(detail) => detail,
        Err(e) => {
            tracing::warn!(slug, error = %e, "clawhub search: failed to parse skill details");
            return None;
        }
    };
    let stats = detail.skill.stats;
    if stats.installs_all_time > 0 {
        Some(stats.installs_all_time)
    } else {
        Some(stats.installs_current)
    }
}
