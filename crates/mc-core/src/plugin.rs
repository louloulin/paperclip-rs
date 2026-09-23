//! Plugin 领域类型（M6-0 anchor **重写**，不是扩充）。
//!
//! 类型来源 = `migrations/upstream/**` 的**真实列**。本文件覆盖 W6 的 plugin 面实体：
//!
//! | 实体 | 表 | 建表迁移 | 追加迁移 | 列数 |
//! | --- | --- | --- | --- | ---: |
//! | [`PluginInstallation`] | `plugin_installation` | `344`（重置后重建） | `362` / `369` / `392` | 15 |
//! | [`PluginStorageEntry`] | `plugin_storage` | `344` | — | 8 |
//! | [`PluginSecret`] | `plugin_secret` | `344` | — | 6 |
//! | [`PluginInvocation`] | `plugin_invocation` | `362` | `399` / `402` | 13 |
//! | [`PluginHookSchedule`] | `plugin_hook_schedule` | `399` | — | 12 |
//! | [`PluginPackage`] | `plugin_package` | `392` | — | 7 |
//! | [`PluginPackageVersion`] | `plugin_package_version` | `392` | — | 9 |
//! | [`PluginPackageFile`] | `plugin_package_file` | `392` | — | 7 |
//!
//! **本波 0 新迁移**：八张表全部已在上游。`344_plugin_v2_reset` 把**上一代** 14 张表
//! （`plugin_release` / `plugin_binding` / `plugin_grant` / `plugin_execution_manifest` …）
//! 连同三个触发器一起 DROP 了；重写前的 `plugin.rs` 描述的正是那一代，**不要再参考**。
//!
//! # 旧 stub 错在哪（重写前的实测）
//!
//! | 旧 stub | 真值 |
//! | --- | --- |
//! | `Plugin`（`display_name` / `install_order` / `package_path` / `config_revision` / `secret_revision` / `via_attribution`） | 六个列**都不存在**；`344` 之后是 [`PluginInstallation`] |
//! | `PluginManifestV1`（`entry` / `actions` / `permissions` / `config_schema`） | **不是**本文件的类型；manifest 契约归 `mc-plugin-host::manifest`（M6-1），本文件不复制（见下「不做什么」） |
//! | `PluginAction` | 同上，属 manifest 契约面 |
//! | `source_url` | `344` 建过，**`392` 已 DROP**（换成 `package_version_id`） |
//! | `manifest` 是自由 JSON | `CHECK jsonb_typeof(manifest) = 'object'`；`granted_scopes` 是 array，`config` 是 object |
//! | plugin 有 `status` 列 | 没有；只有 `enabled BOOLEAN`（[`PluginStatus`] 是**派生**两态，不是列） |
//!
//! # 不做什么（避免两处真值）
//!
//! `plugincontract` 的 **manifest 契约类型**（`Manifest` / `Contributes` / `Surface` /
//! `Hook` / `HookTransport` / `HookSchedule` / `Resource` / `ConfigField` / `ConfigSchema` /
//! `Capabilities`）归 **M6-1 的 `mc-plugin-host`**；`docs/57-M6-PLAN.md` §5 在本文件列过
//! `PluginSurface` / `PluginHook`，但它们是 manifest 的直接投影 —— 在此重复定义会让
//! 「管理员同意的 manifest」出现第二份结构化副本。故本文件**只**保留列/约束/JSONB 直接
//! 支撑的类型；该偏差登记在 `docs/32` §9 的偏离表。
//!
//! 同理，`plugin_package_file.content` 是 `BYTEA`：本文件用 `Vec<u8>`（repos 层口径
//! 见 `docs/57` §2.3：bytea → `Option<Vec<u8>>`，非空时即 `Vec<u8>`）。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::id::Id;
use super::timestamp::Timestamp;

