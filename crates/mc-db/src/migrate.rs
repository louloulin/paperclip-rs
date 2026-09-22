//! 迁移 runner。
//!
//! 把 `*.up.sql` 文件按**文件名词干**升序执行，在 `schema_migrations` 表里记账。
//!
//! 与上游 `multica`（`server/cmd/migrate`）**逐字对齐**的四个语义（docs/25 §6.3.1 C1–C3）：
//!
//! 1. **迁移标识 = 文件名词干**（`001_init`、`446_issue_properties_bigm_index`），
//!    记账表形状 = `schema_migrations (version TEXT PRIMARY KEY, applied_at TIMESTAMPTZ)`。
//!    上游 560 个文件只有 513 个互异**数字**版本（30 个数字各带 2–4 个文件），
//!    所以 `BIGINT version` 会撞主键、丢文件；本地旧库的 `BIGINT` 形态因此**不兼容**，
//!    见 [`Migrator::ensure_table`] 的再基线报错。
//! 2. **顺序 = 词干字典序**（不是全路径、不是数字序）：`migrations/compat/535_*` 必须排在
//!    `migrations/upstream/534_*` 之后，而按路径排会把 `compat/` 排到 `upstream/` 前面。
//! 3. **无外层事务**：上游 runner 逐文件直接执行 SQL，不包 `BEGIN`。这不是疏忽 ——
//!    上游有 `CREATE INDEX CONCURRENTLY`（035/067/074/075/078/080/446…），
//!    显式事务里跑不了。代价是**迁移必须幂等**：中途失败的文件整体重放。
//! 4. **条件迁移**：本机缺 `pg_bigm` 时 `446_issue_properties_bigm_index` 的
//!    `gin_bigm_ops` opclass 建不出来，但**版本照样记账**（上游 `whenOperatorClassAvailable`
//!    的等价物，见 [`OPERATOR_CLASS_GATES`]）。缺失即跳过，绝不静默 —— 每个 gate 都必须
//!    在加载集合里命中一个文件，否则 `run` 直接报错。
//!
//! 文件名规范：`<digits>_<name>.up.sql`；配套 `.down.sql` 不在运行时执行。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use sqlx::{Executor, Postgres};
use tracing::{info, warn};

use crate::pool::Db;

/// 会话级 advisory lock 的键：同一时刻只允许一个 runner 应用迁移。
///
/// 常量本身没有魔力，稳定性才是重点：不同进程必须算出同一个值。
/// （`multica` 的 ASCII 十六进制 6d756c74696361 = "multica" 后接序号 1。）
const MIGRATION_LOCK_KEY: i64 = 0x6d75_6c74_6963_6101;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MigrationStatus {
    Pending,
    Applied,
    Failed,
}

/// 单个迁移步骤。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationStep {
    /// 文件名去掉 `.up.sql`：上游 runner 的 `migrations.ExtractVersion()`，也是记账主键。
    pub version: String,
    /// `version` 里第一个 `_` 之后的部分（仅用于日志/报告）。
    pub name: String,
    pub sql: String,
    pub source: Option<String>,
}

impl MigrationStep {
    pub fn from_file(path: &Path) -> std::io::Result<Self> {
        let filename = path.file_name().and_then(|s| s.to_str()).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "migration path has no filename",
            )
        })?;
        let version = parse_filename(filename).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("invalid migration filename: {filename}"),
            )
        })?;
        let sql = std::fs::read_to_string(path)?;
        let name = version
            .split_once('_')
            .map_or_else(|| version.clone(), |(_, rest)| rest.to_string());
        Ok(Self {
            version,
            name,
            sql,
            source: Some(path.display().to_string()),
        })
    }
}

/// 文件名词干 = 迁移标识；不合规范（非 `<digits>_<name>.up.sql`）返回 `None`。
fn parse_filename(filename: &str) -> Option<String> {
    let stem = filename.strip_suffix(".up.sql")?;
    let (digits, name) = stem.split_once('_')?;
    if digits.is_empty() || name.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(stem.to_string())
}

/// 一条「扩展/操作符类不可用时跳过」的条件迁移（上游 `whenOperatorClassAvailable` 的等价物）。
#[derive(Debug, Clone, Copy)]
pub struct OperatorClassGate {
    /// 迁移词干，必须与加载集合里的某个文件**逐字**相同。
    pub version: &'static str,
    /// 依赖的扩展名。
    pub extension: &'static str,
    /// 访问方法（`gin` / `btree` …）。
    pub access_method: &'static str,
    /// 操作符类名。
    pub operator_class: &'static str,
}

