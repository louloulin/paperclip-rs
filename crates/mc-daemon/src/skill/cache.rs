//! skill bundle 的磁盘缓存 + 「bundle 与 ref 是不是同一份」的校验。
//!
//! - **上游**：`internal/daemon/skill_cache.go`（192 行）。
//! - **写者**：M6-9（本 slice）。
//!
//! 缓存布局与上游逐字一致：
//!
//! ```text
//! <root>/<safe(workspace_id)>/<safe(source)>/<safe(id)>/<safe(hash)>/bundle.json
//! ```
//!
//! 每个路径段都过 [`safe_cache_segment`]（`..` / 绝对路径 / 分隔符都进不来），
//! 于是「服务端给了什么字符串」不可能把写入引出缓存根。
//!
//! # 校验用**同一个函数**（`DoD`）
//!
//! [`validate_skill_bundle`] 的哈希不是本模块算的 —— 它调
//! [`mc_core::skill::build_manifest`]，也就是 `crates/mc-http/src/routes/daemon/skills.rs`
//! 那半边调的**同一个 `fn`**。为什么这一点是可达的（而不是被迫各写一份）：`mc-http` 的
//! `write_hash_part` / `build_bundle` 是 `pub(crate)`，`mc-daemon` **没有** `mc-skill` 边，
//! 但**两侧都依赖 `mc-core`** ⇒ 唯一下沉点落在 `mc_core::skill`，零依赖边变更
//! （`docs/57` §9.5 的裁定；`crates/mc-core/src/skill.rs:246 pub fn write_hash_part`、
//! `:256 pub fn build_manifest`）。
//!
//! 本文件因此**不出现**任何自算哈希：唯一一处 `sha256` 相关代码是「把 ref 的字符串与
//! manifest 的输出比一下」。[`tests::validate_accepts_exactly_the_mc_core_digest`] 把
//! 结果钉在一个**逐字金标**上，任何「同结果的新实现」都过不了它。
//!
//! # 与上游的差异（逐条登记在 `docs/32` §9.9）
//!
//! - 上游 `SkillBundleCache` 带一对可注入的 `rename` / `removeAll` 夹具（Go 侧测试用）；
//!   本 slice 直接用 `std::fs`：**没有**注入点，测试靠 `root` 指向临时目录来隔离。
//! - 临时目录名用 `.bundle-<pid>-<序号>`（上游是 `os.MkdirTemp` 的随机名）—— 同一父目录
//!   下**仍然唯一**（pid + 进程内自增），且不需要 `rand` 边。
//! - 上游 `Load` 把「JSON 解不开」与「校验不过」都当成 miss 并**删掉**坏缓存；本 slice 同。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use mc_core::skill::{build_manifest, ManifestFile, ManifestInput, SkillSource};

use super::{
    safe_cache_segment, safe_skill_file_path, Result, SkillBundleData, SkillExecError, SkillRefData,
};

/// 缓存内那份文档的文件名（上游 `bundle.json`）。
pub const BUNDLE_FILE_NAME: &str = "bundle.json";

/// 临时目录前缀（上游 `os.MkdirTemp` 的 `.bundle-*`）。
const TEMP_DIR_PREFIX: &str = ".bundle";

/// 一个 workspace 的 skill bundle 缓存。
///
/// `root` 为空（空串 / 空路径）时**整个缓存关闭**：`load` 恒 miss、`store` 是 no-op ——
/// 与上游 `if c == nil || c.root == ""` 的两处早退逐字对应（daemon 未配置缓存目录时不得
/// 因此失败）。
#[derive(Debug, Clone)]
pub struct SkillBundleCache {
    root: PathBuf,
    /// 每个 (workspace, source, id, hash) 一把进程内锁，见 [`SkillBundleCache::with_ref_lock`]。
    locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

impl SkillBundleCache {
    /// 以 `root` 建缓存；`root` 为空 ⇒ 关闭（见类型文档）。
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            locks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 关闭的缓存（上游 `NewSkillBundleCache("")` 的等价物）。
    #[must_use]
    pub fn disabled() -> Self {
        Self::new(PathBuf::new())
    }