/// 一次安装（表 `plugin_installation`，15 列）。
///
/// 一行 = 一个 `(workspace, plugin_key)` 的同意结果：`manifest` 是**管理员当时看到并
/// 同意的那份快照**，不是源地址今天吐出来的东西（升级 = 重新抓取 + 重新同意 + 覆盖）。
/// `package_version_id`（`392`，`NOT NULL`）把「浏览器实际运行的字节」钉到一个不可变
/// 版本上；`source_url` 已随 `392` DROP。
///
/// 无外键（仓库策略：关系由应用层在同一事务里维护）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginInstallation {
    pub id: Id,
    pub workspace_id: Id,
    /// `CHECK char_length BETWEEN 3 AND 255`。
    pub plugin_key: String,
    /// `CHECK char_length BETWEEN 1 AND 64`。
    pub version: String,
    /// 同意的 manifest 快照；`CHECK jsonb_typeof = 'object'`。
    pub manifest: serde_json::Value,
    /// `CHECK jsonb_typeof = 'array'`，元素是 [`PluginScope`]。
    pub granted_scopes: Vec<PluginScope>,
    /// `CHECK jsonb_typeof = 'object'`。
    pub config: serde_json::Value,
    pub enabled: bool,
    pub installed_by: Option<Id>,
    /// `362` 加：安装 token 只存**哈希**（`mpi_` 前缀明文只在签发时出现一次）。
    pub token_hash: Option<String>,
    pub token_rotated_at: Option<Timestamp>,
    /// `369` 加：`{"<hook_key>": {...}}`，见 [`PluginMcpApprovals`]。
    pub mcp_approvals: PluginMcpApprovals,
    /// `392` 加：绑定的不可变版本，`NOT NULL`。
    pub package_version_id: Id,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// 安装的启用态（**派生**，不是列）。
///
/// 表里只有 `enabled BOOLEAN`；这里是响应与守卫用的两态投影（M6-5 起）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginStatus {
    Enabled,
    Disabled,
}

impl PluginStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
        }
    }

    #[must_use]
    pub const fn from_enabled(enabled: bool) -> Self {
        if enabled {
            Self::Enabled
        } else {
            Self::Disabled
        }
    }
}

/// 一项授权范围（`granted_scopes` 数组的**元素**）。
///
/// ⚠️ 取值集合的权威是 `mc-plugin-host::capabilities`（M6-1，上游 `plugincontract` 的
/// capability 常量表）——本类型故意**不枚举**取值，只做新类型包装，免得与 M6-1 的契约
/// 两处真值。比较用 [`PluginScope::as_str`]。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PluginScope(String);

impl PluginScope {
    #[must_use]
    pub fn new(scope: impl Into<String>) -> Self {
        Self(scope.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PluginScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// 插件自有的键值状态（表 `plugin_storage`，8 列）。
///
/// 恰好两种 scope（`scope_type CHECK IN ('workspace','user')`，见 [`PluginStorageScope`]）：
/// workspace 是团队共享态（`scope_id` = workspace id），user 是每成员态（= user id）。
/// 配额由应用层写时强制（key ≤1024 字节、value ≤102400 字节，列上有 `octet_length`
/// CHECK；另有 1000 键 / 5MiB 总量软额，见上游 `plugin_storage.go`），**不做淘汰**。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginStorageEntry {
    pub id: Id,
    pub installation_id: Id,
    pub scope_type: PluginStorageScope,
    pub scope_id: Id,
    pub key: String,
    pub value: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// 存储作用域（列 `plugin_storage.scope_type` 的 CHECK 闭集）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginStorageScope {
    /// 团队共享；`scope_id` = workspace id。
    Workspace,
    /// 每成员；`scope_id` = user id（含该成员对插件外部服务的凭据）。
    User,
}

impl PluginStorageScope {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::User => "user",
        }
    }
}

/// 插件密钥（表 `plugin_secret`，6 列）。
///
/// 单独一张表，好让**任何**存储读路径按构造就够不到它：`ciphertext` 用部署密钥
/// （`MULTICA_PLUGIN_SECRET_KEY`，见 `mc-http::state::plugin_key`）加密，任何接口都不回显。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginSecret {
    pub id: Id,
    pub installation_id: Id,
    /// `CHECK char_length BETWEEN 1 AND 128`。
    pub key: String,
    /// AES-256-GCM 密文（`nonce ‖ ciphertext ‖ tag`），`CHECK octet_length > 0`。
    pub ciphertext: Vec<u8>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// 一次 hook 调用（表 `plugin_invocation`，13 列）。
///
/// **故意不是审计日志**：它是 TTL 清扫的运维表，不存请求/响应体，产品里没有任何
/// 「用户的数据后来怎么了」的判定读它。它的职责是：让作者看见自己的端点为什么失败、
/// 喂每 hook 的限流、让事件派发决定何时跳闸。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginInvocation {
    pub id: Id,
    pub installation_id: Id,
    pub workspace_id: Id,
    /// `CHECK char_length BETWEEN 1 AND 128`。
    pub hook_key: String,
    pub trigger: PluginInvocationTrigger,
    pub status: PluginInvocationStatus,
    /// 仅事件触发时有值，其余为 `NULL`。
    pub event_type: Option<String>,
    /// `CHECK BETWEEN 1 AND 10`。
    pub attempt: i32,
    /// `CHECK >= 0`。
    pub latency_ms: i32,
    /// 脱敏、有界、只描述宿主自己的失败（≤500 字符）；**绝不**是响应体。
    pub error: Option<String>,
    /// `399` 加：重试间稳定的投递 id（`CHECK NULL OR length 1..128`）。
    pub delivery_id: Option<String>,
    /// `399` 加：cron 的**计划发生时刻**，与 `created_at`（真实尝试时刻）不同。
    pub planned_at: Option<Timestamp>,
    pub created_at: Timestamp,
}

