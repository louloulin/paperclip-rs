//! 递归 tree 面上的两件事（`routes/skills/import/github.rs` 的子模块，写者同 M6-3）。
//!
//! 1. **skill 目录解析**（上游 `resolveSkillDirFromTree` 一族）：一次 `git/trees?recursive=1`
//!    拿到全部路径，再按「frontmatter 精确匹配 → 常规路径」两级顺序定目录。
//! 2. **支持文件枚举**（上游 `addSupportingFilesFromTree`）：先用 tree 元数据做**算术预检**
//!    （超限的包一个字节都不下），再并发 8 下载、按路径序稳定追加。
//!
//! 上限常量来自 `mc_skill::archive`，本文件不复制数字（`docs/32` §9.6 已登记）。

use std::collections::HashSet;

use mc_skill::archive::{
    ImportError, ImportedSkill, MAX_IMPORT_FILE_COUNT, MAX_IMPORT_FILE_SIZE, MAX_IMPORT_TOTAL_SIZE,
};
use mc_skill::binary::is_likely_binary_file_path;
use mc_skill::frontmatter::parse_skill_frontmatter;

use super::super::fetch::{build_raw_github_url, fetch_raw_file, SourceEndpoints};
use super::GithubTreeEntry;

#[cfg(test)]
mod tests;

// ---------------------------------------------------------------------------
// skill 目录解析
// ---------------------------------------------------------------------------

/// 上游 `extractSkillMdPaths`：tree 里所有 `SKILL.md` blob 的路径。
fn extract_skill_md_paths(entries: &[GithubTreeEntry]) -> Vec<String> {
    entries
        .iter()
        .filter(|entry| {
            entry.kind == "blob" && (entry.path.ends_with("/SKILL.md") || entry.path == "SKILL.md")
        })
        .map(|entry| entry.path.clone())
        .collect()
}

/// 上游 `skillDirFromSkillFilePath`：`skills/foo/SKILL.md` ⇒ `skills/foo`，根 ⇒ `""`。
fn skill_dir_from_skill_file_path(path: &str) -> String {
    if path == "SKILL.md" {
        return String::new();
    }
    path.strip_suffix("/SKILL.md").unwrap_or(path).to_string()
}

/// 上游 `skillNameHints`：把 skill 名拆成「整串 / 各后缀 / 各单词」三波提示（长度 ≥ 3、去重）。
///
/// 顺序有意义：`preferred` 里的路径按这个顺序被逐个取件试探，先命中的目录即被采用。
fn skill_name_hints(skill_name: &str) -> Vec<String> {
    let lower = skill_name.to_lowercase();
    let parts: Vec<&str> = lower.split('-').collect();
    let mut seen: HashSet<String> = HashSet::new();
    let mut hints: Vec<String> = Vec::new();
    let add_hint = |value: &str, seen: &mut HashSet<String>, hints: &mut Vec<String>| {
        let value = value.trim();
        if value.len() < 3 || !seen.insert(value.to_string()) {
            return;
        }
        hints.push(value.to_string());
    };
    add_hint(&lower, &mut seen, &mut hints);
    for index in 1..parts.len() {
        add_hint(&parts[index..].join("-"), &mut seen, &mut hints);
    }
    for part in &parts {
        add_hint(part, &mut seen, &mut hints);
    }
    hints
}

/// 上游 `isLikelySkillPathMatch`：目录（或它的基名）与某个提示互相包含即算「像」。
fn is_likely_skill_path_match(skill_name: &str, skill_path: &str) -> bool {
    let dir = skill_dir_from_skill_file_path(skill_path).to_lowercase();
    let base = dir.rsplit('/').next().unwrap_or("").to_string();
    skill_name_hints(skill_name).iter().any(|hint| {
        dir.contains(hint.as_str()) || base.contains(hint.as_str()) || hint.contains(&base)
    })
}

/// 上游 `partitionSkillMdPaths`：按「名字像不像」分成 preferred / remaining 两波。
fn partition_skill_md_paths(skill_name: &str, paths: &[String]) -> (Vec<String>, Vec<String>) {
    let mut preferred = Vec::new();
    let mut remaining = Vec::new();
    for path in paths {
        if is_likely_skill_path_match(skill_name, path) {
            preferred.push(path.clone());
        } else {
            remaining.push(path.clone());
        }
    }
    (preferred, remaining)
}

/// 上游 `conventionalSkillMdPaths`（顺序有意义：与 tree 之前的探针顺序一致）。
fn conventional_skill_md_paths(skill_name: &str) -> Vec<String> {
    vec![
        format!("skills/{skill_name}/SKILL.md"),
        format!(".claude/skills/{skill_name}/SKILL.md"),
        format!("plugin/skills/{skill_name}/SKILL.md"),
        format!("{skill_name}/SKILL.md"),
    ]
}