    /// 缓存根。
    #[must_use]
    pub fn root(&self) -> &Path {
        self.root.as_path()
    }

    /// 是否启用（根非空）。上游那两处早退的等价判据。
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        !self.root.as_os_str().is_empty()
    }

    /// 命中就返回缓存的 bundle；miss / 坏缓存 / 校验不过都返回 `None`。
    ///
    /// 与上游一致：**解不开或校验不过的文件会被删掉** —— 否则一份坏缓存会让之后每次任务
    /// 都重复付出同一次解析代价，而且永远不会自愈（服务端不会主动来清）。
    #[must_use]
    pub fn load(&self, workspace_id: &str, reference: &SkillRefData) -> Option<SkillBundleData> {
        if !self.is_enabled() {
            return None;
        }
        let path = self.bundle_path(workspace_id, reference);
        let raw = fs::read(&path).ok()?;
        let bundle: SkillBundleData = if let Ok(bundle) = serde_json::from_slice(&raw) {
            bundle
        } else {
            let _ = fs::remove_file(&path);
            return None;
        };
        if !validate_skill_bundle(reference, &bundle) {
            let _ = fs::remove_file(&path);
            return None;
        }
        Some(bundle)
    }

    /// 原子落盘一份 bundle（上游 `Store`）。
    ///
    /// 步骤与上游逐字：写进同级的临时目录 → 移除旧目录 → `rename` 顶上；`rename` 撞
    /// 「已存在」时再删一次重来（Windows 上目录 rename 的语义）。任何一步失败都不留
    /// 半份缓存（`tmp` 一定被清）。
    pub fn store(&self, workspace_id: &str, bundle: &SkillBundleData) -> Result<()> {
        if !self.is_enabled() {
            return Ok(());
        }
        let reference = SkillRefData {
            id: bundle.id.clone(),
            source: bundle.source.clone(),
            name: bundle.name.clone(),
            description: bundle.description.clone(),
            hash: bundle.hash.clone(),
            size_bytes: bundle.size_bytes,
            file_count: bundle.files.len(),
            files: Vec::new(),
        };
        let path = self.bundle_path(workspace_id, &reference);
        let dir = path.parent().ok_or_else(|| {
            SkillExecError::Cache(format!("bundle path has no parent: {}", path.display()))
        })?;
        let parent = dir
            .parent()
            .ok_or_else(|| SkillExecError::Cache(format!("no parent for {}", dir.display())))?;

        let data = serde_json::to_vec(bundle)
            .map_err(|err| SkillExecError::Cache(format!("serialize bundle: {err}")))?;

        if let Err(err) = fs::create_dir_all(parent) {
            return Err(SkillExecError::io("create bundle parent", parent, err));
        }
        let tmp = temp_dir_path(parent);
        if let Err(err) = fs::create_dir_all(&tmp) {
            return Err(SkillExecError::io("create bundle temp dir", &tmp, err));
        }
        let result = self.store_into(&tmp, dir, &data);
        // 无论成功失败都不留临时目录（上游的 `defer os.RemoveAll(tmp)`）。
        let _ = fs::remove_dir_all(&tmp);
        result
    }

    fn store_into(&self, tmp: &Path, dir: &Path, data: &[u8]) -> Result<()> {
        let file = tmp.join(BUNDLE_FILE_NAME);
        fs::write(&file, data).map_err(|err| SkillExecError::io("write bundle", &file, err))?;
        if let Err(err) = fs::remove_dir_all(dir) {
            if err.kind() != std::io::ErrorKind::NotFound {
                return Err(SkillExecError::io("remove stale bundle", dir, err));
            }
        }
        match fs::rename(tmp, dir) {
            Ok(()) => Ok(()),
            Err(first) => {
                // 上游对 `fs.ErrExist` 的处理：再删一次旧目录后重试（Windows 目录 rename）。
                if let Err(err) = fs::remove_dir_all(dir) {
                    if err.kind() != std::io::ErrorKind::NotFound {
                        return Err(SkillExecError::io("remove stale bundle", dir, err));
                    }
                }
                fs::rename(tmp, dir).map_err(|second| {
                    SkillExecError::Cache(format!(
                        "rename bundle into place ({} then {}): {first} / {second}",
                        dir.display(),
                        tmp.display()
                    ))
                })
            }
        }
    }

