//! project 搜索的 SQL 构造与执行（M4-1 / LUM-1472）。
//!
//! 归属：`crate::project` 的子模块。**上游 `buildProjectSearchQuery` 的逐行移植** ——
//! `$N` 编号顺序、排名 tier 顺序、`cancelled` 降权、`include_closed` 过滤都逐条对齐
//! （`server/internal/handler/project.go` `SearchProjects` +
//! `server/internal/handler/search.go` `runSearchQuery`）。
//!
//! 拆出本文件的唯一原因是 `docs/plan1.md` R7 的 800 行/文件硬上限（门 ⑩）。

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::FromRow;
use uuid::Uuid;

use mc_core::Id;
use mc_db::Db;

use super::{
    is_sqlstate, map_sqlx_err, ProjectRow, ProjectSearchError, ProjectSearchHit, PROJECT_COLUMNS,
    SEARCH_DEFAULT_WORK_MEM_MB, SEARCH_STATEMENT_TIMEOUT_MS, SEARCH_WORK_MEM_ENV,
};



// ---------------------------------------------------------------------------
// 搜索 SQL 构造（上游 `buildProjectSearchQuery` 的逐条移植）
// ---------------------------------------------------------------------------

/// 绑定值（搜索 SQL 的 `$N` 按构造顺序编号，绑定顺序必须一致）。
#[derive(Debug, Clone)]
pub(super) enum SearchArg {
    Text(String),
    Uuid(Uuid),
    Int(i64),
}

/// `$N` 编号器 + 入参收集（上游 `nextArg` 闭包）。
struct SearchSql {
    args: Vec<SearchArg>,
}

impl SearchSql {
    fn new() -> Self {
        Self { args: Vec::new() }
    }

    fn push(&mut self, arg: SearchArg) -> String {
        self.args.push(arg);
        format!("${}", self.args.len())
    }

    fn text(&mut self, value: String) -> String {
        self.push(SearchArg::Text(value))
    }

    fn uuid(&mut self, value: Uuid) -> String {
        self.push(SearchArg::Uuid(value))
    }

    fn int(&mut self, value: i64) -> String {
        self.push(SearchArg::Int(value))
    }
}

/// LIKE 通配符转义（上游 `escapeLike`：`\`、`%`、`_`）。
#[must_use]
pub fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// 按 unicode 空白切词并丢掉空串（上游 `splitSearchTerms`）。
#[must_use]
pub fn split_search_terms(q: &str) -> Vec<String> {
    q.split_whitespace()
        .filter(|t| !t.is_empty())
        .map(ToString::to_string)
        .collect()
}

/// 多词查询里每个词都要命中 title / description（上游 tier-3 与 `termConditions` 共用）。
fn term_conditions(terms: &[String]) -> String {
    let parts: Vec<String> = terms
        .iter()
        .map(|tp| {
            format!(
                "(LOWER(p.title) LIKE '%' || {tp} || '%' OR \
                 LOWER(COALESCE(p.description, '')) LIKE '%' || {tp} || '%')"
            )
        })
        .collect();
    parts.join(" AND ")
}

/// title 命中**全部**词（上游 `titleTerms` 拼出的 AND 串）。
fn title_term_conditions(terms: &[String]) -> String {
    let parts: Vec<String> = terms
        .iter()
        .map(|tp| format!("LOWER(p.title) LIKE '%' || {tp} || '%'"))
        .collect();
    parts.join(" AND ")
}

