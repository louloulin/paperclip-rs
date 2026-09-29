//! **装置形态**（device shape）：一个分组除了通用种子之外，上游那条测试**当时**
//! 还需要哪些世界状态，回放器就得把它们造出来。
//!
//! # 这份表解决的是哪一类红
//!
//! `seed.rs` 造的是**每个上游测试都成立**的通用世界（一套 workspace + `mdt_` 令牌 +
//! agent / issue / chat session / task）。但有另一类 fixture 断言的是一个**特定形态**
//! ——「这个 agent 当时没有 runtime」「这个 agent 当时已归档」「这个 workspace 当时
//! 已经有同名 property 定义」。通用世界给不出这些形态，于是 handler 照通用世界的答案
//! 作答（`201` 而不是 `409`），fixture 就红。
//!
//! 🔴 **判据不是「这条 fixture 期望什么码」**。本仓的架构不变式是「`expect` 只出现在
//! `verdict.rs` 的比对侧，请求构造面一次都不读它」—— 本表同样**一个字节都不读**
//! `expect`。判据是「上游那条测试**当时处于什么形态**」，而这件事只在上游源码里。
//! 与 [`crate::upstream_facts`] 的自定义 status 表同一条纪律，理由逐字相同。
//!
//! # 形态一律由**真实路由**造出
//!
//! 与 `seed.rs` 模块头那条纪律一致（唯一的例外是 runtime，因为它没有注册路由）。
//! 归档走 `POST /api/agents/:id/archive`，解绑走
//! `POST /api/runtimes/:id/unbind-agents-and-delete`，property 定义走
//! `POST /api/properties`。**没有一条 `INSERT`** —— 手写 SQL 去摆一个
//! `agent.archived_at` 就是「造个假货去迎合断言」。
//!
//! # 本片**修不了**的那几条，以及为什么
//!
//! 交付评论里的 by-design 登记表逐条写明了「什么装置形态下它才可判定」。它们**不**
//! 在 [`SHAPES`] 里，因为它们缺的不是「一行种子」而是一条**尚不存在的装置能力**
//! （可注入故障的连接池 / 调用方可选 id 的入队路由 / handler 侧的逐字段基线 CAS）。
//! 登记它们比造个替身去迎合断言诚实。

use anyhow::{bail, Context, Result};
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;

use crate::seed::{SEED_DEV_USER_HEADER, SEED_SESSION_HEADER};

/// 一个 agent 在上游那条测试当时的 **runtime 绑定形态**。
///
/// 三个变体与本仓 `mc-repos/src/chat_task/send.rs` 在锁内重读时判的三个 409 一一对应
/// （`SessionArchived` / `AgentArchived` / `NoRuntime`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRuntimeState {
    /// 绑着可用 runtime —— 通用世界（`seed.rs` 的默认），也是绝大多数分组的状态。
    Live,
    /// `agent.runtime_id IS NULL`（上游 `ErrChatTaskAgentNoRuntime`）。
    Unbound,
    /// `agent.archived_at IS NOT NULL`（上游 `ErrChatTaskAgentArchived`）。
    Archived,
}

/// 一个分组需要的**额外**装置形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// 该分组的 agent 当时不是「绑着可用 runtime」的那种形态。
    Agent(AgentRuntimeState),
    /// 该 workspace 当时**已经有**一条同名 property 定义（重名 ⇒ 409）。
    ExistingProperty {
        /// `issue_property.name`（本仓按 `LOWER(name)` 建唯一索引 ⇒ 大小写不敏感）。
        name: &'static str,
        /// `issue_property.type`。
        property_type: &'static str,
    },
}

/// 一条形态声明：挂在**上游 provenance** 上，带上游证据。
#[derive(Debug, Clone, Copy)]
pub struct ShapeDecl {
    /// 上游测试名（= 分组键 `Fixture.source.test`）。
    pub test: &'static str,
    /// 该分组需要的形态。
    pub shape: Shape,
    /// 上游证据：`文件:行` + 那一行做了什么。写在这里是为了让下一个读代码的人
    /// **不必重新 clone 上游**就能核对（与 [`crate::upstream_facts`] 同款理由）。
    pub evidence: &'static str,
}

