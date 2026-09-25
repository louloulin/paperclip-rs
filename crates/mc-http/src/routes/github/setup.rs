//! `GET /api/github/setup`（`router.go:1491`，**公开块**）—— 写者 **M8-1**。
//!
//! # 凭据：`GITHUB_WEBHOOK_SECRET` 签的 state（上游刻意与 webhook 复用同一个 secret）
//!
//! 格式（上游 `signStateForReturn` / `verifyStateWithReturn` 逐字）：
//!
//! ```text
//! <workspaceID>.<nonce>.<sigHex>                    # return_to = github（默认）
//! <workspaceID>.<returnTo>.<nonce>.<sigHex>         # return_to = repositories
//! ```
//!
//! `sigHex = hex(HMAC-SHA256(secret, "<workspaceID>[.<returnTo>].<nonce>"))`，`nonce` 是
//! 12 字节随机数的 hex（24 个字符）。校验**必须**常量时间（`hmac::Mac::verify_slice`），
//! 三个反例（改 workspace / 改 nonce / 改签名）都要失败。
//!
//! # 「未配置」语义（`docs/61` §2.5 的 setup 行）
//!
//! ⚠️ 计划书写的是「state 不合法 ⇒ 400/401」。**上游不是这样**：`GitHubSetupCallback` 在
//! **任何**失败分支上都回 **302**（GitHub 的安装流程会把用户**带回**控制台，回一个错误码
//! 页面会把人卡在 GitHub 那边），只是把错误放进 `&github_error=<kind>` 查询参数：
//!
//! | 情形 | `github_error` |
//! | --- | --- |
//! | `state` 缺失 | `missing_params` |
//! | state 验签失败（含 secret 未配置） | `invalid_state` |
//! | `installation_id` 缺失 | `missing_params` |
//! | `installation_id` 不是整数 | `bad_installation_id` |
//! | state 里的 workspace 不是 UUID | `bad_workspace` |
//! | 落库失败 / pending 消费失败 | `persist_failed` |
//! | 成功 | `github_connected=1` |
//!
//! 本片按**上游实况**实现并把这条差异登记进 `docs/32` §9.12（计划书那格作废）。
//!
//! # 与「能连接」判据的关系
//!
//! 这个端点是**公开**的（没有会话 middleware，也不做 workspace 角色判定）：身份来自 state 的
//! HMAC。`GITHUB_APP_ID` / `GITHUB_APP_PRIVATE_KEY` 没配时**仍然**建行（展示信息回落
//! `unknown` / `User`），下一条 `installation` webhook 会把真名写回来（上游注释逐字）。

use std::collections::HashMap;

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use mc_core::Id;
use mc_repos::github::installation::{GithubInstallationRepo, NewGithubInstallation};
use sha2::{Digest, Sha256};

use crate::routes::github::install::{github_api_base, installation_created_envelope};
use crate::state::AppState;
use mc_vcs_github::dto::GithubInstallationResponse;
use mc_vcs_github::rest::GithubClient;
use mc_vcs_github::AppJwtSigner;

/// 上游 `githubReturnToGitHub`。
pub const RETURN_TO_GITHUB: &str = "github";
/// 上游 `githubReturnToRepositories`。
pub const RETURN_TO_REPOSITORIES: &str = "repositories";

/// 上游 `GitHubSetupCallback` 里 `FRONTEND_ORIGIN` 的缺省值（逐字）。
pub const DEFAULT_FRONTEND_ORIGIN: &str = "http://localhost:3000";

/// state 的 12 字节随机 nonce（上游 `make([]byte, 12)`）。
const STATE_NONCE_BYTES: usize = 12;

/// state 的签发 / 校验失败（**不含**任何 secret 材料）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StateError {
    /// 没配 webhook secret ⇒ 签不出来（上游 `github integration is not configured`）。
    #[error("github integration is not configured")]
    NotConfigured,
    /// `return_to` 不在白名单里（上游 `invalid github return target`）。
    #[error("invalid github return target")]
    InvalidReturnTo,
}

