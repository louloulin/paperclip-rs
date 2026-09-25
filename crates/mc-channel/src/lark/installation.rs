//! lark **安装面**：`lark_installation` 的读 / 写 / 撤销 / upsert（上游
//! `internal/integrations/lark/installation.go`，**159 行** —— 全片最小的一个上游文件，
//! 但它持有本片唯一的**凭据写路径**）。
//!
//! - **写者**：M7-14（`docs/60-M7-PLAN.md` §3.3）。
//! - **上游定位**（注释逐字）：`InstallationService` 是通向 `lark_installation` 一行的
//!   **唯一**路径，它把 `app_secret` 的 `secretbox` 加密集中在一处，于是"调用方永远碰不到
//!   明文"；`Upsert` 是设备流成功与（若有）管理面重装共用的那一格。
//!
//! # 表的选择：**遗留 `lark_*`**（这是合并树的硬约束，不是我的偏好）
//!
//! 上游 `lark/channel_store.go` 把 `GetLarkInstallation…` / `UpsertLarkInstallation` /
//! `ListActiveLarkInstallations` **全部**转发到泛化 `channel_*` 查询（`NewChannelStore` 的
//! 模块注释：*"lark_* calls resolve to channel_* rows (MUL-3515)"*）。**本仓不照抄这条**：
//! `crates/mc-repos/src/channel/installation.rs` 的硬约束（M7-1 落地、M7-12 已建在它上面）
//! 把 lark 钉在**遗留** `lark_installation` 上（`docs/60` §6.4 / R-M7-5：两族表上游同时在用，
//! 合并会静默丢数据）。⇒ 本文件的端口读写的表名是 `lark_installation`，与
//! [`super::channel_store`] 的 `LEGACY_INSTALLATION_TABLE` 同源。口径更正已登记 `docs/32` §30
//! 的 **D1**（与 §32 的同名登记是同一个事实的两面）。
//!
//! # 三条从上游逐字搬来的判决
//!
//! 1. **凭据在服务边界内封好**：明文 `app_secret` 进 [`InstallationParams`]、出
//!    `secretbox` 密文 —— 调用方（含路由层）**拿不到**密文，也就无从泄露；
//! 2. **撤销不删行**：`active → revoked`，行留着供审计；上游逐字 *"The row is preserved
//!    (no DELETE) so audit history remains queryable"*。重装把状态翻回 `active`（`upsert` 的
//!    `DO UPDATE` 里那一格）；
//! 3. **路由槽冲突要分类，不是一句"已占用"**（上游 `liveOwnerConflictMessage`）：槽主可能是
//!    **另一个 workspace**、同 workspace 的**已归档** agent、或同 workspace 的**别的** agent。
//!    三者对管理员是三个不同的下一步 ⇒ [`classify_live_owner`] 那张表是判决的唯一出处。
//!
//! # 凭据纪律（`docs/60` §2.3 / 本片 `DoD` 第 6 条）
//!
//! - [`InstallationParams`] **手写 `Debug`**（它有一个明文 [`AppSecret`]）；
//! - [`Installation`] **手写 `Debug`**：密文只报**长度**（诊断要能看出"配没配 / 多长"，
//!   不需要值），与 [`super::resolvers::LarkInstallation`] 的同款实现一致；
//! - [`InstallError`] 每个变体只带**静态文案 / 字段名 / 平台机器码**，绝无明文、密文、密钥字节；
//! - 本文件**没有**任何 `tracing::*` 插值凭据字段。
//!
//! # 与上游的形态差异（逐条登记 `docs/32` §30）
//!
//! | # | 差异 | 理由 |
//! | --- | --- | --- |
//! | **D1** | 端口读写**遗留** `lark_installation`（上游转发到 `channel_installation`） | 合并树的硬约束，见上 |
//! | **D4** | `InstallationService::new` 的三个 nil 检查整条消失 | 本仓是值类型：`SecretBox` / `Arc<dyn …>` 都没有"半成品"状态 |
//! | **D6** | "一个事务跑三步"（advisory lock → 回收死主 → upsert）落成端口方法 [`LarkInstallationStore::persist`] | 层次铁律：`mc-channel` 不写 SQL（`docs/60` §2.6 第 1 条）⇒ 串行化在实现里，**判决**留在本文件可测 |

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use mc_core::id::Id;
use mc_secrets::secretbox::SecretBox;

