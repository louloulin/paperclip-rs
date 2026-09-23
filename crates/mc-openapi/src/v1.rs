//! 公开 Action API（`/v1/*`）的契约台账：**9 个 Operation + 4 种凭据 + 2 档限流 + `ProblemDetail`**。
//!
//! - **状态**：M6-1 已填充（`docs/57` §4.2）。上游 = `pkg/publicapi/v1/{foundation.go 79,
//!   problem.go 111, routes.go 67, types.go 106}`（`spec.go 14` 见下「已知缺口」）。
//! - **写者**：M6-1（**W**）。M6-7 只读：公开 API 的 route 表必须与本表的 9 条逐条对齐
//!   （`GET|PATCH` 两个 `/issues/{issue_ref}` 形态、`GET|PUT|DELETE` 三个 `/storage/{scope}/{key}`）。
//! - **口径**：**0 路由** —— 这里是**声明**；路由在 M6-7 的 `routes/v1/*`。两边不一致时以
//!   route 表的**实测**为准（门 ⑦），并在 `docs/32` §9 登记。
//! - **路径形态**：台账的 `path` **相对 [`BASE_PATH`]**（上游 `routes.go` 的 `Path*` 常量逐字），
//!   完整形态是 `/v1/issues/{issue_ref}`，与 `docs/fixtures/m6-declared-routes.tsv` 逐字相同；
//!   axum 0.7 需要 `:issue_ref`，转换走 [`Operation::axum_path`] / [`axum_path`] 这一个实现点。
//! - **为什么单独一个文件而不是塞进 `lib.rs`**：`lib.rs` 是既有 `OpenApiSpec` 的手写生成器
//!   （本波的 `/v1` 面与旧面**不共用路径前缀、不共用凭据**）；拆开保住「一个文件一个写者」，
//!   也避免两套 schema 体系在 `render()` 时打架。
//! - **不做什么**：鉴权/限流的**实现**不在这里（凭据校验在 `routes/v1/policy.rs` +
//!   `mc-plugin-host::token`，scope 判定在 `mc-plugin-host::scope`，限流档位靠 `tower_governor`
//!   在 `routes/v1/mod.rs` 加一次）；本文件只声明**路径 + 策略**。
//!
//! ## 与上游的偏差（都在批注里就地标明）
//!
//! 1. **`scope 判定`的唯一实现点在 `mc-plugin-host::scope`**：本表的 `policy.scope` 只是
//!    「这条路径需要哪个 scope」的**声明**，判定逻辑不在这里（`docs/57` §2.4 第 4 条）。
//! 2. **nil map / nil slice 的零值形态**：Go 里 `map[string]any(nil)` 序列化成 `null`、nil
//!    slice 也是 `null`；本仓用 `BTreeMap`/`Vec`（`{}` / `[]`）。客户端拿到的都是空容器，
//!    语义等价；差别只在字节层面（金样例里若有 `null` 要按本仓形态更新）。
//! 3. **`ProblemDetail.error`** 是给既有 Plugin 客户端的兼容别名，等于 `detail`（上游注释
//!    明确要求保留）。新客户端应当分支 `code`、渲染 `detail`。
//! 4. **`WriteProblem` 的写响应部分不在这里**：本文件提供**纯构造**（`problem_detail`），
//!    落 `Content-Type: application/problem+json` + `X-Request-Id`（上游 `middleware.RequestIDHeader`）
//!    是 HTTP 层的事（M6-7）。
//! 5. **`spec.go`（`//go:embed openapi.yaml`）不落**：那份 408 行 YAML 资产不在本片写集内
//!    （本片写集只有本文件）。本仓 `/openapi.json` 由 `lib.rs` 现生成；若 M6-7 要独立的
//!    `/v1` 规范文档，需先在 `docs/32` §9 登记资产 + 写者。

use serde::{Deserialize, Serialize};

/// `/v1` 的路径前缀（上游 `publicapiv1.BasePath`）。
pub const BASE_PATH: &str = "/v1";

