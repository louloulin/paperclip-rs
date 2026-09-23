//! execenv 隔离内核的集成测试：路径守卫、执行锁、task 临时目录 GC、prepare 事务。
//!
//! 全部用真实文件系统（`std::env::temp_dir()` 下的独立基目录，`Drop` 收尾），
//! **不需要**数据库、外部二进制或网络，因此都是常跑测试（不带 `#[ignore]`）。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use mc_daemon::execenv::{
    prune_task_temp_dirs, remove_task_temp_dir, task_temp_dir_holds_content, EnvRoot,
    ExecutionLock, PreparedEnv, TaskTempDir, ENV_ROOT_LOCK_FILE, TASK_TEMP_DIR_PREFIX,
};

/// 每个测试一个独立基目录，测试结束整体删除。
struct TestBase(PathBuf);

impl TestBase {
    fn new(name: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("mc-execenv-test-{name}-{nanos:x}"));
        fs::create_dir_all(&dir).expect("create test base");
        Self(dir)
    }

    fn path(&self) -> &Path {
        self.0.as_path()
    }

    fn entries(&self) -> Vec<PathBuf> {
        let mut names: Vec<PathBuf> = fs::read_dir(self.path())
            .expect("read test base")
            .map(|entry| entry.expect("entry").path())
            .collect();
        names.sort();
        names
    }
}

impl Drop for TestBase {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn prepare_creates_layout_under_canonical_root_and_cleans_up_without_residue() {
    let base = TestBase::new("prepare-ok");
    let prepared =
        PreparedEnv::prepare(base.path(), &["workspace", "codex-home"]).expect("prepare");

    let root = prepared.env_root().to_path_buf();
    assert!(root.is_absolute());
    assert_eq!(root, root.canonicalize().expect("canonical"));
    assert!(root.join("workspace").is_dir());
    assert!(root.join("codex-home").is_dir());
    // 临时目录在基目录下、带前缀，且已发布执行锁标记。
    assert!(root.starts_with(base.path().canonicalize().expect("base canonical")));
    assert!(root.join(ENV_ROOT_LOCK_FILE).exists());

