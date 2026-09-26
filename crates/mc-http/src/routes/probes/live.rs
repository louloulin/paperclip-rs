//! `GET /health` —— liveness 探针（**M10-1 原地填充**，`docs/64` §2.1 / §4.1 第 2 行）。
//!
//! 上游：`server/cmd/server/health.go::liveHandler`（`f41fae6b08fb` L85-91）——
//! **进程活着就 200，不触库**；body `{status:"ok", pid, commit, started_at}`（后三者 omitempty）。
//!
//! ```go
//! func (h *serverHealth) liveHandler(w http.ResponseWriter, _ *http.Request) {
//!     resp := liveResponse{Status: "ok", PID: h.pid, Commit: commit}
//!     if !h.startedAt.IsZero() {
//!         resp.StartedAt = h.startedAt.Format(time.RFC3339)
//!     }
//!     writeJSON(w, http.StatusOK, resp)
//! }
//! ```
//!
//! （上面这段 Go 在文档注释里用**空格**缩进而不是制表符：`clippy::tabs_in_doc_comments`
//! 是 pedantic 档、门 ③ 把它当错误 —— 与代码语义无关，只是文档排版。）
//!
//! ## 语义：`/health` ≠ `/healthz` ≠ `/readyz`（`docs/64` §2.1 的三分法）
//!
//! 本文件**只**回答 liveness —— 进程还在、端口还有人在听。它**不** `Ping` 库、**不**比对
//! `schema_migrations`、**不**读任何 `AppState` 字段（handler 连 `State` 提取器都不挂 ⇒
//! 「不触库」是**结构性**成立的，不是靠"记得别查库"）。就绪判定在 `probes/ready.rs`（M10-2）。
//!
//! ## `pid` / `started_at` 为什么必须在（上游逐字注释的业务理由）
//!
//! 上游 `serverHealth` 的字段注释（`health.go` L40-45 逐字）：
//!
//! > `startedAt` and `pid` identify the process answering `/health`. A 200 alone
//! > only proves something is listening on the port: when a restart fails to
//! > bind, the previous instance keeps serving and every readiness check still
//! > passes, so a caller can configure or test the wrong build without any
//! > visible error. Local tooling compares `started_at` against its own launch
//! > time to prove the answer came from the process it just started.
//!
//! ⇒ 光有 200 **不够**：`started_at` 必须是**进程级常量**（同一个进程的每次请求逐字相同），
//! 客户端拿它跟自己刚启动进程的时刻比，就能识破"新进程没绑上、老进程还在服务"。
//! 若把它写成 `Utc::now()`（请求时刻）这条判据就归零 —— `tests.rs` 里
//! `started_at_is_process_scoped_not_request_scoped` 用「跨秒 + 两次请求逐字相同」把它钉住。
//!
//! ## 形态：只注册**无尾斜杠**那一形态
//!
//! 上游是 plain `r.Get("/health", health.liveHandler)`（`router.go:1399`）⇒ chi 只服务
//! `/health` 一种形态（`docs/64` §1.4 实测 `dual-form required: 0`）。多注册 `/health/`
//! 就是 `EXTRA_ALIAS` 硬失败，本波 `docs/fixtures/slash-alias-allowlist.tsv` 是 **0 数据行**、
//! 没有豁免退路。
//!
//! ## M10-0 anchor 期的两条纪律（保留为历史，勿删）
//!
//! anchor 期本文件是**空** `Router::new()` ⇒ **零注册键**，且明令**不得**注册 501 占位：
//! `/health` 在 ⑨ 有 1 条 fixture（`contracts/golden/health/001-TestHealth-L212.json`，
//! `actor=anonymous`），占位会把它从 `unmounted` 变成 **`mismatch`**（期望 200 / 得到 501）。
//! 本片按该纪律落地**真实现**（不是"先把占位变绿"）。
//!
//! ## 与上游的**已知差异**（登记在 `docs/32-M3-DAEMON-FACE.md` §41）
//!
//! | # | 上游 | 本地 | 影响 |
//! | :-: | --- | --- | --- |
//! | 1 | `startedAt` 在 `newServerHealth()`（`ListenAndServe` 之前）取 `time.Now()` | 进程级 `OnceLock`，在**首次装配探针面**（`router()`，同样在 `TcpListener::bind` 之前）定值 | 可判定性质不变（见上）；Rust 标准库没有"进程启动时刻"API，故不引入平台特有代码 |
//! | 2 | `commit` 由 `-ldflags -X main.commit=` 注入 | `MC_BUILD_COMMIT` 由**构建期** `option_env!` 读入 | 本仓没有 ldflags 等价物；CI 尚未设置该变量 ⇒ dev 构建下键**不出现**（与上游 dev 构建同行为） |
//! | 3 | `writeJSON` 在 body 末尾补 `'\n'` 并显式写 `Content-Length` | axum `Json`（无尾换行，length 自动） | `Content-Type: application/json` 一致；⑨ 判据是 `json_subset`（本 fixture 为空对象）⇒ 不受影响 |

use std::sync::{Arc, OnceLock};

use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;

use crate::state::AppState;

/// 构建期注入的 commit（`MC_BUILD_COMMIT`）。
///
/// 空串 = 未注入（本地 `cargo build`、CI 现状）—— 与上游 dev 构建 `commit == ""` 同行为：
/// 因为 `omitempty`，此时响应里**没有** `commit` 键（不是 `"commit": ""`）。
///
/// 为什么用 `option_env!` 而不是 `std::env::var`：上游是 **ldflags 注入**（编译期常量），
/// 运行期读环境变量会让同一个二进制在不同 env 下报不同的 build 身份 —— 那正好废掉
/// `started_at`/`commit` 这组"回答者是谁"判据。
/// `match` 而不是 `Option::unwrap_or`：后者在 `const` 上下文里**尚未稳定**
/// （`E0658: cannot call conditionally-const method`），本仓 MSRV 1.80 也用不了。
const COMMIT: &str = match option_env!("MC_BUILD_COMMIT") {
    Some(v) => v,
    None => "",
};

