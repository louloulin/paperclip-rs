//! 协作者与订阅者（`autopilot_collaborator` / `autopilot_subscriber`）。
//!
//! - **写者**：M5-2。
//! - **上游**：`AddAutopilotCollaborator`59 / `writeAutopilotCollaborators`17 /
//!   `RemoveAutopilotCollaborator`35 / `parseAutopilotSubscribers`33 /
//!   `lockAndValidateAutopilotSubscribers`39 / `validateAutopilotAssigneeForSave`76 /
//!   `isValidAutopilotAssigneeType`19（`handler/autopilot.go`）。
//! - **必须同事务加锁**：`lockAndValidateAutopilotSubscribers`(39) 上游用 `FOR SHARE` / `FOR UPDATE`
//!   语义 ⇒ 本地落地时以**真库并发测试**为准（不能只写 SELECT）。
//! - **assignee 二态**：`agent` | `squad`（`096` 起 `assignee_type` + `assignee_id`）；
//!   `squad` 在**运行期**解析为 `squad.leader_id`（Squad-as-Leader，`096` / MUL-2429）——
//!   不是把 squad id 直接当 agent id 用。
//! - **`user_type` 只有 `'member'`**（`120` / `128` 的 CHECK）⇒ 用
//!   `mc_core::autopilot::AutopilotUserType`（单变体），不要用 `AutopilotActorType`。
//! - **无外键**：这两张表的相关列在库里没有 FK ⇒ 主体存在性由本文件校验（app 层完整性）。
//!
//! # M5-2 落地了什么
//!
//! 本文件是「订阅者 / 协作者」的**纯函数层**：解析、去重、加锁顺序。DB 侧（advisory 锁、
//! `FOR SHARE` 成员重申、INSERT / DELETE）在 `mc-repos::autopilot::write`；HTTP 侧（403 拒绝体、
//! 成员门槛）在 `mc-http::routes::autopilots::{assignee,subscribers}`。三层不重叠：
//!
//! | 层 | 交出去的东西 | 落点 |
//! | --- | --- | --- |
//! | 纯函数（本文件） | `Vec<SubscriberCandidate>`（已去重、带 `input_index`、已排序可加锁） | `parse_subscribers` / `ordered_for_locking` |
//! | 事务（`mc-repos`） | ①advisory 锁 ②`FOR SHARE` 成员重申 ③assignee 的 `FOR SHARE` ④行锁 | `autopilot::write`（锁序见其模块头） |
//! | HTTP | 400 文案 / 403 拒绝码 / 200 响应形状 | `mc-http::routes::autopilots` |
//!
//! ## 为什么去重与排序在**这里**而不是 SQL
//!
//! 上游 `parseAutopilotSubscribers` 先按**规范化 UUID 字符串**去重（首见者胜，**保留首次的
//! `InputIndex`**），`lockAndValidateAutopilotSubscribers` 再按同一口径**排序**后加锁 ——
//! 两处用同一个键是刻意的：同一批用户无论请求里怎么写（大小写、重复、顺序），拿到的 advisory
//! 锁集合与加锁顺序都相同 ⇒ 并发写同一批订阅者不会死锁（死锁只可能来自**顺序不一致**）。
//! 排序必须用**规范化**后的字符串（`Uuid::to_string()`：小写 + 连字符），不能用请求原文，
//! 否则 `A1B2…` 与 `a1b2…` 会排到两个位置。
//!
//! 这两步都是纯函数，所以放在领域层而不是 HTTP handler：M5-3（trigger 的订阅者）也要走同一条
//! 去重/排序规则，写两份必然漂移。
//!
//! ## 两处刻意的上游怪癖（别顺手「修」）
//!
//! 1. **空数组与缺失不同**：`subscribers: []` 是「一个订阅者都没有」的**断言**；`null` / 缺失在
//!    Create 里同样折成「空」。但 **Update 是整表替换**（`DeleteSubscribersForAutopilot` +
//!    重新插入）⇒ 传 `[]` 会**抹掉**全部订阅者，传 `null`（字段缺失）才保持原样。
//!    handler 侧靠「字段是否出现」区分（MUL-6680）。
//! 2. **同一个 user 在 `subscribers` 里出现两次不是错误**：去重后只订阅一次（`AddAutopilotSubscriber`
//!    本身也 `ON CONFLICT DO NOTHING`）。报错的是 `user_type != 'member'` / 空 `user_id` /
//!    非 UUID —— 错误消息带的是**该次出现的下标** `subscribers[i]…`，去重**不能**让下标漂移。

use crate::error::AutopilotError;