/// 本文件的路由切片。
pub fn router() -> Router<std::sync::Arc<AppState>> {
    Router::new().route("/api/github/setup", get(setup_callback))
}

// ---------------------------------------------------------------------------
// state 的签发 / 校验（install.rs 的 connect 也用它）
// ---------------------------------------------------------------------------

/// 上游 `isAllowedGitHubReturnTo`。
pub fn is_allowed_return_to(return_to: &str) -> bool {
    return_to == RETURN_TO_GITHUB || return_to == RETURN_TO_REPOSITORIES
}

/// HMAC-SHA256（RFC 2104）的**手写**实现。
///
/// 为什么不用 `hmac` crate：`mc-http` 的 manifest **在 M8-0 anchor 之后冻结**
/// （`docs/61` §3.1 的写集纪律 —— M8 各代码片都**不得**改 manifest / `Cargo.lock`），
/// 而 `mc-http` 只有 `sha2`、没有 `hmac` ⇒ 按 RFC 2104 用 `sha2` 现拼一个
/// （ipad/opad 异或 + 两次消化，逐字 20 行）。正确性由 RFC 4231 的官方测试向量钉住
/// （见 `#[cfg(test)]` 的 `hmac_sha256_matches_rfc4231_vectors`）。
fn hmac_sha256(secret: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut key = [0u8; BLOCK];
    if secret.len() > BLOCK {
        key[..32].copy_from_slice(&Sha256::digest(secret));
    } else {
        key[..secret.len()].copy_from_slice(secret);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for index in 0..BLOCK {
        ipad[index] ^= key[index];
        opad[index] ^= key[index];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner);
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&outer.finalize());
    digest
}

/// 常量时间比较（两侧长度固定 32 字节）：**不**提前 return，累积全部差异。
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

/// 12 字节随机 nonce 的 hex（上游 `rand.Read` + `hex.EncodeToString`）。
fn random_nonce_hex() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; STATE_NONCE_BYTES];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// 上游 `signState(workspaceID)`（`return_to = github`）。
///
/// # Errors
///
/// secret 为空 ⇒ [`StateError::NotConfigured`]；`return_to` 非法 ⇒
/// [`StateError::InvalidReturnTo`]。
pub fn sign_state(secret: &str, workspace_id: &str) -> Result<String, StateError> {
    sign_state_for_return(secret, workspace_id, RETURN_TO_GITHUB)
}

/// 上游 `signStateForReturn`：按 `return_to` 决定 3 段 / 4 段形态。
///
/// # Errors
///
/// 同 [`sign_state`]。
pub fn sign_state_for_return(
    secret: &str,
    workspace_id: &str,
    return_to: &str,
) -> Result<String, StateError> {
    sign_state_with_nonce(secret, workspace_id, return_to, &random_nonce_hex())
}

/// 上游 `signStateForReturn` 的**确定性**形态（nonce 由调用方给）—— 测试用它钉住字节，
/// 生产走 [`sign_state_for_return`]。
///
/// # Errors
///
/// 同 [`sign_state`]。
pub fn sign_state_with_nonce(
    secret: &str,
    workspace_id: &str,
    return_to: &str,
    nonce: &str,
) -> Result<String, StateError> {
    if secret.is_empty() {
        return Err(StateError::NotConfigured);
    }
    if !is_allowed_return_to(return_to) {
        return Err(StateError::InvalidReturnTo);
    }
    let payload = if return_to == RETURN_TO_GITHUB {
        format!("{workspace_id}.{nonce}")
    } else {
        format!("{workspace_id}.{return_to}.{nonce}")
    };
    let signature = hex::encode(hmac_sha256(secret.as_bytes(), payload.as_bytes()));
    Ok(format!("{payload}.{signature}"))
}

/// 上游 `verifyState`：只关心 workspace（`return_to` 被丢弃）。
pub fn verify_state(secret: &str, token: &str) -> Option<String> {
    verify_state_with_return(secret, token).map(|(workspace_id, _)| workspace_id)
}

