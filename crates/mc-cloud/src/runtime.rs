//! `/api/cloud-runtime/*`（11 条节点池管理面）的出站契约 —— **写者 M9-11**（`LUM-2116`）。
//!
//! 上游：`internal/handler/cloud_runtime.go`（208 行，11 条 handler）+ `internal/cloudruntime/
//! client.go`（255 行）。传输**只读**复用 [`crate::transport`]（`docs/62` R-M9-7：它的唯一写者
//! 是 anchor）—— 本文件只放**路径常量 + 11 个请求构造函数**。
//!
//! # crate 归属（`docs/62` §9.2 的裁定）
//!
//! `mc-cloud` 而不是新建 crate：上游 `cloud_billing.go:16` 逐字「Fleet and Billing share
//! `:8080`」⇒ 节点池面与计费面是**同一份**客户端。⚠️ **波次账目仍属 `M3`**（owner 单元格的
//! 迁移执行点 = `M9-10`）⇒ 本文件**不改** `scripts/route-owners.tsv` 与
//! `docs/fixtures/upstream-routes.tsv`，只承接实现。
//!
//! # 🔴 anchor 桩文档的路径表**已被本片推翻**（登记 `docs/32` §49）
//!
//! anchor 桩里那张「`/api/cloud-runtime/nodes/{id}`」十行表**不是上游的形状**：它是 M9-0
//! 写桩时对 `client.go` 的**推测**，桩文档自己也写了「本表是**形状提示**，不是判据」。
//! 本片起手逐字复核的结论（`router.go:2299-2311` + `cloud_runtime.go:26-110`）：
//!
//! - **11 条里 0 条有路径参数** —— 节点 id 走**体**（`POST /nodes/start` 传的是 `{"node_id":…}`），
//!   不是 `…/nodes/{id}/start`；
//! - 上游路由是 `r.Route("/api/cloud-runtime", …)` + 组内 `r.Get("/", …)` ⇒ 键是
//!   **`/api/cloud-runtime/`（带尾斜杠）**，不是 `/api/cloud-runtime`；
//! - 11 条全部在**同一个** `r.Route` 组里，组内**没有** `r.Use(...)` ⇒ 本片**不挂**机器凭据闸。
//!
//! 两条反向验收（`DoD` 原文）仍然成立：`/api/cloud-runtime/healthz` 与 `.../readyz`
//! **不是**服务探针 `/healthz` / `/readyz`（那两个在 `router.go:1400-1401`，属 M10）——
//! 路径不同、前缀不同，本片**不**混实现、**不**互相注册。
//!
//! # 11 条的出站契约（逐字取自 `cloud_runtime.go:26-110`）
//!
//! | 本地路由 | 方法 | 出站路径 | `withUserID` | `withQuery` | `withBody` |
//! | --- | :-: | --- | :-: | :-: | :-: |
//! | `GET /api/cloud-runtime/` | GET | `/api/v1/` | ✓ | | |
//! | `GET /api/cloud-runtime/healthz` | GET | `/healthz` | | | |
//! | `GET /api/cloud-runtime/readyz` | GET | `/readyz` | | | |
//! | `GET /api/cloud-runtime/nodes` | GET | `/api/v1/nodes` | ✓ | ✓ | |
//! | `POST /api/cloud-runtime/nodes` | POST | `/api/v1/nodes` | ✓ | | ✓ |
//! | `DELETE /api/cloud-runtime/nodes` | DELETE | `/api/v1/nodes` | ✓ | | ✓ |
//! | `POST /api/cloud-runtime/nodes/start` | POST | `/api/v1/nodes/start` | ✓ | | ✓ |
//! | `POST /api/cloud-runtime/nodes/stop` | POST | `/api/v1/nodes/stop` | ✓ | | ✓ |
//! | `POST /api/cloud-runtime/nodes/reboot` | POST | `/api/v1/nodes/reboot` | ✓ | | ✓ |
//! | `POST /api/cloud-runtime/nodes/status` | POST | `/api/v1/nodes/status` | ✓ | | ✓ |
//! | `POST /api/cloud-runtime/nodes/exec` | POST | `/api/v1/nodes/exec` | ✓ | | ✓ |
//!
//! # 两条探针的**匿名**语义（`withUserID` 关闭）
//!
//! `GET /api/cloud-runtime/healthz` 与 `.../readyz` 是 11 条里**唯一两条不盖章 `X-User-ID`**
//! 的（上游逐字：`cloudRuntimeProxyOptions{}` 全 false）。它们探的是**云侧**那个 fleet 服务
//! 自身，与调用者是谁无关 ⇒ 少一个身份头正是要表达的意思。⚠️ 本仓的**会话闸仍然生效**
//! （`AuthUser` ⇒ 401）—— 上游这两条在 workspace member 组里，本地照抄。
//!
//! # 为什么本文件**没有**响应类型
//!
//! 与 [`crate::billing`] 同一理由（上游 `writeCloudRuntimeResponse` 把云侧状态码与体**原样**
//! 写回客户端）⇒ 建模就是第二个真相源。**出站体的字段级契约只能来自上游测试**，⑨ 的
//! `json_subset` 对本簇为空对象时只证明「挂上了 + 200 + 体是 JSON 对象」。
//!
//! # 计量标签**不**显式钉
//!
//! 上游 `cloudruntime.Request.Op` 在这 11 条上**全是空**（`proxyCloudRuntime` 不传 `Op`）⇒
//! 桶由 [`crate::transport::infer_op`] 按路径推导。这里**照抄那个行为**（不写死常量），
//! 并由 `derived_op_buckets_match_upstream` 逐条把 11 条的推导结果钉住 —— 写死常量会
//! 让"路径改了标签没改"这类漂移**测不出来**。