/// 幂等键请求头（上游 `HeaderIdempotencyKey`）。
pub const HEADER_IDEMPOTENCY_KEY: &str = "Idempotency-Key";
/// 乐观并发请求头（上游 `HeaderIfMatch`）。
pub const HEADER_IF_MATCH: &str = "If-Match";
/// 上游 `middleware.RequestIDHeader`：问题响应的关联 id 同时回在这个头上。
pub const HEADER_REQUEST_ID: &str = "X-Request-Id";
/// 幂等键的最大字节数（上游 `MaxIdempotencyBytes`）。
pub const MAX_IDEMPOTENCY_BYTES: usize = 255;
/// 默认页大小（上游 `DefaultPageSize`）。
pub const DEFAULT_PAGE_SIZE: u32 = 50;
/// 页大小上限（上游 `MaxPageSize`）。
pub const MAX_PAGE_SIZE: u32 = 200;

/// 问题响应的 `Content-Type`（RFC 9457；上游 `ProblemContentType`）。
pub const PROBLEM_CONTENT_TYPE: &str = "application/problem+json";

/// `/v1` 的五个资源路径（上游 `routes.go`）。尾斜杠**不是**语义的一部分。
pub const PATH_CONTEXT: &str = "/context";
pub const PATH_ISSUE: &str = "/issues/{issue_ref}";
pub const PATH_ISSUE_COMMENTS: &str = "/issues/{issue_ref}/comments";
pub const PATH_STORAGE_SCOPE: &str = "/storage/{scope}";
pub const PATH_STORAGE_VALUE: &str = "/storage/{scope}/{key}";

/// 认证这条请求的信任边界（上游 `CredentialKind`）。资源服务只拿到已授权的 actor，
/// **不解析令牌**。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CredentialKind {
    /// 用户 OAuth 会话。
    #[serde(rename = "user_oauth")]
    UserOAuth,
    /// 个人访问令牌（PAT）。
    #[serde(rename = "personal_access_token")]
    PersonalAccessToken,
    /// 插件**安装令牌**（`mpi_…`，长期）。
    #[serde(rename = "plugin_installation")]
    PluginInstallation,
    /// 插件**调用令牌**（`mpc_…`，按一次 invocation 签发）。
    #[serde(rename = "plugin_invocation")]
    PluginInvocation,
}

impl CredentialKind {
    /// 上游字面量（日志/审计/金样例用；与 serde 的 `snake_case` 由测试钉住相等）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UserOAuth => "user_oauth",
            Self::PersonalAccessToken => "personal_access_token",
            Self::PluginInstallation => "plugin_installation",
            Self::PluginInvocation => "plugin_invocation",
        }
    }
}

/// actor 的种类（上游 `ActorKind`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ActorKind {
    /// 工作区成员。
    #[serde(rename = "member")]
    Member,
    /// 插件（以安装身份行动）。
    #[serde(rename = "plugin")]
    Plugin,
}

impl ActorKind {
    /// 上游字面量。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Member => "member",
            Self::Plugin => "plugin",
        }
    }
}

/// 身份（上游 `Actor`）：信任面认证完凭据之后交给共享授权/审计/服务层的**传输无关**身份。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    pub kind: ActorKind,
    pub subject_id: String,
    pub workspace_id: String,
    pub credential: CredentialKind,
}

/// 风险档（上游 `RiskLevel`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RiskLevel {
    /// 只读。
    #[serde(rename = "read")]
    Read,
    /// 内容写入（可逆的正文改动）。
    #[serde(rename = "content_write")]
    ContentWrite,
    /// 高风险（发放凭据、改配置、删资源）。
    #[serde(rename = "high")]
    High,
}

impl RiskLevel {
    /// 上游字面量。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::ContentWrite => "content_write",
            Self::High => "high",
        }
    }
}

/// 限流档（上游 `RateLimitProfile`）：两档，`plugin_strict` 更紧。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RateLimitProfile {
    /// 会话/ PAT 面。
    #[serde(rename = "user_default")]
    UserDefault,
    /// 插件面（安装令牌 / 调用令牌）。
    #[serde(rename = "plugin_strict")]
    PluginStrict,
}

impl RateLimitProfile {
    /// 上游字面量。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UserDefault => "user_default",
            Self::PluginStrict => "plugin_strict",
        }
    }
}

/// 审计状态（上游 `AuditStatus`）：区分「声明了要求」与「真的落了审计 sink」——
/// 不许因为台账里列了一行就声称已强制。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AuditStatus {
    /// 不需要审计。
    #[serde(rename = "not_required")]
    NotRequired,
    /// 台账已声明，sink 未实现。
    #[serde(rename = "planned")]
    Planned,
    /// 已强制。
    #[serde(rename = "enforced")]
    Enforced,
}