/// 响应体 —— 上游 `liveResponse`（`health.go` L56-61）的逐字段对应物。
///
/// `omitempty` 的**三个**字段在这里都用 `skip_serializing_if` 复刻（上游语义：
/// Go 的 `omitempty` 对 `int` 判 0、对 `string` 判空串）：
/// - `pid`：`std::process::id()` 在真实进程里恒非 0 ⇒ **实际必现**；
/// - `commit`：未注入 ⇒ 空串 ⇒ **键不出现**；
/// - `started_at`：进程级单例恒非空 ⇒ **实际必现**。
///
/// `status` **没有** `omitempty`（上游逐字）⇒ 无论何时都必现，且恒 `"ok"`。
#[derive(Debug, Clone, Serialize)]
pub struct LiveResponse {
    /// liveness 结论；上游是**常量** `"ok"`（`liveHandler` 是唯一写入点）。
    pub status: &'static str,
    /// 回答者的 pid（`omitempty`：0 才省略）。
    #[serde(skip_serializing_if = "is_zero")]
    pub pid: u32,
    /// 构建期注入的 commit（`omitempty`：空串才省略）。
    #[serde(skip_serializing_if = "is_blank")]
    pub commit: &'static str,
    /// **进程启动时刻**的 RFC3339（`omitempty`：空串才省略）。
    ///
    /// 格式取 `to_rfc3339_opts(SecondsFormat::Secs, true)` ⇒ `2006-01-02T15:04:05Z`，
    /// 与上游 `time.RFC3339` 的**秒级 + `Z`** 形态一致（`chrono` 默认的 `to_rfc3339()`
    /// 会带纳秒与 `+00:00`，那是**另一个**字面量）。
    #[serde(skip_serializing_if = "is_blank")]
    pub started_at: String,
}

/// 进程启动时刻的**进程级单例**（见模块头「已知差异」第 1 条）。
///
/// 为什么是 `OnceLock` 而不是每次请求取 `Utc::now()`、也不是挂在 `AppState` 上：
/// - 语义要的是「**这个进程**从什么时候开始活着」⇒ 进程级单例是唯一正确的粒度；
/// - `AppState` 是 M10-0 anchor 冻结的共享文件（`docs/32` §39.1）⇒ 本片**无权**给它加字段
///   （写集只有本文件 + `live/tests.rs`）。
///
/// 定值点在 [`router`]（**不是** handler 的首次调用）：`router()` 在生产装配里由
/// `apps/mc-server/src/main.rs:175` 调用，`TcpListener::bind` 在 `:225` —— 与本文件头的
/// 「已知差异」表逐字对应，保证客户端"比我启动的时刻早 ⇒ 这不是我起的进程"这条判据成立。
static STARTED_AT: OnceLock<DateTime<Utc>> = OnceLock::new();

/// 进程启动时刻（RFC3339 秒级、UTC）。
///
/// `pub(crate)` 是给 `tests.rs` 用的：有了它，"响应里的 `started_at` 就是进程级单例"
/// 可以**直接**断言，而不必靠"两次请求恰好相同"这种时序巧合。
pub(crate) fn process_started_at() -> DateTime<Utc> {
    *STARTED_AT.get_or_init(Utc::now)
}

/// 上游 `liveHandler`：**恒 200**（`status: "ok"`）+ `pid` / `commit` / `started_at`。
///
/// 故意**不挂** `State<Arc<AppState>>`：handler 拿不到库、拿不到配置、拿不到任何外部资源 ⇒
/// 「不触库」在类型层面就成立（`docs/64` §6.5 的 M10-1 专属验收第 1 条）。
/// 也因此它在库不可达时**照样** 200（`tests.rs` 用不可达 DSN 的 `connect_lazy` 钉住）。
async fn live() -> Json<LiveResponse> {
    Json(LiveResponse {
        status: "ok",
        pid: std::process::id(),
        commit: COMMIT,
        started_at: timestamp(process_started_at()),
    })
}

/// `/health` 切片（M10-1：把 anchor 期的空 router 换成真注册）。
///
/// 只注册**无尾斜杠**那一形态（模块头「形态」节）。
pub fn router(_state: Arc<AppState>) -> Router<Arc<AppState>> {
    // 进程级单例在此**定值**（理由见 `STARTED_AT` 的文档）。显式调用而不是等 handler
    // 首次访问：否则"启动后一直没人探 /health"会让 `started_at` 漂到第一个请求的秒上，
    // 那正是上游注释要防的"看起来像刚起的进程"。
    let _ = process_started_at();
    Router::new().route("/health", get(live))
}

/// RFC3339、秒级、`Z` 结尾 —— 对齐上游 `time.RFC3339` 的形态。
fn timestamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// `serde` 的 `omitempty` 复刻（`int` 判 0）。
///
/// `#[allow(clippy::trivially_copy_pass_by_ref)]`：签名由 serde 的 `skip_serializing_if`
/// 定死（它以 `&T` 调用谓词），不是随手传引用 —— 与 `routes/config.rs` 的 `is_false` 同款豁免。
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(v: &u32) -> bool {
    *v == 0
}

/// `serde` 的 `omitempty` 复刻（`string` 判空串）。
fn is_blank(v: &str) -> bool {
    v.is_empty()
}

#[cfg(test)]
mod tests;
