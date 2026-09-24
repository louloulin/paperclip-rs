//! 导入源判定与规范化（clawhub.ai / skills.sh / github.com）——**只判源，不取件**。
//!
//! - **写者**：M6-3（`docs/57` §3.2 的 M6-3 文件组）。
//! - **上游**：`internal/handler/skill.go` 的 `detectImportSource`（枚举 `sourceClawHub` /
//!   `sourceSkillsSh` / `sourceGitHub`）与 `parseClawHubSlug` / `parseSkillsShParts` /
//!   `parseGitHubURL`。
//! - **语义（逐条对齐）**：
//!   1. `TrimSpace` 后为空 ⇒ 400（`empty URL`）；
//!   2. 缺 scheme 时补 `https://`（`github.com/a/b` 这种裸写法合法）；
//!   3. 按 **hostname** 判源（`www.` 前缀等价），非法 host ⇒ 400（错误里带支持列表）；
//!   4. **裸 slug 默认 clawhub**：不含 `/` 或不含 `.` 时按 clawhub skill 名处理。
//! - **本仓约定**：返回 `(ImportSource, String /*normalized*/)`，无 IO、无 `reqwest` 依赖；
//!   取件（三个源的 HTTP 调用 + 45s 总超时 + 502/503/504 映射）归 M6-3 的
//!   `routes/skills/import.rs`（`mc-http` 有 reqwest 边，本 crate 故意没有）。
//! - **不做什么**：不解析 GitHub tree 递归（那需要出网）；不做 HTML 抓取。
//!
//! **状态：M6-3 已落地（LUM-1668）**。
//!
//! 行预算（门 ⑩）：桩写 140 行，落地 ~250 行（含用例）。

use url::Url;

/// 上游 `importSource` 三个枚举值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportSource {
    /// 上游 `sourceClawHub`。
    ClawHub,
    /// 上游 `sourceSkillsSh`。
    SkillsSh,
    /// 上游 `sourceGitHub`。
    GitHub,
}

impl ImportSource {
    /// 落进 `skill.config.origin.type` 的字面量（上游三个 `fetchFrom*` 里各自写死的值）。
    pub fn origin_type(self) -> &'static str {
        match self {
            ImportSource::ClawHub => "clawhub",
            ImportSource::SkillsSh => "skills_sh",
            ImportSource::GitHub => "github",
        }
    }

    /// `origin.type` → 源（refresh 用；不在表里 ⇒ 不可刷新）。
    pub fn from_origin_type(origin_type: &str) -> Option<Self> {
        match origin_type {
            "clawhub" => Some(ImportSource::ClawHub),
            "skills_sh" => Some(ImportSource::SkillsSh),
            "github" => Some(ImportSource::GitHub),
            _ => None,
        }
    }
}

/// 源判定 / slug 解析的错误。
///
/// 上游这三个函数返回 `fmt.Errorf` 的裸字符串，route 层把它**原文**塞进 400 的
/// `error` 字段 ⇒ 这里保留 `String`（不做错误分类，分类在 route 层没有对应物）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceError(pub String);

impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SourceError {}

fn err(message: impl Into<String>) -> SourceError {
    SourceError(message.into())
}

