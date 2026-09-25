//! `WeCom` 的数据层**端口**（上游 `internal/integrations/wecom/store.go`，79 行）。
//!
//! - **写者**：M7-15（`docs/60-M7-PLAN.md` §3.3）。
//! - **上游定位**：`store.go` 是"骑在泛化 `channel_*` 表上的数据层适配器"（迁移 `124`），
//!   自己**不写**任何 `wecom` 专属 SQL。本片把它拆成**端口 trait**：
//!   `mc-channel` 只声明要什么，**PG 实现落在 route 层**
//!   （`crates/mc-http/src/routes/channels/wecom.rs`），于是「adapter 不得直接写 DB」
//!   这条边界（`docs/60` §2.6 第 1 条）是**类型层面**的事实而不是纪律。
//!
//! # 为什么端口不止 `store.go` 那三条（`docs/32` §31 的 D6）
//!
//! 上游那三条（`GetInstallationByBotID` / `GetInstallation` / `IsWorkspaceMember`）是
//! **入站读面**，服务的是 M7-16…M7-20 的解析器；本片要交付的是**安装与绑定面**，
//! 所以同一份 `Store` 上还要有 `installation.go` / `binding.go` 用的写面。
//! 两半合成一个文件、两组 trait（读面 `InstallationQueries` / 写面 `InstallationStore`）：
//! 写面是**本片**用，读面是**后续片**用，而两者作用在同一张表上 —— 拆成两个文件会让
//! "一行安装的两个视图"分居两处。
//!
//! # 与上游泛化仓储的关系（本仓实测）
//!
//! `crates/mc-repos/src/channel/installation.rs`（M7-1 写，本片**只读**）提供了
//! `list_active_by_kind` / `get` / `get_in_workspace` / `find_active_by_app_id` / `insert` /
//! `revoke` / 租约 CAS —— 其中**三条上游语句没有对应物**：
//!
//! | 上游语句 | 本仓泛化仓储 | 落点 |
//! | --- | --- | --- |
//! | `ListChannelInstallationsByWorkspace`（**含** revoked） | 只有 `list_active_by_kind`（只给 active） | 写面的 `list_by_workspace` |
//! | `UpsertChannelInstallation`（`ON CONFLICT (workspace_id, agent_id, channel_type)`） | 只有 `insert` | 写面的 `persist` |
//! | `ReclaimDeadChannelInstallationByAppID` + `LockChannelInstallationAppIDSlot` | 无 | 写面的 `persist`（实现内部） |
//!
//! ⇒ 与 M7-4 / M7-5 / M7-9 同手法：它们在 **route 层**以端口实现落地。

use std::fmt;

use async_trait::async_trait;
use mc_core::id::Id;
use serde_json::Value;

use super::types::Installation;

// =====================================================================
// 读面（store.go 的三条 + 管理列表）
// =====================================================================

/// `WeCom` 的**读面**（上游 `Store` 的三条 + 管理列表的一条）。
///
/// 每一条都**必须**按 `channel_type = 'wecom'` 收窄：泛化表是五个 adapter 共享的，
/// 一个 `feishu` 的 id 传进来若不加收窄就会被**静默复用**（上游 `GetInstallation` 的注释逐字）。
#[async_trait]
pub trait InstallationQueries: Send + Sync {
    /// 按机器人标识查安装（路由键是 `config->>'app_id'`，值等于 `bot_id`）。
    ///
    /// # Errors
    ///
    /// 存储层故障（找不到是 `Ok(None)`）。
    async fn get_by_bot_id(&self, bot_id: &str) -> Result<Option<Installation>, String>;

    /// 按主键查安装（收窄到 `wecom`）。
    ///
    /// # Errors
    ///
    /// 存储层故障。
    async fn get(&self, installation_id: Id) -> Result<Option<Installation>, String>;

    /// 入站时**复查**成员资格（`channel_*` 没有成员外键 ⇒ 一条过期绑定可能把消息
    /// 路由给一个已经离开 workspace 的人）。
    ///
    /// # Errors
    ///
    /// 存储层故障。
    async fn is_workspace_member(&self, workspace_id: Id, user_id: Id) -> Result<bool, String>;
}

// =====================================================================
// 写面（安装与绑定面）
// =====================================================================