impl AuditStatus {
    /// 上游字面量。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotRequired => "not_required",
            Self::Planned => "planned",
            Self::Enforced => "enforced",
        }
    }
}

/// 契约种类（上游 `ContractKind`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ContractKind {
    /// 通用 Multica 资源契约，可经各自信任面的授权暴露给多种凭据。
    #[serde(rename = "shared_resource")]
    SharedResource,
    /// 属于插件安装（而非通用资源 API）的扩展面。
    #[serde(rename = "plugin_extension")]
    PluginExtension,
}

impl ContractKind {
    /// 上游字面量。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SharedResource => "shared_resource",
            Self::PluginExtension => "plugin_extension",
        }
    }
}

/// 一条 Operation 的授权与可观测性要求（上游 `OperationPolicy`）。
///
/// **信任面中间件**负责强制 `credentials` + `rate_limits`；**资源授权**负责
/// workspace 与 `scope`（判定实现在 `mc-plugin-host::scope`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationPolicy {
    /// 允许的凭据种类（同一路径在不同信任面上允许的集合不同）。
    pub credentials: &'static [CredentialKind],
    /// 需要的 manifest scope（空串 = 该契约不使用 scope）。
    pub scope: &'static str,
    /// 风险档。
    pub risk: RiskLevel,
    /// 审计状态。
    pub audit: AuditStatus,
    /// 限流档（可以同时挂多档，例如「用户档 + 插件档」共用一条路径）。
    pub rate_limits: &'static [RateLimitProfile],
}

/// 台账里的一条能力（上游 `Operation`）。
///
/// 摘要**故意没有**：上游的 operation summary 在 `openapi.yaml` 里，本仓没有该资产
/// （见模块头偏差 5），自己编 9 条摘要只会与那份 YAML 漂移。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Operation {
    /// HTTP 方法（大写，照上游 `http.MethodGet` 等的字面量）。
    pub method: &'static str,
    /// 路径模板（**相对 [`BASE_PATH`]**，上游 `routes.go` 的 `Path*` 常量逐字；
    /// `{name}` 占位是契约形态，axum 形态见 [`Operation::axum_path`]）。
    pub path: &'static str,
    /// 契约种类。
    pub contract: ContractKind,
    /// 授权与可观测性要求。
    pub policy: OperationPolicy,
}

impl Operation {
    /// 带上 [`BASE_PATH`] 的完整路径（`/v1/context`），与声明路由表逐字同形。
    #[must_use]
    pub fn full_path(&self) -> String {
        format!("{BASE_PATH}{}", self.path)
    }

    /// axum 0.7 的挂载形态（`/v1/issues/:issue_ref`）。
    #[must_use]
    pub fn axum_path(&self) -> String {
        axum_path(&self.full_path())
    }
}

/// 共享资源的凭据集合（上游 `sharedCredentials`）。
pub const SHARED_CREDENTIALS: &[CredentialKind] = &[
    CredentialKind::UserOAuth,
    CredentialKind::PersonalAccessToken,
    CredentialKind::PluginInstallation,
    CredentialKind::PluginInvocation,
];

/// 插件扩展面的凭据集合（上游 `pluginCredentials`）。
pub const PLUGIN_CREDENTIALS: &[CredentialKind] = &[
    CredentialKind::PluginInstallation,
    CredentialKind::PluginInvocation,
];

/// 共享资源的限流档（上游 `sharedRateLimits`）。
pub const SHARED_RATE_LIMITS: &[RateLimitProfile] = &[
    RateLimitProfile::UserDefault,
    RateLimitProfile::PluginStrict,
];

/// 插件扩展面的限流档（上游 `pluginRateLimits`）。
pub const PLUGIN_RATE_LIMITS: &[RateLimitProfile] = &[RateLimitProfile::PluginStrict];