/// 本片（`LUM-2588`，族 `DEVICE_*` 4 条 / 7 个 fixture）登记的全部形态。
///
/// 每一条都能被实现坏掉而红：handler 若不再把归档 agent 判成 409，形态照样种得下、
/// fixture 照样红。形态表只负责**把前置摆出来**，判定仍在 handler 手里。
pub const SHAPES: &[ShapeDecl] = &[
    ShapeDecl {
        test: "TestChatSend_UnboundAgentReturnsStructuredConflict",
        shape: Shape::Agent(AgentRuntimeState::Unbound),
        evidence: "agent_runtime_required_test.go:60-78 建了一个**没有 runtime 的** agent \
                   （`dbfx.Agent` 不带 runtime / 解绑后）再 `SendChatMessage`，断言 409",
    },
    ShapeDecl {
        test: "TestSendChatMessage_ArchivedAgent",
        shape: Shape::Agent(AgentRuntimeState::Archived),
        evidence: "chat_test.go:240-249 先把 agent 归档（`dbfx.ArchiveAgent` / \
                   `POST /api/agents/:id/archive`）再 `SendChatMessage`，断言 409",
    },
    ShapeDecl {
        test: "TestSendChatMessage_RuntimeAccessDeniedReturnsStructuredConflict",
        shape: Shape::Agent(AgentRuntimeState::Unbound),
        evidence: "runtime_access_denied_test.go:55-73 注入一个**调用方用不了**的 runtime \
                   再 `SendChatMessage`，断言 409。🔴 本仓的发送面**没有** runtime ACL \
                   判定（见模块文档「本片修不了」），唯一可达的同义形态是「agent 绑不到 \
                   任何 runtime」⇒ 落 [`AgentRuntimeState::Unbound`]，两者在本仓是同一条 \
                   409（`ErrChatTaskAgentNoRuntime`）",
    },
    ShapeDecl {
        test: "TestPropertyDefinitionCRUD",
        shape: Shape::ExistingProperty {
            name: "severity",
            property_type: "text",
        },
        evidence: "property_test.go:114 是该测试的**重名**用例：前面的 case 已经建过 \
                   `severity` 定义，这条再 `POST /api/properties {\"name\":\"severity\"}` \
                   ⇒ 409。本仓唯一索引是 `(workspace_id, LOWER(name))`（properties.rs:347）",
    },
];

/// 某个分组需要的全部形态（0..n 条）。
pub fn shapes_for(test: &str) -> impl Iterator<Item = &'static ShapeDecl> + '_ {
    SHAPES.iter().filter(move |d| d.test == test)
}

/// 把某个分组的形态**全部**造出来，返回造了几条。
///
/// 🔴 顺序在这里是**语义**：agent 形态排在 property 形态之前没有依赖，但两者都排在
/// `seed.rs` 建完通用世界**之后** —— 形态是「在可用世界上再改一处」，不是「从零搭」。
/// 反过来先造形态再种通用行，`seed_task` 的那次聊天发送就会撞上 409 而种不出任务行。
pub async fn apply(
    router: &Router,
    session: &str,
    workspace_id: Uuid,
    agent: Uuid,
    runtime_id: Uuid,
    test: &str,
) -> Result<usize> {
    let mut applied = 0;
    for decl in shapes_for(test) {
        match decl.shape {
            Shape::Agent(AgentRuntimeState::Live) => continue,
            Shape::Agent(AgentRuntimeState::Unbound) => {
                unbind_agent(router, session, workspace_id, agent, runtime_id).await?;
            }
            Shape::Agent(AgentRuntimeState::Archived) => {
                archive_agent(router, session, workspace_id, agent).await?;
            }
            Shape::ExistingProperty {
                name,
                property_type,
            } => {
                create_property(router, session, workspace_id, name, property_type).await?;
            }
        }
        applied += 1;
    }
    Ok(applied)
}

/// `POST /api/agents/:id/archive` —— 把 agent 归档（`agent.archived_at` 落值）。
///
/// 归档**连带取消在飞任务**（`crud.rs:635` 的 `CancelTasksForArchivedAgent`，与上游同款）。
/// 那不是副作用而是这条形态的语义：上游归档 agent 时它的任务同样被取消。
async fn archive_agent(
    router: &Router,
    session: &str,
    workspace_id: Uuid,
    agent: Uuid,
) -> Result<()> {
    let uri = format!("/api/agents/{agent}/archive?workspace_id={workspace_id}");
    dispatch(router, session, "POST", &uri, json!({})).await
}

/// `POST /api/runtimes/:id/unbind-agents-and-delete` —— 把 agent 的 `runtime_id` 置空。
///
/// 这是本仓**唯一**能把 `agent.runtime_id` 变成 `NULL` 的入口：
/// `create_agent` 恒写 `runtime_id: Some(..)`（`crud.rs:174`），`update_agent` 在
/// `runtime_id` 缺省时直接 `Ok(())` 不动那一列（`crud.rs:495`）—— 没有 `PUT` 能解绑。
///
/// `expected_active_agent_ids` 必须**逐字列出**当前绑着的 agent：留空会走
/// `DeleteRuntimeError::PlanChanged`（409 `runtime_delete_plan_changed`）而不是执行。
async fn unbind_agent(
    router: &Router,
    session: &str,
    workspace_id: Uuid,
    agent: Uuid,
    runtime_id: Uuid,
) -> Result<()> {
    let uri = format!("/api/runtimes/{runtime_id}/unbind-agents-and-delete");
    let body = json!({ "expected_active_agent_ids": [agent.to_string()] });
    // `?workspace_id=` 挂在 body 之外：这条路由的 workspace 来自 runtime 行本身
    // （`load_member_for_runtime`），查询参数只用来满足路由层的 workspace 解析。
    let uri = format!("{uri}?workspace_id={workspace_id}");
    dispatch(router, session, "POST", &uri, body).await
}