/// 本仓已知的条件迁移。**每一条都必须在加载集合里命中文件**，否则 `run` 报错
/// （上游的教训：指向不存在对象的 hook 是静默 no-op）。
pub const OPERATOR_CLASS_GATES: &[OperatorClassGate] = &[OperatorClassGate {
    version: "446_issue_properties_bigm_index",
    extension: "pg_bigm",
    access_method: "gin",
    operator_class: "gin_bigm_ops",
}];

fn gate_for(version: &str) -> Option<&'static OperatorClassGate> {
    OPERATOR_CLASS_GATES.iter().find(|g| g.version == version)
}

/// 迁移 runner。
#[derive(Default, Clone)]
pub struct Migrator;

impl Migrator {
    /// 确保 `schema_migrations` 是上游形状（`version TEXT PRIMARY KEY`）。
    ///
    /// 存量库（W0-B2 之前的 `BIGINT version` + `name`/`source` 记账表）**无法原地升级**：
    /// 那批库的表是本仓 28 张本地子集（`comment.body` vs 上游 `comment.content`、
    /// `text` 型 `actor_id` vs 上游 `uuid` …），上游 560 个迁移会在 `CREATE TABLE` 上撞车。
    /// 所以这里给的是**明确的再基线报错**，而不是把两种形状混着用。
    pub async fn ensure_table(db: &Db) -> Result<(), crate::DbError> {
        let data_type: Option<String> = sqlx::query_scalar(
            "SELECT data_type FROM information_schema.columns \
             WHERE table_schema = current_schema() \
               AND table_name = 'schema_migrations' AND column_name = 'version'",
        )
        .fetch_optional(db.pool())
        .await?;

        match data_type.as_deref() {
            Some("text") => Ok(()),
            Some(other) => Err(legacy_ledger_error(other)),
            None => {
                let exists: bool =
                    sqlx::query_scalar("SELECT to_regclass('schema_migrations') IS NOT NULL")
                        .fetch_one(db.pool())
                        .await?;
                if exists {
                    return Err(legacy_ledger_error("no `version` column"));
                }
                sqlx::query(
                    "CREATE TABLE IF NOT EXISTS schema_migrations ( \
                         version TEXT PRIMARY KEY, \
                         applied_at TIMESTAMPTZ NOT NULL DEFAULT now() \
                     )",
                )
                .execute(db.pool())
                .await?;
                Ok(())
            }
        }
    }

    /// 应用给定的迁移列表（按词干升序，跳过已应用的）。返回本次应用的条数。
    ///
    /// 全程持有一把会话级 advisory lock：并发 runner（例如两个 daemon 同时启动）
    /// 不会互相踩；锁在同一个连接上显式释放，连接归还池子时不带走锁。
    pub async fn run(db: &Db, steps: Vec<MigrationStep>) -> Result<usize, crate::DbError> {
        if steps.is_empty() {
            warn!("no migrations loaded; nothing to apply");
            return Ok(0);
        }
        Self::ensure_table(db).await?;
        for gate in OPERATOR_CLASS_GATES {
            if !steps.iter().any(|s| s.version == gate.version) {
                return Err(crate::DbError::MigrationManifest(format!(
                    "conditional migration {} is not in the loaded set — the gate would be a silent no-op",
                    gate.version
                )));
            }
        }

        let mut conn = db.pool().acquire().await?;
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(MIGRATION_LOCK_KEY)
            .execute(&mut *conn)
            .await?;
        let result = Self::run_locked(&mut conn, steps).await;
        // 无论成败都解锁：池子归还的是**同一个会话**，锁漏掉就等于把库锁死。
        let unlock = sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(MIGRATION_LOCK_KEY)
            .execute(&mut *conn)
            .await;
        if let Err(e) = unlock {
            warn!(error = %e, "releasing the migration advisory lock failed");
        }
        result
    }

    async fn run_locked(
        conn: &mut sqlx::PgConnection,
        steps: Vec<MigrationStep>,
    ) -> Result<usize, crate::DbError> {
        let applied = applied_on(&mut *conn).await?;
        let applied: std::collections::HashSet<String> = applied.into_iter().collect();

        let mut pending: Vec<MigrationStep> = steps
            .into_iter()
            .filter(|s| !applied.contains(&s.version))
            .collect();
        pending.sort_by(|a, b| a.version.cmp(&b.version));

        if pending.is_empty() {
            info!("no pending migrations");
            return Ok(0);
        }
        info!(count = pending.len(), "applying migrations");

        let mut applied_now = 0usize;
        for step in pending {
            Self::apply(conn, &step).await?;
            applied_now += 1;
        }
        Ok(applied_now)
    }

