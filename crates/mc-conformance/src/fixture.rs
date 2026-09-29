//! fixture 模型：golden 契约的解析与内存表示。
//!
//! 从 `lib.rs` 拆出（门 ⑩ 第 8 批）。**对外路径逐字不变**：这些符号仍由 crate 根
//! `pub use fixture::{…}` 重导出，`mc_conformance::Fixture` / `::ActorKind` 等引用照旧。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::requirements::{requirement, REPO_SIDE_PRECONDITIONS};
use crate::SCHEMA_VERSION;

// ---------------------------------------------------------------------------
// fixture 模型
// ---------------------------------------------------------------------------

/// actor 类型：上游是怎么把自己"介绍"给 handler 的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    /// 没带任何身份 header。
    Anonymous,
    /// 带 `X-User-ID`。
    Member,
    /// 带 `X-Agent-ID` / `X-Task-ID`。
    Agent,
    /// 带 `Authorization`（个人访问令牌）。
    Token,
    /// daemon 身份：上游把它放在**请求 context** 里（`middleware.WithDaemonContext`），
    /// 所以线上一个身份 header 都没有 —— 而这不等于「匿名」。
    ///
    /// 🔴 这个 variant 是 §201.2 子根因 A 的修复面：抽取器以前只看字面 header，
    /// 于是把 21 条 daemon 场景标成 [`Self::Anonymous`]，回放时**不带任何凭据**，
    /// 每个都在 `routes/daemon/scope.rs` 的 `unauthorized("missing Authorization header")`
    /// 那一层得 401，**根本走不到被测逻辑**（而期望值是 200/404/400/500 各异的）。
    Daemon,
    /// 系统内部调用。
    System,
}

impl ActorKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anonymous => "anonymous",
            Self::Member => "member",
            Self::Agent => "agent",
            Self::Token => "token",
            Self::Daemon => "daemon",
            Self::System => "system",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Actor {
    pub kind: ActorKind,
    #[serde(default)]
    pub upstream_identity: BTreeMap<String, String>,
    /// 身份是怎么施加的（仅 [`ActorKind::Daemon`] 有值）：上游把 daemon 身份放在**请求
    /// context** 里，所以它不出现在任何一个 header 上。保留这个字段是为了让「为什么这条
    /// 不是 anonymous」在 fixture 本身里可读 —— 判据靠的是它，不靠回忆。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_source: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Expect {
    pub status: u16,
    #[serde(default)]
    pub json_subset: serde_json::Value,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub file: String,
    pub line: u64,
    pub test: String,
    pub site: String,
    #[serde(default)]
    pub via: String,
    #[serde(default)]
    pub commit: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Extraction {
    #[serde(default)]
    pub notes: Vec<String>,
    /// `$symbol -> 语义`，由抽取器声明；回放器只接受自己会绑定的语义。
    #[serde(default)]
    pub bindings: BTreeMap<String, String>,
    /// 上游测试在驱动这次请求前**额外装配**了什么（daemon 身份 / cookie 会话 /
    /// mock DB / 假 cloud proxy / 拒绝一切的限流器 / stub 掉的 OAuth 往返）。
    ///
    /// 抽取器写这一份（它读得到上游代码），[`REPO_SIDE_PRECONDITIONS`] 写本仓那一份；
    /// [`Fixture::requirement_ids`] 是两者的合流。
    #[serde(default)]
    pub requires: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fixture {
    pub schema_version: u32,
    pub id: String,
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub path_params: BTreeMap<String, String>,
    #[serde(default)]
    pub query: BTreeMap<String, String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    pub actor: Actor,
    #[serde(default)]
    pub body: Option<serde_json::Value>,
    pub expect: Expect,
    pub source: Source,
    #[serde(default)]
    pub extraction: Extraction,
}

impl Fixture {
    /// fixture 自己的完整性检查 —— 坏 fixture 必须在加载时报错，不能悄悄跳过。
    pub fn verify(&self) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(anyhow!(
                "{}: schema_version {} != {}",
                self.id,
                self.schema_version,
                SCHEMA_VERSION
            ));
        }
        if self.id.trim().is_empty() || self.method.trim().is_empty() || self.path.trim().is_empty()
        {
            return Err(anyhow!("fixture has empty id/method/path"));
        }
        if !self.path.starts_with('/') {
            return Err(anyhow!("{}: path must be absolute: {}", self.id, self.path));
        }
        // 前提 id 必须在回放器的登记表里（见 `requirements`）：两边不同名是拼写错误，
        // 而一个拼错的 id 会让「不可判定」的理由变成空白 —— 正是本 crate 禁止的形态。
        for id in self.requirement_ids() {
            if requirement(id).is_none() {
                return Err(anyhow!("{}: unknown requirement id {id:?}", self.id));
            }
        }
        // 每个 `{name}` 都必须有 path_params 提供取值，否则请求会带着字面量 `{name}` 打出去。
        for seg in self.path.split('/') {
            if let Some(name) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
                if !self.path_params.contains_key(name) {
                    return Err(anyhow!(
                        "{}: path uses {{{name}}} but path_params has no entry",
                        self.id
                    ));
                }
            }
        }
        Ok(())
    }

    /// fixture 的领域（`contracts/golden/<domain>/<case>.json` 的 `<domain>`）。
    #[must_use]
    pub fn domain(&self) -> String {
        self.id.split('/').next().unwrap_or("_").to_string()
    }

    /// 这条场景的全部前提：抽取器记的（上游装配了什么）+ [`REPO_SIDE_PRECONDITIONS`]
    /// 记的（本仓的实现第一步就要什么）。去重且有序，让报告可字节复现。
    #[must_use]
    pub fn requirement_ids(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = self
            .extraction
            .requires
            .iter()
            .map(String::as_str)
            .collect();
        for (method, path, extra) in REPO_SIDE_PRECONDITIONS {
            if self.method.eq_ignore_ascii_case(method) && self.path == *path {
                ids.extend(extra.iter().copied());
            }
        }
        ids.sort_unstable();
        ids.dedup();
        ids
    }
}

/// 读单个 fixture 文件。
pub fn load_file(path: &Path) -> Result<Fixture> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let fx: Fixture =
        serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
    fx.verify()
        .with_context(|| format!("verify {}", path.display()))?;
    Ok(fx)
}

/// 读一个 golden 目录下所有 `*/**.json`，按 id 排序（顺序稳定 ⇒ 报告可复现）。
pub fn load_dir(dir: &Path) -> Result<Vec<Fixture>> {
    let mut files: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("read_dir {}", dir.display()))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        for sub in std::fs::read_dir(entry.path())? {
            let sub = sub?;
            let p = sub.path();
            if p.extension().and_then(|e| e.to_str()) == Some("json") {
                files.push(p);
            }
        }
    }
    files.sort();
    let mut out = Vec::with_capacity(files.len());
    for p in files {
        out.push(load_file(&p)?);
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}