/// `/v1` 的能力台账（上游 `Operations`，9 条，逐条同序）。
///
/// 顺序与 `docs/fixtures/m6-declared-routes.tsv` 的 `/v1` 段相同（context 1 /
/// issues 4 / storage 4），由 `operations_match_the_declared_v1_routes` 逐个钉住。
pub const OPERATIONS: &[Operation] = &[
    Operation {
        method: "GET",
        path: PATH_CONTEXT,
        contract: ContractKind::PluginExtension,
        policy: OperationPolicy {
            credentials: PLUGIN_CREDENTIALS,
            scope: "",
            risk: RiskLevel::Read,
            audit: AuditStatus::NotRequired,
            rate_limits: PLUGIN_RATE_LIMITS,
        },
    },
    Operation {
        method: "GET",
        path: PATH_ISSUE,
        contract: ContractKind::SharedResource,
        policy: OperationPolicy {
            credentials: SHARED_CREDENTIALS,
            scope: "issues:read",
            risk: RiskLevel::Read,
            audit: AuditStatus::Planned,
            rate_limits: SHARED_RATE_LIMITS,
        },
    },
    Operation {
        method: "PATCH",
        path: PATH_ISSUE,
        contract: ContractKind::SharedResource,
        policy: OperationPolicy {
            credentials: SHARED_CREDENTIALS,
            scope: "issues:write",
            risk: RiskLevel::ContentWrite,
            audit: AuditStatus::Planned,
            rate_limits: SHARED_RATE_LIMITS,
        },
    },
    Operation {
        method: "GET",
        path: PATH_ISSUE_COMMENTS,
        contract: ContractKind::SharedResource,
        policy: OperationPolicy {
            credentials: SHARED_CREDENTIALS,
            scope: "comments:read",
            risk: RiskLevel::Read,
            audit: AuditStatus::Planned,
            rate_limits: SHARED_RATE_LIMITS,
        },
    },
    Operation {
        method: "POST",
        path: PATH_ISSUE_COMMENTS,
        contract: ContractKind::SharedResource,
        policy: OperationPolicy {
            credentials: SHARED_CREDENTIALS,
            scope: "comments:write",
            risk: RiskLevel::ContentWrite,
            audit: AuditStatus::Planned,
            rate_limits: SHARED_RATE_LIMITS,
        },
    },
    Operation {
        method: "GET",
        path: PATH_STORAGE_SCOPE,
        contract: ContractKind::PluginExtension,
        policy: OperationPolicy {
            credentials: PLUGIN_CREDENTIALS,
            scope: "",
            risk: RiskLevel::Read,
            audit: AuditStatus::NotRequired,
            rate_limits: PLUGIN_RATE_LIMITS,
        },
    },
    Operation {
        method: "GET",
        path: PATH_STORAGE_VALUE,
        contract: ContractKind::PluginExtension,
        policy: OperationPolicy {
            credentials: PLUGIN_CREDENTIALS,
            scope: "",
            risk: RiskLevel::Read,
            audit: AuditStatus::NotRequired,
            rate_limits: PLUGIN_RATE_LIMITS,
        },
    },
    Operation {
        method: "PUT",
        path: PATH_STORAGE_VALUE,
        contract: ContractKind::PluginExtension,
        policy: OperationPolicy {
            credentials: PLUGIN_CREDENTIALS,
            scope: "",
            risk: RiskLevel::ContentWrite,
            audit: AuditStatus::Planned,
            rate_limits: PLUGIN_RATE_LIMITS,
        },
    },
    Operation {
        method: "DELETE",
        path: PATH_STORAGE_VALUE,
        contract: ContractKind::PluginExtension,
        policy: OperationPolicy {
            credentials: PLUGIN_CREDENTIALS,
            scope: "",
            risk: RiskLevel::ContentWrite,
            audit: AuditStatus::Planned,
            rate_limits: PLUGIN_RATE_LIMITS,
        },
    },
];

/// 按 `(method, path)` 取台账条目：方法大小写不敏感；`path` 可以带 `BASE_PATH` 前缀
/// （M6-7 的 route 表用完整路径）也可以不带（台账本身用相对路径）。
#[must_use]
pub fn operation_for(method: &str, path: &str) -> Option<&'static Operation> {
    let path = path.strip_prefix(BASE_PATH).unwrap_or(path);
    OPERATIONS
        .iter()
        .find(|operation| operation.method.eq_ignore_ascii_case(method) && operation.path == path)
}

/// 契约形态 → axum 0.7 形态：`{name}` ⇒ `:name`（**唯一转换点**；多数调用点直接用
/// [`Operation::axum_path`]）。
///
/// `{name}` 在 axum 的 matchit 里是**字面量**段：忘转换的后果是路径静默 404，不是编译错。
#[must_use]
pub fn axum_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    let mut rest = path;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        out.push(':');
        out.push_str(&rest[start + 1..start + end]);
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    out
}