/// 上游 `skillMdNotFoundError`。
fn skill_md_not_found_error(owner: &str, repo: &str, skill_name: &str) -> ImportError {
    ImportError::invalid(format!(
        "SKILL.md not found in repository {owner}/{repo} for skill {skill_name}"
    ))
}

/// 上游 `findMatchingSkillDirByFrontmatter`：逐个取 raw `SKILL.md`，frontmatter `name` 逐字相等才认。
async fn find_matching_skill_dir_by_frontmatter(
    client: &reqwest::Client,
    endpoints: &SourceEndpoints,
    raw_prefix: &str,
    skill_name: &str,
    skill_paths: &[String],
) -> Option<(String, String)> {
    for skill_path in skill_paths {
        let Ok(body) = fetch_raw_file(
            client,
            endpoints,
            &build_raw_github_url(raw_prefix, skill_path),
        )
        .await
        else {
            tracing::warn!(path = %skill_path, "github import: fallback SKILL.md fetch failed");
            continue;
        };
        let content = String::from_utf8_lossy(&body).into_owned();
        if parse_skill_frontmatter(&content).name == skill_name {
            return Some((skill_dir_from_skill_file_path(skill_path), content));
        }
    }
    None
}

/// 上游 `acceptConventionalSkillDir`：按路径接受一个常规位置（`skills/<name>/SKILL.md` 等），
/// **不**校验 frontmatter 名 —— 但**永远不收仓库根**，所以「根 SKILL.md 名字与 slug 撞车」
/// 仍只能经 frontmatter 那一波被选中（上游那条撞车修复不被破坏）。
async fn accept_conventional_skill_dir(
    client: &reqwest::Client,
    endpoints: &SourceEndpoints,
    raw_prefix: &str,
    skill_name: &str,
    tree_skill_paths: &[String],
    tree_complete: bool,
) -> Option<(String, String)> {
    let present: HashSet<&str> = tree_skill_paths.iter().map(String::as_str).collect();
    for candidate in conventional_skill_md_paths(skill_name) {
        if tree_complete && !present.contains(candidate.as_str()) {
            // 完整的 tree 已经证明这条路径不存在 ⇒ 不必出网。
            continue;
        }
        let Ok(body) = fetch_raw_file(
            client,
            endpoints,
            &build_raw_github_url(raw_prefix, &candidate),
        )
        .await
        else {
            if tree_complete {
                tracing::warn!(path = %candidate, "skills.sh import: conventional SKILL.md fetch failed");
            }
            continue;
        };
        return Some((
            skill_dir_from_skill_file_path(&candidate),
            String::from_utf8_lossy(&body).into_owned(),
        ));
    }
    None
}

/// 上游 `resolveSkillDirFromTree`（**只保留未截断分支**，见 `github.rs` 文件头的偏离说明）。
///
/// 顺序有意义：先 frontmatter 精确匹配（`preferred` 没命中就看 `remaining`），再退回「按路径接受」。
pub(super) async fn resolve_skill_dir_from_tree(
    client: &reqwest::Client,
    endpoints: &SourceEndpoints,
    raw_prefix: &str,
    owner: &str,
    repo: &str,
    skill_name: &str,
    tree: &[GithubTreeEntry],
) -> Result<(String, String), ImportError> {
    let skill_paths = extract_skill_md_paths(tree);
    let (preferred, remaining) = partition_skill_md_paths(skill_name, &skill_paths);
    if let Some(found) = find_matching_skill_dir_by_frontmatter(
        client, endpoints, raw_prefix, skill_name, &preferred,
    )
    .await
    {
        return Ok(found);
    }
    if let Some(found) = find_matching_skill_dir_by_frontmatter(
        client, endpoints, raw_prefix, skill_name, &remaining,
    )
    .await
    {
        return Ok(found);
    }
    if let Some(found) = accept_conventional_skill_dir(
        client,
        endpoints,
        raw_prefix,
        skill_name,
        &skill_paths,
        true,
    )
    .await
    {
        return Ok(found);
    }
    Err(skill_md_not_found_error(owner, repo, skill_name))
}

// ---------------------------------------------------------------------------
// 支持文件（tree 面，并发 8）
// ---------------------------------------------------------------------------

/// 上游 `treeDownloadConcurrency`。
const TREE_DOWNLOAD_CONCURRENCY: usize = 8;