use mc_core::Id;

use crate::transport::Request;

/// 本地面前缀（11 条；`GET` 那条**带**尾斜杠，见模块头）。
pub const CLOUD_RUNTIME_PREFIX: &str = "/api/cloud-runtime";

/// 云侧节点池面前缀（上游 `cloud_runtime.go` 里 `"/api/v1/nodes"` 系列的那一半）。
pub const RUNTIME_UPSTREAM_PREFIX: &str = "/api/v1";

// ---------------------------------------------------------------------------
// 云侧路径（11 条，逐字；**唯一的**拼接点都在下面的函数里）
// ---------------------------------------------------------------------------

/// `GET /api/cloud-runtime/` 的出站路径。
///
/// ⚠️ **带尾斜杠，且必须带**（上游 `cloud_runtime.go:27` 逐字 `"/api/v1/"`）。这是全 11 条
/// 里唯一一条**不**落在 [`RUNTIME_UPSTREAM_PREFIX`] 之下干净拼出来的路径 ——
/// `format!("{PREFIX}/")` 与逐字串是同一个值，测试逐字比对。
pub const SERVICE_PATH: &str = "/api/v1/";
/// `GET /api/cloud-runtime/healthz` 的出站路径（**云侧** fleet 服务的探针，匿名）。
pub const HEALTHZ_PATH: &str = "/healthz";
/// `GET /api/cloud-runtime/readyz` 的出站路径（**云侧** fleet 服务的探针，匿名）。
pub const READYZ_PATH: &str = "/readyz";
/// `GET|POST|DELETE /api/cloud-runtime/nodes` 的出站路径（三条**共用**）。
pub const NODES_PATH: &str = "/api/v1/nodes";
/// `POST /api/cloud-runtime/nodes/start`。
pub const NODES_START_PATH: &str = "/api/v1/nodes/start";
/// `POST /api/cloud-runtime/nodes/stop`。
pub const NODES_STOP_PATH: &str = "/api/v1/nodes/stop";
/// `POST /api/cloud-runtime/nodes/reboot`。
pub const NODES_REBOOT_PATH: &str = "/api/v1/nodes/reboot";
/// `POST /api/cloud-runtime/nodes/status`。
pub const NODES_STATUS_PATH: &str = "/api/v1/nodes/status";
/// `POST /api/cloud-runtime/nodes/exec`。
pub const NODES_EXEC_PATH: &str = "/api/v1/nodes/exec";

/// `withBody` 打开的那 7 条共用一个体读取装置 ⇒ 它们共用这个构造函数。
///
/// 体**逐字**转发（handler 已在读体时做过 1 MiB / 空体 / JSON 语法三道判定）—— 本函数
/// **不** trim、**不**重编码：`POST /api/v1/nodes` 会让云侧签一条带 `node-pat` 的 EC2 启动
/// 载荷，任何重排都会改掉字节。
fn body_request(
    method: reqwest::Method,
    path: &str,
    user_id: Id,
    request_id: Option<&str>,
    body: Vec<u8>,
) -> Request {
    Request {
        method,
        path: path.to_string(),
        user_id: Some(user_id),
        request_id: request_id.map(str::to_string),
        // `op` 留空 ⇒ 走 [`crate::transport::infer_op`] 的路径推导（上游同款，见模块头）。
        ..Request::default()
    }
    .with_body(body)
}

/// 带身份、不带体也不带 query 的那条（`GET /api/cloud-runtime/`）。
fn identity_request(
    method: reqwest::Method,
    path: &str,
    user_id: Id,
    request_id: Option<&str>,
) -> Request {
    Request {
        method,
        path: path.to_string(),
        user_id: Some(user_id),
        request_id: request_id.map(str::to_string),
        ..Request::default()
    }
}

/// `GET /api/cloud-runtime/`（上游 `GetCloudRuntimeService`，`withUserID`）。
#[must_use]
pub fn service_request(user_id: Id, request_id: Option<&str>) -> Request {
    identity_request(reqwest::Method::GET, SERVICE_PATH, user_id, request_id)
}