/// 一个非法字段（上游 `FieldError`）。可选，好让端点逐步接入字段级校验而不改外层信封。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FieldError {
    pub field: String,
    pub code: String,
    pub message: String,
}

/// `/v1` 的稳定错误信封（上游 `Problem`，RFC 9457 风格 + Multica 的稳定 `code`）。
///
/// `error` 是既有 Plugin 客户端消费的兼容别名（= `detail`）；新客户端应当分支 `code`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProblemDetail {
    /// `urn:multica:problem:<code>`。
    #[serde(rename = "type")]
    pub problem_type: String,
    pub title: String,
    pub status: u16,
    pub code: String,
    pub detail: String,
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<FieldError>,
    /// 兼容别名，恒等于 `detail`。
    pub error: String,
}

/// 状态码 → 稳定错误码（上游 `CodeForStatus`）。
///
/// 与上游逐字一致：`422` 是 `incompatible`（**不是** `unprocessable_entity`）、`507` 是
/// `quota_exceeded`、`502` 是 `upstream_unavailable`；其余（含 `405`）落 `internal_error`。
#[must_use]
pub const fn code_for_status(status: u16) -> &'static str {
    match status {
        400 => "invalid_request",
        401 => "unauthorized",
        402 => "payment_required",
        403 => "forbidden",
        404 => "not_found",
        409 => "conflict",
        422 => "incompatible",
        429 => "rate_limited",
        507 => "quota_exceeded",
        503 => "service_unavailable",
        502 => "upstream_unavailable",
        _ => "internal_error",
    }
}

/// 状态码 → 标题（上游 `http.StatusText`）。
///
/// **只覆盖本契约能产生的状态码**（偏差 4）：未知状态码回 `Request failed`，
/// 而 Go 会给出该码的注册文案（例如 `418 ⇒ "I'm a teapot"`）。
#[must_use]
pub const fn status_title(status: u16) -> &'static str {
    match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        402 => "Payment Required",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        507 => "Insufficient Storage",
        _ => "Request failed",
    }
}

/// 构造问题响应（上游 `WriteProblem` 的**纯**部分）。
///
/// `code` 为空 ⇒ 按 [`code_for_status`] 取；`request_id` 由调用方从请求上下文/请求头取
/// （本仓的请求 id 中间件在 `mc-http`，本 crate 不引 `uuid`）。
/// 写响应时还要落 `Content-Type: application/problem+json` 与 `X-Request-Id` 头。
#[must_use]
pub fn problem_detail(status: u16, code: &str, detail: &str, request_id: &str) -> ProblemDetail {
    let code = if code.is_empty() {
        code_for_status(status)
    } else {
        code
    };
    ProblemDetail {
        problem_type: format!("urn:multica:problem:{code}"),
        title: status_title(status).to_string(),
        status,
        code: code.to_string(),
        detail: detail.to_string(),
        request_id: request_id.to_string(),
        errors: Vec::new(),
        error: detail.to_string(),
    }
}

/// `404` 的规范问题体（上游 `NotFound`）。
#[must_use]
pub fn not_found_problem(request_id: &str) -> ProblemDetail {
    problem_detail(404, "not_found", "resource not found", request_id)
}

/// `405` 的规范问题体（上游 `MethodNotAllowed`；注意 `code` 是显式给的，
/// 不走 [`code_for_status`]）。
#[must_use]
pub fn method_not_allowed_problem(request_id: &str) -> ProblemDetail {
    problem_detail(405, "method_not_allowed", "method not allowed", request_id)
}

/// 游标分页的公共元数据（上游 `PageInfo`）。游标**不透明**，且绑定当前 actor 与查询过滤。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageInfo {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub next_cursor: String,
}

/// 线格式 DTO（上游 `types.go` 整文件）。
///
/// 私有模块 + 显式再导出：`mc_openapi::v1::Issue` 这个公开路径不变，但「契约台账」与
/// 「线格式 DTO」两份东西各自一个文件（一个文件一个写者）。
mod dto;

pub use dto::{
    Comment, CommentListResponse, Context, ContextIssue, ContextUser, ContextWorkspace,
    CreateCommentRequest, Issue, PatchIssueRequest, PutStorageValueRequest, StorageKey,
    StorageKeyListResponse, StorageValueResponse,
};

#[cfg(test)]
mod tests;
