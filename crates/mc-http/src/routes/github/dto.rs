//! GitHub 面的响应 DTO 再导出 —— 写者 **M8-1**，**M8-4 只读**。
//!
//! 实现与形状在 `mc_vcs_github::dto`（`docs/61` §1.2：`githubInstallationToResponse` /
//! `githubPullRequestToResponse` 落在 M8-1 的写集，M8-4 只读）。
//! 本文件只是 HTTP 层的**再导出面**，避免两个写者各起一份映射。
//!
//! M8-1 相对 anchor 暂定形状的三处修订（逐条在 `mc_vcs_github::dto` 的模块头与
//! `docs/32` §9.12 里）：`connect` 响应改 `url`、installation 响应改
//! `workspace_id` + `Option<i64>`、repository 响应改上游八字段 + 分页信封。
pub use mc_vcs_github::dto::{
    GithubConnectResponse, GithubInstallationResponse, GithubInstallationsResponse,
    GithubPageParam, GithubPageParamError, GithubPullRequestResponse, GithubRepositoriesResponse,
    GithubRepositoryResponse,
};
