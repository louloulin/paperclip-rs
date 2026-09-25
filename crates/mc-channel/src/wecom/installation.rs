//! `WeCom` 安装面：**list / get / revoke + BYO 安装**（上游
//! `internal/integrations/wecom/installation.go`，**545 行**）。
//!
//! - **写者**：M7-15（`docs/60-M7-PLAN.md` §3.3）。
//! - **上游定位**（注释逐字）：`installation.go` 是 `wecom` 安装行的**写面** ——
//!   它把智能机器人密钥的 `secretbox` 加密**集中在一处**，于是"调用方永远碰不到明文"；
//!   并且它是通向 `channel_installation` 里一行 `wecom` 的**唯一**路径
//!   （管理 CLI 与 HTTP 安装端点都走 `Upsert`）。
//!
//! # BYO 模型
//!
//! 管理员在 `WeCom` 管理后台建一个智能机器人，把 `bot_id` + 长连接密钥贴进 Multica。
//! **没有 OAuth 码交换**：活的校验就是**按协议握一次手**（拨号 → `aibot_subscribe` →
//! 读回 ack），见 [`crate::wecom::credentials`]。
//!
//! # 三条从上游逐字搬来的判决
//!
//! 1. **`bot_id` 与密钥是必填**（[`InstallError::InvalidParams`] ⇒ 400）；
//! 2. **路由槽是全局的、且回收会硬删别人的行** ⇒ `Upsert` 的顺序是承重的：
//!    *先*读槽主并分类、*再*探针、*最后*才写。倒过来会做出真实的伤害 ——
//!    探针自己**就是**对平台的一次副作用（订阅会把当前在线的持有者踢下线），
//!    所以一个**注定要被拒**的请求若先探针，就会把合法持有者踢掉 ⇒ 一个无权碰这个 bot 的
//!    调用者只要重放几次就能把它 DoS。上游逐字：*"a request that is about to be REFUSED
//!    still knocks the rightful owner offline"* ⇒ 被拒的请求**什么都不碰**（本地与 `WeCom`
//!    都不碰）；
//! 3. **"证明控制权"没有关掉的开关**：探针是**必填**的构造参数（上游 `ErrProbeRequired`：
//!    一个 nil 探针是接线 bug，不是一种部署模式 —— fail closed 到构造期而不是第一次安装）。
//!    本仓是值类型（`Arc<dyn CredentialProbe>` 非 `Option`），于是这条在**类型层面**成立
//!    （`docs/32` §31 的 D3）。
//!
//! # 与上游的形态差异（逐条登记 `docs/32` §31）
//!
//! | # | 差异 | 理由 |
//! | --- | --- | --- |
//! | **D3** | `box` / `probe` / `tx` 的 **nil 检查**整条消失 | 本仓是值类型：`SecretBox`、`Arc<dyn CredentialProbe>` 都没有"半成品"状态 ⇒ 三个 `Err…Required` 哨兵没有对应物 |
//! | **D6** | 那个"一个事务跑五步"（advisory lock → 读槽主 → 分类 → 回收死主 → upsert）落成**端口方法** [`InstallationStore::persist`] | 层次铁律：`mc-channel` 不写 SQL（`docs/60` §2.6 第 1 条）⇒ 串行化在实现里，**判决**留在本文件可测 |
//! | **D7** | **不做**换机器人时的**依赖行清扫**（上游 `ClearChannelInstallationBotScopedRows`：`channel_user_binding` / `channel_chat_session_binding` / `channel_task_delivery` / `channel_outbound_message`） | 它要同时删四张表、且必须在**同一个**事务里与 upsert 一起提交；本片写集不含那些表的仓储 ⇒ 登记为缺口（`wecom/dedupe.rs` 一族的 M7-16…M7-20 与 M7-21 收口）。**后果说清**：换机器人后，旧 bot 命名空间里的一条 `channel_user_binding` 会存活到用户重新绑定 —— 上游注释承认它自己也只有一个"短窗口 + 自愈"的缓解 |
//!
//! # 凭据纪律（`docs/60` §2.3）
//!
//! - [`InstallationParams`] **手写 `Debug`**（它有一个**明文** `PlaintextSecret`）；
//! - [`InstallError`] 每个变体只带**静态文案 / 平台自己的 errcode / 字段名**，
//!   绝无明文、密文、密钥字节；
//! - 本文件**没有**任何 `tracing::*` 插值凭据字段。