use super::params::AppSecret;
use super::types::{OpenId, Region};

/// 本 adapter 的平台判别式（诊断用）。
#[must_use]
pub fn kind() -> mc_core::channel::ChannelKind {
    super::resolvers::TYPE_LARK
}

// =====================================================================
// 行投影
// =====================================================================

/// 遗留 `lark_installation` 的一行（迁移 `109` + `112`/`116` 的 **16 列**）。
///
/// 与 [`super::resolvers::LarkInstallation`] 的差别只有一条：**多三个时间戳**。两者都留着是
/// 故意的 —— 入站回路（M7-12/13）不需要时间戳，而本片的 wire 形状（`LarkInstallationResponse`
/// 的 `installed_at` / `created_at` / `updated_at`）需要；把它们塞进 resolver 的投影会让
/// 那个已经合并的类型为三个消费点各加一次无用的解码。
///
/// **手写 `Debug`**：`app_secret_encrypted` 只报长度。
#[derive(Clone, PartialEq, Eq)]
pub struct Installation {
    /// 主键。
    pub id: Id,
    /// 所属 workspace。
    pub workspace_id: Id,
    /// 绑定的 agent（`UNIQUE(workspace_id, agent_id)` 的一半）。
    pub agent_id: Id,
    /// 应用标识（`cli_…`）。**不是**秘密：上游日志逐字打印它。
    pub app_id: String,
    /// `secretbox` 密文（`nonce(12) ‖ ct ‖ tag`）；只有解密器读它。
    pub app_secret_encrypted: Vec<u8>,
    /// 租户键；`None` = 那一行没写。
    pub tenant_key: Option<String>,
    /// Bot 的按安装 `open_id`。
    pub bot_open_id: OpenId,
    /// Bot 的跨应用稳定 `union_id`；`None` / 空 = 未回填（迁移 `112`）。
    pub bot_union_id: Option<String>,
    /// 该安装所在的云（迁移 `116` 的 `CHECK` 只认两个值）。
    pub region: Region,
    /// 安装发起人（`NOT NULL`）。
    pub installer_user_id: Id,
    /// `active` / `revoked`。
    pub status: String,
    /// 首次落库时刻。
    pub installed_at: DateTime<Utc>,
    /// 行创建时刻。
    pub created_at: DateTime<Utc>,
    /// 行最后更新时刻。
    pub updated_at: DateTime<Utc>,
}

// `missing_fields_in_debug` 是**故意**豁免的：三个时间戳没有诊断价值，而"密文只报长度"
// 才是这条实现的**全部**意图（与 `resolvers::LarkInstallation` 同款、同一个理由）。
#[allow(clippy::missing_fields_in_debug)]
impl fmt::Debug for Installation {
    /// 手写脱敏：密文只报**长度**（凭据纪律第 1 条）。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Installation")
            .field("id", &self.id)
            .field("workspace_id", &self.workspace_id)
            .field("agent_id", &self.agent_id)
            .field("app_id", &self.app_id)
            .field("app_secret_encrypted_len", &self.app_secret_encrypted.len())
            .field("tenant_key", &self.tenant_key)
            .field("bot_open_id", &self.bot_open_id)
            .field("has_bot_union_id", &self.bot_union_id.is_some())
            .field("region", &self.region)
            .field("installer_user_id", &self.installer_user_id)
            .field("status", &self.status)
            .finish()
    }
}

impl Installation {
    /// 是否还能承载长连接 / 分派事件（`active`）。
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.status == "active"
    }

    /// Bot 的 `union_id`（空串 = 未回填）—— 回填判据的口。
    #[must_use]
    pub fn bot_union_id_or_empty(&self) -> &str {
        self.bot_union_id.as_deref().unwrap_or("")
    }
}

// =====================================================================
// 错误
// =====================================================================