/// `POST /api/properties` —— 建一条 property 定义。
///
/// 409 在这里是**成功**而不是失败：形状登记本身就是「已经存在」，重跑同一分组
/// （单测会这么做）撞上 `(workspace_id, LOWER(name))` 唯一索引同样是期望中的世界状态。
async fn create_property(
    router: &Router,
    session: &str,
    workspace_id: Uuid,
    name: &str,
    property_type: &str,
) -> Result<()> {
    let uri = format!("/api/properties?workspace_id={workspace_id}");
    let body = json!({ "name": name, "type": property_type });
    let (status, _) = send(router, session, "POST", &uri, body).await?;
    if status == StatusCode::CONFLICT || status.is_success() {
        return Ok(());
    }
    bail!("POST {uri} for property {name:?} returned {status}")
}

/// 发一次带 JSON body 的请求，返回 `(状态码, 响应体)`；非 2xx/409 一律 `Err`。
async fn dispatch(
    router: &Router,
    session: &str,
    method: &str,
    uri: &str,
    body: serde_json::Value,
) -> Result<()> {
    let (status, raw) = send(router, session, method, uri, body).await?;
    if !status.is_success() {
        bail!(
            "{method} {uri} -> {status} {}",
            String::from_utf8_lossy(&raw)
        );
    }
    Ok(())
}

async fn send(
    router: &Router,
    session: &str,
    method: &str,
    uri: &str,
    body: serde_json::Value,
) -> Result<(StatusCode, Vec<u8>)> {
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                // 🔴 两个身份头都要发：与 `seed.rs::get` 同一条纪律（只发一个 ⇒ 401，
                // 而 401 在种子里长得像「路由没挂」）。
                .header(SEED_SESSION_HEADER, session)
                .header(SEED_DEV_USER_HEADER, session)
                .body(Body::from(body.to_string()))?,
        )
        .await
        .with_context(|| format!("dispatch {method} {uri}"))?;
    let status = resp.status();
    let raw = to_bytes(resp.into_body(), 1 << 20)
        .await
        .with_context(|| format!("read the body of {method} {uri}"))?;
    Ok((status, raw.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// 重复登记会让「一个分组落进两张不同的形态」变成静默行为 —— 而形态是**改世界**的，
    /// 两条互相打架的形态里后一条会悄悄盖掉前一条。
    #[test]
    fn no_test_is_declared_twice() {
        let mut seen = BTreeSet::new();
        for decl in SHAPES {
            assert!(seen.insert(decl.test), "重复登记 {}", decl.test);
            assert!(!decl.evidence.is_empty(), "{} 没有上游证据", decl.test);
        }
    }

    /// 登记的形态必须**真的被本仓的校验接受**，否则种子会在回放中途才报错 ——
    /// 而那时它长得像「handler 坏了」。
    #[test]
    fn declared_property_rows_satisfy_the_repo_constraints() {
        for decl in SHAPES {
            let Shape::ExistingProperty {
                name,
                property_type,
            } = decl.shape
            else {
                continue;
            };
            assert!(
                mc_repos::property::validate_name(name).is_ok(),
                "{}: name {name:?} 过不了 `validate_name`",
                decl.test
            );
            assert!(
                mc_repos::property::validate_type(property_type).is_ok(),
                "{}: type {property_type:?} 过不了 `validate_type`",
                decl.test
            );
        }
    }

    /// 双向：登记了 ⇒ 查得到；查得到 ⇒ 登记过（`shapes_for` 不会凭空多出一条）。
    #[test]
    fn shapes_for_selects_exactly_the_declared_rows() {
        let rows: Vec<_> = shapes_for("TestPropertyDefinitionCRUD").collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].shape,
            Shape::ExistingProperty {
                name: "severity",
                property_type: "text"
            }
        );
        // 未登记的测试必须一条都查不到 —— 否则形态会漫到没被上游证据支持的分组上。
        assert_eq!(shapes_for("TestIssuesCRUDThroughRouter").count(), 0);
        assert_eq!(shapes_for("").count(), 0);
    }

    /// `AgentRuntimeState::Live` 不该被登记成一条形态：它是通用世界的默认，
    /// 登记它等于给「什么都不用做」发一张通行证。
    #[test]
    fn the_live_state_is_never_declared() {
        for decl in SHAPES {
            assert_ne!(
                decl.shape,
                Shape::Agent(AgentRuntimeState::Live),
                "{} 把通用默认登记成了形态",
                decl.test
            );
        }
    }
}