/// 上游 `detectImportSource`：返回源与**补过 scheme 的** URL。
///
/// 注意裸 slug 分支返回的是 `raw`（**未补 scheme**）—— 上游就是这么写的，refresh
/// 依赖它把 `origin.source_url` 再喂回来。
pub fn detect_import_source(raw: &str) -> Result<(ImportSource, String), SourceError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(err("empty URL"));
    }

    let normalized = if raw.starts_with("http://") || raw.starts_with("https://") {
        raw.to_string()
    } else {
        format!("https://{raw}")
    };

    // Go `url.Parse` 对相对串不报错（host 为空），Rust 的 `Url::parse` 会 —— 两边都收敛到
    // 「没有 host」这一支，再走下面的裸 slug 兜底。
    let host = Url::parse(&normalized)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_lowercase))
        .unwrap_or_default();

    match host.as_str() {
        "skills.sh" | "www.skills.sh" => return Ok((ImportSource::SkillsSh, normalized)),
        "clawhub.ai" | "www.clawhub.ai" => return Ok((ImportSource::ClawHub, normalized)),
        "github.com" | "www.github.com" => return Ok((ImportSource::GitHub, normalized)),
        _ => {}
    }

    // 无 host（裸 slug）⇒ 默认 clawhub。上游的判据是「不含 `/` 或 不含 `.`」。
    if !raw.contains('/') || !raw.contains('.') {
        return Ok((ImportSource::ClawHub, raw.to_string()));
    }
    Err(err(format!(
        "unsupported source: {host} (supported: clawhub.ai, skills.sh, github.com)"
    )))
}

/// 取出 URL 的路径段（去掉空段）。相对串（裸 slug）按字面路径处理。
///
/// Go 那边 `url.Parse` 的 `Path` 是**已解码**的，`url` crate 的 `path()` 是编码形态 ⇒
/// 逐段再做一次 percent 解码对齐（见 `docs/32` §9.6 的偏差登记）。
fn path_segments(raw: &str) -> Vec<String> {
    let path = match Url::parse(raw) {
        Ok(parsed) => parsed.path().to_string(),
        Err(_) => raw.to_string(),
    };
    path.trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .map(|segment| percent_decode(segment).unwrap_or_else(|| segment.to_string()))
        .collect()
}

/// `url.PathUnescape` 的最小移植：非法 `%` 序列保留原样（Go 返回 error，上游对
/// GitHub 段会把 error 变成 400；这里返回 `None` 让调用方决定）。
fn percent_decode(segment: &str) -> Option<String> {
    let bytes = segment.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return None;
            }
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
            let byte = u8::from_str_radix(hex, 16).ok()?;
            out.push(byte);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// 上游 `parseClawHubSlug`：`/{owner}/{slug}` 取最后一段，`/{slug}` 取唯一一段。
pub fn parse_clawhub_slug(raw: &str) -> Result<String, SourceError> {
    let segments = path_segments(raw);
    if segments.len() == 2 {
        return Ok(segments[1].clone());
    }
    if segments.len() == 1 {
        return Ok(segments[0].clone());
    }
    if segments.is_empty() {
        return Err(err("missing skill slug in URL"));
    }
    Err(err(format!("could not extract skill slug from URL: {raw}")))
}

/// 上游 `parseSkillsShParts`：`skills.sh/{owner}/{repo}/{skill-name}`。
pub fn parse_skills_sh_parts(raw: &str) -> Result<(String, String, String), SourceError> {
    let segments = path_segments(raw);
    if segments.len() != 3 {
        return Err(err(format!(
            "expected URL format: skills.sh/{{owner}}/{{repo}}/{{skill-name}}, got: {}",
            path_of(raw)
        )));
    }
    Ok((
        segments[0].clone(),
        segments[1].clone(),
        segments[2].clone(),
    ))
}

fn path_of(raw: &str) -> String {
    match Url::parse(raw) {
        Ok(parsed) => parsed.path().to_string(),
        Err(_) => raw.to_string(),
    }
}

/// 上游 `githubSpec`：github.com URL 解析出的分量。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitHubSpec {
    pub owner: String,
    pub repo: String,
    /// 空 ⇒ 调用方去解析默认分支。
    pub ref_: String,
    /// repo 内的相对目录，根为 `""`。
    pub skill_dir: String,
    /// `/tree/` `/blob/` 之后的**原始**路径段（`ref` 里带 `/` 时用来向 API 求证边界）。
    pub ref_segments: Vec<String>,
    /// `"tree"` / `"blob"`；根 URL 为 `""`。
    pub kind: String,
}