/// 上游 `verifyStateWithReturn`：常量时间验签，返回 `(workspace_id, return_to)`。
///
/// 逐条对齐上游：secret 为空 ⇒ `None`；段数不是 3/4 ⇒ `None`；4 段形态里 `return_to` 不合法
/// ⇒ `None`；签名不匹配 ⇒ `None`。
pub fn verify_state_with_return(secret: &str, token: &str) -> Option<(String, String)> {
    if secret.is_empty() {
        return None;
    }
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 && parts.len() != 4 {
        return None;
    }
    let workspace_id = parts[0].to_string();
    let (return_to, nonce_index) = if parts.len() == 4 {
        let return_to = parts[1];
        if !is_allowed_return_to(return_to) {
            return None;
        }
        (return_to.to_string(), 2)
    } else {
        (RETURN_TO_GITHUB.to_string(), 1)
    };
    let signature_hex = parts[nonce_index + 1];
    let signature = hex::decode(signature_hex).ok()?;
    let payload = parts[..=nonce_index].join(".");
    let expected = hmac_sha256(secret.as_bytes(), payload.as_bytes());
    if !constant_time_eq(&expected, &signature) {
        return None;
    }
    Some((workspace_id, return_to))
}

/// 上游 `githubSettingsURL`：`{frontend}/settings?tab={returnTo}`（frontend 去尾斜杠，
/// `returnTo` 非法时回落 `github`）。
pub fn github_settings_url(frontend: &str, return_to: &str) -> String {
    let return_to = if is_allowed_return_to(return_to) {
        return_to
    } else {
        RETURN_TO_GITHUB
    };
    format!(
        "{}/settings?tab={}",
        frontend.trim_end_matches('/'),
        percent_encode_query(return_to)
    )
}

/// `return_to` / `frontend` 的 query 编码（只处理本文件用到的两个字面量与一个 URL）。
fn percent_encode_query(value: &str) -> String {
    crate::routes::github::install::percent_encode_query_value(value)
}

/// 上游 `strings.TrimSpace(os.Getenv("FRONTEND_ORIGIN"))` + 空串回落
/// [`DEFAULT_FRONTEND_ORIGIN`]。
///
/// ⚠️ 这是本文件**唯一**读 env 的地方（`docs/61` §3.1 的纪律：部署**密钥**的唯一读取口是
/// `AppState` 的 `github_keys`；`FRONTEND_ORIGIN` 不是密钥，且 anchor 没有把它放进
/// `AppState`，所以照上游在调用点读）。
pub fn frontend_origin_from_env() -> String {
    frontend_origin_from(|| std::env::var("FRONTEND_ORIGIN").ok())
}

/// `FRONTEND_ORIGIN` 的纯函数形态（测试注入）。
pub fn frontend_origin_from<F>(get: F) -> String
where
    F: Fn() -> Option<String>,
{
    get()
        .map(|raw| raw.trim().to_string())
        .filter(|trimmed| !trimmed.is_empty())
        .unwrap_or_else(|| DEFAULT_FRONTEND_ORIGIN.to_string())
}

// ---------------------------------------------------------------------------
// GET /api/github/setup
// ---------------------------------------------------------------------------