use std::fmt;
use std::sync::Arc;

use mc_core::channel::InstallationStatus;
use mc_core::id::Id;
use mc_secrets::secretbox::SecretBox;

use super::credentials::{
    CredentialProbe, CredentialsError, CredentialsResolver, InstallationCredentials,
    PlaintextSecret, ProbeError, SecretboxCredentialsResolver,
};
use super::store::{InstallationStore, PersistInstall, PersistOutcome, SlotOwner};
use super::types::{Installation, KIND};

/// 本 adapter 的平台判别式（诊断 / 注册用）。
#[must_use]
pub fn kind() -> mc_core::channel::ChannelKind {
    KIND
}

// =====================================================================
// 错误（上游 installation.go 的哨兵）
// =====================================================================

/// 安装面的失败。
///
/// 每个变体只带**静态文案**、**字段名**或**平台自己的 errcode** —— 既不回显 `bot_id` 之外的
/// 身份、也不回显密钥 / 密文（凭据纪律第 2 条）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InstallError {
    /// 本 workspace 里没有这一行（上游 `ErrInstallationNotFound`；HTTP 404）。
    #[error("wecom installation not found")]
    NotFound,
    /// 一个必填字段缺失（上游 `ErrInvalidInstallationParams`；HTTP 400）。
    ///
    /// 它意味着"调用方补上一个字段就能成"，与"`WeCom` 说了不"和"我们问不到 `WeCom`"
    /// 是**三种不同的下一步**，所以三者各有自己的错误码。
    #[error("wecom: {field} is required")]
    InvalidParams { field: &'static str },
    /// 这个 bot 已连到**另一个** Multica workspace。
    #[error("wecom: this bot is already connected to a different Multica workspace")]
    OwnedByAnotherWorkspace,
    /// 已连到**同一个** workspace 里的另一个（活着、未归档的）agent。
    #[error("wecom: this bot is already connected to another agent in this workspace")]
    OwnedBySameWorkspace,
    /// 已连到同 workspace 里一个**已归档**的 agent（归档可逆 ⇒ bot 还占着槽位）。
    #[error("wecom: this bot is connected to an archived agent in this workspace")]
    OwnedByArchivedAgent,
    /// `WeCom` 明确拒了这对凭据（HTTP **400**：唯一一条可以怪凭据的分支）。
    #[error("wecom: WeCom rejected this bot id and secret (errcode {errcode})")]
    CredentialsRejected { errcode: i32 },
    /// 够不着 `WeCom`，所以**没能**验证（HTTP **503**：输入没问题，是检查做不成）。
    #[error("wecom: could not reach WeCom to verify this bot (errcode {errcode})")]
    CredentialsUnverifiable { errcode: i32 },
    /// 加密失败（不透明）。
    #[error("wecom: sealing the pasted secret failed")]
    Seal,
    /// `config` blob 编码失败。
    #[error("wecom: encoding the installation config failed")]
    Encode,
    /// 存储层故障（不透明：不回显 SQL / 参数）。
    #[error("wecom: installation store failure: {message}")]
    Store { message: String },
}

