//! `DingTalk` **群清单**与 **bot 可读身份**（上游 `internal/integrations/dingtalk/bot_identity.go`
//! 250 行 + `internal/handler/dingtalk.go` 的 `listDingTalkGroups` 一族）。
//!
//! - **写者**：M7-9（`docs/60-M7-PLAN.md` §3.3；写集勘误见 `docs/32` §23 的 D1）。
//! - **上游定位**：`DingTalk` 不给"某个机器人在哪些群里"的清单接口 ⇒ Multica 只能**观察**：
//!   一条被成功处理的群消息就是一次"这个机器人出现在这个群"的证据，写进
//!   `dingtalk_group_presence`（安装维度）与 `dingtalk_bot_identity`（安装级身份）。
//!   设置页 / agent 详情页读这两张表渲染群清单。
//!
//! # 本文件的三块（别混）
//!
//! | 块 | 内容 | 上游 |
//! | --- | --- | --- |
//! | **清单装配** | [`GroupInventory`] / [`Group`] / [`GroupBot`] + 纯函数 [`assemble_inventory`] | `listDingTalkGroups` 的后半段（分组 / 排序 / 分页 / 可见性过滤） |
//! | **查询计划** | [`GroupQuery`] → [`GroupQueryPlan`]（`activity` / `installation_id` / `limit` / `offset` 的**逐条校验**） | `listDingTalkGroups` 的前半段（三个 400 分支） |
//! | **bot 名** | [`BotNameResolver`]（实现 M7-7 的 `jobs::BotNameSource`）+ 两个端口 | `bot_identity.go` 的 `BotNameResolver` |
//!
//! # 三处形态差异（登记 `docs/32` §23 的 D5，逐条）
//!
//! 1. **观察者是同步端口**：M7-7 把 `jobs::BotNameSource::bot_name` 定成**同步**签名
//!    （`docs/32` §19 的 D8：实现自己管缓存），而真实现要打平台 `OpenAPI`（async）⇒
//!    [`BotNameResolver`] 用一个**独立线程 + current-thread 运行时**把 future 跑在同步上下文里
//!    （与 `slack::media::ThreadedFetcher` / `dingtalk::media::block_on_engine` 同款）。
//!    代价与那两处相同：每次未命中的解析多一个线程。
//! 2. **凭据来源是端口**：[`resolvers::GroupPresenceObserver::observe`] 只给
//!    `ResolvedInstallation`（含 adapter 的 `InstallationRow`,里面有**密文** config），而
//!    [`jobs::BotNameSource::bot_name`] 只给 `app_key` ⇒ 后者需要一个"按 `AppKey` 取凭据"的
//!    端口（[`InstallationCredentialSource`]）。两条路都不让 adapter 直接碰 SQL
//!    （`docs/60` §2.6 第 1 条）。
//! 3. **三张表的读写语句不在本片写集**：`crates/mc-repos/src/channel/**` 对本片**只读**
//!    （`docs/60` §3.3 的写集表）⇒ 它们以**端口实现**的形态落在
//!    `crates/mc-http/src/routes/channels/dingtalk/store.rs`（与 M7-4 / M7-5 的三条上游查询
//!    同一手法）。M7-7 的诚实默认值 [`resolvers::NoGroupPresence`] / `jobs::NoBotName` 因此
//!    有了真实现：[`PresenceObserver`] / [`BotNameResolver`]（装配是宿主的交接项）。
//!
//! # 权限拒绝要有自己的缓存（上游逐字，别省）
//!
//! `qyapi_chat_manage` 是**按 `DingTalk` 应用**授予的，不是按群。上游因此把"权限被拒"这条
//! 缓存 **30 秒**并**跨群共享**，否则"没权限"这个正常状态会给每条入站消息打一次 `OpenAPI`。
//! 成功的名字缓存 **1 小时**（改名会收敛），群相关的其它失败缓存 **1 分钟**。

use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use mc_core::id::Id;
use serde::{Deserialize, Serialize};

use super::config::Credentials;

/// 活跃群的窗口（上游 `dingTalkActiveGroupWindow = 90 * 24h`）。
pub const ACTIVE_GROUP_WINDOW_DAYS: i64 = 90;

/// 非活跃分页的默认页大小（上游 `dingTalkInactivePageSize = 20`）。
pub const INACTIVE_PAGE_SIZE: i64 = 20;