    async fn apply(
        conn: &mut sqlx::PgConnection,
        step: &MigrationStep,
    ) -> Result<(), crate::DbError> {
        let start = Instant::now();
        let mut skipped_conditionally = false;

        if let Some(gate) = gate_for(&step.version) {
            let available = operator_class_available(conn, gate).await?;
            if !available {
                warn!(
                    version = %step.version,
                    operator_class = gate.operator_class,
                    extension = gate.extension,
                    "skipping migration: operator class unavailable; version is recorded anyway"
                );
                skipped_conditionally = true;
            }
        }

        let mut statements = 0usize;
        if !skipped_conditionally {
            // 逐句执行、**不包事务**（CONCURRENTLY 的要求，见模块头）。
            for stmt in split_sql_statements(&step.sql) {
                let trimmed = stmt.trim();
                if trimmed.is_empty() {
                    continue;
                }
                statements += 1;
                if let Err(e) = conn.execute(sqlx::raw_sql(trimmed)).await {
                    warn!(
                        version = %step.version,
                        error = %e,
                        statement = trimmed.lines().next().unwrap_or(""),
                        "statement failed; the version is not recorded, so the file replays on the next run"
                    );
                    return Err(crate::DbError::Pool(format!(
                        "migration {} failed: {e}",
                        step.version
                    )));
                }
            }
        }

        sqlx::query(
            "INSERT INTO schema_migrations(version) VALUES ($1) ON CONFLICT (version) DO NOTHING",
        )
        .bind(&step.version)
        .execute(&mut *conn)
        .await?;

        info!(
            version = %step.version,
            name = %step.name,
            elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX),
            statements,
            skipped_conditionally,
            "migration applied"
        );
        Ok(())
    }

    /// 列出已应用的迁移词干（升序）。
    pub async fn list_applied(db: &Db) -> Result<Vec<String>, sqlx::Error> {
        applied_on(db.pool()).await
    }

    /// 上游就绪语义（`AllVersions()` 的等价物）：**已加载的每个版本都必须已记账**。
    ///
    /// 上游用它防「编号低于已应用版本的乱序补丁漏记录」；只比条数（`list_applied().len()`）
    /// 会漏掉这种情况。返回尚未记账的版本。
    pub async fn pending(db: &Db, steps: &[MigrationStep]) -> Result<Vec<String>, sqlx::Error> {
        let applied: std::collections::HashSet<String> =
            applied_on(db.pool()).await?.into_iter().collect();
        let mut pending: Vec<String> = steps
            .iter()
            .filter(|s| !applied.contains(&s.version))
            .map(|s| s.version.clone())
            .collect();
        pending.sort();
        Ok(pending)
    }

    /// 递归加载 `root` 下的所有 `*.up.sql`，按**词干**排序；词干重复 → 报错。
    ///
    /// 递归是必要的：运行时集合 = `migrations/upstream/`（560 个上游逐字文件）
    /// + `migrations/compat/`（本仓补丁，`535_` 起），两处都在 `migrations/` 之下。
    ///
    /// 重复词干（例如误把上游文件抄进 compat）必须炸掉，而不是靠加载顺序决定谁生效。
    pub fn load_dir(root: &Path) -> std::io::Result<Vec<MigrationStep>> {
        Self::load_dirs(&[root.to_path_buf()])
    }

    fn collect(root: &Path) -> std::io::Result<Vec<MigrationStep>> {
        if !root.exists() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("migrations directory not found: {}", root.display()),
            ));
        }

        let mut paths = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let entries = std::fs::read_dir(&dir)?;
            for entry in entries {
                let path = entry?.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|f| f.ends_with(".up.sql"))
                {
                    paths.push(path);
                }
            }
        }
        // 目录枚举顺序不定；先排一次序，让重复词干的报错文本稳定。
        paths.sort();
        paths
            .into_iter()
            .map(|p| MigrationStep::from_file(&p))
            .collect()
    }

    /// 把多个目录合并成一个迁移集合（跨目录同样禁止重复词干）。
    pub fn load_dirs(dirs: &[PathBuf]) -> std::io::Result<Vec<MigrationStep>> {
        let mut steps = Vec::new();
        for dir in dirs {
            steps.extend(Self::collect(dir)?);
        }
        let mut by_version: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for step in &steps {
            by_version
                .entry(step.version.clone())
                .or_default()
                .push(step.source.clone().unwrap_or_default());
        }
        if let Some((version, files)) = by_version.iter().find(|(_, f)| f.len() > 1) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "duplicate migration version {version}: {} — migration keys must be unique",
                    files.join(", ")
                ),
            ));
        }
        steps.sort_by(|a, b| a.version.cmp(&b.version));
        Ok(steps)
    }

    /// 把 migrations 目录打包成单个 Vec<MigrationStep>：可放进 `mc-server` 主流程。
    pub fn load(root: impl AsRef<Path>) -> std::io::Result<Vec<MigrationStep>> {
        Self::load_dir(root.as_ref())
    }
}