/// 上游 `GitHubSetupCallback`（`github.go:501`）。
///
/// **公开**：不挂 `AuthUser` 提取器（缺会话也必须能走完 —— GitHub 的浏览器跳转不带我们的
/// 会话 cookie）。`X-Multica-User-Id` 只用来**尽力**记录 `connected_by`，缺失不是错误。
async fn setup_callback(
    State(state): State<std::sync::Arc<AppState>>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let frontend = frontend_origin_from_env();
    let mut settings_url = github_settings_url(&frontend, RETURN_TO_GITHUB);
    let secret = state.github_keys.webhook_secret.clone().unwrap_or_default();

    // ① state 缺失 / 验签失败。
    let Some(token) = non_empty(query.get("state")) else {
        return redirect(&format!("{settings_url}&github_error=missing_params"));
    };
    let Some((workspace_id, return_to)) = verify_state_with_return(&secret, token) else {
        return redirect(&format!("{settings_url}&github_error=invalid_state"));
    };
    // 验签通过后 settingsURL 改由 state 里的 return_to 决定（上游逐字）。
    settings_url = github_settings_url(&frontend, &return_to);

    // ② installation_id 缺失 / 非整数 / workspace 非 UUID。
    let Some(installation_id_raw) = non_empty(query.get("installation_id")) else {
        return redirect(&format!("{settings_url}&github_error=missing_params"));
    };
    let Ok(installation_id) = installation_id_raw.trim().parse::<i64>() else {
        return redirect(&format!("{settings_url}&github_error=bad_installation_id"));
    };
    let Ok(workspace_uuid) = Id::parse(workspace_id.trim()) else {
        return redirect(&format!("{settings_url}&github_error=bad_workspace"));
    };

    // ③ 向 GitHub 解析展示信息（**永不失败**：拿不到就留 `unknown` / `User` 占位）。
    let account = fetch_installation_account(&state, installation_id).await;

    // ④ 尽力记录连接者（公开回调里可能没有会话 —— 上游同判）。
    let connected_by = optional_user_id(&headers);

    let repo = GithubInstallationRepo::new(state.db.clone());
    let installed = match repo
        .upsert(NewGithubInstallation {
            workspace_id: workspace_uuid,
            installation_id,
            account_login: account.login,
            account_type: account.account_type,
            account_avatar_url: account.avatar_url,
            connected_by_id: connected_by,
        })
        .await
    {
        Ok(installed) => installed,
        Err(error) => {
            // 只记 `RepoError` 的 Display（含约束名，**不含**任何密钥材料）。
            tracing::error!(%error, "github: failed to persist installation");
            return redirect(&format!("{settings_url}&github_error=persist_failed"));
        }
    };

    // ⑤ 消费 webhook 早到留下的 pending 行（上游 `consumePendingGitHubInstallation`）。
    let installed = match repo.get_pending(installation_id).await {
        Ok(Some(pending)) => {
            if let Ok(refreshed) = repo
                .upsert(NewGithubInstallation {
                    workspace_id: workspace_uuid,
                    installation_id,
                    account_login: pending.account_login,
                    account_type: pending.account_type,
                    account_avatar_url: pending.account_avatar_url,
                    connected_by_id: installed.connected_by_id.map(Id),
                })
                .await
            {
                if let Ok(()) = repo.delete_pending(installation_id).await {
                    refreshed
                } else {
                    tracing::error!("github: failed to clear pending installation");
                    return redirect(&format!("{settings_url}&github_error=persist_failed"));
                }
            } else {
                tracing::error!("github: failed to apply pending installation metadata");
                return redirect(&format!("{settings_url}&github_error=persist_failed"));
            }
        }
        Ok(None) => installed,
        Err(_) => {
            tracing::error!("github: failed to read pending installation");
            return redirect(&format!("{settings_url}&github_error=persist_failed"));
        }
    };

    // ⑥ 广播（**最弱角色视图**：不带 `installation_id`）。
    let broadcast = GithubInstallationResponse::from_row(&installed).without_installation_id();
    state.realtime.publish(installation_created_envelope(
        workspace_id.trim(),
        &broadcast,
    ));

    redirect(&format!("{settings_url}&github_connected=1"))
}

/// 上游 `fetchInstallationAccount`：能签 App JWT 就带 `Authorization`，否则裸跑；失败回落占位。
async fn fetch_installation_account(
    state: &AppState,
    installation_id: i64,
) -> mc_vcs_github::rest::InstallationAccount {
    let keys = &state.github_keys;
    let app_jwt = if keys.is_app_configured() {
        let app_id = keys.app_id.clone().unwrap_or_default();
        let pem = keys.private_key_pem.clone().unwrap_or_default();
        match AppJwtSigner::from_pem(app_id, &pem)
            .map_err(|e| e.to_string())
            .and_then(|signer| signer.sign_app_jwt(now_unix()).map_err(|e| e.to_string()))
        {
            Ok(token) => Some(token),
            Err(e) => {
                // 私钥配错是运维可行动的错误 —— 留一条面包屑（**不含**密钥材料）。
                tracing::warn!(error = %e, "github: sign App JWT failed");
                None
            }
        }
    } else {
        None
    };
    GithubClient::new(github_api_base())
        .fetch_installation_account(app_jwt.as_deref(), installation_id)
        .await
}