/// 安装面的失败（上游 `installation.go` 的哨兵 + `liveOwnerConflictMessage` 的三分类）。
///
/// 每个变体只带**静态文案**、**字段名**或**机器码** —— 既不回显凭据，也不回显 SQL。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InstallError {
    /// 本 workspace 里没有这一行（HTTP 404）。
    #[error("lark installation not found")]
    NotFound,
    /// 一个必填字段缺失（HTTP 400）。
    #[error("lark: {field} is required")]
    InvalidParams {
        /// 缺失的字段名。
        field: &'static str,
    },
    /// 这个 `app_id` 已连到**另一个** Multica workspace（HTTP 409）。
    #[error("lark: this app is already connected to a different Multica workspace")]
    OwnedByAnotherWorkspace,
    /// 已连到**同一个** workspace 里的另一个（活着、未归档的）agent（HTTP 409）。
    #[error("lark: this app is already connected to another agent in this workspace")]
    OwnedBySameWorkspace,
    /// 已连到同 workspace 里一个**已归档**的 agent（归档可逆 ⇒ 行还占着槽位；HTTP 409）。
    #[error("lark: this app is connected to an archived agent in this workspace")]
    OwnedByArchivedAgent,
    /// `app_id` 的槽位被一个**活着的**持有者占着，但分类读不到它（罕见竞态；HTTP 409）。
    ///
    /// 上游在这种情况下回那句兜底文案（*"already connected to another agent…"*）；本仓把它
    /// 单独成一档，好让用例能断言"**没有**把竞态伪装成三分类之一"。
    #[error("lark: this app is already connected elsewhere")]
    ConflictUnclassified,
    /// 这个 Lark 身份已绑到**别的** Multica 用户（HTTP 409）。
    #[error("lark: this Lark account is already bound to a different Multica user")]
    AlreadyAssigned,
    /// 兑换者不是该 workspace 的成员（HTTP 403）。
    #[error("lark: the redeemer is not a member of this workspace")]
    NotWorkspaceMember,
    /// 加密失败（不透明；HTTP 500）。
    #[error("lark: sealing the app secret failed")]
    Seal,
    /// 存储层故障（不透明：不回显 SQL / 参数；HTTP 500）。
    #[error("lark: installation store failure: {message}")]
    Store {
        /// 端口回的错误正文（实现侧保证不带凭据）。
        message: String,
    },
}

impl InstallError {
    /// 稳定错误码（各端点的 `writeError` / `writeFeatureDisabled` 文案族）。
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound => "lark_installation_not_found",
            Self::InvalidParams { .. } => "lark_install_rejected",
            Self::OwnedByAnotherWorkspace => "lark_app_owned_by_another_workspace",
            Self::OwnedBySameWorkspace => "lark_app_owned_by_same_workspace",
            Self::OwnedByArchivedAgent => "lark_app_owned_by_archived_agent",
            Self::ConflictUnclassified => "lark_app_already_connected",
            Self::AlreadyAssigned => "lark_binding_already_assigned",
            Self::NotWorkspaceMember => "lark_binding_not_workspace_member",
            Self::Seal | Self::Store { .. } => "lark_install_failed",
        }
    }

    /// 上游响应矩阵的状态码。
    ///
    /// `seal / store / unexpected` 一律 500（上游逐字：*"encrypt / persist / unexpected failures
    /// are server-side, not the user's credentials"*）—— 一次数据库抖动**不该**推着管理员去
    /// 轮换一条本来没问题的密钥。
    #[must_use]
    pub fn http_status(&self) -> u16 {
        match self {
            Self::NotFound => 404,
            Self::InvalidParams { .. } => 400,
            Self::OwnedByAnotherWorkspace
            | Self::OwnedBySameWorkspace
            | Self::OwnedByArchivedAgent
            | Self::ConflictUnclassified
            | Self::AlreadyAssigned => 409,
            Self::NotWorkspaceMember => 403,
            Self::Seal | Self::Store { .. } => 500,
        }
    }
}

// =====================================================================
// 入参
// =====================================================================

