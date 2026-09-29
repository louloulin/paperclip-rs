//! 本片（`LUM-2572`）的**两处「抽取器没说、装置得说」的装配**。
//!
//! 与 `seed.rs` 的分工：`seed.rs` 建的是「每个分组一整套通用行」（workspace /
//! `mdt_` 令牌 / agent / issue / chat session / task），本文件建的是
//! [`crate::upstream_facts`] 那张表点名的那两样东西 —— 它们**不是通用的**，逐条
//! 挂在上游 provenance 上：
//!
//! 1. [`seed_custom_statuses`]：把上游 `createTestCustomStatus` 建过的目录项补上，
//!    好让 `POST /api/issues {"status": "<自定义 key>"}` 不再 400。
//! 2. [`seed_outsider`]：建一个**不拥有任何被种资源**的 workspace，并在它里面登记
//!    一枚 `mdt_` 令牌 —— 上游跨空间探针用的就是这种身份（`daemon/gc.rs:15-17`
//!    的「workspace 不匹配与行不存在返回同一个 404」正是被它证明的）。
//!
//! 两者都走**真实路由 / 真实仓储**，不绕过任何一层：目录项走
//! `POST /api/issue-statuses`（handler 会照常校验 `name` / `category` 并落到
//! `issue_status` 表），令牌走 [`crate::daemon_token::register`]（`daemon_token`
//! 表的 `REFERENCES workspace(id)` 外键因此**真的**被满足）。

use anyhow::{bail, Context, Result};
use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::json;
use tower::ServiceExt;
use uuid::Uuid;

use crate::seed::{Outsider, SEED_DEV_USER_HEADER, SEED_SESSION_HEADER};
use crate::upstream_facts;

/// outsider workspace 的「分组名」。它**不是**某个上游测试名 —— 它只被
/// [`crate::seed::seed_workspace`] 用来拼一个不撞车的 name/slug。
pub const OUTSIDER_GROUP: &str = "outsider-identity";

/// 建出某个分组该有的自定义 status 目录项，返回**新建**了几条。
///
/// 🔴 **只建 [`crate::upstream_facts::CUSTOM_STATUS_ROWS`] 点名的那些**，不是「扫本组
/// fixture 里出现的 `body.status`」：语料里另有 4 条仍在绿的 fixture 同样带目录外的
/// key（`in_use_a` / `not_a_status` / `active`×2）却**期望 400** —— 扫 fixture 会把
/// 那 4 条弄红。理由逐字写在 `upstream_facts.rs` 的模块文档里。
pub async fn seed_custom_statuses(
    router: &Router,
    session: &str,
    workspace_id: Uuid,
    test: &str,
) -> Result<usize> {
    let mut created = 0;
    for row in upstream_facts::custom_statuses_for(test) {
        if post_status(router, session, workspace_id, row).await? {
            created += 1;
        }
    }
    Ok(created)
}

/// 一次 `POST /api/issue-statuses`；`Ok(true)` = 新建，`Ok(false)` = 已存在。
///
/// 已存在按「成功」处理：`(workspace_id, key)` 有唯一索引，重跑同一条分组
/// （单测会这么做）不该报错。
async fn post_status(
    router: &Router,
    session: &str,
    workspace_id: Uuid,
    row: &upstream_facts::CustomStatusRow,
) -> Result<bool> {
    let body = json!({ "name": row.name, "key": row.key, "category": row.category });
    let uri = format!("/api/issue-statuses?workspace_id={workspace_id}");
    let resp = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(&uri)
                .header("content-type", "application/json")
                .header(SEED_SESSION_HEADER, session)
                .header(SEED_DEV_USER_HEADER, session)
                .body(Body::from(body.to_string()))?,
        )
        .await
        .with_context(|| format!("dispatch POST {uri}"))?;
    let status = resp.status();
    if status == StatusCode::CONFLICT {
        return Ok(false);
    }
    if !status.is_success() {
        let bytes = to_bytes(resp.into_body(), 1 << 20).await?;
        bail!(
            "POST {uri} for key {:?} returned {status}: {}",
            row.key,
            String::from_utf8_lossy(&bytes)
        );
    }
    Ok(true)
}

/// 建一个 outsider workspace 并在它里面登记一枚 `mdt_` 令牌。
///
/// outsider 的定义是**否定式**的：它不拥有本次回放种下的任何 agent / issue /
/// chat session / task，所以任何指向那些行的 daemon 请求都会落到
/// `require_workspace_access` 的 404 分支上 —— 这正是上游跨空间探针要证明的事。
pub async fn seed_outsider(
    router: &Router,
    db: &mc_db::pool::Db,
    session: &str,
    run: &str,
) -> Result<Outsider> {
    let workspace_id = crate::seed::seed_workspace(router, session, OUTSIDER_GROUP, 0, run)
        .await
        .context("seed the outsider workspace")?;
    let token = crate::daemon_token::register(
        &mc_repos::daemon::DaemonRepo::new(db),
        mc_core::Id::from(workspace_id),
    )
    .await
    .context("register the outsider daemon token")?;
    Ok(Outsider {
        workspace_id,
        daemon_token: token.raw,
    })
}
