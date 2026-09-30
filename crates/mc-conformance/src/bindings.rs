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
/// 🔴 解析必须带**分组键**（`Fixture.source.test`，见 [`crate::seed::group_of`]）：
/// `$testWorkspaceID` 与四类实体行都按分组各有一份，所以 `resolve("$testIssueID")`
/// 这种不带分组的问法在语义上是**缺参数**的（§213）。
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
    /// database 层现场签发并登记的四档 `mk_pat_` 明文（见 [`crate::pat_token`]）。
    /// `None` = 这一层没有 PAT 面；stateless 层就是这种。
    pat_tokens: Option<crate::pat_token::Credentials>,
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
            // `Credentials` 自己的 `Debug` 已经脱敏，这里直接落字段即可。
            .field("pat_tokens", &self.pat_tokens)
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
            pat_tokens: None,
        }
    }

    #[must_use]
    pub fn new(user_id: Uuid, workspace_id: Uuid) -> Self {
        Self {
            user_id,
            workspace_id,
            daemon_token: None,
            seed: None,
            pat_tokens: None,
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
            pat_tokens: None,
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
            pat_tokens: None,
        }
    }

    /// 把 database 层刚签出来的四档 PAT 挂上去（见 [`crate::pat_token::register`]）。
    ///
    /// 单独一个构造器而不是改 [`Self::with_seeded`] 的签名：[`crate::seed`] 的四行实体
    /// 是「哪些行存在」，PAT 是「哪一枚凭据」，两件事的测试各自要一个不带另一件的绑定表。
    #[must_use]
    pub fn with_pat_tokens(mut self, tokens: crate::pat_token::Credentials) -> Self {
        self.pat_tokens = Some(tokens);
        self
    }

    /// 该符号指向的那枚 PAT 明文（`None` = 这个符号不是 PAT，或这一层没签）。
    #[must_use]
    pub fn pat_secret(&self, sym: &str) -> Option<&str> {
        self.pat_tokens.as_ref().and_then(|c| c.secret_for(sym))
    }

    /// 这次回放能不能施加 daemon 身份（兜底分组那一枚）。
    #[must_use]
    pub fn daemon_token(&self) -> Option<&str> {
        self.daemon_token.as_deref()
    }

    /// **该分组**的 `$testWorkspaceID`：分组自己那个 workspace。
    ///
    /// 缺省回落到兜底 workspace 而不是报错：不引用 `$testWorkspaceID` 的分组根本不
    /// 会走到这里；stateless 层（没有种子）更是只有一个 workspace —— 那正是它该拿的值。
    #[must_use]
    pub fn workspace_for(&self, group: &str) -> Uuid {
        self.seed
            .as_ref()
            .and_then(|s| s.workspace(group))
            .unwrap_or(self.workspace_id)
    }

    /// **该分组**的任务令牌行（`agent` 档那枚 `X-Task-ID` 该指向哪一行）。
    ///
    /// 为什么要单独一个入口而不是把它登记成一个 `$test…` 符号：抽取器发出来的
    /// `X-Task-ID` 是**逐字字面量**（借来的行 id，不是符号），而装置按分组各建一行
    /// （主键全局唯一，12 个分组不可能共用一个字面量）。所以这里做的是一次**绑定**：
    /// 「fixture 点名的那枚令牌 ⇒ 该分组装置真的种出来的那一行」。
    /// 拿不到（stateless 层没种子 / 该分组没声明）时返回 `None`，由 `plan` 保留
    /// 字面量原样 —— 那样得到的是 handler 自己判出来的 404，而不是装置编的。
    #[must_use]
    pub fn task_token_task_for(&self, group: &str) -> Option<Uuid> {
        self.seed.as_ref().and_then(|s| s.task_token_task(group))
    }

    /// **该分组**的 `mdt_` 明文。
    ///
    /// 令牌按分组而不是全局一枚：`daemon_token.workspace_id` 有指向 workspace 的外键，
    /// 而「daemon 身份被限定在某个 workspace 内」正是上游断言的那件事 —— 现在每个分组
    /// 都有自己的 workspace，令牌也就必须跟着分组走。
    #[must_use]
    pub fn daemon_token_for(&self, group: &str) -> Option<&str> {
        self.seed
            .as_ref()
            .and_then(|s| s.daemon_token(group))
            .or(self.daemon_token.as_deref())
    }

    /// 这条 **fixture** 要用的 `mdt_` 明文。
    ///
    /// 与 [`Self::daemon_token_for`] 的差别只有一个：上游把「跨空间探针」编码在**请求
    /// context 的 workspaceID** 里（`middleware.WithDaemonContext`），而抽取器没有留下
    /// 那个位置 ⇒ [`crate::upstream_facts`] 按 `(source.test, source.line)` 把哪些请求
    /// 用的是 outsider 身份**逐条登记**出来。**不读 `expect`**（本仓架构不变式：
    /// `expect` 只在 `verdict.rs` 的比对侧出现）。
    ///
    /// outsider 令牌不存在（stateless 层 / 语料没点名）时**回落**到分组自己的令牌 ——
    /// 回落而不是另给一句错误文案：stateless 层的 `report.json` 是门 ⑨ 的判据，
    /// 错误文案变了会让那条门以「报告漂移」的形态红。
    #[must_use]
    pub fn daemon_token_for_fixture(&self, group: &str, source: &crate::Source) -> Option<&str> {
        if crate::upstream_facts::is_foreign_daemon_request(&source.test, source.line) {
            if let Some(token) = self.seed.as_ref().and_then(|s| s.outsider_daemon_token()) {
                return Some(token);
            }
        }
        self.daemon_token_for(group)
    }

    /// 符号 → 真值。**唯一**的汇合点：身份两类，实体行四类，外加一枚明文凭据
    /// （`$testPAT*`，见 [`crate::pat_token`]）。
    ///
    /// 🔴 未知符号返回 `None`（由 `resolve` 变成 `unbound symbol` 错误）而不是
    /// 猜一个值：猜出来的 UUID 会让请求落在一个不存在的行上，症状是 `404`，
    /// 而 `404` 与「这条 fixture 本来就不该过」在报告里**长得一样**。
    fn lookup(&self, group: &str, sym: &str) -> Option<String> {
        // `$testPAT*` 不是任何一行**行 id**（`Id`），而是一枚明文凭据：在这里就返回，
        // 不走下面那条「符号 → 行」的路。
        if let Some(secret) = self.pat_secret(sym) {
            return Some(secret.to_string());
        }
        let id = match sym {
            // 身份两类是**全回放共享**的：上游的 `$testUserID` 就是「跑这次测试的用户」，
            // 把它也分组化会把「同一个用户拥有两个 workspace」这条语义编错。
            "$testUserID" => Some(self.user_id),
            "$testWorkspaceID" => Some(self.workspace_for(group)),
            _ => self.seed.as_ref().and_then(|s| s.get(group, sym)),
        }?;
        Some(id.to_string())
    }

    /// 解析一个 fixture 里的取值：`$symbol` 走绑定表，其余当字面量。
    ///
    /// `group` 是这条 fixture 的分组键（[`crate::seed::group_of`]）—— 同一个符号在
    /// 不同分组里指**不同的行**，所以它不是一个可以省的默认参数。
    pub fn resolve(&self, group: &str, raw: &str) -> Result<String, String> {
        let trimmed = raw.trim();
        if trimmed.starts_with('$') {
            return self
                .lookup(group, trimmed)
                .ok_or_else(|| format!("unbound symbol {trimmed}"));
        }
        Ok(trimmed.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: &str = "TestSeeded";

    fn seeded() -> Bindings {
        Bindings::with_seeded(
            Uuid::from_u128(7),
            Uuid::from_u128(8),
            "mdt_default".into(),
            seed::Seed::default().with_group(
                G,
                seed::GroupSeed {
                    workspace_id: Uuid::from_u128(18),
                    daemon_token: "mdt_secret".into(),
                    agent: Uuid::from_u128(9),
                    issue: Uuid::from_u128(10),
                    chat_session: Uuid::from_u128(11),
                    task: Uuid::from_u128(12),
                    task_token_task: None,
                },
            ),
        )
    }

    #[test]
    fn resolve_leaves_literals_alone_and_refuses_unknown_symbols() {
        let b = Bindings::stateless();
        assert_eq!(b.resolve(G, "  hello ").unwrap(), "hello");
        assert_eq!(b.resolve(G, "$testUserID").unwrap(), b.user_id.to_string());
        assert_eq!(
            b.resolve(G, "$testWorkspaceID").unwrap(),
            b.workspace_id.to_string()
        );
        // stateless 层没有实体行：四个域符号必须**报错**而不是给一个编出来的 UUID。
        for sym in seed::Seed::SYMBOLS {
            let err = b.resolve(G, sym).expect_err("stateless 层不该供得起实体行");
            assert!(err.contains("unbound symbol"), "{sym}: {err}");
        }
        // 完全未知的符号同样报错 —— 这是「抽取器与回放器必须同步」的承重断言。
        assert!(b.resolve(G, "$testProjectID").is_err());
    }

    #[test]
    fn seeded_bindings_resolve_every_entity_symbol() {
        let b = seeded();
        assert_eq!(
            b.resolve(G, "$testAgentID").unwrap(),
            Uuid::from_u128(9).to_string()
        );
        assert_eq!(
            b.resolve(G, "$testIssueID").unwrap(),
            Uuid::from_u128(10).to_string()
        );
        assert_eq!(
            b.resolve(G, "$testChatSessionID").unwrap(),
            Uuid::from_u128(11).to_string()
        );
        assert_eq!(
            b.resolve(G, "$testTaskID").unwrap(),
            Uuid::from_u128(12).to_string()
        );
        // 身份类符号与实体行互不干扰。
        assert_eq!(
            b.resolve(G, "$testUserID").unwrap(),
            Uuid::from_u128(7).to_string()
        );
        assert_eq!(b.daemon_token(), Some("mdt_default"));
    }

    /// §213 的承重断言：**同一个符号在不同分组里指不同的行**，而没被种的分组走兜底。
    ///
    /// 少了这一条，「按分组种」会悄悄退化回「按回放种」而没有人发现。
    #[test]
    fn the_same_symbol_resolves_per_group_and_falls_back_otherwise() {
        let b = seeded();
        assert_eq!(
            b.resolve(G, "$testWorkspaceID").unwrap(),
            Uuid::from_u128(18).to_string()
        );
        assert_eq!(b.daemon_token_for(G), Some("mdt_secret"));

        let other = "TestNotSeeded";
        assert!(b.resolve(other, "$testIssueID").is_err());
        assert_eq!(
            b.resolve(other, "$testWorkspaceID").unwrap(),
            Uuid::from_u128(8).to_string()
        );
        assert_eq!(b.daemon_token_for(other), Some("mdt_default"));
    }

    /// `LUM-2572`：**登记为跨空间的上游请求**用 outsider 令牌，其余一切都用分组自己的。
    ///
    /// 承重之处是「按什么选」：选依据是 fixture 的 `Source`（provenance），而这里
    /// 故意把同一条分组下**两条 `(test, line)` 不同**的 fixture 放在一起断言 ——
    /// 一条拿 outsider、一条拿分组令牌。若哪天有人改成读 `expect` 或改成「整组一个
    /// 令牌」，这条会红。
    #[test]
    fn foreign_daemon_fixtures_take_the_outsider_token_and_the_rest_do_not() {
        let (test, foreign_line) = (
            crate::upstream_facts::FOREIGN_DAEMON_REQUESTS[0].test,
            crate::upstream_facts::FOREIGN_DAEMON_REQUESTS[0].line,
        );
        let mut seed = seed::Seed::default().with_outsider(seed::Outsider {
            workspace_id: Uuid::from_u128(0x0777),
            daemon_token: "mdt_outsider".into(),
        });
        seed = seed.with_group(
            G,
            seed::GroupSeed {
                workspace_id: Uuid::from_u128(18),
                daemon_token: "mdt_secret".into(),
                agent: Uuid::from_u128(9),
                issue: Uuid::from_u128(10),
                chat_session: Uuid::from_u128(11),
                task: Uuid::from_u128(12),
                task_token_task: None,
            },
        );
        let b = Bindings::with_seeded(
            Uuid::from_u128(7),
            Uuid::from_u128(8),
            "mdt_default".into(),
            seed,
        );
        assert_eq!(
            b.seed.as_ref().unwrap().outsider_workspace(),
            Some(Uuid::from_u128(0x0777))
        );

        let src = |line: u64| crate::Source {
            file: "server/internal/handler/daemon_test.go".into(),
            line,
            test: test.to_string(),
            site: "testutil.Call".into(),
            via: "handler".into(),
            commit: String::new(),
        };
        assert_eq!(
            b.daemon_token_for_fixture(G, &src(foreign_line)),
            Some("mdt_outsider")
        );
        assert_eq!(
            b.daemon_token_for_fixture(G, &src(foreign_line + 6)),
            Some("mdt_secret"),
            "同一条测试里的同空间对照必须仍然拿分组自己的令牌"
        );
        // stateless / 没建 outsider 时**回落**，不另给一套文案。
        let no_outsider =
            Bindings::with_daemon_token(Uuid::from_u128(7), Uuid::from_u128(8), "mdt_x".into());
        assert_eq!(
            no_outsider.daemon_token_for_fixture(G, &src(foreign_line)),
            Some("mdt_x")
        );
    }
}