/// `autopilot.assignee_type` 的 agent 分支（`096` 起是这个二选一）。
pub const ASSIGNEE_TYPE_AGENT: &str = "agent";

/// `autopilot.assignee_type` 的 squad 分支（运行期解析队长，见模块头）。
pub const ASSIGNEE_TYPE_SQUAD: &str = "squad";

/// 上游 `isValidAutopilotAssigneeType` 的等价词表。
pub const ASSIGNEE_TYPES: [&str; 2] = [ASSIGNEE_TYPE_AGENT, ASSIGNEE_TYPE_SQUAD];

/// 上游 `subscribers[i].user_type must be 'member'` 里的那个唯一合法值。
///
/// 表上还有 CHECK（`120` / `128`），所以这不是「可配置项」而是常量：直接取
/// `mc_core::autopilot::AutopilotUserType` 的单变体（`as_str()` 是 `const fn`），不另写字面量。
/// SQL 侧的孪生常量是 `mc_repos::autopilot::USER_TYPE_MEMBER`。
pub const USER_TYPE_MEMBER: &str = mc_core::autopilot::AutopilotUserType::Member.as_str();

/// 上游 `isValidAutopilotAssigneeType`：`agent` | `squad`。
///
/// 注意 Create 里 `assignee_type` 缺省是 `agent`（`crate::dto::DEFAULT_ASSIGNEE_TYPE`），而
/// **空串不是**合法值 —— 缺省由 handler 补，不是由本函数宽容处理。
#[must_use]
pub fn is_valid_assignee_type(raw: &str) -> bool {
    ASSIGNEE_TYPES.contains(&raw)
}

/// 上游 `SubscriberInput`（`handler/autopilot.go` @385）的请求形状。
///
/// 两个字段都是 `String`（不是 `Uuid`）**是刻意的**：上游 `user_id` 要先做「空串」检查再
/// 解析 UUID，错误消息也按「空」与「非法」分开；用 `Uuid` 反序列化会把两者都折成
/// `invalid request body`。
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct SubscriberInput {
    /// 目前只有 `"member"`。
    pub user_type: String,
    /// 成员的用户 id（UUID 的**任意**大小写形态，非 UUID 是 400）。
    pub user_id: String,
}

/// 通过校验的一条订阅者，带**首次出现的下标**。
///
/// `input_index` 只用于报错文案（`subscribers[i] is not a member of this workspace`）：
/// 到了加锁/写库阶段下标已经没有意义，所以它不参与去重与排序的**判定**，只跟着行走。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubscriberCandidate {
    /// 规范化后的用户 id（`Uuid::to_string()`）。
    pub user_id: uuid::Uuid,
    /// 该用户在请求数组里**首次**出现的下标。
    pub input_index: usize,
}

/// 上游 `parseAutopilotSubscribers`(33)：逐项校验 + 按规范化 UUID 去重（首见者胜）。
///
/// 顺序逐字对照上游：
///
/// 1. `user_type != "member"` → `subscribers[i].user_type must be 'member'`
/// 2. `user_id == ""` → `subscribers[i].user_id is required`
/// 3. 非 UUID → `subscribers[i].user_id must be a valid uuid`（**本地文案**，见 `docs/47`）
/// 4. 已出现过的规范化 UUID ⇒ 丢弃（**保留首次的 `input_index`**）
///
/// 空切片返回空 `Vec`（不是 `None`）：Create 把「缺失 / `null`」与「空数组」都折成空，
/// Update 里两者的区别由 handler 的「字段是否出现」决定，与本函数无关。
///
/// # Errors
///
/// 上述 1–3 任一失败即 400（[`AutopilotError::validation`]，无 `code` ⇒ HTTP 侧走嵌套错误体）。
pub fn parse_subscribers(
    raw: &[SubscriberInput],
) -> Result<Vec<SubscriberCandidate>, AutopilotError> {
    let mut seen = std::collections::HashSet::with_capacity(raw.len());
    let mut out = Vec::with_capacity(raw.len());
    for (index, item) in raw.iter().enumerate() {
        if item.user_type != USER_TYPE_MEMBER {
            return Err(AutopilotError::validation(format!(
                "subscribers[{index}].user_type must be 'member'"
            )));
        }
        if item.user_id.is_empty() {
            return Err(AutopilotError::validation(format!(
                "subscribers[{index}].user_id is required"
            )));
        }
        let user_id = uuid::Uuid::parse_str(item.user_id.trim()).map_err(|_| {
            AutopilotError::validation(format!("subscribers[{index}].user_id must be a valid uuid"))
        })?;
        if seen.insert(user_id) {
            out.push(SubscriberCandidate {
                user_id,
                input_index: index,
            });
        }
    }
    Ok(out)
}

