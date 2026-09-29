//! `mc-conformance` —— 上游 golden fixture 的本地回放器。
//!
//! # 这个 crate 解决什么问题
//!
//! 本仓是上游 `multica`（Go）的 Rust 重写。**"同样的输入产生上游等价的输出"**
//! 如果只靠人读代码，就是不可证伪的声明。本 crate 把上游测试里**可判定的**
//! 请求/响应断言抽成语言无关的 golden fixture（由
//! `scripts/extract_upstream_fixtures.py` 生成，见 `contracts/golden/`），
//! 再用 `tower::ServiceExt::oneshot` 打到本仓真实的 axum router 上重放：
//!
//! ```text
//! upstream Go 测试  --extract-->  contracts/golden/**.json  --replay-->  this repo's router
//! ```
//!
//! 于是「契约等价」变成一个可以按 fixture 数出来的比例（`docs/27-W0-GOLDEN-FIXTURES.md`）。
//!
//! # 判定口径（不猜、不掩盖）
//!
//! 每个 fixture **恰好**产生一行结果，落进下面五类之一：
//!
//! | outcome | 含义 |
//! |---|---|
//! | `pass` | 状态码一致，且 `expect.json_subset` 是响应 body 的子集 |
//! | `mismatch` | 打到了已实现的路由，但状态码/字段与上游断言不符（**这才是缺陷**）|
//! | `unmounted` | 本仓没有这条路由（404 空 body / 405）—— 属"未实现"，单独计数 |
//! | `placeholder` | 路由存在但是 M0 占位实现（501 / `{"code":"not_implemented"}`）|
//! | `unevaluable` | 本仓无法构造这次请求（如需要真凭据、缺装配、或需要本层不具备的替身）|
//!
//! `unmounted` / `placeholder` / `unevaluable` **不算失败**，但一定出现在报告里 ——
//! 分子分母都从报告行里数出来，所以"等价率"不可能靠遮掉难看的行来变好看。
//!
//! # `requires`：先问「这一层能不能判定」，再回放
//!
//! 一条 fixture 的期望值只在**它背后的场景能被重建**时才是契约。上游很多测试在驱动
//! handler 之前先装配了额外状态：请求 context 里的 daemon 身份、cookie/JWT 会话、
//! 注入的 mock DB、`&Handler{}` 这种没接线的裸 handler、假的 cloud proxy、拒绝一切的
//! 限流器替身、被 stub 掉的 Google OAuth 往返。抽取器以前只看得到**字面 header**，于是
//! 把这些场景统统标成 `anonymous`，stateless 层就「不装配任何资源地」把它们跑了一遍，
//! 再把基础设施的短路响应当成行为差异记下来（`docs/37` §201.2 的三个子根因）。
//!
//! 现在抽取器把每条场景的前提写进 `extraction.requires`（`scripts/extract_upstream_fixtures.py`
//! 的 `requirements_for`），本仓自己那部分前提（哪些端点一上来就查库）写在
//! [`REPO_SIDE_PRECONDITIONS`]。层在回放之前先过 [`requirements`]：**前提凑不齐就不判定**，
//! 落 `unevaluable` 并写明缺哪一样。
//!
//! 🔴 **这是本 crate 最容易被误用的一处**：`mismatch` 变小本身**不是**成果。把它变小有两条路 ——
//! 真的补上了装配（`pass` 变多），或者把场景改判成不可判定（`unevaluable` 变多）。
//! 只有第一条是进步。`docs/37` §203 记的就是这一刀怎么下的。
//!
//! # 两层回放（tier）
//!
//! - **stateless**：`Db` 用 `connect_lazy` 指向一个不可达端口，不建任何连接。
//!   匿名断言（401 一类）在这一层就能判定，且完全确定 —— CI 里跑的就是这层，
//!   所以 `report.json` 可以 `--check` 字节比对。
//! - **database**：真库 + 迁移 + 一个种子用户/workspace owner 成员，
//!   `X-Multica-Session` 用种子用户 id 注入。`member` actor 的 fixture 需要这层。
//!
//! 同一 fixture 可能两层都跑，报告里两层的 outcome 都留着；汇总时取**更强**的那层
//! （`pass` > `mismatch` > `unmounted`/`placeholder`/`unevaluable`），并在
//! `tier` 字段里写明这个结论来自哪一层。

//! 下面这一层实现拆成四个子模块，**crate 根的对外符号逐字不变**（门 ⑩ 第 8 批）：
//! `fixture`（契约解析）/ `request_plan`（请求构造）/ `verdict`（判定）/ `replay`（回放）。

pub mod bindings;
pub mod daemon_token;
mod fixture;
pub mod harness;
mod replay;
mod request_plan;
pub mod report;
pub mod requirements;
pub mod seed;
mod verdict;

pub use bindings::Bindings;
pub use fixture::{load_dir, load_file, Actor, ActorKind, Expect, Extraction, Fixture, Source};
pub use replay::{merge, replay_one, run_tier, to_row, Tier, TierRouters};
pub use report::{OfflineSplit, Report, Row, Totals};
pub use request_plan::plan;
pub use requirements::{
    actor_credential, actor_credential_detail, credential_satisfied_by, missing_requirements,
    request_target_is_encodable, requirement, requirements_detail, unplannable_request_detail,
    ActorCredential, Requirement, ACTOR_CREDENTIALS, PARTIAL_GOLDEN_ROOTS, REPO_SIDE_PRECONDITIONS,
    REQUIREMENTS,
};

pub use verdict::{json_subset, judge, FixtureOutcome, Observed, Outcome};

/// fixture 文件格式版本；与 `scripts/extract_upstream_fixtures.py::SCHEMA_VERSION` 对齐。
pub const SCHEMA_VERSION: u32 = 1;

/// stateless 层的身份：固定值，保证报告可字节复现（这一层没有数据库，
/// 身份只用来走 401 判定与把 UUID 填进路径段，不需要真实存在）。
pub const STATELESS_USER_ID: u128 = 1;
/// 见 [`STATELESS_USER_ID`]。
pub const STATELESS_WORKSPACE_ID: u128 = 2;