/// 落一行 BYO 安装的入参（上游 `UpsertChannelInstallationParams` 的契约子集）。
///
/// `config` 里**已经**是封好的密文（[`Installation::encode_config`] 的产物）——
/// 本结构不接触明文，也不该有明文字段。
#[derive(Debug, Clone, PartialEq)]
pub struct PersistInstall {
    pub workspace_id: Id,
    pub agent_id: Id,
    pub installer_user_id: Id,
    /// 路由键（**必须**等于 `config["app_id"]`；实现要断言这一点）。
    pub bot_id: String,
    /// 完整 `config` blob（密文已在里面）。
    pub config: Value,
    /// 机器人显示名（只进 `config`；留在这里是为了让实现不必再解一遍 JSON）。
    pub bot_display_name: String,
}

/// 落库的判决（上游 `persistInstall` 的返回值，或它翻出来的冲突哨兵）。
#[derive(Debug, Clone, PartialEq)]
pub enum PersistOutcome {
    /// 落好了（新建**或**原地更新：同一个 `(workspace, agent)` 再装同一个 bot 就是刷新）。
    Stored(Box<Installation>),
    /// 这个 bot 的路由槽被**另一个** workspace 占着。
    OwnedByAnotherWorkspace,
    /// 被同一个 workspace 里**另一个活着的** agent 占着。
    OwnedBySameWorkspace,
    /// 被同一个 workspace 里一个**已归档**的 agent 占着（归档可逆 ⇒ 槽位没被释放）。
    OwnedByArchivedAgent,
}

/// 机器人路由槽的**当前持有者**（上游 `GetChannelInstallationSlotOwnerByAppID` 的投影）。
///
/// 四条信息就是冲突分类的**全部**输入：槽位在哪个 workspace / 哪个 agent、那一行是不是
/// 已被撤销、那个 agent 是否已归档、以及 workspace / agent 行**还在不在**（孤儿行的槽位
/// 算空）。把它抽成一个值类型，分类就成了一个可单测的纯函数。
///
/// 四个 `bool` 就是上游那三条 SQL 的实际返回列（`status` / `agent_archived_at` /
/// `workspace_exists` / `agent_exists`）⇒ 不折成状态机：折叠会丢掉“已撤销”与
/// “孤儿”可以同时为真的信息，而分类判据要逐条对得上。
#[allow(clippy::struct_excessive_bools)] // 四个 bool = 上游那三条 SQL 的四列，逐列对齐
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotOwner {
    pub workspace_id: Id,
    pub agent_id: Id,
    /// 持有行 `status = 'revoked'`（持有者自己说"我不要了"）。
    pub revoked: bool,
    /// 持有 agent 已归档（归档**可逆** ⇒ 不当成死主）。
    pub agent_archived: bool,
    /// 持有 workspace 行还在。
    pub workspace_exists: bool,
    /// 持有 agent 行还在。
    pub agent_exists: bool,
}

impl SlotOwner {
    /// 死主 = 孤儿（workspace / agent 行已消失）。死主的槽位应被回收，不算冲突。
    #[must_use]
    pub fn is_orphan(&self) -> bool {
        !self.workspace_exists || !self.agent_exists
    }
}

/// 安装行的**写面**（上游 `installation.go` 用到的那些语句 + `persistInstall` 的事务）。
#[async_trait]
pub trait InstallationStore: Send + Sync {
    /// 机器人路由键的当前持有者（上游 `GetChannelInstallationSlotOwnerByAppID`）。
    ///
    /// ⚠️ 这是**事务外**的预读（上游在 `LockChannelInstallationAppIDSlot` 之后读）。
    /// 它的用途只有一个：在**动 `WeCom` 之前**把注定要被拒的请求挡掉（见
    /// [`super::installation::InstallationService::upsert`] 的注释）；权威判定在 `persist`
    /// 内部（同一个事务里重查），所以这里读到旧值只会多跑一次 `persist`，不会写错。
    ///
    /// # Errors
    ///
    /// 存储层故障。
    async fn slot_owner(&self, bot_id: &str) -> Result<Option<SlotOwner>, String>;

    /// 读 `(workspace, agent)` 上**当前**那一行（上游 `currentInstallation`：没有键在
    /// 三元组上的查询，于是过滤该 workspace 的 wecom 行再按 agent 挑）。
    ///
    /// # Errors
    ///
    /// 存储层故障。
    async fn current_for(
        &self,
        workspace_id: Id,
        agent_id: Id,
    ) -> Result<Option<Installation>, String>;

