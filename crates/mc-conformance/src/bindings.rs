//! 绑定表：把 fixture 里的 `$symbol` 变成这次回放用的真值。
//!
//! 从 `lib.rs` 拆出来的原因不是「好看」而是门 ⑩：`lib.rs` 已在
//! `scripts/file_size_baseline.tsv` 里（1024 行），而基线里的文件**只允许变短**。
//! 本片（`LUM-2494`）要给 `Bindings` 加域符号字段，唯一的合规做法是把整块搬走
//! 而不是就地加几行 —— 就地加会让门 ⑩ 直接红，而改基线是本片明令禁止的。
//!
//! 与 [`crate::seed`] 的分工：`Bindings` 持有**身份**（user / workspace / `mdt_`），
//! [`crate::seed::Seed`] 持有**实体行**（agent / issue / chat session / task）。
//! 两者合成一次回放能发出的全部符号；`lookup` 是它们唯一的汇合点。

use uuid::Uuid;

use crate::{daemon_token, seed, STATELESS_USER_ID, STATELESS_WORKSPACE_ID};

/// 一次回放用的身份。抽取器只声明 `BINDABLE` 那几类可绑定语义：
/// 除它们以外的符号已在抽取期被 skip 成 `value_unresolved`，所以这里的
/// `resolve` 对未知符号直接报错而不是猜一个值。
///
/// 🔴 `daemon_token` 是**明文凭据**且只在内存里：`Debug` 手写脱敏，报告侧
/// （`Report::from_rows`）只登记 `user_id` / `workspace_id`，连字段都不给它。
/// 见 [`daemon_token`] 模块文档的「明文只活在内存里」。
#[derive(Clone)]
pub struct Bindings {
    pub user_id: Uuid,
    pub workspace_id: Uuid,
    /// database 层现场签发并登记的 `mdt_` 明文（`None` = 没有 daemon 身份，
    /// stateless 层就是这种：它连库都没有，签不出来也用不上）。
    daemon_token: Option<String>,
    /// database 层用**真实路由**种出来的那几行（见 [`seed`]）。`None` = stateless 层。
    seed: Option<seed::Seed>,
}

impl std::fmt::Debug for Bindings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bindings")
            .field("user_id", &self.user_id)
            .field("workspace_id", &self.workspace_id)
            .field(
                "daemon_token",
                &self
                    .daemon_token
                    .as_ref()
                    .map(|_| format!("{}<redacted>", daemon_token::DAEMON_TOKEN_PREFIX)),
            )
            // 种子行不是凭据，Debug 里逐字打出来：排查「这条 fixture 拿到的是哪一行」
            // 时能直接看见，而不必先把 report 跑一遍。（`report.json` 不走 Debug。）
            .field("seed", &self.seed)
            .finish()
    }
}

impl Bindings {
    #[must_use]
    pub fn stateless() -> Self {
        Self {
            user_id: Uuid::from_u128(STATELESS_USER_ID),
            workspace_id: Uuid::from_u128(STATELESS_WORKSPACE_ID),
            daemon_token: None,
            seed: None,
        }
    }

    #[must_use]
    pub fn new(user_id: Uuid, workspace_id: Uuid) -> Self {
        Self {
            user_id,
            workspace_id,
            daemon_token: None,
            seed: None,
        }
    }

    /// database 层的绑定：带一枚**已登记**的 `mdt_` 明文。
    #[must_use]
    pub fn with_daemon_token(user_id: Uuid, workspace_id: Uuid, token: String) -> Self {
        Self {
            user_id,
            workspace_id,
            daemon_token: Some(token),
            seed: None,
        }
    }