/// 上游 `addSupportingFilesFromTree`。
///
/// 三条上限里，**单文件**与**总数**用 tree 的 `size` 算术预检（不出网即可拒掉超限的包），
/// 下载时仍会各自带上 1 MiB 的流式兜底（tree 里的 `size` 是 API 报的，不能当授权）。
pub(super) async fn add_supporting_files_from_tree(
    client: &reqwest::Client,
    endpoints: &SourceEndpoints,
    result: &mut ImportedSkill,
    tree: &[GithubTreeEntry],
    raw_prefix: &str,
    skill_dir: &str,
) -> Result<(), ImportError> {
    let base_path = if skill_dir.is_empty() {
        String::new()
    } else {
        format!("{skill_dir}/")
    };

    // 与下载循环 / `add_file` 的过滤条件保持一致：跳过 SKILL.md、LICENSE、二进制素材。
    // 过滤留在这里，下面的算术上限才与「实际会入库的内容」对得上。
    let mut eligible: Vec<(String, String, i64)> = Vec::new();
    for entry in tree {
        if entry.kind != "blob" {
            continue;
        }
        if !base_path.is_empty() && !entry.path.starts_with(&base_path) {
            continue;
        }
        let rel_path = entry
            .path
            .strip_prefix(base_path.as_str())
            .unwrap_or(&entry.path);
        if rel_path.is_empty() {
            continue;
        }
        let lower_base = rel_path.rsplit('/').next().unwrap_or("").to_lowercase();
        if matches!(
            lower_base.as_str(),
            "skill.md" | "license" | "license.txt" | "license.md"
        ) {
            continue;
        }
        if is_likely_binary_file_path(rel_path) {
            continue;
        }
        eligible.push((entry.path.clone(), rel_path.to_string(), entry.size()));
    }
    // 稳定顺序：导入结果与下载时序无关。
    eligible.sort_by(|a, b| a.1.cmp(&b.1));

    // 算术预检：超限的包连一个文件都不用下。
    if eligible.len() > MAX_IMPORT_FILE_COUNT {
        return Err(ImportError::cap(format!(
            "import bundle would contain {} files, exceeding the {MAX_IMPORT_FILE_COUNT} file limit",
            eligible.len()
        )));
    }
    let mut total_size: i64 = 0;
    for (_, rel_path, size) in &eligible {
        if *size > MAX_IMPORT_FILE_SIZE.cast_signed() {
            return Err(ImportError::cap(format!(
                "{rel_path} is {size} bytes, exceeding the {MAX_IMPORT_FILE_SIZE} byte per-file limit"
            )));
        }
        total_size += *size;
    }
    let total_limit = i64::try_from(MAX_IMPORT_TOTAL_SIZE).unwrap_or(i64::MAX);
    if total_size > total_limit {
        return Err(ImportError::cap(format!(
            "import bundle is {total_size} bytes, exceeding the {MAX_IMPORT_TOTAL_SIZE} byte limit"
        )));
    }

    // 并发下载（上限 8）：单条取件失败按旧循环的宽容**跳过**，但**上限违例**致命 ——
    // 静默丢文件会产出一个「看起来合法」的不完整包。
    let mut contents: Vec<Option<String>> = (0..eligible.len()).map(|_| None).collect();
    let mut pending: tokio::task::JoinSet<Result<(usize, Option<String>), ImportError>> =
        tokio::task::JoinSet::new();
    let mut next = 0usize;
    while next < eligible.len() && pending.len() < TREE_DOWNLOAD_CONCURRENCY {
        spawn_tree_download(&mut pending, client, endpoints, raw_prefix, &eligible, next);
        next += 1;
    }
    while let Some(joined) = pending.join_next().await {
        match joined {
            Ok(Ok((index, content))) => contents[index] = content,
            Ok(Err(error)) => {
                pending.abort_all();
                return Err(error);
            }
            Err(_) => {
                // 任务 panic（或运行中被取消）：按「可重试的上游故障」处理，绝不静默少文件。
                pending.abort_all();
                return Err(ImportError::unavailable(
                    "github import: supporting file download aborted",
                ));
            }
        }
        if next < eligible.len() {
            spawn_tree_download(&mut pending, client, endpoints, raw_prefix, &eligible, next);
            next += 1;
        }
    }

    for (index, (_, rel_path, _)) in eligible.iter().enumerate() {
        if let Some(content) = contents[index].take() {
            result.add_file(rel_path.as_str(), content)?;
        }
    }
    Ok(())
}

fn spawn_tree_download(
    pending: &mut tokio::task::JoinSet<Result<(usize, Option<String>), ImportError>>,
    client: &reqwest::Client,
    endpoints: &SourceEndpoints,
    raw_prefix: &str,
    eligible: &[(String, String, i64)],
    index: usize,
) {
    let client = client.clone();
    let endpoints = endpoints.clone();
    let url = build_raw_github_url(raw_prefix, &eligible[index].0);
    let rel_path = eligible[index].1.clone();
    pending.spawn(async move {
        match fetch_raw_file(&client, &endpoints, &url).await {
            Ok(body) => Ok((index, Some(String::from_utf8_lossy(&body).into_owned()))),
            Err(error) => {
                // 上限违例致命（静默截断 = 不完整的包）；单条取件失败按旧循环的宽容跳过。
                if error.is_cap() {
                    return Err(ImportError::cap(format!("github import: {rel_path}: {error}")));
                }
                tracing::warn!(path = %rel_path, error = %error, "github import: file download failed");
                Ok((index, None))
            }
        }
    });
}