    /// workspace 的**全部** `WeCom` 安装（**含 revoked**：审计与历史要看得见）。调用方过滤。
    ///
    /// # Errors
    ///
    /// 存储层故障。
    async fn list_by_workspace(&self, workspace_id: Id) -> Result<Vec<Installation>, String>;

    /// workspace 收窄的单条（另一个 workspace 猜 id ⇒ `Ok(None)`，与不存在同结果）。
    ///
    /// # Errors
    ///
    /// 存储层故障。
    async fn get_in_workspace(
        &self,
        installation_id: Id,
        workspace_id: Id,
    ) -> Result<Option<Installation>, String>;

    /// 撤销（`active → revoked`；行**保留**供审计，重装把状态翻回 `active`）。
    ///
    /// # Errors
    ///
    /// 存储层故障。
    async fn revoke(&self, workspace_id: Id, installation_id: Id) -> Result<bool, String>;

    /// 落一行（上游 `persistInstall` 的整个事务语义：锁槽 → 读主 → 回收死主 → upsert）。
    ///
    /// # Errors
    ///
    /// 存储层故障（产品判决见 [`PersistOutcome`]）。
    async fn persist(&self, params: &PersistInstall) -> Result<PersistOutcome, String>;
}

// =====================================================================
// 端口不可打印
// =====================================================================

/// 端口 trait 对象的 `Debug` 占位（实现者大多不可打印）。
#[derive(Clone, Copy)]
pub struct OpaquePort;

impl fmt::Debug for OpaquePort {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("<dyn port>")
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    //! 端口的内存替身（本片与 route 层测试共用；**不是**生产实现）。
    //!
    //! 形态照上游 `installation_test.go` 的 fake：一串内存行 + 一个"谁占着路由槽"的判定。

    use super::{
        async_trait, Id, Installation, InstallationQueries, InstallationStore, PersistInstall,
        PersistOutcome, SlotOwner, Value,
    };
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// 内存安装表（含 revoked 行）。
    #[derive(Default)]
    pub(crate) struct MemoryInstallStore {
        rows: Mutex<Vec<Installation>>,
        /// `(workspace, agent) → 是否归档`（`persist` 的冲突分类要吃它）。
        archived: Mutex<HashMap<(Id, Id), bool>>,
        /// `workspace_id / agent_id → 行已消失`（孤儿判定；默认都在）。
        missing: Mutex<HashMap<Id, bool>>,
        /// 记录最后一次 `persist` 收到的 `config`（"明文不得入库"的判据）。
        pub(crate) last_config: Mutex<Option<Value>>,
        /// `persist` 被调用的次数（"被拒的请求不该写库"的判据）。
        pub(crate) persist_calls: Mutex<usize>,
    }

    impl MemoryInstallStore {
        /// 空表。
        #[must_use]
        pub(crate) fn new() -> Self {
            Self::default()
        }

        /// 预置一行。
        pub(crate) fn insert_row(&self, row: Installation) {
            self.rows.lock().expect("lock").push(row);
        }

        /// 标记一个 `(workspace, agent)` 已归档（冲突分类的第三种）。
        pub(crate) fn mark_archived(&self, workspace_id: Id, agent_id: Id) {
            self.archived
                .lock()
                .expect("lock")
                .insert((workspace_id, agent_id), true);
        }

        /// 把一个 workspace / agent 行标成**已消失**（孤儿槽位）。
        pub(crate) fn mark_missing(&self, id: Id) {
            self.missing.lock().expect("lock").insert(id, true);
        }

        /// 当前行数（审计用）。
        pub(crate) fn len(&self) -> usize {
            self.rows.lock().expect("lock").len()
        }

        /// `persist` 被调用过几次。
        pub(crate) fn persist_calls(&self) -> usize {
            *self.persist_calls.lock().expect("lock")
        }

        /// 槽主投影（分类由 `installation::classify_slot_owner` 做，替身只搬数据）。
        fn slot_owner_of(&self, bot_id: &str) -> Option<SlotOwner> {
            let rows = self.rows.lock().expect("lock");
            let row = rows.iter().find(|row| row.bot_id == bot_id)?;
            let missing = self.missing.lock().expect("lock");
            let gone = |id: &Id| missing.get(id).copied().unwrap_or(false);
            Some(SlotOwner {
                workspace_id: row.workspace_id,
                agent_id: row.agent_id,
                revoked: !row.is_active(),
                agent_archived: self
                    .archived
                    .lock()
                    .expect("lock")
                    .get(&(row.workspace_id, row.agent_id))
                    .copied()
                    .unwrap_or(false),
                workspace_exists: !gone(&row.workspace_id),
                agent_exists: !gone(&row.agent_id),
            })
        }
    }

