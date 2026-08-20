#![forbid(unsafe_code)]

//! Lint migration files for required header comments.
//!
//! Each migration file must start with a header comment block of at least
//! `MIN_HEADER_LINES` lines explaining:
//!   - what the migration does
//!   - which table(s) it affects
//!   - which upstream Node module/paperclip-rs crate uses it
//!   - any breaking change or rollback note
//!
//! Exit code 0 = all pass, 1 = at least one missing header.

use std::path::{Path, PathBuf};

/// Minimum number of comment lines required (after stripping `--`).
const MIN_HEADER_LINES: usize = 5;

#[derive(Debug)]
struct Failure {
    path: PathBuf,
    reason: String,
}

fn lint_one(path: &Path) -> Result<(), String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("read failed: {e}"))?;
    // Skip empty files
    if content.trim().is_empty() {
        return Err("empty file".into());
    }
    // Count leading comment lines (drizzle uses `--` for single-line comments)
    let mut comment_lines = 0usize;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("--") {
            comment_lines += 1;
        } else {
            break;
        }
    }
    if comment_lines >= MIN_HEADER_LINES {
        Ok(())
    } else {
        Err(format!(
            "header has {comment_lines} comment lines, expected >= {MIN_HEADER_LINES}"
        ))
    }
}

pub fn run(migrations_dir: &Path) -> (usize, usize, Vec<Failure>) {
    let mut total = 0usize;
    let mut passed = 0usize;
    let mut failures = Vec::new();

    let entries = match std::fs::read_dir(migrations_dir) {
        Ok(e) => e,
        Err(e) => {
            failures.push(Failure {
                path: migrations_dir.to_path_buf(),
                reason: format!("read_dir failed: {e}"),
            });
            return (0, 0, failures);
        }
    };

    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("sql"))
        .collect();
    paths.sort();

    for path in paths {
        total += 1;
        match lint_one(&path) {
            Ok(()) => passed += 1,
            Err(reason) => failures.push(Failure {
                path,
                reason,
            }),
        }
    }

    (total, passed, failures)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_migration(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    #[test]
    fn passes_with_full_header() {
        let tmp = tempfile::tempdir().unwrap();
        let _path = write_migration(
            tmp.path(),
            "0001_test.sql",
            "-- Migration: add users table\n-- Tables: users\n-- Upstream: pc-auth\n-- Used by: pc-server startup\n-- Note: initial schema\nCREATE TABLE users (id uuid);\n",
        );
        let (total, passed, failures) = run(tmp.path());
        assert_eq!(total, 1);
        assert_eq!(passed, 1);
        assert!(failures.is_empty());
    }

    #[test]
    fn fails_with_too_few_header_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let _path = write_migration(
            tmp.path(),
            "0001_test.sql",
            "-- one line header\nCREATE TABLE users (id uuid);\n",
        );
        let (total, passed, failures) = run(tmp.path());
        assert_eq!(total, 1);
        assert_eq!(passed, 0);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].reason.contains("expected >= 5"));
    }

    #[test]
    fn fails_with_no_header() {
        let tmp = tempfile::tempdir().unwrap();
        let _path = write_migration(
            tmp.path(),
            "0001_test.sql",
            "CREATE TABLE users (id uuid);\n",
        );
        let (total, passed, _failures) = run(tmp.path());
        assert_eq!(total, 1);
        assert_eq!(passed, 0);
    }

    #[test]
    fn skips_non_sql_files() {
        let tmp = tempfile::tempdir().unwrap();
        write_migration(tmp.path(), "0001_test.txt", "-- a\n-- b\n-- c\n-- d\n-- e\ndata\n");
        let (total, _, _) = run(tmp.path());
        assert_eq!(total, 0);
    }

    #[test]
    fn empty_migration_file_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let _path = write_migration(tmp.path(), "0001_empty.sql", "");
        let (total, passed, failures) = run(tmp.path());
        assert_eq!(total, 1);
        assert_eq!(passed, 0);
        assert!(failures[0].reason.contains("empty"));
    }
}

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "../pc-db/migrations/drizzle".into());
    let path = PathBuf::from(&dir);
    let (total, passed, failures) = run(&path);
    println!("Migration header lint:");
    println!("  Total   : {total}");
    println!("  Passed  : {passed}");
    println!("  Failed  : {}", failures.len());
    for f in &failures {
        println!("  - {}: {}", f.path.display(), f.reason);
    }
    if failures.is_empty() {
        std::process::exit(0);
    } else {
        std::process::exit(1);
    }
}