/// 一次安装 / 重装的入参（上游 `InstallationParams`）。
///
/// **手写 `Debug`**：结构体里有一个**明文** [`AppSecret`] —— 默认 `Debug` 会把它写进任何
/// `{:?}` 插值、`assert_eq!` 失败回显与 panic backtrace（凭据纪律第 1 条）。
#[derive(Clone)]
pub struct InstallationParams {
    /// 目标 workspace。
    pub workspace_id: Id,
    /// 目标 agent。
    pub agent_id: Id,
    /// 应用标识（`cli_…`；`NOT NULL`）。
    pub app_id: String,
    /// 明文 `app_secret`（服务边界内立刻封好）。
    pub app_secret: AppSecret,
    /// 可选租户键（空串归一成 `None`）。
    pub tenant_key: Option<String>,
    /// Bot 的按安装 `open_id`（`NOT NULL`）。
    pub bot_open_id: OpenId,
    /// Bot 的 `union_id`（设备流成功路径才有；`None` = 留给回填）。
    pub bot_union_id: Option<String>,
    /// 安装发起人（`NOT NULL`）。
    pub installer_user_id: Id,
    /// 该安装所在的云（设备流 mid-poll 的 `tenant_brand` 决定；空回落飞书）。
    pub region: Region,
}

impl fmt::Debug for InstallationParams {
    /// 手写脱敏（凭据纪律第 1 条）。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InstallationParams")
            .field("workspace_id", &self.workspace_id)
            .field("agent_id", &self.agent_id)
            .field("app_id", &self.app_id)
            .field("app_secret", &self.app_secret)
            .field("tenant_key", &self.tenant_key)
            .field("bot_open_id", &self.bot_open_id)
            .field("has_bot_union_id", &self.bot_union_id.is_some())
            .field("installer_user_id", &self.installer_user_id)
            .field("region", &self.region)
            .finish()
    }
}

impl InstallationParams {
    /// 装配（空串 `tenant_key` / 空串 `bot_union_id` 归一成 `None`）。
    #[must_use]
    #[allow(clippy::too_many_arguments)] // 上游的字段集：逐字对齐比"分组"重要（调用点只有两处）。
    pub fn new(
        workspace_id: Id,
        agent_id: Id,
        app_id: impl Into<String>,
        app_secret: impl Into<String>,
        bot_open_id: OpenId,
        installer_user_id: Id,
    ) -> Self {
        Self {
            workspace_id,
            agent_id,
            app_id: app_id.into().trim().to_string(),
            app_secret: AppSecret::new(app_secret.into().trim()),
            tenant_key: None,
            bot_open_id,
            bot_union_id: None,
            installer_user_id,
            region: Region::default(),
        }
    }

    /// 补 `tenant_key`（空串 ⇒ `None`）。
    #[must_use]
    pub fn with_tenant_key(mut self, tenant_key: impl Into<String>) -> Self {
        let raw = tenant_key.into().trim().to_string();
        self.tenant_key = if raw.is_empty() { None } else { Some(raw) };
        self
    }

    /// 补 `bot_union_id`（空串 ⇒ `None`）。
    #[must_use]
    pub fn with_bot_union_id(mut self, union_id: impl Into<String>) -> Self {
        let raw = union_id.into().trim().to_string();
        self.bot_union_id = if raw.is_empty() { None } else { Some(raw) };
        self
    }

    /// 指定云。
    #[must_use]
    pub fn with_region(mut self, region: Region) -> Self {
        self.region = region;
        self
    }

    /// 换一个 workspace（用例用它造"缺字段 / 跨区"的入参）。
    #[must_use]
    pub fn with_workspace(mut self, workspace_id: Id) -> Self {
        self.workspace_id = workspace_id;
        self
    }

    /// 换一个 bot `open_id`（用例用它造缺 `bot_open_id` 的入参）。
    #[must_use]
    pub fn with_bot_open_id(mut self, bot_open_id: OpenId) -> Self {
        self.bot_open_id = bot_open_id;
        self
    }
}

/// 必填字段的**预检**（上游 `validateInstallationParams` 的 `switch`，**逐条同序**）。
///
/// 它**不**向 Lark 验证任何东西（设备流路径的验证是 mid-poll 的那些响应码）。
///
/// # Errors
///
/// 第一个缺失的字段 ⇒ [`InstallError::InvalidParams`]。
pub fn validate_installation_params(params: &InstallationParams) -> Result<(), InstallError> {
    let field = if params.workspace_id.0.is_nil() {
        "workspace_id"
    } else if params.agent_id.0.is_nil() {
        "agent_id"
    } else if params.installer_user_id.0.is_nil() {
        "installer_user_id"
    } else if params.app_id.is_empty() {
        "app_id"
    } else if params.app_secret.is_empty() {
        "app_secret"
    } else if params.bot_open_id.is_empty() {
        "bot_open_id"
    } else {
        return Ok(());
    };
    Err(InstallError::InvalidParams { field })
}

