//! daemon ws 连接身份 —— 上游 `server/internal/daemonws/hub.go` L23–L151 的
//! `ClientIdentity` 逐字冻结。
//!
//! 身份**由调用方注入**：hub 不认识 token、header、JWT 或任何鉴权面，只接收一个已经
//! 解析好的 [`ClientIdentity`]。上游同理 —— `HandleWebSocket(w, r, identity)` 的三个入参
//! 里，`identity` 由 HTTP handler（`handler/daemon.go`）在握手前构造。
//!
//! 因此本模块只有纯函数：工作区 scope 归一化（[`ClientIdentity::authorized_workspace_ids`]）、
//! runtime 集合归一化（[`ClientIdentity::runtime_set`]）与握手校验
//! （[`ClientIdentity::validate`]，上游 `hub.go:940–L942` 的 400 条件）。
//!
//! # 本模块**不**做的事
//!
//! 不查库、不解析凭据、不判断「这个 daemon 是否真的拥有这个 runtime」（那是
//! `RuntimeLeases` 的 liveness 查询，属于 M3-7 的握手切片），也不做任何授权缓存。

use std::collections::HashSet;

/// 上游 `hub.go:23` `ClientIdentity`。
///
/// `runtime_ids` 与 `user_id` 至少有一个非空（[`ClientIdentity::validate`]）；其余字段
/// 只用于日志与索引。上游的 `RuntimeLeases` / `RuntimeTokenExpiresAt` 属于**连接期授权
/// 结果**（要查库），本 crate 不持有，见 `docs/38-M3-WS-TRANSPORT.md` §偏差。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClientIdentity {
    /// 上游 `DaemonID`：日志/审计用，不参与索引。
    pub daemon_id: String,
    /// 上游 `UserID`：`by_user` 索引键（多租户 daemon 的 `daemon:workspaces_changed` 面）。
    pub user_id: String,
    /// 上游 `WorkspaceID`：**遗留的单工作区**字段，仅在 `workspace_ids` 为空时生效。
    pub workspace_id: String,
    /// 上游 `WorkspaceIDs`：多工作区 scope（优先）。
    pub workspace_ids: Vec<String>,
    /// 上游 `RuntimeIDs`：本连接被授权的 runtime 集合（`by_runtime` 索引键）。
    pub runtime_ids: Vec<String>,
    /// 上游 `ClientVersion`：日志用。
    pub client_version: String,
    /// 上游 `Capabilities`：日志用（能力协商的**唯一**真值在心跳 ack 的
    /// `server_capabilities`，不是这里）。
    pub capabilities: String,
}

impl ClientIdentity {
    /// 空身份（`validate` 会拒）。等价于 `ClientIdentity::default()`，给调用方一个
    /// 显式构造点。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 上游 `hub.go:102` `AuthorizedWorkspaceIDs`：多工作区字段优先，为空时回退单工作区
    /// 字段；逐项 trim、去空、去重、**保持出现顺序**。
    #[must_use]
    pub fn authorized_workspace_ids(&self) -> Vec<String> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut out: Vec<String> = Vec::new();
        for raw in &self.workspace_ids {
            let id = raw.trim();
            if id.is_empty() {
                continue;
            }
            if seen.insert(id.to_owned()) {
                out.push(id.to_owned());
            }
        }
        if out.is_empty() {
            let id = self.workspace_id.trim();
            if !id.is_empty() && seen.insert(id.to_owned()) {
                out.push(id.to_owned());
            }
        }
        out
    }

    /// 上游 `hub.go:126` `PrimaryWorkspaceID`：scope 首项，空 scope 返回 `""`。
    #[must_use]
    pub fn primary_workspace_id(&self) -> String {
        self.authorized_workspace_ids()
            .into_iter()
            .next()
            .unwrap_or_default()
    }

    /// 上游 `hub.go:134` `AllowsWorkspace`：**空 scope 一律放行**（遗留调用方直接构造
    /// identity 时没有工作区字段，收紧会误杀）。
    #[must_use]
    pub fn allows_workspace(&self, workspace_id: &str) -> bool {
        let ids = self.authorized_workspace_ids();
        if ids.is_empty() {
            return true;
        }
        ids.iter().any(|id| id == workspace_id)
    }

    /// 连接的 runtime 集合（`hub.go:944` 的 `runtimes` map）：跳空串，其余原样（上游不
    /// trim runtime id）。
    #[must_use]
    pub fn runtime_set(&self) -> HashSet<String> {
        self.runtime_ids
            .iter()
            .filter(|id| !id.is_empty())
            .cloned()
            .collect()
    }

    /// 上游 `hub.go:940`：`runtime_ids` 与 `user_id` **都为空** → 400。
    ///
    /// 判定用的是**原始长度**而不是 [`ClientIdentity::runtime_set`] 的去空结果 —— 上游
    /// 同样如此（全空白 runtime id 也算「声明了 runtime」）。
    ///
    /// # Errors
    ///
    /// 连接既无 runtime 也无用户身份时返回 [`IdentityError::MissingScope`]。
    pub fn validate(&self) -> Result<(), IdentityError> {
        if self.runtime_ids.is_empty() && self.user_id.is_empty() {
            return Err(IdentityError::MissingScope);
        }
        Ok(())
    }
}