impl InstallError {
    /// 稳定错误码（上游 `handler/wecom_web.go` 的 `writeWecomInstallError`，**逐字**）。
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound => "wecom_installation_not_found",
            Self::InvalidParams { .. } => "wecom_install_rejected",
            Self::OwnedByAnotherWorkspace => "wecom_bot_owned_by_another_workspace",
            Self::OwnedBySameWorkspace => "wecom_bot_owned_by_same_workspace",
            Self::OwnedByArchivedAgent => "wecom_bot_owned_by_archived_agent",
            Self::CredentialsRejected { .. } => "wecom_credentials_rejected",
            Self::CredentialsUnverifiable { .. } => "wecom_credentials_unverifiable",
            Self::Seal | Self::Encode | Self::Store { .. } => "wecom_install_failed",
        }
    }

    /// 上游响应矩阵的状态码。
    ///
    /// `Encrypt / persist / unexpected failures are server-side, not the user's credentials`
    /// （上游逐字）⇒ 一律 500：一个三十秒的 Postgres 抖动**不该**推着管理员去轮换一条
    /// 本来没问题的密钥，而 `WeCom` 密钥轮换之后**取不回来**。
    #[must_use]
    pub fn http_status(&self) -> u16 {
        match self {
            Self::NotFound => 404,
            Self::InvalidParams { .. } | Self::CredentialsRejected { .. } => 400,
            Self::OwnedByAnotherWorkspace
            | Self::OwnedBySameWorkspace
            | Self::OwnedByArchivedAgent => 409,
            Self::CredentialsUnverifiable { .. } => 503,
            Self::Seal | Self::Encode | Self::Store { .. } => 500,
        }
    }

    /// 从探针失败映射（**分流**在 [`ProbeError`] 已经做完）。
    #[must_use]
    pub fn from_probe(error: ProbeError) -> Self {
        match error {
            ProbeError::Rejected { errcode } => Self::CredentialsRejected { errcode },
            ProbeError::Unverifiable { errcode } => Self::CredentialsUnverifiable { errcode },
        }
    }

    /// 从槽位冲突映射（`classify_slot_owner` 的 `Err` 侧）。
    #[must_use]
    pub fn from_conflict(outcome: PersistOutcome) -> Self {
        match outcome {
            PersistOutcome::OwnedByAnotherWorkspace => Self::OwnedByAnotherWorkspace,
            PersistOutcome::OwnedBySameWorkspace => Self::OwnedBySameWorkspace,
            PersistOutcome::OwnedByArchivedAgent => Self::OwnedByArchivedAgent,
            // 唯一一个**不是**冲突的变体：`classify_slot_owner` 从不产生它（`Ok(())` 才是）。
            PersistOutcome::Stored(row) => Self::Store {
                message: format!("unexpected stored outcome for {}", row.id.as_string()),
            },
        }
    }

    /// 从落库判决映射（`Ok` = 落好了）。
    pub fn from_persist(outcome: PersistOutcome) -> Result<Installation, Self> {
        match outcome {
            PersistOutcome::Stored(row) => Ok(*row),
            conflict => Err(Self::from_conflict(conflict)),
        }
    }
}

// =====================================================================
// 入参
// =====================================================================

/// BYO 安装的入参（上游 `InstallationParams`）：管理员从管理后台抄来的**一对明文** +
/// 目标 `(workspace, agent)`。
///
/// **手写 `Debug`**：结构体里有一个**明文**密钥（凭据纪律第 1 条）—— 默认 `Debug` 会把它
/// 原样写进任何 `{:?}` 插值、`assert_eq!` 失败回显与 panic backtrace。
#[derive(Clone)]
pub struct InstallationParams {
    pub workspace_id: Id,
    pub agent_id: Id,
    /// 安装发起人（`installer_user_id`；`NOT NULL`）。
    pub installer_user_id: Id,
    /// 智能机器人标识（管理后台可见，**不是秘密**；既是握手帧里的身份也是路由键）。
    pub bot_id: String,
    /// 明文长连接密钥（只在创建机器人时显示一次；服务边界内立刻封好）。
    pub secret: PlaintextSecret,
    /// 机器人在会话里的显示名（可选；见 [`Installation`] 的字段注释）。
    pub bot_display_name: String,
}

impl fmt::Debug for InstallationParams {
    /// 手写脱敏（凭据纪律第 1 条）。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InstallationParams")
            .field("workspace_id", &self.workspace_id)
            .field("agent_id", &self.agent_id)
            .field("installer_user_id", &self.installer_user_id)
            .field("bot_id", &self.bot_id)
            .field("secret", &self.secret)
            .field("bot_display_name", &self.bot_display_name)
            .finish()
    }
}

impl InstallationParams {
    /// 装配（密钥只经 [`PlaintextSecret::new`]）。
    #[must_use]
    pub fn new(
        workspace_id: Id,
        agent_id: Id,
        installer_user_id: Id,
        bot_id: impl Into<String>,
        secret: impl Into<String>,
        bot_display_name: impl Into<String>,
    ) -> Self {
        Self {
            workspace_id,
            agent_id,
            installer_user_id,
            bot_id: bot_id.into().trim().to_string(),
            secret: PlaintextSecret::new(secret.into().trim()),
            bot_display_name: bot_display_name.into().trim().to_string(),
        }
    }
}