fn legacy_ledger_error(found: &str) -> crate::DbError {
    crate::DbError::MigrationManifest(format!(
        "schema_migrations is not the upstream ledger (version column: {found}; expected TEXT). \
         This database was migrated by the pre-W0-B2 runner (local 0001–0004, 28 tables with local \
         column shapes such as comment.body / text-typed actor_id). That shape cannot be upgraded in \
         place: the upstream set re-creates those tables from scratch. Rebuild the database instead \
         (see docs/26-SCHEMA-SWITCHOVER.md §3), e.g. \
         `DROP DATABASE <db>; CREATE DATABASE <db>;` then re-run `mc-migrate run --dir migrations`."
    ))
}

async fn applied_on<'e, E>(executor: E) -> Result<Vec<String>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_scalar::<_, String>("SELECT version FROM schema_migrations ORDER BY version")
        .fetch_all(executor)
        .await
}

async fn operator_class_available(
    conn: &mut sqlx::PgConnection,
    gate: &OperatorClassGate,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = $1) \
           AND EXISTS ( \
                 SELECT 1 FROM pg_opclass o JOIN pg_am a ON a.oid = o.opcmethod \
                 WHERE o.opcname = $2 AND a.amname = $3 \
               )",
    )
    .bind(gate.extension)
    .bind(gate.operator_class)
    .bind(gate.access_method)
    .fetch_one(conn)
    .await
}

// --------------------------------------------------------------------------- #
// SQL 切分
// --------------------------------------------------------------------------- #

/// 把一份迁移文件切成语句：忽略 `--` / `/* */`（可嵌套）注释与字符串/标识符里的 `;`，
/// 并**正确识别 `$tag$ … $tag$` 美元引体**（上游大量 `DO $$ … ; … $$;` 块，
/// 旧实现把 `$$` 当成「一个 `$` 进入、下一个 `$` 退出」，块内的 `;` 会被误切）。
fn split_sql_statements(input: &str) -> Vec<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                let mut depth = 1usize;
                while i < bytes.len() && depth > 0 {
                    if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        i += 2;
                    } else if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            b'\'' => i = skip_single_quoted(input, i),
            b'"' => i = skip_double_quoted(input, i),
            b'$' => match dollar_quote_at(input, i) {
                Some((tag, open_len)) => {
                    let close = format!("${tag}$");
                    i = input[i + open_len..]
                        .find(&close)
                        .map_or(bytes.len(), |off| i + open_len + off + close.len());
                }
                None => i += 1,
            },
            b';' => {
                if has_sql_content(&input[start..i]) {
                    out.push(input[start..i].to_string());
                }
                i += 1;
                start = i;
            }
            _ => i += 1,
        }
    }
    if has_sql_content(&input[start..]) {
        out.push(input[start..].to_string());
    }
    out
}

/// `i` 处若是 `$tag$` 美元引体开头，返回 `(tag, 长度)`；`$1` 之类占位符返回 `None`。
fn dollar_quote_at(input: &str, i: usize) -> Option<(&str, usize)> {
    let bytes = input.as_bytes();
    let mut j = i + 1;
    while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
        j += 1;
    }
    if bytes.get(j) == Some(&b'$') {
        Some((&input[i + 1..j], j - i + 1))
    } else {
        None
    }
}