    /// database 层的绑定：带 `mdt_` 明文**和**已种下的实体行。
    ///
    /// 🔴 这是**新增**构造器而不是改 [`Bindings::with_daemon_token`] 的签名：后者
    /// 已经有 21 个调用点（与 `mc_http::AppState::new` 同一条纪律），而「多一个参数」
    /// 这种改法会把一个**签名形状**变成一次跨仓改动。形状变了就新开一个构造器。
    #[must_use]
    pub fn with_seeded(user_id: Uuid, workspace_id: Uuid, token: String, seed: seed::Seed) -> Self {
        Self {
            user_id,
            workspace_id,
            daemon_token: Some(token),
            seed: Some(seed),
        }
    }

    /// 这次回放能不能施加 daemon 身份。
    #[must_use]
    pub fn daemon_token(&self) -> Option<&str> {
        self.daemon_token.as_deref()
    }

    /// 符号 → 真值。**唯一**的汇合点：身份两类，实体行四类。
    ///
    /// 🔴 未知符号返回 `None`（由 `resolve` 变成 `unbound symbol` 错误）而不是
    /// 猜一个值：猜出来的 UUID 会让请求落在一个不存在的行上，症状是 `404`，
    /// 而 `404` 与「这条 fixture 本来就不该过」在报告里**长得一样**。
    fn lookup(&self, sym: &str) -> Option<String> {
        let id = match sym {
            "$testUserID" => Some(self.user_id),
            "$testWorkspaceID" => Some(self.workspace_id),
            _ => self.seed.as_ref().and_then(|s| s.get(sym)),
        }?;
        Some(id.to_string())
    }

    /// 解析一个 fixture 里的取值：`$symbol` 走绑定表，其余当字面量。
    pub fn resolve(&self, raw: &str) -> Result<String, String> {
        let trimmed = raw.trim();
        if trimmed.starts_with('$') {
            return self
                .lookup(trimmed)
                .ok_or_else(|| format!("unbound symbol {trimmed}"));
        }
        Ok(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_leaves_literals_alone_and_refuses_unknown_symbols() {
        let b = Bindings::stateless();
        assert_eq!(b.resolve("  hello ").unwrap(), "hello");
        assert_eq!(b.resolve("$testUserID").unwrap(), b.user_id.to_string());
        assert_eq!(
            b.resolve("$testWorkspaceID").unwrap(),
            b.workspace_id.to_string()
        );
        // stateless 层没有实体行：四个域符号必须**报错**而不是给一个编出来的 UUID。
        for sym in seed::Seed::SYMBOLS {
            let err = b.resolve(sym).expect_err("stateless 层不该供得起实体行");
            assert!(err.contains("unbound symbol"), "{sym}: {err}");
        }
        // 完全未知的符号同样报错 —— 这是「抽取器与回放器必须同步」的承重断言。
        assert!(b.resolve("$testProjectID").is_err());
    }

    #[test]
    fn seeded_bindings_resolve_every_entity_symbol() {
        let b = Bindings::with_seeded(
            Uuid::from_u128(7),
            Uuid::from_u128(8),
            "mdt_secret".into(),
            seed::Seed {
                agent: Some(Uuid::from_u128(9)),
                issue: Some(Uuid::from_u128(10)),
                chat_session: Some(Uuid::from_u128(11)),
                task: Some(Uuid::from_u128(12)),
            },
        );
        assert_eq!(
            b.resolve("$testAgentID").unwrap(),
            Uuid::from_u128(9).to_string()
        );
        assert_eq!(
            b.resolve("$testIssueID").unwrap(),
            Uuid::from_u128(10).to_string()
        );
        assert_eq!(
            b.resolve("$testChatSessionID").unwrap(),
            Uuid::from_u128(11).to_string()
        );
        assert_eq!(
            b.resolve("$testTaskID").unwrap(),
            Uuid::from_u128(12).to_string()
        );
        // 身份类符号与实体行互不干扰。
        assert_eq!(
            b.resolve("$testUserID").unwrap(),
            Uuid::from_u128(7).to_string()
        );
        assert_eq!(b.daemon_token(), Some("mdt_secret"));
    }
}