/// 必填字段的**预检**（上游 `validateInstallationParams` 的 `switch` 逐条）。
///
/// 它**不**向 `WeCom` 验证任何东西（那是 [`InstallationService::upsert`] 里探针的事）。
///
/// # Errors
///
/// 第一个缺失的字段 ⇒ [`InstallError::InvalidParams`]。
pub fn validate_installation_params(params: &InstallationParams) -> Result<(), InstallError> {
    let field = if params.installer_user_id.0.is_nil() {
        "installer_user_id"
    } else if params.bot_id.is_empty() {
        "bot_id"
    } else if params.secret.is_empty() {
        "secret"
    } else {
        return Ok(());
    };
    Err(InstallError::InvalidParams { field })
}

/// 槽主分类（上游 `botSlotConflictErr` 的 `switch`，**逐条同序**）。
///
/// `Ok(())` = 这次安装**可以**动这个 `(wecom, bot_id)` 槽；`Err(…)` = 用它拒掉，且
/// **两个副作用都不许发生**（探针的订阅、回收的硬删）。
///
/// 它与实现内部的回收判据**共用同一张表**，所以两边不会各自漂成"哪些行算可抢的"：
///
/// | 槽主状态 | 判决 |
/// | --- | --- |
/// | 无行 | 自由 |
/// | 孤儿（workspace / agent 行已消失） | 自由（回收会清掉它，且它没连着谁） |
/// | 已撤销 | 自由（持有者自己说"我不要了"） |
/// | 活跃，是本调用方的行 | 自由（重装 / 轮换密钥，原地刷新） |
/// | 活跃，是别人 workspace 的 | [`PersistOutcome::OwnedByAnotherWorkspace`] |
/// | 活跃，同 workspace 的**已归档** agent | [`PersistOutcome::OwnedByArchivedAgent`] |
/// | 活跃，同 workspace 的别的 agent | [`PersistOutcome::OwnedBySameWorkspace`] |
pub fn classify_slot_owner(
    owner: SlotOwner,
    workspace_id: Id,
    agent_id: Id,
) -> Result<(), PersistOutcome> {
    if owner.is_orphan() || owner.revoked {
        return Ok(());
    }
    if owner.workspace_id == workspace_id && owner.agent_id == agent_id {
        return Ok(());
    }
    if owner.workspace_id != workspace_id {
        return Err(PersistOutcome::OwnedByAnotherWorkspace);
    }
    if owner.agent_archived {
        return Err(PersistOutcome::OwnedByArchivedAgent);
    }
    Err(PersistOutcome::OwnedBySameWorkspace)
}

/// 显示名的承接（上游 `Upsert` 的那三行注释逐字）：
/// 对话框里显示名是可选的，所以管理员轮换一条泄露的密钥时会留空 —— 留空**不该**把群命令
/// 打回"空白启发式"。**换机器人是另一回事**：旧名字属于旧机器人，承下来会让新机器人
/// 响应一个不是它的 @提及。
#[must_use]
pub fn resolve_display_name(
    requested: &str,
    carried: Option<&Installation>,
    bot_id: &str,
) -> String {
    if !requested.is_empty() {
        return requested.to_string();
    }
    match carried {
        Some(row) if row.bot_id == bot_id => row.bot_display_name.clone(),
        _ => String::new(),
    }
}

// =====================================================================
// 服务
// =====================================================================

/// BYO 安装服务（上游 `InstallationService`）。
///
/// **不派生 `Debug`**：它持有封装盒与端口（都能打印，但"能打印"本身不该存在）。
pub struct InstallationService {
    store: Arc<dyn InstallationStore>,
    probe: Arc<dyn CredentialProbe>,
    boxed: SecretBox,
}

impl InstallationService {
    /// 装配。三个参数都**必填**（上游逐字：`we refuse plaintext storage even in dev`；
    /// 探针没有"关掉的开关"）。
    #[must_use]
    pub fn new(
        store: Arc<dyn InstallationStore>,
        probe: Arc<dyn CredentialProbe>,
        boxed: SecretBox,
    ) -> Self {
        Self {
            store,
            probe,
            boxed,
        }
    }