/// 从单引号串的起始位置跳到串尾之后（`''` 转义；`E'…'` 里的反斜杠转义也认）。
fn skip_single_quoted(input: &str, i: usize) -> usize {
    let bytes = input.as_bytes();
    let escape_string = i > 0
        && matches!(bytes[i - 1], b'e' | b'E')
        && (i < 2 || !(bytes[i - 2].is_ascii_alphanumeric() || bytes[i - 2] == b'_'));
    let mut j = i + 1;
    while j < bytes.len() {
        match bytes[j] {
            b'\\' if escape_string => j += 2,
            b'\'' if bytes.get(j + 1) == Some(&b'\'') => j += 2,
            b'\'' => return j + 1,
            _ => j += 1,
        }
    }
    j
}

/// 从双引号标识符的起始位置跳到其闭合引号之后（`""` 转义）。
fn skip_double_quoted(input: &str, i: usize) -> usize {
    let bytes = input.as_bytes();
    let mut j = i + 1;
    while j < bytes.len() {
        match bytes[j] {
            b'"' if bytes.get(j + 1) == Some(&b'"') => j += 2,
            b'"' => return j + 1,
            _ => j += 1,
        }
    }
    j
}

/// 判断切分出的语句是否含非注释、非空白内容（纯注释残留不算语句）。
fn has_sql_content(stmt: &str) -> bool {
    let bytes = stmt.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                i += 2;
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                let mut depth = 1usize;
                while i < bytes.len() && depth > 0 {
                    if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        i += 2;
                    } else if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            b'\'' => i = skip_single_quoted(stmt, i),
            b'"' => i = skip_double_quoted(stmt, i),
            b'$' => match dollar_quote_at(stmt, i) {
                Some((tag, open_len)) => {
                    let close = format!("${tag}$");
                    i = stmt[i + open_len..]
                        .find(&close)
                        .map_or(bytes.len(), |off| i + open_len + off + close.len());
                }
                None => i += 1,
            },
            c if c.is_ascii_whitespace() => i += 1,
            _ => return true,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_filename_uses_the_file_stem_as_the_key() {
        assert_eq!(
            parse_filename("0001_init.up.sql").as_deref(),
            Some("0001_init")
        );
        assert_eq!(
            parse_filename("0240_chat_explicit_origin_backfill.up.sql").as_deref(),
            Some("0240_chat_explicit_origin_backfill")
        );
    }

    #[test]
    fn parse_filename_accepts_upstream_multi_file_versions() {
        // 上游 30 个数字版本各带 2–4 个文件：词干才是身份（109 一档就有 4 个）。
        let a = parse_filename("109_agent_task_waiting_local_directory.up.sql");
        let b = parse_filename("109_drop_agent_skills_local.up.sql");
        assert_eq!(a.as_deref(), Some("109_agent_task_waiting_local_directory"));
        assert_eq!(b.as_deref(), Some("109_drop_agent_skills_local"));
        assert_ne!(a, b);
    }

    #[test]
    fn parse_filename_rejects_down_and_junk() {
        assert!(parse_filename("0001_init.down.sql").is_none());
        assert!(parse_filename("init.up.sql").is_none());
        assert!(parse_filename("0001_.up.sql").is_none());
        assert!(parse_filename("0001_init.up.sql.bak").is_none());
    }

    #[test]
    fn ordering_is_lexicographic_on_the_stem() {
        // 词干序（不是数字序）：001_init < 020_task_session < 109_a < 535_pat…
        let mut v = [
            "535_pat_revoked_at",
            "020_task_session",
            "109_agent_task_waiting_local_directory",
            "001_init",
            "109_drop_agent_skills_local",
        ];
        v.sort_unstable();
        assert_eq!(
            v,
            [
                "001_init",
                "020_task_session",
                "109_agent_task_waiting_local_directory",
                "109_drop_agent_skills_local",
                "535_pat_revoked_at",
            ]
        );
    }

    #[test]
    fn every_declared_gate_exists_in_the_vendored_set_and_names_its_opclass() {
        // gate 指向不存在的文件 = 静默 no-op；gate 指向改了内容的文件 = 白跳过。
        let upstream = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../migrations/upstream");
        let steps = Migrator::load_dir(&upstream).expect("load vendored upstream migrations");
        for gate in OPERATOR_CLASS_GATES {
            let step = steps
                .iter()
                .find(|s| s.version == gate.version)
                .unwrap_or_else(|| panic!("{} is missing from migrations/upstream", gate.version));
            assert!(
                step.sql.contains(gate.operator_class),
                "{} no longer mentions {} — the conditional gate is stale",
                gate.version,
                gate.operator_class
            );
        }
    }

    #[test]
    fn load_dir_merges_subdirectories_by_stem() {
        let root = std::env::temp_dir().join(format!("mc-db-load-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("upstream")).unwrap();
        std::fs::create_dir_all(root.join("compat")).unwrap();
        std::fs::write(root.join("upstream/002_b.up.sql"), "SELECT 2;").unwrap();
        std::fs::write(root.join("upstream/001_a.up.sql"), "SELECT 1;").unwrap();
        std::fs::write(root.join("compat/535_c.up.sql"), "SELECT 535;").unwrap();
        // 非迁移文件必须被忽略
        std::fs::write(root.join("upstream/MANIFEST.sha256"), "deadbeef").unwrap();
        std::fs::write(root.join("upstream/001_a.down.sql"), "SELECT 0;").unwrap();

        let steps = Migrator::load_dir(&root).unwrap();
        let versions: Vec<&str> = steps.iter().map(|s| s.version.as_str()).collect();
        assert_eq!(versions, ["001_a", "002_b", "535_c"]);
        assert_eq!(steps[2].name, "c");

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn load_dir_rejects_duplicate_stems() {
        let root = std::env::temp_dir().join(format!("mc-db-dup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("upstream")).unwrap();
        std::fs::create_dir_all(root.join("compat")).unwrap();
        std::fs::write(root.join("upstream/001_a.up.sql"), "SELECT 1;").unwrap();
        std::fs::write(root.join("compat/001_a.up.sql"), "SELECT 1;").unwrap();

        let err = Migrator::load_dir(&root).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(err
            .to_string()
            .contains("duplicate migration version 001_a"));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn load_dir_rejects_a_bad_filename() {
        let root = std::env::temp_dir().join(format!("mc-db-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("init.up.sql"), "SELECT 1;").unwrap();
        assert!(Migrator::load_dir(&root).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn split_sql_handles_multiple_statements() {
        let sql = "CREATE TABLE foo (id INT); CREATE TABLE bar (id INT);";
        let stmts = split_sql_statements(sql);
        assert_eq!(stmts.len(), 2);
        assert!(stmts[0].contains("foo"));
        assert!(stmts[1].contains("bar"));
    }

    #[test]
    fn split_sql_ignores_semicolon_inside_strings() {
        let stmts = split_sql_statements("INSERT INTO t (x) VALUES ('a;b');");
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].contains("'a;b'"));
    }

    #[test]
    fn split_sql_handles_escaped_quotes() {
        let stmts = split_sql_statements("INSERT INTO t (x) VALUES ('it''s; fine');SELECT 1;");
        assert_eq!(stmts.len(), 2, "{stmts:?}");
    }

    #[test]
    fn split_sql_strips_comments() {
        let stmts =
            split_sql_statements("-- comment; still comment\nCREATE TABLE t (id INT); -- trailing");
        assert_eq!(stmts.len(), 1);
        assert!(stmts[0].contains("CREATE TABLE"));
        assert_eq!(split_sql_statements("/* hi */ ;").len(), 0);
    }

    #[test]
    fn split_sql_keeps_do_blocks_whole() {
        // 上游 032/033/036/039/076 全是这个形状：块内的 `;` 不能切。
        let sql = "DO $$ BEGIN\n  CREATE INDEX a ON t (x);\n  DROP INDEX IF EXISTS b;\nEXCEPTION WHEN OTHERS THEN\n  RAISE NOTICE 'skip';\nEND $$;\nSELECT 1;";
        let stmts = split_sql_statements(sql);
        assert_eq!(stmts.len(), 2, "{stmts:?}");
        assert!(stmts[0].contains("RAISE NOTICE"));
    }

    #[test]
    fn split_sql_handles_tagged_dollar_quotes() {
        let sql = "CREATE FUNCTION f() RETURNS void AS $fn$ BEGIN PERFORM 1; END; $fn$ LANGUAGE plpgsql;SELECT 2;";
        let stmts = split_sql_statements(sql);
        assert_eq!(stmts.len(), 2, "{stmts:?}");
        assert!(stmts[0].contains("$fn$"));
    }

    #[test]
    fn split_sql_handles_nested_block_comments() {
        let sql = "/* outer /* inner; */ still comment */ SELECT 1;";
        let stmts = split_sql_statements(sql);
        assert_eq!(stmts.len(), 1, "{stmts:?}");
    }
}