/// 非活跃分页的**上限**（上游 `dingTalkInactiveMaxPage = 100`）。
pub const INACTIVE_MAX_PAGE: i64 = 100;

/// 上游响应里恒为 `true` 的那一位（本仓没有"拿不到清单"的平台状态）。
pub const GROUP_DISCOVERY_SUPPORTED: bool = true;

/// 群观察的窗口（`now - 90d`）。
#[must_use]
pub fn active_since(now: DateTime<Utc>) -> DateTime<Utc> {
    now - Duration::days(ACTIVE_GROUP_WINDOW_DAYS)
}

// =====================================================================
// wire 形状（上游 `ListDingTalkGroupsResponse` 一族）
// =====================================================================

/// 群里的**一个**已连接机器人（上游 `DingTalkGroupBotResponse`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupBot {
    pub installation_id: String,
    /// 面向产品的身份（agent id）。
    pub agent_id: String,
    /// `DingTalk` 可读身份（拿得到 `qyapi_chat_manage` 才有）。
    pub bot_name: String,
    /// 身份问题（如 `missing_qyapi_chat_manage`）；空 = 没问题。
    pub bot_identity_issue: String,
    /// RFC3339；未知为空串（上游 `lastActiveAt` 的零值语义）。
    pub last_active_at: String,
    pub mention_count: i64,
}

/// 同一个 `openConversationId` 下的所有机器人（上游 `DingTalkGroupResponse`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub conversation_id: String,
    pub conversation_title: String,
    pub bots: Vec<GroupBot>,
}

/// 安装级的 bot 身份（上游 `botIdentities` 这个 map 的值）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupBotIdentity {
    pub installation_id: String,
    pub agent_id: String,
    pub bot_name: String,
    pub bot_identity_issue: String,
}

/// `GET …/dingtalk/groups` 的响应（上游 `ListDingTalkGroupsResponse`，**五个键逐字**）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupInventory {
    pub groups: Vec<Group>,
    pub group_discovery_supported: bool,
    /// `installation_id → 该安装的非活跃群数`（本页/本过滤条件下）。
    pub inactive_group_counts: HashMap<String, i64>,
    /// `installation_id → 安装级 bot 身份`。
    pub bot_identities: HashMap<String, GroupBotIdentity>,
    /// 非活跃分页的下一页游标；无更多（或不是非活跃查询）⇒ 缺席。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<i64>,
}

impl GroupInventory {
    /// 未配置 / "什么都没观察到"的那一版（上游 `h.DingTalkInstall == nil` 分支逐字）。
    ///
    /// ⚠️ `group_discovery_supported` 是 **`true`**（不是 `false`）：上游这条分支给的
    /// 是"能发现，只是暂时没有数据"，与 lark 的 `install_supported:false` **不是**同一个意思。
    #[must_use]
    pub fn empty() -> Self {
        Self {
            groups: Vec::new(),
            group_discovery_supported: GROUP_DISCOVERY_SUPPORTED,
            inactive_group_counts: HashMap::new(),
            bot_identities: HashMap::new(),
            next_offset: None,
        }
    }
}

// =====================================================================
// 查询计划（上游 `listDingTalkGroups` 的三个 400 分支）
// =====================================================================

/// 上游从 `r.URL.Query()` 读的四个键（**原样**收，不在 DTO 层做类型转换）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GroupQuery {
    #[serde(default)]
    pub activity: String,
    #[serde(default)]
    pub installation_id: String,
    #[serde(default)]
    pub limit: String,
    #[serde(default)]
    pub offset: String,
}

impl GroupQuery {
    /// 转成本仓的 `HashMap` 查询串形状（`AgentScope::resolve` 的入参，agent 级路由要它）。
    ///
    /// **只带非空值**：上游 `q.Get(name) != ""` 的空值语义就是"没传"。
    #[must_use]
    pub fn as_map(&self) -> HashMap<String, String> {
        let mut map = HashMap::new();
        for (key, value) in [
            ("activity", &self.activity),
            ("installation_id", &self.installation_id),
            ("limit", &self.limit),
            ("offset", &self.offset),
        ] {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                map.insert(key.to_string(), trimmed.to_string());
            }
        }
        map
    }
}

