//! R389 — Integration tests for Node-faithful skill materialization.
//!
//! Mirrors Node parity surface in `adapter-utils/src/server-utils.ts`:
//! - `MATERIALIZED_SKILL_SENTINEL` (L129)
//! - `MATERIALIZED_SKILL_LOCK_OWNER` (L130)
//! - `MATERIALIZED_SKILL_LOCK_STALE_MS` (L131)
//! - `hashSkillDirectory` (L2920-2966)
//! - `materializedSkillFingerprintMatches` (L2968-2976)
//! - `acquireMaterializeLock` (L2978-3000)
//! - `removeStaleMaterializeLock` (L3003-3026)
//! - `isPidAlive` (L3006-3013)
//! - `materializePaperclipSkillCopy` (L3038-3120)

use pc_acpx::{
    hash_skill_directory, is_pid_alive, materialize_paperclip_skill_copy,
    materialized_skill_fingerprint_matches, remove_stale_materialize_lock, AcpxError,
    MATERIALIZED_SKILL_LOCK_OWNER, MATERIALIZED_SKILL_LOCK_STALE_MS, MATERIALIZED_SKILL_SENTINEL,
};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn unique_dir(label: &str) -> PathBuf {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "pc-acpx-r389-{label}-{secs}-{}",
        std::process::id()
    ))
}