/// 调用触发器（列 `plugin_invocation.trigger`；`399` 的 CHECK + `402` VALIDATE 后为闭集）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginInvocationTrigger {
    /// 沙箱界面按下的按钮：代表**那个人**（`comment.author_id` 仍是成员）。
    Ui,
    /// 服务端手发。
    Manual,
    /// 事件派发：没有「人」可借，作者身份落安装 id。
    Event,
    /// agent 工具调用。
    Agent,
    /// `399` 加的 cron 触发。
    Schedule,
}

impl PluginInvocationTrigger {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ui => "ui",
            Self::Manual => "manual",
            Self::Event => "event",
            Self::Agent => "agent",
            Self::Schedule => "schedule",
        }
    }

    /// CHECK 闭集（`399` 之后）。
    pub const ALL: [Self; 5] = [
        Self::Ui,
        Self::Manual,
        Self::Event,
        Self::Agent,
        Self::Schedule,
    ];
}

/// 调用结果（列 `plugin_invocation.status` 的 CHECK 闭集）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginInvocationStatus {
    Ok,
    Failed,
    Timeout,
    Refused,
}

impl PluginInvocationStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Failed => "failed",
            Self::Timeout => "timeout",
            Self::Refused => "refused",
        }
    }
}

/// HTTP hook 的持久化调度声明（表 `plugin_hook_schedule`，12 列）。
///
/// 权威仍是**同意的 manifest**；本表是让每个已安装 hook 拿到激活纪元与稳定 scheduler
/// 作用域的**执行投影**，免得每个 tick 重新解析全部安装。`next_run_at` 是**展示用**投影：
/// 它陈旧或为 `NULL` 时，派发正确性仍能从 `cron + activated_at + sys_cron_executions` 恢复。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginHookSchedule {
    pub id: Id,
    pub installation_id: Id,
    pub workspace_id: Id,
    pub hook_key: String,
    /// `CHECK char_length BETWEEN 1 AND 255`。
    pub cron_expression: String,
    /// IANA 名（`CHECK char_length BETWEEN 1 AND 255`）。
    pub timezone: String,
    /// 每次 enable/reconcile 换代；派发据此忽略上一代的在途投递。
    pub generation: Id,
    pub activated_at: Timestamp,
    pub next_run_at: Option<Timestamp>,
    pub enabled: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// 每 workspace 一个可发布的插件身份（表 `plugin_package`，7 列）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginPackage {
    pub id: Id,
    pub workspace_id: Id,
    /// `CHECK char_length BETWEEN 3 AND 255`。
    pub plugin_key: String,
    /// `CHECK char_length BETWEEN 1 AND 160`。
    pub name: String,
    pub created_by: Option<Id>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// 一个已发布版本（表 `plugin_package_version`，9 列）。
///
/// **按构造不可变**：应用里没有任何语句更新这一行，重发同版本是冲突而不是覆盖 ——
/// 「管理员同意的」与「浏览器运行的」必须指同一批字节。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginPackageVersion {
    pub id: Id,
    pub package_id: Id,
    /// 反范式列：让每次读都能不经 join 就限定 workspace。
    pub workspace_id: Id,
    /// `CHECK char_length BETWEEN 1 AND 64`。
    pub version: String,
    pub manifest: serde_json::Value,
    /// 上传包体的 sha256，**纯 hex**（`CHECK char_length = 64`，无 `sha256:` 前缀）。
    pub digest: String,
    /// `CHECK >= 0`。
    pub size_bytes: i64,
    pub published_by: Option<Id>,
    pub created_at: Timestamp,
}

/// 版本里带的一个文件（表 `plugin_package_file`，7 列）。
///
/// 存 Postgres 而不是对象存储：包体由服务端限到几百 KB，这样「manifest 快照」与
/// 「它点名的代码」同事务提交、同事务恢复，不会出现半个插件可用。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginPackageFile {
    pub id: Id,
    pub version_id: Id,
    /// `CHECK char_length BETWEEN 1 AND 1024`。
    pub path: String,
    /// `BYTEA`。
    pub content: Vec<u8>,
    /// `CHECK >= 0`。
    pub size_bytes: i64,
    /// **纯 hex**（`CHECK char_length = 64`）。
    pub sha256: String,
    pub created_at: Timestamp,
}