/// 查询计划的校验失败（**逐条**对应上游的 400 文案，别合并）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GroupQueryError {
    /// `activity` 只接受空 / `inactive`。
    #[error("activity must be inactive when provided")]
    Activity,
    /// 非活跃查询必须带 `installation_id`。
    #[error("installation_id is required for inactive groups")]
    InstallationRequired,
    /// `installation_id` 不是 uuid。
    #[error("installation_id must be a valid uuid")]
    InstallationId,
    /// `limit` 必须是 1..=100 的整数。
    #[error("limit must be between 1 and 100")]
    Limit,
    /// `offset` 必须是非负整数。
    #[error("offset must be a non-negative integer")]
    Offset,
}

impl GroupQueryError {
    /// 稳定错误码（诊断 / 日志用）。
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Activity => "dingtalk_group_activity_invalid",
            Self::InstallationRequired => "dingtalk_group_installation_required",
            Self::InstallationId => "dingtalk_group_installation_id_invalid",
            Self::Limit => "dingtalk_group_limit_invalid",
            Self::Offset => "dingtalk_group_offset_invalid",
        }
    }
}

/// 校验过的查询计划（`assemble_inventory` 与端口实现都读它）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupQueryPlan {
    /// 是否按 agent 收窄（agent 级路由恒 `true`）。
    pub filter_by_agent: bool,
    /// 是否要非活跃群（`activity=inactive`）。
    pub include_inactive: bool,
    /// 非活跃查询必须带的安装 id。
    pub installation_id: Option<Id>,
    /// 页大小（活跃查询恒 0 = 不分页）。
    pub page_limit: i64,
    /// 页偏移。
    pub page_offset: i64,
}

impl GroupQueryPlan {
    /// 上游 `listDingTalkGroups` 的四个 400 分支 + 分页默认值，**逐条**照抄。
    ///
    /// # Errors
    ///
    /// 四条校验各有一个变体（别合并成一条"参数不对"）。
    pub fn parse(filter_by_agent: bool, query: &GroupQuery) -> Result<Self, GroupQueryError> {
        let activity = query.activity.trim();
        if !activity.is_empty() && activity != "inactive" {
            return Err(GroupQueryError::Activity);
        }
        let include_inactive = activity == "inactive";
        let installation_raw = query.installation_id.trim();
        let mut installation_id = None;
        if include_inactive {
            if installation_raw.is_empty() {
                return Err(GroupQueryError::InstallationRequired);
            }
            installation_id =
                Some(Id::parse(installation_raw).map_err(|_| GroupQueryError::InstallationId)?);
        }
        let mut page_limit = 0;
        let mut page_offset = 0;
        if include_inactive {
            page_limit = INACTIVE_PAGE_SIZE;
            let limit = query.limit.trim();
            if !limit.is_empty() {
                let parsed: i64 = limit.parse().map_err(|_| GroupQueryError::Limit)?;
                if !(1..=INACTIVE_MAX_PAGE).contains(&parsed) {
                    return Err(GroupQueryError::Limit);
                }
                page_limit = parsed;
            }
            let offset = query.offset.trim();
            if !offset.is_empty() {
                let parsed: i64 = offset.parse().map_err(|_| GroupQueryError::Offset)?;
                if parsed < 0 {
                    return Err(GroupQueryError::Offset);
                }
                page_offset = parsed;
            }
        }
        Ok(Self {
            filter_by_agent,
            include_inactive,
            installation_id,
            page_limit,
            page_offset,
        })
    }
}

// =====================================================================
// 端口行（`dingtalk_group_presence` / `dingtalk_bot_identity` 的投影）
// =====================================================================

/// `dingtalk_group_presence` 的一行（本 adapter 需要的列）。
#[derive(Debug, Clone, PartialEq)]
pub struct GroupPresenceRow {
    pub installation_id: Id,
    pub agent_id: Id,
    pub conversation_id: String,
    pub conversation_title: String,
    pub bot_name: String,
    pub bot_identity_issue: String,
    pub last_active_at: Option<DateTime<Utc>>,
    pub mention_count: i64,
}

/// "某个安装还有多少个非活跃群"（上游 `CountInactiveDingTalkGroupPresencesByWorkspace`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InactiveGroupCount {
    pub installation_id: Id,
    pub agent_id: Id,
    pub group_count: i64,
}