/// `GET /api/cloud-runtime/healthz`（上游 `GetCloudRuntimeHealth`）。
///
/// ⚠️ **不注入身份**（`withUserID` 关闭）—— 见模块头「两条探针的**匿名**语义」。
#[must_use]
pub fn healthz_request(request_id: Option<&str>) -> Request {
    Request {
        method: reqwest::Method::GET,
        path: HEALTHZ_PATH.to_string(),
        request_id: request_id.map(str::to_string),
        ..Request::default()
    }
}

/// `GET /api/cloud-runtime/readyz`（上游 `GetCloudRuntimeReady`，同样不注入身份）。
#[must_use]
pub fn readyz_request(request_id: Option<&str>) -> Request {
    Request {
        method: reqwest::Method::GET,
        path: READYZ_PATH.to_string(),
        request_id: request_id.map(str::to_string),
        ..Request::default()
    }
}

/// `GET /api/cloud-runtime/nodes`（上游 `ListCloudRuntimeNodes`）—— 11 条里**唯一**带 query 的。
#[must_use]
pub fn list_nodes_request(
    user_id: Id,
    request_id: Option<&str>,
    query: Vec<(String, String)>,
) -> Request {
    identity_request(reqwest::Method::GET, NODES_PATH, user_id, request_id).with_query(query)
}

/// `POST /api/cloud-runtime/nodes`（上游 `CreateCloudRuntimeNode`）。
///
/// 上传逐字注明的理由（`cloud_runtime.go:47-53`）：云侧在 `POST /api/v1/nodes` 期间**自己**
/// 铸造节点级 `mcn_` PAT 并经 SSM bootstrap 注入实例 ⇒ 本地**不再**转发调用方的 `mul_` PAT。
/// 本地照抄这个取舍：只转发体。
#[must_use]
pub fn create_node_request(user_id: Id, request_id: Option<&str>, body: Vec<u8>) -> Request {
    body_request(reqwest::Method::POST, NODES_PATH, user_id, request_id, body)
}

/// `DELETE /api/cloud-runtime/nodes`（上游 `DeleteCloudRuntimeNode`）—— 节点 id 在**体**里。
#[must_use]
pub fn delete_node_request(user_id: Id, request_id: Option<&str>, body: Vec<u8>) -> Request {
    body_request(
        reqwest::Method::DELETE,
        NODES_PATH,
        user_id,
        request_id,
        body,
    )
}

/// `POST /api/cloud-runtime/nodes/start`（上游 `StartCloudRuntimeNode`）。
#[must_use]
pub fn start_node_request(user_id: Id, request_id: Option<&str>, body: Vec<u8>) -> Request {
    body_request(
        reqwest::Method::POST,
        NODES_START_PATH,
        user_id,
        request_id,
        body,
    )
}

/// `POST /api/cloud-runtime/nodes/stop`（上游 `StopCloudRuntimeNode`）。
#[must_use]
pub fn stop_node_request(user_id: Id, request_id: Option<&str>, body: Vec<u8>) -> Request {
    body_request(
        reqwest::Method::POST,
        NODES_STOP_PATH,
        user_id,
        request_id,
        body,
    )
}

/// `POST /api/cloud-runtime/nodes/reboot`（上游 `RebootCloudRuntimeNode`）。
#[must_use]
pub fn reboot_node_request(user_id: Id, request_id: Option<&str>, body: Vec<u8>) -> Request {
    body_request(
        reqwest::Method::POST,
        NODES_REBOOT_PATH,
        user_id,
        request_id,
        body,
    )
}

/// `POST /api/cloud-runtime/nodes/status`（上游 `GetCloudRuntimeNodeStatus`）。
///
/// ⚠️ 上游用 **`POST`** 查状态（`GetCloudRuntimeNodeStatus` 也是 POST）⇒ 本地**不**"顺手"
/// 改成 GET：改了就与上游 ⑦ 的键不符。
#[must_use]
pub fn node_status_request(user_id: Id, request_id: Option<&str>, body: Vec<u8>) -> Request {
    body_request(
        reqwest::Method::POST,
        NODES_STATUS_PATH,
        user_id,
        request_id,
        body,
    )
}

/// `POST /api/cloud-runtime/nodes/exec`（上游 `ExecCloudRuntimeNode`）。
#[must_use]
pub fn exec_node_request(user_id: Id, request_id: Option<&str>, body: Vec<u8>) -> Request {
    body_request(
        reqwest::Method::POST,
        NODES_EXEC_PATH,
        user_id,
        request_id,
        body,
    )
}

/// 查询串解析（上游 `r.URL.Query()`）—— **不重写第二份**。
///
/// [`crate::billing::parse_query`] 是 M9-1 已实作的同一口径（保序、保多值、`+` ⇒ 空格）；
/// 四个代理簇共用一份解析器，少一处"两片对同一段 query 的理解不同"的可能。
pub use crate::billing::parse_query;

#[cfg(test)]
mod tests;