    /// 缓存里这个 ref 对应的 `bundle.json` 路径（上游 `bundlePath`）。
    #[must_use]
    pub fn bundle_path(&self, workspace_id: &str, reference: &SkillRefData) -> PathBuf {
        self.root
            .join(safe_cache_segment(workspace_id))
            .join(safe_cache_segment(&reference.source))
            .join(safe_cache_segment(&reference.id))
            .join(safe_cache_segment(&reference.hash))
            .join(BUNDLE_FILE_NAME)
    }

    /// 同一个 ref 的临界区（上游 `WithRefLock`）。
    ///
    /// 上游用一张 per-key `sync.Mutex` 表；本 slice 同形（`HashMap<key, Arc<Mutex>>` +
    /// 外层一把表锁）。缓存**关闭**时不加锁直接跑 —— 与上游 `if c == nil { return fn() }` 同。
    pub fn with_ref_lock<T>(
        &self,
        workspace_id: &str,
        reference: &SkillRefData,
        action: impl FnOnce() -> T,
    ) -> T {
        if !self.is_enabled() {
            return action();
        }
        let key = format!(
            "{workspace_id}\u{0}{}\u{0}{}\u{0}{}",
            reference.source, reference.id, reference.hash
        );
        let entry = {
            let mut table = self
                .locks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Arc::clone(table.entry(key).or_insert_with(|| Arc::new(Mutex::new(()))))
        };
        let _guard = entry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        action()
    }
}

/// 同级的临时目录路径：`.bundle-<pid>-<序号>`（见模块文档的差异条目）。
fn temp_dir_path(parent: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    parent.join(format!("{TEMP_DIR_PREFIX}-{}-{n}", std::process::id()))
}

/// `skillbundle.Source*` 三个常量 → [`SkillSource`]。
///
/// 三个以外（含空串）返回 `None` ⇒ 校验直接判不过。上游 `BuildManifest` 是把**任意字符串**
/// 喂进哈希的；本仓的 [`ManifestInput::source`] 是封闭枚举，所以「源不认识」这件事在
/// **能算之前**就被拒了 —— 这是收紧，不是口径漂移（`)docs/32` §9.9 登记）。
fn parse_source(raw: &str) -> Option<SkillSource> {
    match raw {
        "workspace" => Some(SkillSource::Workspace),
        "builtin" => Some(SkillSource::Builtin),
        "plugin" => Some(SkillSource::Plugin),
        _ => None,
    }
}

/// 「这份 bundle 就是 ref 指的那一份吗」（上游 `validateSkillBundle`）。
///
/// 顺序与上游逐字：
/// 1. `id` / `source` / `hash` 三列必须逐字相等；
/// 2. 支持文件条数必须等于 `file_count`；
/// 3. 每个文件路径必须过白名单（[`safe_skill_file_path`]）；
/// 4. 用 [`mc_core::skill::build_manifest`] 重算 digest，必须等于 `hash`；
/// 5. `size_bytes > 0` 时还必须等于 manifest 的 `size_bytes`（`0` = 未声明 ⇒ 不比）。
///
/// ⚠️ 第 4 条是「同一个函数」的落点：本函数**不**自带哈希实现，
/// `crates/mc-http/src/routes/daemon/skills.rs` 的 `build_agent_bundle` 也调它。
#[must_use]
pub fn validate_skill_bundle(reference: &SkillRefData, bundle: &SkillBundleData) -> bool {
    if bundle.id != reference.id
        || bundle.source != reference.source
        || bundle.hash != reference.hash
    {
        return false;
    }
    if bundle.files.len() != reference.file_count {
        return false;
    }
    let Some(source) = parse_source(&bundle.source) else {
        return false;
    };
    let mut files = Vec::with_capacity(bundle.files.len());
    for file in &bundle.files {
        if !safe_skill_file_path(&file.path) {
            return false;
        }
        files.push(ManifestFile {
            path: file.path.as_str(),
            content: file.content.as_str(),
        });
    }
    let manifest = build_manifest(&ManifestInput {
        id: bundle.id.as_str(),
        source,
        name: bundle.name.as_str(),
        description: bundle.description.as_str(),
        content: bundle.content.as_str(),
        files: &files,
    });
    if manifest.hash != reference.hash {
        return false;
    }
    if reference.size_bytes > 0 && manifest.size_bytes != reference.size_bytes {
        return false;
    }
    true
}