    /// 借出封装盒（route 层要把同一个盒交给解封器）。
    #[must_use]
    pub fn boxed(&self) -> &SecretBox {
        &self.boxed
    }

    /// 列 workspace 的全部 `WeCom` 安装（**含 revoked**）。
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

    /// workspace 收窄的单条（另一个 workspace 猜 id ⇒ 与不存在同一个结果）。
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

    /// 撤销（`active → revoked`；行**保留**供审计，重装把状态翻回 `active`）。
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

    /// 解出一份明文凭据（给重连路径与诊断用；纯解封，不碰网络）。
    ///
    /// # Errors
    ///
    /// 密文坏 / 明文不是 UTF-8。
    pub fn credentials(
        &self,
        installation: &Installation,
    ) -> Result<InstallationCredentials, CredentialsError> {
        // 解封器是一次薄包装，没有需要缓存的状态 ⇒ 每次现建（上游 `CredentialsResolver` 同形）。
        SecretboxCredentialsResolver::new(self.boxed.clone()).credentials(installation)
    }

    /// BYO 安装（上游 `Upsert`：预检 → 读槽主 → 分类 → 探针 → 封 → 承接显示名 → 落库）。
    ///
    /// 顺序是承重的，见模块文档的三条判决（尤其是第 2 条：被拒的请求**什么都不碰**）。
    ///
    /// # Errors
    ///
    /// 必填字段缺失 / 三类槽位冲突 / 凭据被拒 / 够不着 `WeCom` / 封或落库失败。
    pub async fn upsert(&self, params: InstallationParams) -> Result<Installation, InstallError> {
        validate_installation_params(&params)?;

        // ① 预读槽主并分类 —— 在碰 `WeCom` **之前**。注定要被拒的请求在这里就返回，
        //    于是它既没写库，也没探针（探针会踢掉当前在线的持有者）。
        if let Some(owner) = self
            .store
            .slot_owner(&params.bot_id)
            .await
            .map_err(|message| InstallError::Store { message })?
        {
            classify_slot_owner(owner, params.workspace_id, params.agent_id)
                .map_err(InstallError::from_conflict)?;
        }

        // ② 证明控制权。这一步**本身**是对平台的一次副作用（订阅会挤掉在线持有者），
        //    这正是它必须排在①之后的原因。
        self.probe
            .probe(&params.bot_id, &params.secret)
            .await
            .map_err(InstallError::from_probe)?;

        // ③ 封。明文从这一刻起不再出现在任何本地结构里。
        let sealed = self
            .boxed
            .seal(params.secret.expose().as_bytes())
            .map_err(|_| InstallError::Seal)?;

        // ④ 显示名承接：只有**同一个 bot**的重装才继承（换机器人不继承）。
        let carried = self
            .store
            .current_for(params.workspace_id, params.agent_id)
            .await
            .map_err(|message| InstallError::Store { message })?;
        let bot_display_name =
            resolve_display_name(&params.bot_display_name, carried.as_ref(), &params.bot_id);

        let config = Installation {
            id: Id::nil(),
            workspace_id: params.workspace_id,
            agent_id: params.agent_id,
            installer_user_id: params.installer_user_id,
            status: InstallationStatus::Active,
            bot_id: params.bot_id.clone(),
            secret_encrypted: sealed,
            bot_display_name: bot_display_name.clone(),
            config: serde_json::Value::Null,
            installed_at: chrono::Utc::now(),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
        .encode_config()
        .map_err(|_| InstallError::Encode)?;

        // ⑤ 落库。端口内部把"锁槽 → 重读槽主 → 回收死主 → upsert"跑在**一个事务**里，
        //    所以①的读只是预检，权威判定在这里（`docs/32` §31 的 D6）。
        let outcome = self
            .store
            .persist(&PersistInstall {
                workspace_id: params.workspace_id,
                agent_id: params.agent_id,
                installer_user_id: params.installer_user_id,
                bot_id: params.bot_id.clone(),
                config,
                bot_display_name,
            })
            .await
            .map_err(|message| InstallError::Store { message })?;
        InstallError::from_persist(outcome)
    }
}

#[cfg(test)]
mod tests;