fn write_file(path: &PathBuf, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

async fn cleanup(path: &PathBuf) {
    let _ = tokio::fs::remove_dir_all(path).await;
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

#[test]
fn constants_match_node_literals() {
    assert_eq!(
        MATERIALIZED_SKILL_SENTINEL,
        ".paperclip-materialized-skill.json"
    );
    assert_eq!(MATERIALIZED_SKILL_LOCK_OWNER, "owner.json");
    assert_eq!(MATERIALIZED_SKILL_LOCK_STALE_MS, 30_000);
}

// ---------------------------------------------------------------------------
// materializePaperclipSkillCopy — Node-faithful guards
// ---------------------------------------------------------------------------

#[tokio::test]
async fn materialize_rejects_source_inside_target() {
    let outer = unique_dir("r389-ancestor");
    let inner = outer.join("inner");
    tokio::fs::create_dir_all(&inner).await.unwrap();
    let err = materialize_paperclip_skill_copy(&outer, &inner)
        .await
        .expect_err("target-inside-source must be rejected");
    assert!(
        matches!(err, AcpxError::MaterializeSelfReference { .. }),
        "expected MaterializeSelfReference, got {err:?}"
    );
    cleanup(&outer).await;
}

#[tokio::test]
async fn materialize_rejects_target_inside_source() {
    let outer = unique_dir("r389-descendant");
    let inner = outer.join("inner");
    tokio::fs::create_dir_all(&inner).await.unwrap();
    let err = materialize_paperclip_skill_copy(&inner, &outer)
        .await
        .expect_err("source-inside-target must be rejected");
    assert!(matches!(err, AcpxError::MaterializeSelfReference { .. }));
    cleanup(&outer).await;
}

#[tokio::test]
async fn materialize_rejects_symlink_root() {
    let real = unique_dir("r389-sl-root-real");
    let link = unique_dir("r389-sl-root-link");
    let target = unique_dir("r389-sl-root-tgt");
    tokio::fs::create_dir_all(&real).await.unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let err = materialize_paperclip_skill_copy(&link, &target)
        .await
        .expect_err("symlink root must be rejected");
    assert!(matches!(err, AcpxError::MaterializeSymlinkRoot { .. }));
    cleanup(&real).await;
    cleanup(&link).await;
    cleanup(&target).await;
}

#[tokio::test]
async fn materialize_rejects_non_directory_root() {
    let file = unique_dir("r389-file");
    let target = unique_dir("r389-file-tgt");
    write_file(&file, "not a directory");
    let err = materialize_paperclip_skill_copy(&file, &target)
        .await
        .expect_err("non-directory root must be rejected");
    assert!(matches!(err, AcpxError::MaterializeNotDirectory { .. }));
    cleanup(&file).await;
    cleanup(&target).await;
}

#[tokio::test]
async fn materialize_repeated_call_short_circuits_to_cache_hit() {
    let source = unique_dir("r389-cache-src");
    let target = unique_dir("r389-cache-tgt");
    write_file(&source.join("SKILL.md"), "v1");
    let first = materialize_paperclip_skill_copy(&source, &target)
        .await
        .unwrap();
    assert!(first.copied_files >= 1);
    // Second call observes the sentinel — no files copied.
    let second = materialize_paperclip_skill_copy(&source, &target)
        .await
        .unwrap();
    assert_eq!(second.copied_files, 0);
    assert!(second.skipped_symlinks.is_empty());
    cleanup(&source).await;
    cleanup(&target).await;
}

#[tokio::test]
async fn materialize_cache_invalidates_after_source_mutation() {
    let source = unique_dir("r389-inv-src");
    let target = unique_dir("r389-inv-tgt");
    write_file(&source.join("a.txt"), "v1");
    let first = materialize_paperclip_skill_copy(&source, &target)
        .await
        .unwrap();
    assert!(first.copied_files >= 1);
    write_file(&source.join("a.txt"), "v2-different");
    let second = materialize_paperclip_skill_copy(&source, &target)
        .await
        .unwrap();
    assert!(second.copied_files >= 1);
    // And the new content is visible in the target.
    assert_eq!(
        tokio::fs::read_to_string(target.join("a.txt"))
            .await
            .unwrap(),
        "v2-different"
    );
    cleanup(&source).await;
    cleanup(&target).await;
}

#[tokio::test]
async fn materialize_sentinel_records_copied_files_and_fingerprint() {
    let source = unique_dir("r389-sent-src");
    let target = unique_dir("r389-sent-tgt");
    write_file(&source.join("a.txt"), "alpha");
    write_file(&source.join("b/c.txt"), "beta");
    let result = materialize_paperclip_skill_copy(&source, &target)
        .await
        .unwrap();
    let sentinel_path = target.join(MATERIALIZED_SKILL_SENTINEL);
    assert!(tokio::fs::try_exists(&sentinel_path).await.unwrap_or(false));
    let raw = tokio::fs::read_to_string(&sentinel_path).await.unwrap();
    let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(value["version"], 1);
    assert!(value["sourceFingerprint"].is_string());
    let recorded = value["copiedFiles"].as_u64().unwrap();
    assert!(recorded >= 2, "sentinel must record copiedFiles >= 2");
    assert_eq!(recorded as usize, result.copied_files);
    cleanup(&source).await;
    cleanup(&target).await;
}

#[tokio::test]
async fn materialize_preserves_file_mode_on_unix() {
    use std::os::unix::fs::PermissionsExt;
    let source = unique_dir("r389-mode-src");
    let target = unique_dir("r389-mode-tgt");
    let file = source.join("executable.sh");
    write_file(&file, "#!/bin/sh\necho hi\n");
    let mut perms = tokio::fs::metadata(&file).await.unwrap().permissions();
    perms.set_mode(0o755);
    tokio::fs::set_permissions(&file, perms.clone())
        .await
        .unwrap();

    materialize_paperclip_skill_copy(&source, &target)
        .await
        .unwrap();
    let copied = target.join("executable.sh");
    let copied_perms = tokio::fs::metadata(&copied).await.unwrap().permissions();
    assert_eq!(copied_perms.mode() & 0o777, 0o755);
    cleanup(&source).await;
    cleanup(&target).await;
}

#[tokio::test]
async fn materialize_drops_external_symlinks() {
    let source = unique_dir("r389-sl-src");
    let target = unique_dir("r389-sl-tgt");
    let external = unique_dir("r389-sl-external");
    write_file(&external.join("file.txt"), "real-content");
    write_file(&source.join("real.txt"), "real");
    std::os::unix::fs::symlink(&external.join("file.txt"), source.join("link.txt")).unwrap();
    let result = materialize_paperclip_skill_copy(&source, &target)
        .await
        .unwrap();
    assert!(!result.skipped_symlinks.is_empty());
    // The target must not contain a symlink.
    let symlink_meta = tokio::fs::symlink_metadata(target.join("link.txt")).await;
    assert!(symlink_meta.is_err() || !symlink_meta.unwrap().file_type().is_symlink());
    cleanup(&source).await;
    cleanup(&target).await;
    cleanup(&external).await;
}

// ---------------------------------------------------------------------------
// hashSkillDirectory
// ---------------------------------------------------------------------------

#[tokio::test]
async fn hash_skill_directory_is_stable_across_runs() {
    let dir = unique_dir("r389-hash-stable");
    write_file(&dir.join("a.txt"), "alpha");
    write_file(&dir.join("b/c.txt"), "beta");
    let a = hash_skill_directory(&dir).await.unwrap();
    let b = hash_skill_directory(&dir).await.unwrap();
    assert_eq!(a, b);
    assert_eq!(a.len(), 64);
    cleanup(&dir).await;
}

#[tokio::test]
async fn hash_skill_directory_is_order_invariant_for_children() {
    // Two directories with the same children inserted in different
    // orders must hash to the same value (Node sorts children by
    // `localeCompare` before hashing).
    let a = unique_dir("r389-hash-order-a");
    let b = unique_dir("r389-hash-order-b");
    write_file(&a.join("z.txt"), "1");
    write_file(&a.join("a.txt"), "2");
    write_file(&b.join("a.txt"), "2");
    write_file(&b.join("z.txt"), "1");
    let ha = hash_skill_directory(&a).await.unwrap();
    let hb = hash_skill_directory(&b).await.unwrap();
    assert_eq!(ha, hb);
    cleanup(&a).await;
    cleanup(&b).await;
}

// ---------------------------------------------------------------------------
// materializedSkillFingerprintMatches
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fingerprint_matches_returns_false_without_sentinel() {
    let dir = unique_dir("r389-fp-missing");
    tokio::fs::create_dir_all(&dir).await.unwrap();
    let result = materialized_skill_fingerprint_matches(&dir, "any-fingerprint").await;
    assert!(!result);
    cleanup(&dir).await;
}

#[tokio::test]
async fn fingerprint_matches_requires_version_one() {
    let dir = unique_dir("r389-fp-version");
    tokio::fs::create_dir_all(&dir).await.unwrap();
    let sentinel = dir.join(MATERIALIZED_SKILL_SENTINEL);
    tokio::fs::write(
        &sentinel,
        "{\"version\": 2, \"sourceFingerprint\": \"x\"}\n",
    )
    .await
    .unwrap();
    let result = materialized_skill_fingerprint_matches(&dir, "x").await;
    assert!(!result);
    cleanup(&dir).await;
}

#[tokio::test]
async fn fingerprint_matches_returns_true_for_matching_value() {
    let dir = unique_dir("r389-fp-match");
    tokio::fs::create_dir_all(&dir).await.unwrap();
    let sentinel = dir.join(MATERIALIZED_SKILL_SENTINEL);
    tokio::fs::write(
        &sentinel,
        "{\"version\": 1, \"sourceFingerprint\": \"abc\"}\n",
    )
    .await
    .unwrap();
    let result = materialized_skill_fingerprint_matches(&dir, "abc").await;
    assert!(result);
    cleanup(&dir).await;
}

// ---------------------------------------------------------------------------
// removeStaleMaterializeLock + isPidAlive
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stale_lock_is_removed_when_owner_pid_is_dead() {
    let dir = unique_dir("r389-lock-dead");
    tokio::fs::create_dir_all(&dir).await.unwrap();
    // Owner file with a definitely-dead PID (very large value).
    let owner_path = dir.join(MATERIALIZED_SKILL_LOCK_OWNER);
    let payload = serde_json::json!({
        "pid": u32::MAX,
        "createdAt": "2020-01-01T00:00:00.000Z",
    });
    tokio::fs::write(
        &owner_path,
        format!("{}\n", serde_json::to_string(&payload).unwrap()),
    )
    .await
    .unwrap();
    let removed = remove_stale_materialize_lock(&dir, MATERIALIZED_SKILL_LOCK_STALE_MS).await;
    assert!(removed);
    assert!(!tokio::fs::try_exists(&dir).await.unwrap_or(false));
}

#[tokio::test]
async fn stale_lock_is_kept_when_owner_is_live() {
    let dir = unique_dir("r389-lock-live");
    tokio::fs::create_dir_all(&dir).await.unwrap();
    let owner_path = dir.join(MATERIALIZED_SKILL_LOCK_OWNER);
    let payload = serde_json::json!({
        "pid": std::process::id(),
        "createdAt": chrono_now_iso(),
    });
    tokio::fs::write(
        &owner_path,
        format!("{}\n", serde_json::to_string(&payload).unwrap()),
    )
    .await
    .unwrap();
    let removed = remove_stale_materialize_lock(&dir, MATERIALIZED_SKILL_LOCK_STALE_MS).await;
    assert!(!removed);
    assert!(tokio::fs::try_exists(&dir).await.unwrap_or(false));
    cleanup(&dir).await;
}

#[tokio::test]
async fn stale_lock_is_removed_when_age_exceeds_threshold() {
    let dir = unique_dir("r389-lock-old");
    tokio::fs::create_dir_all(&dir).await.unwrap();
    let owner_path = dir.join(MATERIALIZED_SKILL_LOCK_OWNER);
    let payload = serde_json::json!({
        "pid": std::process::id(),
        // 5 minutes ago — older than the 30 s default.
        "createdAt": chrono_now_iso_offset(-300_000),
    });
    tokio::fs::write(
        &owner_path,
        format!("{}\n", serde_json::to_string(&payload).unwrap()),
    )
    .await
    .unwrap();
    let removed = remove_stale_materialize_lock(&dir, MATERIALIZED_SKILL_LOCK_STALE_MS).await;
    assert!(removed);
}

#[test]
fn pid_alive_handles_zero_and_self() {
    assert!(!is_pid_alive(0));
    assert!(is_pid_alive(std::process::id()));
    // PIDs at the top of u32 space are virtually never live.
    assert!(!is_pid_alive(u32::MAX));
}

// ---------------------------------------------------------------------------
// Date helpers (Node `new Date().toISOString()` parity)
// ---------------------------------------------------------------------------

fn chrono_now_iso() -> String {
    chrono_now_iso_offset(0)
}

fn chrono_now_iso_offset(delta_ms: i64) -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
        + delta_ms;
    let total_secs = (secs / 1000) as i64;
    let ms = (secs.rem_euclid(1000)) as u64;
    let days = total_secs / 86_400;
    let secs_of_day = total_secs.rem_euclid(86_400) as u64;
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy as i64 - (153 * mp + 2) as i64 / 5 + 1;
    let m = if mp < 10 {
        (mp + 3) as i64
    } else {
        (mp - 9) as i64
    };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}.{ms:03}Z")
}