// =====================================================================
// 槽位分类（纯函数；端口实现与用例共用同一张表）
// =====================================================================

/// `app_id` 路由槽的**活着的**持有者（上游 `GetChannelInstallationOwnerByAppID` 的三列投影）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveOwner {
    /// 持有者的 workspace。
    pub workspace_id: Id,
    /// 持有者的 agent 是否已**归档**（归档可逆 ⇒ 槽位仍被占）。
    pub agent_archived: bool,
}

/// 把"槽位被活着的主占着"分类成三个对管理员**各不相同的下一步**
/// （上游 `liveOwnerConflictMessage` 的 `switch`，逐条同序）。
///
/// 上游那句兜底文案（分类读不到时）在本仓是 [`InstallError::ConflictUnclassified`] ——
/// 它是**第四档**，不是前三档的别名：用例因此能断言"读不到 ≠ 别人的 workspace"。
#[must_use]
pub fn classify_live_owner(owner: &LiveOwner, requesting_workspace_id: Id) -> InstallError {
    if owner.workspace_id != requesting_workspace_id {
        return InstallError::OwnedByAnotherWorkspace;
    }
    if owner.agent_archived {
        return InstallError::OwnedByArchivedAgent;
    }
    InstallError::OwnedBySameWorkspace
}

/// 落库判决（端口 `persist` 的返回；分类在**端口内部**用 [`classify_live_owner`] 做完）。
#[derive(Debug)]
pub enum PersistOutcome {
    /// 落好了（`INSERT … ON CONFLICT (workspace_id, agent_id) DO UPDATE` 的 `RETURNING`）。
    Stored(Box<Installation>),
    /// `app_id` 的唯一冲突，且分类把活着的持有者归到了某一档。
    Conflict(InstallError),
    /// `app_id` 的唯一冲突，但分类读不到持有者（罕见竞态）。
    UnclassifiedConflict,
}

impl PersistOutcome {
    /// 判决 → `Result`（`Stored` 是唯一的 `Ok`）。
    ///
    /// # Errors
    ///
    /// 两个冲突档 ⇒ 对应的 [`InstallError`]。
    pub fn into_result(self) -> Result<Installation, InstallError> {
        match self {
            Self::Stored(row) => Ok(*row),
            Self::Conflict(error) => Err(error),
            Self::UnclassifiedConflict => Err(InstallError::ConflictUnclassified),
        }
    }
}

// =====================================================================
// 端口
// =====================================================================

/// `lark_installation` 的读 / 写口。
///
/// 实现住在 `mc-http`（`routes/channels/lark/store.rs` 的 PG 实现）—— 因为本片写集**不含**
/// `crates/mc-repos/src/channel/**`（`docs/60` §3.3 的只读面），而那条铁律（"adapter 不得直接
/// 写 DB"）由此在**类型层面**成立：`mc-channel` 只拿到这个 trait。登记 `docs/32` §30 的 **D6**。
#[async_trait]
pub trait LarkInstallationStore: Send + Sync {
    /// 列 workspace 的全部安装（**含 revoked**；上游 `ListByWorkspace` 逐字）。
    async fn list_by_workspace(&self, workspace_id: Id) -> Result<Vec<Installation>, String>;

    /// workspace 收窄的单条：另一个 workspace 猜 id ⇒ `Ok(None)`（与不存在同结果）。
    async fn get_in_workspace(
        &self,
        installation_id: Id,
        workspace_id: Id,
    ) -> Result<Option<Installation>, String>;

    /// `active → revoked`（行**保留**；上游 `SetLarkInstallationStatus`）。
    async fn revoke(&self, workspace_id: Id, installation_id: Id) -> Result<bool, String>;

    /// 一个事务里的三步：串行化 `app_id` 槽 → 回收**死主** → upsert（见 **D6**）。
    async fn persist(
        &self,
        params: &InstallationParams,
        sealed: &[u8],
    ) -> Result<PersistOutcome, String>;

