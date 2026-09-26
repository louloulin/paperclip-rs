//! `/api/cloud-runtime/*`（11 条节点池管理面）的出站路径与响应类型位 —— **写者 M9-11**。
//!
//! # ⚠️ 本文件的**来源**（`docs/64-M10-PLAN.md` §5.2 的追加请求）
//!
//! 本文件与 [`crate::lib`] 里的 `pub mod runtime;` 由 **M9-0** 预声明，**M9-11**
//! （`LUM-2116`）原地填充。理由（`docs/62` §9.2 的裁定）：这 11 条的 **crate 归属 = `mc-cloud`**
//! —— 上游 `internal/cloudruntime/client.go` 本来就是 cloud-runtime 与 billing
//! **同一份客户端**（`cloud_billing.go:16` 逐字：「Fleet and Billing share `:8080`」），
//! 拆成两处会立刻出现第二份 base-URL 解析与错误映射。
//! ⚠️ **波次账目仍属 `M3`**（owner 单元格的迁移执行点 = `M9-10`）⇒ 本 crate 只承接
//! **实现**，不改 `scripts/route-owners.tsv` / `docs/fixtures/upstream-routes.tsv`。
//!
//! # M9-11 要填什么
//!
//! 上游 `internal/handler/cloud_runtime.go`（208 行，11 条 handler）+ `cloudRuntimeProxyOptions`
//! （`withUserID` / `withQuery` / `withBody` 三个开关逐条不同）：
//!
//! | 本地路由 | 方法 | 出站路径 | `withUserID` | `withQuery` | `withBody` |
//! | --- | :-: | --- | :-: | :-: | :-: |
//! | `/api/cloud-runtime` | GET | `/api/v1/` | ✓ | | |
//! | `/api/cloud-runtime/healthz` | GET | `/healthz` | | | |
//! | `/api/cloud-runtime/readyz` | GET | `/readyz` | | | |
//! | `/api/cloud-runtime/nodes` | GET | `/api/v1/nodes` | ✓ | ✓ | |
//! | `/api/cloud-runtime/nodes` | POST | `/api/v1/nodes` | ✓ | | ✓ |
//! | `/api/cloud-runtime/nodes/{id}` | GET | `/api/v1/nodes/{id}` | ✓ | | |
//! | `/api/cloud-runtime/nodes/{id}` | DELETE | `/api/v1/nodes/{id}` | ✓ | | |
//! | `/api/cloud-runtime/nodes/{id}/reboot` | POST | `…/reboot` | ✓ | | |
//! | `/api/cloud-runtime/nodes/{id}/start` | POST | `…/start` | ✓ | | |
//! | `/api/cloud-runtime/nodes/{id}/stop` | POST | `…/stop` | ✓ | | |
//! | `/api/cloud-runtime/nodes/exec` | POST | `/api/v1/nodes/exec` | ✓ | | ✓ |
//!
//! ⚠️ 上表的逐行 `router.go` 行号与 `withXxx` 取值必须由 **M9-11 起手逐字复核**
//! （`awk -F'\t' '!/^#/ && $3=="M3" && $2 ~ /^\/api\/cloud-runtime/' docs/fixtures/upstream-routes.tsv`）；
//! 本表是**形状提示**，不是判据（`docs/62` §9.2 只钉了 crate 归属，没钉这 11 行的细节）。
//!
//! # 两条反向验收（M9-11 的 `DoD` 原文）
//!
//! 1. `GET /api/cloud-runtime/healthz` 与 `.../readyz` **不是**上游的服务探针
//!    `/healthz` / `/readyz`（那两个在 `router.go:1400-1401`，属 M10）⇒ **不得**混实现、
//!    **不得**互相注册（路径不同、前缀不同，混了会撞 ⑦ 的 owner 归属）；
//! 2. 11 条**全部**在 workspace **member** 组（无 fixture、无 anonymous 面）。
//!
//! # 本文件在 anchor 期是**空桩**
//!
//! anchor 不实现任何路由逻辑（`docs/62` §5）。路径常量与响应类型归 M9-11 原地填充；
//! 传输**只读**复用 [`crate::transport`]（`docs/62` R-M9-7：`transport.rs` 的唯一写者是
//! anchor，此后冻结）。
