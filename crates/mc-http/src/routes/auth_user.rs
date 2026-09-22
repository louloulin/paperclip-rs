//! 鉴权提取器：当前用户的最小表示。
//!
//! 文件命名：`auth_user.rs`（而不是 `auth.rs`）——避免与 M1 sub-issue B 实现的
//! `/api/auth/login` 等路由文件冲突。
//!
//! M1 阶段策略：
//! - `AuthUser` 从 `X-Multica-User-Id` header 提取 `Id`
//! - 缺失 header → 401 unauthorized
//! - sub-issue B 完成 session / cookie 中间件后，会替换或包装这个提取器
//!
//! 设计目的：让 M1 的 invitation / PAT handler 在没有完整 auth 链的情况下
//! 仍能完成端到端测试（test 中显式带 `X-Multica-User-Id` 即可）。
//!
//! 注意：当前实现**不**校验 header 与数据库中 user 的对应关系——只信任 header。
//! 这与 upstream multica 的 dev-mode 行为一致；生产模式由 sub-issue B 接管。

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::header::HeaderName;

use mc_core::Id;
use mc_errors::Error;

use crate::error::ApiError;

pub const USER_ID_HEADER: HeaderName = HeaderName::from_static("x-multica-user-id");

/// 当前已登录用户（M1 dev-mode）。
#[derive(Debug, Clone, Copy)]
pub struct AuthUser {
    id: Id,
}

impl AuthUser {
    pub fn id(self) -> Id {
        self.id
    }
}

impl<S> FromRequestParts<S> for AuthUser
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let header_value = parts
            .headers
            .get(&USER_ID_HEADER)
            .ok_or_else(|| ApiError(Error::Unauthorized {
                message: "missing X-Multica-User-Id header (M1 dev-mode auth)".into(),
            }))?;
        let raw = header_value.to_str().map_err(|_| {
            ApiError(Error::Unauthorized {
                message: "invalid X-Multica-User-Id header (non-ascii)".into(),
            })
        })?;
        let id = Id::parse(raw.trim()).map_err(|_| {
            ApiError(Error::Unauthorized {
                message: "invalid X-Multica-User-Id header (not a uuid)".into(),
            })
        })?;
        Ok(Self { id })
    }
}

/// `AuthUser::from_id`：测试 helper。
#[allow(dead_code)]
impl AuthUser {
    pub fn from_id(id: Id) -> Self {
        Self { id }
    }
}