/// `X-Multica-User-Id` 的**可选**读取（公开路由不能用 `AuthUser` 提取器：它会 401）。
fn optional_user_id(headers: &HeaderMap) -> Option<Id> {
    headers
        .get(crate::routes::auth_user::USER_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|raw| Id::parse(raw.trim()).ok())
}

/// `q.Get(name)` 的非空语义（trim 之后为空 = 没传）。
fn non_empty(value: Option<&String>) -> Option<&str> {
    value
        .map(String::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

/// 302（上游 `http.Redirect(..., http.StatusFound)`）。
///
/// ⚠️ 不能用 `axum::response::Redirect::to`：它发的是 **303**（See Other），而上游逐字是
/// **302 Found**。GitHub 的浏览器跳转链路对两者都能走，但契约等价门与前端行为快照看的是
/// 状态码本身，所以这里手写 `(302, Location)`。
fn redirect(location: &str) -> Response {
    (
        axum::http::StatusCode::FOUND,
        [(axum::http::header::LOCATION, location)],
    )
        .into_response()
}

/// 当前 Unix 秒。
fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "webhook-secret";

    #[test]
    fn hmac_sha256_matches_rfc4231_vectors() {
        // RFC 4231 §4.2（Test Case 1）：key = 20 × 0x0b，data = "Hi There"。
        let case1 = hmac_sha256(&[0x0b; 20], b"Hi There");
        assert_eq!(
            hex::encode(case1),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        // RFC 4231 §4.3（Test Case 2）：key = "Jefe"，data = "what do ya want for nothing?"。
        let case2 = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            hex::encode(case2),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // 密钥长于 block（64B）时先摘要（RFC 2104 的第一条分支）。
        let long = hmac_sha256(
            &[0xaa; 131],
            b"Test Using Larger Than Block-Size Key - Hash Key First",
        );
        assert_eq!(
            hex::encode(long),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
        assert!(constant_time_eq(&[1, 2, 3], &[1, 2, 3]));
        assert!(!constant_time_eq(&[1, 2, 3], &[1, 2, 4]));
        assert!(!constant_time_eq(&[1, 2], &[1, 2, 3]));
    }

    #[test]
    fn state_round_trips_and_binds_workspace() {
        let workspace = "8f14e45f-ceea-467e-b1d2-5a4b1b1b1b1b";
        let token = sign_state_for_return(SECRET, workspace, RETURN_TO_GITHUB).expect("sign");
        assert_eq!(token.split('.').count(), 3, "默认形态是 3 段");
        assert_eq!(
            verify_state_with_return(SECRET, &token),
            Some((workspace.to_string(), RETURN_TO_GITHUB.to_string()))
        );
        assert_eq!(verify_state(SECRET, &token).as_deref(), Some(workspace));

        let repo_token =
            sign_state_for_return(SECRET, workspace, RETURN_TO_REPOSITORIES).expect("sign");
        assert_eq!(repo_token.split('.').count(), 4, "repositories 形态是 4 段");
        assert_eq!(
            verify_state_with_return(SECRET, &repo_token),
            Some((workspace.to_string(), RETURN_TO_REPOSITORIES.to_string()))
        );
    }

    #[test]
    fn state_signature_is_constant_time_verified_and_tamper_proof() {
        let workspace = "8f14e45f-ceea-467e-b1d2-5a4b1b1b1b1b";
        let nonce = "00112233445566778899aabb";
        let token =
            sign_state_with_nonce(SECRET, workspace, RETURN_TO_GITHUB, nonce).expect("sign");
        // 钉住逐字节形态：`<ws>.<nonce>.<hex(hmac)>`。
        let expected_sig = hex::encode(hmac_sha256(
            SECRET.as_bytes(),
            format!("{workspace}.{nonce}").as_bytes(),
        ));
        assert_eq!(token, format!("{workspace}.{nonce}.{expected_sig}"));

        // 反例一：换 workspace。
        let moved = token.replace(workspace, "00000000-0000-0000-0000-000000000000");
        assert!(verify_state_with_return(SECRET, &moved).is_none());
        // 反例二：换 nonce。
        let renonced = token.replace(nonce, "ffffffffffffffffffffffff");
        assert!(verify_state_with_return(SECRET, &renonced).is_none());
        // 反例三：签名差 1 位。
        let mut sig = expected_sig.clone();
        let last = sig.pop().unwrap();
        sig.push(if last == '0' { '1' } else { '0' });
        assert!(verify_state_with_return(SECRET, &format!("{workspace}.{nonce}.{sig}")).is_none());
        // 反例四：错 secret。
        assert!(verify_state_with_return("other", &token).is_none());
        // 反例五：段数不对 / 签名不是 hex。
        assert!(verify_state_with_return(SECRET, "a.b").is_none());
        assert!(verify_state_with_return(SECRET, "a.b.c.d.e").is_none());
        assert!(verify_state_with_return(SECRET, &format!("{workspace}.{nonce}.zz")).is_none());
        // 反例六：4 段形态里 return_to 不合法。
        let bad_return = format!("{workspace}.evil.{nonce}.{expected_sig}");
        assert!(verify_state_with_return(SECRET, &bad_return).is_none());
    }

    #[test]
    fn missing_secret_signs_and_verifies_nothing() {
        assert_eq!(
            sign_state("", "ws"),
            Err(StateError::NotConfigured),
            "secret 缺失 ⇒ 签不出来（上游 errors.New）"
        );
        assert_eq!(
            sign_state_for_return(SECRET, "ws", "evil"),
            Err(StateError::InvalidReturnTo)
        );
        assert!(verify_state_with_return("", "a.b.c").is_none());
        assert!(verify_state("", "a.b.c").is_none());
        assert_eq!(
            StateError::NotConfigured.to_string(),
            "github integration is not configured"
        );
        assert_eq!(
            StateError::InvalidReturnTo.to_string(),
            "invalid github return target"
        );
    }

    #[test]
    fn initial_three_segment_state_is_accepted_as_github_return() {
        // 上游 `signState` 的 3 段形态在 `verifyStateWithReturn` 里被读回 `github`。
        let token = sign_state(SECRET, "ws").expect("sign");
        assert_eq!(
            verify_state_with_return(SECRET, &token),
            Some(("ws".to_string(), RETURN_TO_GITHUB.to_string()))
        );
    }

    #[test]
    fn settings_url_uses_the_canonical_tab_and_trims_slashes() {
        assert_eq!(
            github_settings_url("http://localhost:3000", RETURN_TO_GITHUB),
            "http://localhost:3000/settings?tab=github"
        );
        assert_eq!(
            github_settings_url("http://localhost:3000/", RETURN_TO_REPOSITORIES),
            "http://localhost:3000/settings?tab=repositories"
        );
        assert_eq!(
            github_settings_url("http://x", "evil"),
            "http://x/settings?tab=github",
            "非法 return_to 回落 github（上游 `githubSettingsURL`）"
        );
    }

    #[test]
    fn frontend_origin_defaults_and_trims() {
        assert_eq!(frontend_origin_from(|| None), DEFAULT_FRONTEND_ORIGIN);
        assert_eq!(
            frontend_origin_from(|| Some("   ".into())),
            DEFAULT_FRONTEND_ORIGIN
        );
        assert_eq!(
            frontend_origin_from(|| Some("  https://app.example  ".into())),
            "https://app.example"
        );
    }

    #[test]
    fn non_empty_trims_and_rejects_blank_query_values() {
        assert_eq!(non_empty(None), None);
        assert_eq!(non_empty(Some(&String::new())), None);
        assert_eq!(non_empty(Some(&"   ".to_string())), None);
        assert_eq!(non_empty(Some(&" 42 ".to_string())), Some("42"));
    }
}