/// `dingtalk_bot_identity` 的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupIdentityRow {
    pub installation_id: Id,
    pub agent_id: Id,
    pub bot_name: String,
    pub bot_identity_issue: String,
}

/// `list_presences` 的入参。
///
/// 收成一个结构体（而不是七个位置参数）：上游那条语句的七个 `sqlc.arg` 是**一起**决定
/// "取哪一批行"的，分组之后调用点更难写错，`clippy::too_many_arguments` 也不必开豁免。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresenceQuery {
    pub workspace_id: Id,
    /// 非空 ⇒ 只取这个 agent 的行（agent 级路由的 `filterByAgent`）。
    pub agent_id: Option<Id>,
    /// 非空 ⇒ 只取这个安装的行（非活跃分页必须带它）。
    pub installation_id: Option<Id>,
    /// `activity=inactive`。
    pub include_inactive: bool,
    /// 活跃窗口的下界（`now - 90d`）。
    pub active_since: DateTime<Utc>,
    pub page_offset: i64,
    /// `0` ⇒ 不分页。
    pub page_limit: i64,
}

/// 群清单的读面（上游 `ListDingTalkGroupPresencesByWorkspace` +
/// `CountInactiveDingTalkGroupPresencesByWorkspace` + `ListDingTalkBotIdentitiesByWorkspace`
/// + `GetChannelInstallationInWorkspace` 的 `mayListInactiveDingTalkInstallation` 那一步）。
#[async_trait]
pub trait GroupInventoryStore: Send + Sync {
    /// 群存在性行（活跃 / 非活跃由 `include_inactive` 与 `active_since` 决定）。
    ///
    /// `page_limit = 0` ⇒ 不分页（上游活跃查询走的就是这条）。
    async fn list_presences(&self, query: &PresenceQuery) -> Result<Vec<GroupPresenceRow>, String>;

    /// 非活跃群计数（按安装聚合）。
    async fn count_inactive(
        &self,
        workspace_id: Id,
        agent_id: Option<Id>,
        active_since: DateTime<Utc>,
    ) -> Result<Vec<InactiveGroupCount>, String>;

    /// 安装级 bot 身份。
    async fn list_bot_identities(
        &self,
        workspace_id: Id,
        agent_id: Option<Id>,
    ) -> Result<Vec<GroupIdentityRow>, String>;

    /// 上游 `mayListInactiveDingTalkInstallation`：请求的安装是不是**本 workspace 的、活跃的、
    /// 且调用者看得到**。四种不通过（不存在 / 非活跃 / 跨 workspace / 不可见）**同**结果
    /// ⇒ 调用者无法通过群数据或 `next_offset` 区分它们。
    async fn may_list_inactive_installation(
        &self,
        workspace_id: Id,
        installation_id: Id,
        agent_id: Option<Id>,
    ) -> Result<bool, String>;

    /// 上游 `ForgetDingTalkGroupPresence`：摘掉一条观察（会话与消息历史**保留**）。
    ///
    /// 返回是否真的删了一行（`false` ⇒ 上游 404 `dingtalk group not found`）。
    async fn forget_presence(
        &self,
        workspace_id: Id,
        installation_id: Id,
        conversation_id: &str,
    ) -> Result<bool, String>;

    /// 按 `AppKey` 取一个活跃安装的**明文**凭据（bot 名解析要它）。
    ///
    /// 找不到 / 解不开 ⇒ `Ok(None)`（**不**是错误：那只是"这个名字拿不到"）。
    async fn credentials_by_app_key(&self, app_key: &str) -> Result<Option<Credentials>, String>;
}

// =====================================================================
// 清单装配（纯函数，**所有**可见性 / 排序 / 分页判决都在这里）
// =====================================================================