/// 握手阶段的身份拒绝原因（上游 `hub.go:941` 只有一种）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IdentityError {
    /// 「既没有 `runtime_ids` 也没有用户身份」—— 上游回 400，
    /// 错误对象 `{"error":"runtime_ids or user identity required"}`。
    #[error("runtime_ids or user identity required")]
    MissingScope,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(workspace_ids: &[&str], workspace_id: &str) -> ClientIdentity {
        ClientIdentity {
            workspace_ids: workspace_ids.iter().map(|s| (*s).to_owned()).collect(),
            workspace_id: workspace_id.to_owned(),
            ..ClientIdentity::default()
        }
    }

    #[test]
    fn workspace_scope_prefers_list_and_dedups_in_order() {
        let i = identity(&[" b ", "a", "b", "", "c"], "legacy");
        assert_eq!(i.authorized_workspace_ids(), vec!["b", "a", "c"]);
        assert_eq!(i.primary_workspace_id(), "b");
        assert!(i.allows_workspace("a"));
        assert!(!i.allows_workspace("legacy"));
        assert!(!i.allows_workspace("z"));
    }

    #[test]
    fn workspace_scope_falls_back_to_legacy_field() {
        let i = identity(&[], " legacy ");
        assert_eq!(i.authorized_workspace_ids(), vec!["legacy"]);
        assert!(i.allows_workspace("legacy"));
    }

    #[test]
    fn empty_workspace_scope_stays_permissive() {
        let i = identity(&[], "  ");
        assert!(i.authorized_workspace_ids().is_empty());
        assert!(i.allows_workspace("anything"));
    }

    #[test]
    fn runtime_set_skips_blank_ids() {
        let i = ClientIdentity {
            runtime_ids: vec!["r1".to_owned(), String::new(), "r2".to_owned()],
            ..ClientIdentity::default()
        };
        assert_eq!(i.runtime_set().len(), 2);
        assert!(i.runtime_set().contains("r1"));
    }

    #[test]
    fn validate_requires_runtime_or_user() {
        assert_eq!(
            ClientIdentity::default().validate(),
            Err(IdentityError::MissingScope)
        );
        let by_runtime = ClientIdentity {
            runtime_ids: vec!["r1".to_owned()],
            ..ClientIdentity::default()
        };
        assert!(by_runtime.validate().is_ok());
        let by_user = ClientIdentity {
            user_id: "u1".to_owned(),
            ..ClientIdentity::default()
        };
        assert!(by_user.validate().is_ok());
        // 上游按**原始长度**判定：全空白的 runtime id 也算声明了 runtime。
        let blank = ClientIdentity {
            runtime_ids: vec![String::new()],
            ..ClientIdentity::default()
        };
        assert!(blank.validate().is_ok());
        assert!(blank.runtime_set().is_empty());
    }
}