    prepared.cleanup().expect("cleanup");
    assert!(base.entries().is_empty(), "清理后基目录必须为空");
}

#[test]
fn prepare_failure_midway_leaves_no_residue() {
    let base = TestBase::new("prepare-fail");
    let err =
        PreparedEnv::prepare(base.path(), &["workspace", "../escape"]).expect_err("must fail");
    assert!(
        matches!(
            err,
            mc_daemon::execenv::ExecEnvError::InvalidRelativePath { .. }
        ),
        "越界相对路径必须被判非法，实际：{err}"
    );
    assert!(
        base.entries().is_empty(),
        "准备中途失败不得留下任何残留（含已创建的临时目录与锁标记）"
    );
}

#[test]
fn join_checked_rejects_traversal_and_absolute_paths() {
    let base = TestBase::new("path-syntax");
    let root = EnvRoot::create(&base.path().join("root")).expect("env root");

    assert!(root.join_checked(Path::new("../outside")).is_err());
    assert!(root.join_checked(Path::new("a/../../b")).is_err());
    assert!(root.join_checked(Path::new("/etc/passwd")).is_err());
    // 正常的相对路径（含尚未存在的层级）仍然放行。
    assert!(root.join_checked(Path::new("a/b/c")).is_ok());
    assert!(root.join_checked(Path::new("./a/./b")).is_ok());
}

#[cfg(unix)]
#[test]
fn join_checked_resolves_symlinks_before_prefix_check() {
    let base = TestBase::new("path-symlink");
    let root_path = base.path().join("root");
    let root = EnvRoot::create(&root_path).expect("env root");
    let outside = base.path().join("outside");
    fs::create_dir_all(&outside).expect("outside");

    // 1) 指向 root 之外的链接 ⇒ 解析后越界 ⇒ 拒绝（无论目标存不存在）。
    std::os::unix::fs::symlink(&outside, root_path.join("escape")).expect("symlink");
    assert!(root.join_checked(Path::new("escape/leak")).is_err());
    assert!(root.assert_inside(&root_path.join("escape")).is_err());

    // 2) 悬空链接：canonicalize 失败 ⇒ 拒绝（只解析「最深的已存在祖先」的实现会误放行）。
    std::os::unix::fs::symlink(
        base.path().join("does-not-exist"),
        root_path.join("dangling"),
    )
    .expect("symlink");
    assert!(root.join_checked(Path::new("dangling/file")).is_err());

    // 3) 指向 root **之内**的链接仍放行（上游 codex-home 链接依赖这一形态）。
    let shared = root_path.join("shared");
    fs::create_dir_all(&shared).expect("shared");
    std::os::unix::fs::symlink(&shared, root_path.join("link")).expect("symlink");
    assert!(root.join_checked(Path::new("link/child")).is_ok());
    assert!(root.ensure_dir(Path::new("link/child")).is_ok());
    assert!(shared.join("child").is_dir());
}

#[test]
fn execution_lock_is_exclusive_and_released_on_drop() {
    let base = TestBase::new("lock");
    let dir = base.path().join("env");
    fs::create_dir_all(&dir).expect("dir");

    let held = ExecutionLock::acquire_fresh(&dir)
        .expect("acquire")
        .expect("first acquire wins");
    assert!(held.marker().ends_with(ENV_ROOT_LOCK_FILE));
    assert!(
        !dir.join(".task_lock.claiming").exists(),
        "发布标记后不得留下 claim 占位文件"
    );

    // 另一个进程（GC / 另一个 daemon）用 probe 看「持有者还活着吗」。
    assert!(
        ExecutionLock::probe(&dir).expect("probe").is_none(),
        "已持有的执行锁不得被第二次取得"
    );

    held.release();
    let again = ExecutionLock::probe(&dir)
        .expect("probe")
        .expect("释放后标记变为可取得");
    drop(again);
    // 未发布的目录 probe 不报错，只是「没有标记」。
    assert!(ExecutionLock::probe(&base.path().join("no-such-dir"))
        .expect("probe missing dir")
        .is_none());
}

#[test]
fn task_temp_dir_removal_is_content_first_and_idempotent() {
    let base = TestBase::new("temp-remove");
    let temp = TaskTempDir::create(base.path()).expect("create task temp");
    let dir = temp.path().to_path_buf();
    assert!(dir
        .file_name()
        .expect("name")
        .to_string_lossy()
        .starts_with(TASK_TEMP_DIR_PREFIX));
    fs::create_dir_all(dir.join("workspace/nested")).expect("nested");
    fs::write(dir.join("workspace/nested/file.txt"), b"x").expect("write");
    assert!(task_temp_dir_holds_content(&dir).expect("holds content"));

    temp.cleanup().expect("cleanup");
    assert!(!dir.exists(), "清理后 task 临时目录必须整体消失");
    // 幂等：对已消失的目录再删一次不报错。
    remove_task_temp_dir(&dir).expect("idempotent");
}

#[test]
fn prune_reclaims_dead_dirs_and_keeps_live_ones() {
    let base = TestBase::new("prune");

    // ① 死者：有标记（未被任何人持有）+ 内容。
    let dead = base.path().join(format!("{TASK_TEMP_DIR_PREFIX}dead"));
    fs::create_dir_all(dead.join("workspace")).expect("dead dir");
    fs::write(dead.join("workspace/file.txt"), b"x").expect("write");
    fs::write(dead.join(ENV_ROOT_LOCK_FILE), b"").expect("marker");

    // ② 活着：锁被本进程持有。
    let live = TaskTempDir::create(base.path()).expect("live temp");
    fs::write(live.path().join("in-flight.txt"), b"x").expect("write live content");

    // ③ 遗留物（无标记、有内容）⇒ 超龄才回收；④ 无标记且空 ⇒ 永远只靠年龄兜底，这里留。
    let legacy = base.path().join(format!("{TASK_TEMP_DIR_PREFIX}legacy"));
    fs::create_dir_all(&legacy).expect("legacy dir");
    fs::write(legacy.join("leftover.txt"), b"x").expect("write legacy");
    let legacy_empty = base
        .path()
        .join(format!("{TASK_TEMP_DIR_PREFIX}legacy-empty"));
    fs::create_dir_all(&legacy_empty).expect("legacy empty");

    // 非 daemon 目录（名字不带前缀，例如共享 /tmp 里别人的东西）绝不在回收面内。
    let foreign = base.path().join("other-program");
    fs::create_dir_all(&foreign).expect("foreign dir");
    fs::write(foreign.join("keep.txt"), b"x").expect("write foreign");

    let report =
        prune_task_temp_dirs(base.path(), Duration::ZERO, SystemTime::now()).expect("prune");
    // 死者的标记未被持有 ⇒ 回收；超龄且带内容的遗留物 ⇒ 回收。read_dir 顺序不保证。
    let mut removed = report.removed.clone();
    removed.sort();
    let mut expected_removed = vec![dead.clone(), legacy.clone()];
    expected_removed.sort();
    assert_eq!(removed, expected_removed);
    assert_eq!(report.kept_in_use, vec![live.path().to_path_buf()]);
    assert_eq!(report.kept_legacy, vec![legacy_empty.clone()]);
    assert!(!dead.exists(), "死者的目录必须被回收");
    assert!(
        live.path().join("in-flight.txt").exists(),
        "在用的目录不得被碰"
    );
    assert!(!legacy.join("leftover.txt").exists(), "超龄遗留物应被回收");
    assert!(
        foreign.join("keep.txt").exists(),
        "非 daemon 目录不得被回收"
    );
}

#[test]
fn prune_honours_legacy_ttl_for_unmarked_dirs() {
    let base = TestBase::new("prune-ttl");
    let legacy = base.path().join(format!("{TASK_TEMP_DIR_PREFIX}fresh"));
    fs::create_dir_all(&legacy).expect("legacy dir");
    fs::write(legacy.join("leftover.txt"), b"x").expect("write legacy");

    // 一个「刚刚创建、还没发布标记」的目录：即便有内容，也不能按 0 秒 TTL 之外的宽松 TTL 回收。
    let report = prune_task_temp_dirs(base.path(), Duration::from_secs(3600), SystemTime::now())
        .expect("prune");
    assert_eq!(report.removed, Vec::<PathBuf>::new());
    assert_eq!(report.kept_legacy, vec![legacy.clone()]);
    assert!(legacy.join("leftover.txt").exists());
}

/// execenv 的唯一对外可观测面（错误 `Display` + `tracing` warn）只回显**路径与错误类型**，
/// 绝不回显文件内容——本片尚未落地凭据注入面，这条是留给 p1 的结构性约束：
/// 凭据一旦进来，只能经 `mc-telemetry` 的 redaction 通道，不得走本层自己的日志。
#[test]
fn execenv_errors_never_echo_file_contents() {
    let base = TestBase::new("no-secret-in-errors");
    let secret = "sk-live-DEADBEEF-not-a-real-token";
    fs::write(base.path().join("credential.env"), secret).expect("write credential");

    let err =
        PreparedEnv::prepare(base.path(), &["workspace", "../escape"]).expect_err("must fail");
    let rendered = format!("{err}");
    assert!(
        !rendered.contains(secret),
        "execenv 错误面不得回显文件内容：{rendered}"
    );
    // 回滚只删自己创建的临时目录，不碰基目录里的其它东西。
    assert_eq!(base.entries(), vec![base.path().join("credential.env")]);
}