    /// 活跃安装里 `bot_union_id` 为空的那批（上游 `ListActiveLarkInstallations` 的过滤面）。
    async fn list_active_missing_union_id(&self) -> Result<Vec<Installation>, String>;

    /// 回填一条 `bot_union_id`。
    async fn set_bot_union_id(&self, installation_id: Id, union_id: &str) -> Result<(), String>;

    /// 把 `region='feishu'` 的**全部**行翻成 `lark`（回填；返回影响行数）。
    async fn relabel_region_to_lark(&self) -> Result<u64, String>;
}

// =====================================================================
// 服务
// =====================================================================

/// 安装服务（上游 `InstallationService`）。
///
/// **不派生 `Debug`**：它持有封装盒与端口（"能打印"本身不该存在）。
pub struct InstallationService {
    store: Arc<dyn LarkInstallationStore>,
    boxed: SecretBox,
}

impl InstallationService {
    /// 装配。两个参数都**必填**（上游逐字：*"we refuse to fall back to plaintext storage even
    /// in test or dev configurations"*）—— 本仓是值类型，于是这条在**类型层面**成立（**D4**）。
    #[must_use]
    pub fn new(store: Arc<dyn LarkInstallationStore>, boxed: SecretBox) -> Self {
        Self { store, boxed }
    }

    /// 借出封装盒（回填与注册服务要解/封）。
    #[must_use]
    pub fn boxed(&self) -> &SecretBox {
        &self.boxed
    }

    /// 列 workspace 的全部安装（含 revoked）。
    ///
    /// # Errors
    ///
    /// 存储层故障。
    pub async fn list_by_workspace(
        &self,
        workspace_id: Id,
    ) -> Result<Vec<Installation>, InstallError> {
        self.store
            .list_by_workspace(workspace_id)
            .await
            .map_err(|message| InstallError::Store { message })
    }

    /// workspace 收窄的单条。
    ///
    /// # Errors
    ///
    /// 不存在 ⇒ [`InstallError::NotFound`]；存储层故障。
    pub async fn get_in_workspace(
        &self,
        installation_id: Id,
        workspace_id: Id,
    ) -> Result<Installation, InstallError> {
        self.store
            .get_in_workspace(installation_id, workspace_id)
            .await
            .map_err(|message| InstallError::Store { message })?
            .ok_or(InstallError::NotFound)
    }

    /// 撤销（`active → revoked`；行保留，重装把状态翻回 `active`）。
    ///
    /// # Errors
    ///
    /// 存储层故障。
    pub async fn revoke(
        &self,
        workspace_id: Id,
        installation_id: Id,
    ) -> Result<bool, InstallError> {
        self.store
            .revoke(workspace_id, installation_id)
            .await
            .map_err(|message| InstallError::Store { message })
    }

    /// 解出明文 `app_secret`（WS hub 的消费口；**不**用于任何展示面）。
    ///
    /// # Errors
    ///
    /// 密文坏 / 明文不是 UTF-8。
    pub fn decrypt_app_secret(
        &self,
        installation: &Installation,
    ) -> Result<AppSecret, InstallError> {
        let plain = self
            .boxed
            .open(&installation.app_secret_encrypted)
            .map_err(|_| InstallError::Seal)?;
        let text = String::from_utf8(plain).map_err(|_| InstallError::Seal)?;
        Ok(AppSecret::new(text))
    }

    /// 建 / 就地刷新一条安装（上游 `Upsert`：预检 → 封 → 端口里的事务）。
    ///
    /// # Errors
    ///
    /// 必填字段缺失 / 三类槽位冲突 / 封或落库失败。
    pub async fn upsert(&self, params: &InstallationParams) -> Result<Installation, InstallError> {
        validate_installation_params(params)?;
        // 封发生在**进事务之前**：Seal 的代价不该坐在 DB 行锁里（上游逐字同一取舍）。
        let sealed = self
            .boxed
            .seal(params.app_secret.expose().as_bytes())
            .map_err(|_| InstallError::Seal)?;
        self.store
            .persist(params, &sealed)
            .await
            .map_err(|message| InstallError::Store { message })?
            .into_result()
    }
}

#[cfg(test)]
pub(crate) mod tests;