    #[async_trait]
    impl InstallationQueries for MemoryInstallStore {
        async fn get_by_bot_id(&self, bot_id: &str) -> Result<Option<Installation>, String> {
            Ok(self
                .rows
                .lock()
                .expect("lock")
                .iter()
                .find(|row| row.bot_id == bot_id)
                .cloned())
        }

        async fn get(&self, installation_id: Id) -> Result<Option<Installation>, String> {
            Ok(self
                .rows
                .lock()
                .expect("lock")
                .iter()
                .find(|row| row.id == installation_id)
                .cloned())
        }

        async fn is_workspace_member(
            &self,
            _workspace_id: Id,
            _user_id: Id,
        ) -> Result<bool, String> {
            Ok(true)
        }
    }

    #[async_trait]
    impl InstallationStore for MemoryInstallStore {
        async fn slot_owner(&self, bot_id: &str) -> Result<Option<SlotOwner>, String> {
            Ok(self.slot_owner_of(bot_id))
        }

        async fn current_for(
            &self,
            workspace_id: Id,
            agent_id: Id,
        ) -> Result<Option<Installation>, String> {
            Ok(self
                .rows
                .lock()
                .expect("lock")
                .iter()
                .find(|row| row.workspace_id == workspace_id && row.agent_id == agent_id)
                .cloned())
        }

        async fn list_by_workspace(&self, workspace_id: Id) -> Result<Vec<Installation>, String> {
            Ok(self
                .rows
                .lock()
                .expect("lock")
                .iter()
                .filter(|row| row.workspace_id == workspace_id)
                .cloned()
                .collect())
        }

        async fn get_in_workspace(
            &self,
            installation_id: Id,
            workspace_id: Id,
        ) -> Result<Option<Installation>, String> {
            Ok(self
                .rows
                .lock()
                .expect("lock")
                .iter()
                .find(|row| row.id == installation_id && row.workspace_id == workspace_id)
                .cloned())
        }

        async fn revoke(&self, workspace_id: Id, installation_id: Id) -> Result<bool, String> {
            let mut rows = self.rows.lock().expect("lock");
            let mut changed = false;
            for row in rows.iter_mut() {
                if row.id == installation_id && row.workspace_id == workspace_id && row.is_active()
                {
                    row.status = mc_core::channel::InstallationStatus::Revoked;
                    changed = true;
                }
            }
            Ok(changed)
        }

        /// 上游 `persistInstall` 的判决**顺序**（本替身逐条照抄分类，不做 SQL）：
        /// 死主（revoked / 孤儿）不算冲突、自己的槽不算冲突、其余按 workspace/归档分类。
        async fn persist(&self, params: &PersistInstall) -> Result<PersistOutcome, String> {
            *self.last_config.lock().expect("lock") = Some(params.config.clone());
            *self.persist_calls.lock().expect("lock") += 1;
            if let Some(owner) = self.slot_owner_of(&params.bot_id) {
                if let Err(conflict) = super::super::installation::classify_slot_owner(
                    owner,
                    params.workspace_id,
                    params.agent_id,
                ) {
                    return Ok(conflict);
                }
            }
            let config = params.config.clone();
            let sealed = super::super::types::InstallConfig::from_value(&config)
                .map_err(|error| error.to_string())?
                .secret_bytes()
                .map_err(|error| error.to_string())?;
            let now = chrono::Utc::now();
            let row = Installation {
                id: Id::new(),
                workspace_id: params.workspace_id,
                agent_id: params.agent_id,
                installer_user_id: params.installer_user_id,
                status: mc_core::channel::InstallationStatus::Active,
                bot_id: params.bot_id.clone(),
                secret_encrypted: sealed,
                bot_display_name: params.bot_display_name.clone(),
                config,
                installed_at: now,
                created_at: now,
                updated_at: now,
            };
            self.insert_row(row.clone());
            Ok(PersistOutcome::Stored(Box::new(row)))
        }
    }
}