/// 构造 project 搜索 SQL + 绑定值（上游 `buildProjectSearchQuery` 的逐行移植）。
///
/// 返回的 `args` 顺序与 SQL 的 `$N` 顺序严格一致：`$1` 短语、`$2` workspace、多词时
/// `$3..` 各词、最后两个是 limit / offset。
pub(super) fn build_project_search_query(phrase: &str, terms: &[String], include_closed: bool) -> (String, Vec<SearchArg>) {
    let phrase = phrase.to_lowercase();
    let terms: Vec<String> = terms.iter().map(|t| t.to_lowercase()).collect();

    let mut q = SearchSql::new();
    let escaped_phrase = escape_like(&phrase);
    let phrase_param = q.text(escaped_phrase);
    let phrase_contains = format!("'%' || {phrase_param} || '%'");
    let phrase_starts_with = format!("{phrase_param} || '%'");
    // workspace 占位符先占号，值由调用方在构造完成后填入（上游 `args[1] = wsUUID`）。
    let ws_param = q.uuid(Uuid::nil());

    let mut term_params: Vec<String> = Vec::new();
    if terms.len() > 1 {
        for t in &terms {
            let et = escape_like(t);
            term_params.push(q.text(et));
        }
    }

    let mut where_parts = vec![format!(
        "(LOWER(p.title) LIKE {phrase_contains} OR \
         LOWER(COALESCE(p.description, '')) LIKE {phrase_contains})"
    )];
    if term_params.len() > 1 {
        where_parts.push(format!("({})", term_conditions(&term_params)));
    }
    let mut where_clause = format!("({})", where_parts.join(" OR "));
    if !include_closed {
        where_clause.push_str(" AND p.status NOT IN ('completed', 'cancelled')");
    }

    let mut rank_cases = vec![
        format!("WHEN LOWER(p.title) = {phrase_param} THEN 0"),
        format!("WHEN LOWER(p.title) LIKE {phrase_starts_with} THEN 1"),
        format!("WHEN LOWER(p.title) LIKE {phrase_contains} THEN 2"),
    ];
    if term_params.len() > 1 {
        rank_cases.push(format!("WHEN ({}) THEN 3", title_term_conditions(&term_params)));
    }
    rank_cases.push(format!(
        "WHEN LOWER(COALESCE(p.description, '')) LIKE {phrase_contains} THEN 4"
    ));
    let rank_expr = format!("CASE {} ELSE 5 END", rank_cases.join(" "));

    // 已取消的 project 是废弃工作：直接命中（title 全等）除外，整体降权。
    let cancelled_rank = format!(
        "CASE WHEN p.status = 'cancelled' AND LOWER(p.title) <> {phrase_param} THEN 1 ELSE 0 END"
    );

    let match_source_expr = if term_params.len() > 1 {
        format!(
            "CASE WHEN LOWER(p.title) LIKE {phrase_contains} THEN 'title' \
             WHEN ({}) THEN 'title' ELSE 'description' END",
            title_term_conditions(&term_params)
        )
    } else {
        format!(
            "CASE WHEN LOWER(p.title) LIKE {phrase_contains} THEN 'title' ELSE 'description' END"
        )
    };

    let limit_param = q.int(0);
    let offset_param = q.int(0);

    let sql = format!(
        "SELECT {PROJECT_COLUMNS}, {match_source_expr} AS match_source \
         FROM project p \
         WHERE p.workspace_id = {ws_param} AND {where_clause} \
         ORDER BY {cancelled_rank}, {rank_expr}, p.updated_at DESC \
         LIMIT {limit_param} OFFSET {offset_param}"
    );

    (sql, q.args)
}

/// 搜索结果行（`PROJECT_COLUMNS` + `match_source`；project 列名与 `project` 表同名）。
#[derive(Debug, Clone, FromRow)]
struct ProjectSearchDbRow {
    id: Uuid,
    workspace_id: Uuid,
    title: String,
    description: Option<String>,
    icon: Option<String>,
    status: String,
    priority: String,
    lead_type: Option<String>,
    lead_id: Option<Uuid>,
    start_date: Option<NaiveDate>,
    due_date: Option<NaiveDate>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    match_source: String,
}

impl ProjectSearchDbRow {
    fn into_hit(self) -> ProjectSearchHit {
        ProjectSearchHit {
            project: ProjectRow {
                id: self.id,
                workspace_id: self.workspace_id,
                title: self.title,
                description: self.description,
                icon: self.icon,
                status: self.status,
                priority: self.priority,
                lead_type: self.lead_type,
                lead_id: self.lead_id,
                start_date: self.start_date,
                due_date: self.due_date,
                created_at: self.created_at,
                updated_at: self.updated_at,
            },
            match_source: self.match_source,
        }
    }
}

/// 在内容里取一段围绕 query 首次出现的片段（上游 `extractSnippet`）。
///
/// 用**字符**（rune）切片，不会切断多字节 UTF-8（CJK 内容必需）；多词查询先试短语，
/// 短语没命中就取最早出现的单个词；都没有则截断前 120 字符并加 `...`。
#[must_use]
pub fn extract_snippet(content: &str, query: &str) -> String {
    let runes: Vec<char> = content.chars().collect();
    let lower_runes: Vec<char> = content.to_lowercase().chars().collect();
    let query_runes: Vec<char> = query.to_lowercase().chars().collect();

    let mut idx = find_char_slice(&lower_runes, &query_runes);
    let mut match_len = query_runes.len();

    if idx.is_none() {
        let terms: Vec<String> = query
            .to_lowercase()
            .split_whitespace()
            .map(ToString::to_string)
            .collect();
        if terms.len() > 1 {
            let mut earliest: Option<(usize, usize)> = None;
            for term in &terms {
                let term_runes: Vec<char> = term.chars().collect();
                if let Some(pos) = find_char_slice(&lower_runes, &term_runes) {
                    if earliest.is_none_or(|(best, _)| pos < best) {
                        earliest = Some((pos, term_runes.len()));
                    }
                }
            }
            if let Some((pos, len)) = earliest {
                idx = Some(pos);
                match_len = len;
            }
        }
    }

    let Some(idx) = idx else {
        return if runes.len() > 120 {
            format!("{}...", runes[..120].iter().collect::<String>())
        } else {
            content.to_string()
        };
    };

    let start = idx.saturating_sub(40);
    let end = (idx + match_len + 80).min(runes.len());
    let mut snippet: String = runes[start..end].iter().collect();
    if start > 0 {
        snippet = format!("...{snippet}");
    }
    if end < runes.len() {
        snippet.push_str("...");
    }
    snippet
}