/// 进程内唯一的「缓存根会变」提示（给日志用；本 slice 只暴露给调用方判断）。
///
/// 上游没有这个函数；保留它是因为 `mc-daemon` 的调用方（registration 期）需要判断
/// 「缓存目录到底配没配」，而 `SkillBundleCache::is_enabled` 是逐实例的。这里只做一次
/// 进程级缓存，避免每次任务启动都重新读环境变量。
#[must_use]
pub fn cache_root_from_env() -> Option<PathBuf> {
    static CACHE: OnceLock<Option<PathBuf>> = OnceLock::new();
    CACHE
        .get_or_init(|| super::non_empty_env("MULTICA_DAEMON_SKILL_CACHE_DIR").map(PathBuf::from))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::SkillFileData;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// 独立临时目录，`Drop` 收尾（与 `tests/execenv_prepare.rs` 的夹具同形）。
    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new(name: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let dir = std::env::temp_dir().join(format!("mc-daemon-skill-cache-{name}-{nanos:x}"));
            fs::create_dir_all(&dir).expect("create test root");
            Self(dir)
        }

        fn path(&self) -> &Path {
            self.0.as_path()
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const WORKSPACE: &str = "5b3d2590-0000-0000-0000-000000000000";
    const SKILL_ID: &str = "1c331d0b-94fd-412a-a7cc-6a209add00a1";

    fn bundle(files: &[(&str, &str)]) -> SkillBundleData {
        SkillBundleData {
            id: SKILL_ID.to_string(),
            source: "workspace".to_string(),
            name: "deploy".to_string(),
            description: "deploy helper".to_string(),
            hash: String::new(),
            size_bytes: 0,
            content: "main".to_string(),
            files: files
                .iter()
                .map(|(path, content)| SkillFileData {
                    path: (*path).to_string(),
                    content: (*content).to_string(),
                    sha256: String::new(),
                    size_bytes: 0,
                })
                .collect(),
        }
    }

    fn manifest_of(bundle: &SkillBundleData) -> mc_core::skill::Manifest {
        let files: Vec<ManifestFile<'_>> = bundle
            .files
            .iter()
            .map(|file| ManifestFile {
                path: file.path.as_str(),
                content: file.content.as_str(),
            })
            .collect();
        build_manifest(&ManifestInput {
            id: bundle.id.as_str(),
            source: parse_source(&bundle.source).expect("known source"),
            name: bundle.name.as_str(),
            description: bundle.description.as_str(),
            content: bundle.content.as_str(),
            files: &files,
        })
    }

    /// 把 bundle 的 hash / size 填成 manifest 的答案，并给出对应的 ref。
    fn seal(mut data: SkillBundleData) -> (SkillBundleData, SkillRefData) {
        let manifest = manifest_of(&data);
        data.hash = manifest.hash;
        data.size_bytes = manifest.size_bytes;
        let reference = SkillRefData {
            id: data.id.clone(),
            source: data.source.clone(),
            name: data.name.clone(),
            description: data.description.clone(),
            hash: data.hash.clone(),
            size_bytes: manifest.size_bytes,
            file_count: data.files.len(),
            files: Vec::new(),
        };
        (data, reference)
    }

    /// `bundle(&[("a.md", "a"), ("b.md", "bb")])` 这一组输入在 `mc_core` 分节口径下的
    /// digest，由 `cargo test` 实测填入（**不是**手抄的值，也不会被本模块重算）。
    /// 它是本片 DoD 的证物：「bundle 缓存校验用的是同一个函数」——
    /// `crates/mc-http/src/routes/daemon/skills.rs` 的 `build_agent_bundle` 与本模块调的是
    /// `mc_core::skill::build_manifest` 同一个符号，而这条金标把它的字节口径钉死。
    const GOLDEN_HASH: &str =
        "sha256:e1fb47095775209b81c5d404e36f5b2285ac7744cb39960cc761cf211543d79d";

    /// 校验认的**就是** `mc_core::skill::build_manifest` 的那份 digest。
    ///
    /// 两条断言合起来才成立：① 金标把 `mc_core` 的字节口径钉死（手工按分节口径重算过，
    /// 见 `crates/mc-core/src/skill.rs` 的同名用例）；② 「换一个 64 位 hex 就判不过」
    /// 证明本模块**没有**接受任何自己算出来的值。任何「同结果的新实现」都会在 ① 上先红，
    /// 任何「本地又抄了一份哈希」都会在 ② 上露出（它不会正好等于 mc_core 的答案，除非
    /// 它算的确实是同一套字节）。
    #[test]
    fn validate_accepts_exactly_the_mc_core_digest() {
        let (data, reference) = seal(bundle(&[("a.md", "a"), ("b.md", "bb")]));

        // ① 金标：分节顺序 = v1, source, id, name, description, content，然后按 path 升序
        //    每个文件的 (path, "sha256:<hex>", content)，每节 `len:value\n`。
        assert_eq!(data.hash, GOLDEN_HASH);

        // ② 认这个 digest，且只认这个。
        assert!(validate_skill_bundle(&reference, &data));
        let mut tampered = reference.clone();
        tampered.hash = format!("sha256:{}", "0".repeat(64));
        assert!(!validate_skill_bundle(&tampered, &data));
    }

    #[test]
    fn validate_rejects_id_and_source_drift() {
        let (data, reference) = seal(bundle(&[]));

        let mut wrong_id = reference.clone();
        wrong_id.id = "other".into();
        assert!(!validate_skill_bundle(&wrong_id, &data));

        let mut wrong_source = reference.clone();
        wrong_source.source = "plugin".into();
        assert!(!validate_skill_bundle(&wrong_source, &data));
    }

    /// 源不认识 ⇒ 在能算之前就拒（本仓 [`ManifestInput::source`] 是封闭枚举）。
    #[test]
    fn unknown_sources_are_rejected_before_hashing() {
        assert!(parse_source("workspace").is_some());
        assert!(parse_source("builtin").is_some());
        assert!(parse_source("plugin").is_some());
        for raw in ["", "git", "Workspace", "workspace "] {
            assert!(parse_source(raw).is_none(), "{raw:?} must be unknown");
        }
    }

    #[test]
    fn validate_rejects_file_count_drift() {
        let (data, mut reference) = seal(bundle(&[("a.md", "a")]));
        reference.file_count = 2;
        assert!(!validate_skill_bundle(&reference, &data));
        assert!(validate_skill_bundle(
            &{
                let mut ok = reference.clone();
                ok.file_count = 1;
                ok
            },
            &data
        ));
    }

    #[test]
    fn validate_rejects_unsafe_file_paths() {
        for bad in ["/a.md", "../a.md", "a//b.md", "a\\b.md"] {
            let (data, reference) = seal(bundle(&[(bad, "x")]));
            assert!(
                !validate_skill_bundle(&reference, &data),
                "path {bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn validate_checks_size_bytes_only_when_declared() {
        let (data, mut reference) = seal(bundle(&[]));
        reference.size_bytes = data.size_bytes + 1;
        assert!(!validate_skill_bundle(&reference, &data));
        reference.size_bytes = 0; // 未声明 ⇒ 不比
        assert!(validate_skill_bundle(&reference, &data));
    }

    #[test]
    fn bundle_path_is_sanitized_per_segment() {
        let cache = SkillBundleCache::new("/cache");
        let mut reference = SkillRefData {
            id: "../evil".into(),
            source: "wor kspace".into(),
            name: String::new(),
            description: String::new(),
            hash: "sha256:aa".into(),
            size_bytes: 0,
            file_count: 0,
            files: Vec::new(),
        };
        let path = cache.bundle_path("..", &reference);
        assert_eq!(
            path,
            Path::new("/cache/_../wor_kspace/.._evil/sha256_aa/bundle.json")
        );
        reference.id = String::new();
        assert_eq!(
            cache.bundle_path("", &reference),
            Path::new("/cache/_/wor_kspace/_/sha256_aa/bundle.json")
        );
    }

    #[test]
    fn store_then_load_round_trips_a_valid_bundle() {
        let root = TestRoot::new("round-trip");
        let cache = SkillBundleCache::new(root.path());
        let (data, reference) = seal(bundle(&[("a.md", "a")]));

        cache.store(WORKSPACE, &data).expect("store");
        let path = cache.bundle_path(WORKSPACE, &reference);
        assert!(path.is_file(), "bundle.json must exist");
        assert_eq!(cache.load(WORKSPACE, &reference), Some(data));

        // store 是原子的：同级不留 `.bundle-*` 临时目录。
        let leftovers: Vec<String> = fs::read_dir(path.parent().expect("dir").parent().expect("p"))
            .expect("read")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.starts_with(TEMP_DIR_PREFIX))
            .collect();
        assert!(leftovers.is_empty(), "temp dirs left behind: {leftovers:?}");
    }

    #[test]
    fn load_evicts_a_bundle_whose_hash_no_longer_matches() {
        let root = TestRoot::new("evict");
        let cache = SkillBundleCache::new(root.path());
        let (data, reference) = seal(bundle(&[]));
        cache.store(WORKSPACE, &data).expect("store");

        let mut drifted = reference.clone();
        drifted.hash = format!("sha256:{}", "1".repeat(64));
        assert_eq!(
            cache.load(WORKSPACE, &drifted),
            None,
            "a drifted ref must miss"
        );
    }

    #[test]
    fn load_evicts_unparsable_json() {
        let root = TestRoot::new("bad-json");
        let cache = SkillBundleCache::new(root.path());
        let (_, reference) = seal(bundle(&[]));
        let path = cache.bundle_path(WORKSPACE, &reference);
        fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        fs::write(&path, b"{not json").expect("write garbage");

        assert_eq!(cache.load(WORKSPACE, &reference), None);
        assert!(!path.exists(), "unparsable cache file must be evicted");
    }

    #[test]
    fn disabled_cache_is_a_no_op() {
        let cache = SkillBundleCache::disabled();
        let (data, reference) = seal(bundle(&[]));
        assert!(!cache.is_enabled());
        assert_eq!(cache.load(WORKSPACE, &reference), None);
        cache.store(WORKSPACE, &data).expect("no-op store");
        assert_eq!(
            cache.with_ref_lock(WORKSPACE, &reference, || 7),
            7,
            "disabled cache still runs the closure"
        );
    }

    #[test]
    fn with_ref_lock_runs_the_closure_under_the_key() {
        let root = TestRoot::new("lock");
        let cache = SkillBundleCache::new(root.path());
        let (_, reference) = seal(bundle(&[]));
        let value = cache.with_ref_lock(WORKSPACE, &reference, || "inside".to_string());
        assert_eq!(value, "inside");
    }

    #[test]
    fn cache_root_from_env_is_read_once() {
        // 只断言它与环境变量同源（同一个进程内只算一次）——不依赖外部是否设置。
        let expected =
            super::super::non_empty_env("MULTICA_DAEMON_SKILL_CACHE_DIR").map(PathBuf::from);
        assert_eq!(cache_root_from_env(), expected);
        assert_eq!(cache_root_from_env(), expected);
    }
}