/// 上游 `parseGitHubURL`。三种形态：
///
/// - `github.com/{owner}/{repo}`
/// - `github.com/{owner}/{repo}/tree/{ref}/{path...}`
/// - `github.com/{owner}/{repo}/blob/{ref}/{path.../SKILL.md}`
pub fn parse_github_url(raw: &str) -> Result<GitHubSpec, SourceError> {
    let segments = path_segments(raw);
    if segments.len() < 2 || segments[0].is_empty() || segments[1].is_empty() {
        return Err(err(format!(
            "expected URL format: github.com/{{owner}}/{{repo}}[/tree/{{ref}}/{{path}}], got: {}",
            path_of(raw)
        )));
    }
    let mut spec = GitHubSpec {
        owner: segments[0].clone(),
        repo: segments[1].trim_end_matches(".git").to_string(),
        ..GitHubSpec::default()
    };
    if segments.len() == 2 {
        return Ok(spec);
    }
    let kind = segments[2].clone();
    if kind != "tree" && kind != "blob" {
        return Err(err(format!(
            "unsupported URL form: github.com/{}/{}/{}/... (use /tree/{{ref}}/... or /blob/{{ref}}/.../SKILL.md)",
            spec.owner, spec.repo, kind
        )));
    }
    if segments.len() < 4 || segments[3].is_empty() {
        return Err(err(format!("missing ref after /{kind}/")));
    }
    spec.kind.clone_from(&kind);
    let mut rest: Vec<String> = segments[3..].to_vec();
    if kind == "blob" {
        let last = rest.last().cloned().unwrap_or_default();
        if !last.eq_ignore_ascii_case("SKILL.md") {
            return Err(err("blob URL must point to a SKILL.md file"));
        }
        rest.pop();
        if rest.is_empty() {
            return Err(err("missing ref after /blob/"));
        }
    }
    // 乐观拆分：假定 ref 只有一段。`fetchFromGitHub` 会再用 API 求证并覆写。
    spec.ref_.clone_from(&rest[0]);
    if rest.len() > 1 {
        spec.skill_dir = rest[1..].join("/");
    }
    spec.ref_segments = rest;
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_maps_every_supported_host() {
        for (raw, want) in [
            ("https://clawhub.ai/acme/review", ImportSource::ClawHub),
            ("https://www.clawhub.ai/acme/review", ImportSource::ClawHub),
            ("https://skills.sh/acme/repo/skill", ImportSource::SkillsSh),
            (
                "https://www.skills.sh/acme/repo/skill",
                ImportSource::SkillsSh,
            ),
            ("https://github.com/acme/repo", ImportSource::GitHub),
            ("github.com/acme/repo", ImportSource::GitHub),
            ("review-helper", ImportSource::ClawHub),
            ("acme/review-helper", ImportSource::ClawHub),
        ] {
            let (source, _) = detect_import_source(raw).expect(raw);
            assert_eq!(source, want, "detect_import_source({raw:?})");
        }
    }

    #[test]
    fn detect_bare_slug_returns_the_raw_string_not_the_schemed_one() {
        // 上游的裸 slug 分支返回 raw（未补 scheme）—— refresh 之后会把 origin.source_url
        // 再喂回来，所以这条形态必须逐字保留。
        let (_, normalized) = detect_import_source("  review-helper  ").expect("bare slug");
        assert_eq!(normalized, "review-helper");
        let (_, normalized) = detect_import_source("github.com/a/b").expect("schemeless");
        assert_eq!(normalized, "https://github.com/a/b");
    }

    #[test]
    fn detect_rejects_empty_and_unsupported_hosts() {
        assert_eq!(detect_import_source("   ").unwrap_err().0, "empty URL");
        let message = detect_import_source("https://example.com/a/b")
            .unwrap_err()
            .0;
        assert!(
            message.starts_with("unsupported source: example.com"),
            "{message}"
        );
        assert!(message.contains("supported: clawhub.ai, skills.sh, github.com"));
    }

    #[test]
    fn parse_clawhub_slug_takes_the_last_segment() {
        assert_eq!(
            parse_clawhub_slug("https://clawhub.ai/acme/review").unwrap(),
            "review"
        );
        assert_eq!(
            parse_clawhub_slug("https://clawhub.ai/review").unwrap(),
            "review"
        );
        assert_eq!(parse_clawhub_slug("review").unwrap(), "review");
        assert_eq!(
            parse_clawhub_slug("https://clawhub.ai/").unwrap_err().0,
            "missing skill slug in URL"
        );
        assert!(parse_clawhub_slug("https://clawhub.ai/a/b/c")
            .unwrap_err()
            .0
            .starts_with("could not extract skill slug from URL"));
    }

    #[test]
    fn parse_skills_sh_parts_requires_exactly_three_segments() {
        let (owner, repo, skill) =
            parse_skills_sh_parts("https://skills.sh/acme/repo/skill").unwrap();
        assert_eq!(
            (owner.as_str(), repo.as_str(), skill.as_str()),
            ("acme", "repo", "skill")
        );
        let message = parse_skills_sh_parts("https://skills.sh/acme/repo")
            .unwrap_err()
            .0;
        assert!(
            message.starts_with(
                "expected URL format: skills.sh/{owner}/{repo}/{skill-name}, got: /acme/repo"
            ),
            "{message}"
        );
    }

    #[test]
    fn parse_github_url_covers_the_three_documented_forms() {
        let root = parse_github_url("https://github.com/acme/repo").unwrap();
        assert_eq!((root.owner.as_str(), root.repo.as_str()), ("acme", "repo"));
        assert_eq!(root.ref_, "");
        assert_eq!(root.skill_dir, "");
        assert!(root.ref_segments.is_empty());

        let tree =
            parse_github_url("https://github.com/acme/repo/tree/release/v2/skills/foo").unwrap();
        assert_eq!(tree.kind, "tree");
        assert_eq!(tree.ref_, "release");
        assert_eq!(tree.skill_dir, "v2/skills/foo");
        assert_eq!(
            tree.ref_segments,
            vec!["release", "v2", "skills", "foo"],
            "raw segments must survive for API disambiguation"
        );

        let blob =
            parse_github_url("https://github.com/acme/repo/blob/main/skills/foo/SKILL.md").unwrap();
        assert_eq!(blob.kind, "blob");
        assert_eq!(blob.ref_, "main");
        assert_eq!(blob.skill_dir, "skills/foo");
    }

    #[test]
    fn parse_github_url_rejects_the_documented_bad_forms() {
        assert!(parse_github_url("https://github.com/acme")
            .unwrap_err()
            .0
            .starts_with("expected URL format"));
        assert!(parse_github_url("https://github.com/acme/repo/issues/1")
            .unwrap_err()
            .0
            .starts_with("unsupported URL form"));
        assert_eq!(
            parse_github_url("https://github.com/acme/repo/tree/")
                .unwrap_err()
                .0,
            "missing ref after /tree/"
        );
        assert_eq!(
            parse_github_url("https://github.com/acme/repo/blob/main/README.md")
                .unwrap_err()
                .0,
            "blob URL must point to a SKILL.md file"
        );
    }

    #[test]
    fn parse_github_url_decodes_escaped_segments_and_strips_dot_git() {
        let spec =
            parse_github_url("https://github.com/acme/repo.git/tree/main/my%20skill").unwrap();
        assert_eq!(spec.repo, "repo");
        assert_eq!(spec.skill_dir, "my skill");
    }

    #[test]
    fn origin_type_round_trips() {
        for source in [
            ImportSource::ClawHub,
            ImportSource::SkillsSh,
            ImportSource::GitHub,
        ] {
            assert_eq!(
                ImportSource::from_origin_type(source.origin_type()),
                Some(source)
            );
        }
        assert_eq!(ImportSource::from_origin_type("runtime_local"), None);
        assert_eq!(ImportSource::from_origin_type(""), None);
    }
}