/// 管理员对某个 MCP transport hook 工具清单的批准（`plugin_installation.mcp_approvals` 的值）。
///
/// 存在的理由：http hook 的端点与形状都在 manifest 里写死，而 MCP 服务器**运行时**决定
/// 自己提供什么、且随时能改；没有这个钉，一次「安装插件」就等于对「下周它想提供什么」
/// 的永久授权。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginMcpApproval {
    pub tools: Vec<PluginApprovedTool>,
    /// 上游 jsonb 里是 RFC3339 字符串（`#[serde(transparent)]` 同形）。
    pub approved_at: Timestamp,
    /// 省略或写 UUID 字符串 —— 空串不是合法 [`Id`]，写侧不得用 `""` 代替「无」。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<Id>,
}

/// 被批准的一个工具：名字 + schema 摘要（漂移即停止调用）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginApprovedTool {
    pub name: String,
    pub schema_digest: String,
}

/// `mcp_approvals` 整体：**按 hook key 索引**（一次安装可以有多个 mcp hook，各自批准）。
pub type PluginMcpApprovals = BTreeMap<String, PluginMcpApproval>;

/// 两种插件凭据（上游 `service/plugin_token.go` 的两个前缀常量）。
///
/// - [`PluginTokenKind::Install`]：`mpi_` 前缀，让插件自己的后端**没有真人**也能调 Action API；
///   库里只存哈希，数据库被读也铸不出可用凭据；轮换 = 改 `token_hash` + 写 `token_rotated_at`。
/// - [`PluginTokenKind::Callback`]：`mpc_` 前缀，hook 处理器回调用的短命 token（进程内，不落库）。
///
/// ⚠️ 这**不违反** surface 规则：token 永远不进 iframe，surface 也拿不到它；只是「系统里
/// 没有 token」这句话变成了「token 只在服务器之间移动」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginTokenKind {
    Install,
    Callback,
}

impl PluginTokenKind {
    /// Token 明文前缀（签发时用；库里只留哈希）。
    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Install => "mpi_",
            Self::Callback => "mpc_",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_vocabularies_match_migration_checks() {
        // `plugin_storage.scope_type`（344）
        assert_eq!(PluginStorageScope::Workspace.as_str(), "workspace");
        assert_eq!(PluginStorageScope::User.as_str(), "user");
        // `plugin_invocation.trigger`（399 的 CHECK + 402 VALIDATE）
        assert_eq!(
            PluginInvocationTrigger::ALL.map(PluginInvocationTrigger::as_str),
            ["ui", "manual", "event", "agent", "schedule"]
        );
        // `plugin_invocation.status`（362）
        assert_eq!(PluginInvocationStatus::Ok.as_str(), "ok");
        assert_eq!(PluginInvocationStatus::Refused.as_str(), "refused");
        // 凭据前缀（service/plugin_token.go）
        assert_eq!(PluginTokenKind::Install.prefix(), "mpi_");
        assert_eq!(PluginTokenKind::Callback.prefix(), "mpc_");
    }

    #[test]
    fn plugin_status_is_derived_from_enabled() {
        // 表里没有 status 列；这是投影，不是第二份真值。
        assert_eq!(PluginStatus::from_enabled(true).as_str(), "enabled");
        assert_eq!(PluginStatus::from_enabled(false).as_str(), "disabled");
    }

    #[test]
    fn scope_is_a_transparent_newtype() {
        let scope = PluginScope::new("issue:read");
        assert_eq!(scope.as_str(), "issue:read");
        assert_eq!(scope.to_string(), "issue:read");
        let json = serde_json::to_string(&scope).unwrap();
        assert_eq!(json, "\"issue:read\"");
    }

    #[test]
    fn mcp_approvals_are_keyed_by_hook() {
        // 369 的 jsonb 形状：{"<hook_key>": {"tools": [...], "approved_at": ..., "approved_by": ...}}
        let raw = r#"{"sync":{"tools":[{"name":"pull","schema_digest":"sha256:ab"}],
                       "approved_at":"2026-01-02T03:04:05Z","approved_by":null}}"#;
        let approvals: PluginMcpApprovals = serde_json::from_str(raw).unwrap();
        let approval = approvals.get("sync").expect("hook key 是索引");
        assert_eq!(approval.tools.len(), 1);
        assert_eq!(approval.tools[0].name, "pull");
        assert_eq!(approval.approved_by, None);
    }
}