/// 子串查找（按字符），返回起始下标（上游 `findRuneSubstring`）。
fn find_char_slice(haystack: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| haystack[i..i + needle.len()] == *needle)
}

/// `DATABASE_SEARCH_WORK_MEM_MB` → `SET LOCAL work_mem` 的值（上游 `searchWorkMemValue`）。
///
/// 空 = 默认 64；非法或超出 `0..=64` = 默认 64 并告警；`0` = 不使用（`None`）。
fn search_work_mem_value() -> Option<String> {
    let raw = std::env::var(SEARCH_WORK_MEM_ENV).unwrap_or_default();
    let trimmed = raw.trim();
    let parsed = trimmed.parse::<u32>();
    let mb = match parsed {
        Ok(v) if v <= SEARCH_DEFAULT_WORK_MEM_MB => v,
        _ => {
            if !trimmed.is_empty() {
                tracing::warn!(
                    name = SEARCH_WORK_MEM_ENV,
                    value = trimmed,
                    default_mb = SEARCH_DEFAULT_WORK_MEM_MB,
                    "invalid search work_mem; using default"
                );
            }
            SEARCH_DEFAULT_WORK_MEM_MB
        }
    };
    if mb == 0 {
        None
    } else {
        Some(format!("{mb}MB"))
    }
}

/// 搜索事务的 `SET LOCAL` 语句（顺序与上游一致：timeout → `work_mem` → read-only）。
pub(super) fn search_setup_statements() -> Vec<String> {
    let mut stmts = vec![format!(
        "SET LOCAL statement_timeout = {}",
        SEARCH_STATEMENT_TIMEOUT_MS
    )];
    if let Some(work_mem) = search_work_mem_value() {
        // 只由上面解析出的有界整数拼出，插值不会引入 SQL 语法或用户输入。
        stmts.push(format!("SET LOCAL work_mem = '{work_mem}'"));
    }
    stmts.push("SET LOCAL transaction_read_only = on".to_string());
    stmts
}

pub(super) fn search_map_err(err: sqlx::Error) -> ProjectSearchError {
    if is_sqlstate(&err, "57014") {
        ProjectSearchError::Timeout
    } else {
        ProjectSearchError::Repo(map_sqlx_err(err))
    }
}

/// 执行搜索：短命只读事务 + 事务级 `SET LOCAL`（上游 `runSearchQuery`）。
pub(super) async fn run_search(
    db: &Db,
    workspace_id: Id,
    query: &str,
    limit: i64,
    offset: i64,
    include_closed: bool,
) -> std::result::Result<Vec<ProjectSearchHit>, ProjectSearchError> {
    let terms = split_search_terms(query);
    let (sql, mut args) = build_project_search_query(query, &terms, include_closed);
    // 占位符先占号、值后填：$2 = workspace，末尾两个 = limit / offset（上游 `args[1] = wsUUID`）。
    if args.len() >= 2 {
        args[1] = SearchArg::Uuid(workspace_id.0);
    }
    let tail = args.len();
    if tail >= 2 {
        args[tail - 2] = SearchArg::Int(limit);
        args[tail - 1] = SearchArg::Int(offset);
    }

    let mut tx = db
        .pool()
        .begin()
        .await
        .map_err(|e| ProjectSearchError::Repo(map_sqlx_err(e)))?;

    for stmt in search_setup_statements() {
        sqlx::query(&stmt)
            .execute(&mut *tx)
            .await
            .map_err(search_map_err)?;
    }

    let mut q = sqlx::query_as::<_, ProjectSearchDbRow>(&sql);
    for arg in &args {
        q = match arg {
            SearchArg::Text(v) => q.bind(v.clone()),
            SearchArg::Uuid(v) => q.bind(*v),
            SearchArg::Int(v) => q.bind(*v),
        };
    }
    let rows = q.fetch_all(&mut *tx).await.map_err(search_map_err)?;

    tx.commit()
        .await
        .map_err(|e| ProjectSearchError::Repo(map_sqlx_err(e)))?;

    Ok(rows.into_iter().map(ProjectSearchDbRow::into_hit).collect())
}