/// 上游 `lockAndValidateAutopilotSubscribers` 的排序半步：按**规范化 UUID 字符串**升序。
///
/// 调用方拿到的顺序就是**必须**的加锁顺序（先 advisory 键、后 `FOR SHARE` 成员行，见
/// `mc_repos::autopilot::write` 的锁序表）。去重已经由 [`parse_subscribers`] 做过，这里只排序；
/// 但仍不假设输入已去重（重复的 `user_id` 排在一起，加锁两次是幂等的，`FOR SHARE` 也不冲突）。
#[must_use]
pub fn ordered_for_locking(candidates: &[SubscriberCandidate]) -> Vec<SubscriberCandidate> {
    let mut ordered = candidates.to_vec();
    ordered.sort_by_key(|candidate| candidate.user_id.to_string());
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(user_type: &str, user_id: &str) -> SubscriberInput {
        SubscriberInput {
            user_type: user_type.to_string(),
            user_id: user_id.to_string(),
        }
    }

    #[test]
    fn assignee_type_whitelist_is_exactly_agent_and_squad() {
        assert!(is_valid_assignee_type("agent"));
        assert!(is_valid_assignee_type("squad"));
        assert!(!is_valid_assignee_type(""));
        assert!(!is_valid_assignee_type("Agent"));
        assert!(!is_valid_assignee_type("member"));
        assert_eq!(ASSIGNEE_TYPES.len(), 2);
    }

    #[test]
    fn empty_subscribers_is_an_empty_list_not_an_error() {
        assert_eq!(parse_subscribers(&[]).unwrap(), Vec::new());
    }

    #[test]
    fn user_type_must_be_member_and_error_names_the_index() {
        let err = parse_subscribers(&[input("member", "u"), input("agent", "u")]).unwrap_err();
        assert_eq!(err.to_string(), "subscribers[1].user_type must be 'member'");
    }

    #[test]
    fn empty_user_id_is_reported_before_uuid_parsing() {
        let err = parse_subscribers(&[input("member", "")]).unwrap_err();
        assert_eq!(err.to_string(), "subscribers[0].user_id is required");
    }

    #[test]
    fn malformed_user_id_names_the_index() {
        let err = parse_subscribers(&[input("member", "not-a-uuid")]).unwrap_err();
        assert_eq!(
            err.to_string(),
            "subscribers[0].user_id must be a valid uuid"
        );
    }

    #[test]
    fn duplicate_user_ids_collapse_keeping_the_first_index() {
        // 同一个 UUID 的两种大小写形态命中同一条去重键（规范化后比较）。
        let first = "0b6f0e5a-1c2d-4e3f-8a9b-0c1d2e3f4a5b";
        let second = "0B6F0E5A-1C2D-4E3F-8A9B-0C1D2E3F4A5B";
        let parsed = parse_subscribers(&[
            input("member", "11111111-1111-4111-8111-111111111111"),
            input("member", first),
            input("member", second),
        ])
        .unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[1].input_index, 1);
        assert_eq!(parsed[1].user_id.to_string(), first);
    }

    #[test]
    fn lock_order_is_canonical_uuid_string_ascending() {
        let candidates = vec![
            SubscriberCandidate {
                user_id: uuid::Uuid::parse_str("ffffffff-ffff-4fff-8fff-ffffffffffff").unwrap(),
                input_index: 0,
            },
            SubscriberCandidate {
                user_id: uuid::Uuid::parse_str("00000000-0000-4000-8000-000000000000").unwrap(),
                input_index: 1,
            },
        ];
        let ordered = ordered_for_locking(&candidates);
        assert_eq!(ordered[0].input_index, 1);
        assert_eq!(ordered[1].input_index, 0);
        // 输入不被就地改动（调用方可能还要用原顺序回填响应）。
        assert_eq!(candidates[0].input_index, 0);
    }

    #[test]
    fn uppercase_input_locks_in_the_same_order_as_lowercase() {
        let lower =
            parse_subscribers(&[input("member", "aa000000-0000-4000-8000-000000000000")]).unwrap();
        let upper =
            parse_subscribers(&[input("member", "AA000000-0000-4000-8000-000000000000")]).unwrap();
        assert_eq!(
            ordered_for_locking(&lower)[0].user_id,
            ordered_for_locking(&upper)[0].user_id
        );
    }

    #[test]
    fn user_type_member_constant_matches_the_single_variant() {
        assert_eq!(USER_TYPE_MEMBER, "member");
    }
}