/// 把端口回来的三组行装配成上游的响应形状。
///
/// `visible_agent_ids`：
/// - `Some(set)` ⇒ 只保留这些 agent 的行（普通成员看到的"自己能打开的 agent"）；
/// - `None` ⇒ 不过滤（owner / admin 的完整清单，或 agent 级路由）。
///
/// `agent_id = Some(id)` ⇒ 只保留这个 agent 的行（agent 级路由的 `filterByAgent`）。
/// 注意两者**都要**过：上游 agent 级路由传了 `filterByAgent` 而 `visibleAgentIDs` 是 `nil`。
#[must_use]
pub fn assemble_inventory<S: std::hash::BuildHasher>(
    plan: &GroupQueryPlan,
    agent_id: Option<Id>,
    mut rows: Vec<GroupPresenceRow>,
    inactive_counts: Vec<InactiveGroupCount>,
    identity_rows: Vec<GroupIdentityRow>,
    visible_agent_ids: Option<&HashSet<Id, S>>,
) -> GroupInventory {
    // 分页：端口取 `limit + 1` 行，多出来的那一行只用来判"还有下一页"。
    let mut next_offset = None;
    if plan.include_inactive && plan.page_limit > 0 {
        let limit = usize::try_from(plan.page_limit).unwrap_or(usize::MAX);
        if rows.len() > limit {
            rows.truncate(limit);
            next_offset = Some(plan.page_offset + plan.page_limit);
        }
    }

    let visible = |candidate: Id| -> bool {
        if let Some(id) = agent_id {
            if candidate != id {
                return false;
            }
        }
        visible_agent_ids.is_none_or(|set| set.contains(&candidate))
    };

    let mut inactive_group_counts = HashMap::new();
    for row in inactive_counts {
        if visible(row.agent_id) {
            inactive_group_counts.insert(row.installation_id.to_string(), row.group_count);
        }
    }

    let mut bot_identities = HashMap::new();
    for row in identity_rows {
        if !visible(row.agent_id) {
            continue;
        }
        let installation_id = row.installation_id.to_string();
        bot_identities.insert(
            installation_id.clone(),
            GroupBotIdentity {
                installation_id,
                agent_id: row.agent_id.to_string(),
                bot_name: row.bot_name.clone(),
                bot_identity_issue: row.bot_identity_issue.clone(),
            },
        );
    }

    let mut groups: Vec<Group> = Vec::new();
    let mut index_by_conversation: HashMap<String, usize> = HashMap::new();
    for row in rows {
        if !visible(row.agent_id) {
            continue;
        }
        let index = if let Some(index) = index_by_conversation.get(&row.conversation_id) {
            // 上游：同群后续行只在**当前标题为空**时才补标题。
            if groups[*index].conversation_title.is_empty() && !row.conversation_title.is_empty() {
                groups[*index]
                    .conversation_title
                    .clone_from(&row.conversation_title);
            }
            *index
        } else {
            groups.push(Group {
                conversation_id: row.conversation_id.clone(),
                conversation_title: row.conversation_title.clone(),
                bots: Vec::new(),
            });
            let index = groups.len() - 1;
            index_by_conversation.insert(row.conversation_id.clone(), index);
            index
        };
        groups[index].bots.push(GroupBot {
            installation_id: row.installation_id.to_string(),
            agent_id: row.agent_id.to_string(),
            bot_name: row.bot_name.clone(),
            bot_identity_issue: row.bot_identity_issue.clone(),
            last_active_at: row.last_active_at.map(rfc3339).unwrap_or_default(),
            mention_count: row.mention_count,
        });
    }

    // 每个群里的机器人按 `installation_id` 稳定升序（上游 `sort.SliceStable`）。
    for group in &mut groups {
        group
            .bots
            .sort_by(|left, right| left.installation_id.cmp(&right.installation_id));
    }
    // 群按**标题**升序，标题相同按会话 id；空标题排最后（上游三级比较，逐字）。
    groups.sort_by(|left, right| {
        let (a, b) = (&left.conversation_title, &right.conversation_title);
        if a == b {
            return left.conversation_id.cmp(&right.conversation_id);
        }
        if a.is_empty() {
            return std::cmp::Ordering::Greater;
        }
        if b.is_empty() {
            return std::cmp::Ordering::Less;
        }
        a.cmp(b)
    });

    GroupInventory {
        groups,
        group_discovery_supported: GROUP_DISCOVERY_SUPPORTED,
        inactive_group_counts,
        bot_identities,
        next_offset,
    }
}

/// `DateTime<Utc>` → RFC3339（上游 `time.Time.UTC().Format(time.RFC3339)`）。
#[must_use]
pub fn rfc3339(value: DateTime<Utc>) -> String {
    value.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

pub mod bot_name;

pub use bot_name::*;

#[cfg(test)]
mod tests;
