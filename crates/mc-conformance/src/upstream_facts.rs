//! 抽取器**没记下来**、但装置必须知道的上游事实（本片 `LUM-2572`，族 `REALM_DIFF`）。
//!
//! # 为什么需要这样一份表，而不是从 fixture 反推
//!
//! 本片 11 条 fixture 红在**装置面**，但缺口不是「少种了一行」那么齐整 —— 抽取器把
//! 上游请求里**两个语义位置**丢掉了，而这两个位置恰好决定了判定结果：
//!
//! 1. **自定义 status 的目录项**：上游 `issue_status_test.go` 的 `createTestCustomStatus`
//!    （`:38`）**直接调 `Queries.CreateIssueStatusEntry`**，不走 HTTP ⇒ 抽取器只看 HTTP
//!    调用，回放时目录里没有那个 key ⇒ `POST /api/issues` 400 `unknown status`。
//! 2. **daemon 身份落在哪个 workspace**：上游 `daemon_test.go` 的跨空间用例把
//!    `middleware.WithDaemonContext(ctx, workspaceID, daemonID)` 的 `workspaceID` 写成
//!    **另一个 workspace**（`"00000000-0000-0000-0000-000000000000"`，或
//!    `daemon_task_lookup_test.go:130` 的 `uuid.NewString()`）。本仓 daemon 身份来自
//!    `daemon_token` 表（带 `REFERENCES workspace(id)` 外键），而 `seed.rs` 给每个分组
//!    签发的令牌**就落在该分组自己的 workspace** ⇒ 同空间访问 ⇒ 200。
//!
//! 🔴 **两条都不能从 fixture 反推**：
//!
//! * 自定义 status：语料里有 **4 条仍在绿的** fixture 同样带一个目录外的
//!   `body.status`（`in_use_a` / `not_a_status` / `active`×2）却**期望 400**
//!   （`issues/TestPreviouslyArchivedStatusRemainsReadable` 等）。「扫本组 fixture 的
//!   `body.status` 就建目录项」会把这 4 条**弄红** —— 判据必须是「上游那条测试**建过**
//!   这个 key」，而这件事只在上游源码里。
//! * daemon 身份：跨空间那条 request 与它的同空间对照 fixture **逐字段相同**
//!   （只有 `id` / `expect.status` / `source.line` 不同），抽取器没有留下任何身份痕迹。
//!
//! ⇒ 所以这份表按 **fixture 自己的 provenance**（`Source.test` + `Source.line`）登记，
//! 每一条都带上游 `file:line` 证据。它**不读 `expect`**：本仓的架构不变式是
//! 「`expect` 只出现在 `verdict.rs` 的比对侧，请求构造面一次都不读它」，读 `expect`
//! 去挑令牌会让 fixture 变成永远无法失败的恒真式。
//!
//! # 这份表不是「答案抄写」
//!
//! 它回答的是「上游那条请求当时用的是哪个身份 / 目录里当时有什么」，而不是
//! 「这条 fixture 期望什么码」。绑定之后 handler 仍然要**真的**把 404/201 做出来：
//! 表里的每一条都可以因为实现坏了而失败，`expect` 一个字节都没参与。
//! 表与语料的一致性由 `tests/golden.rs` 的
//! `declared_upstream_facts_match_the_golden_corpus` 逐条钉住 —— 上游改名/挪行会让它
//! **响亮地红**，而不是悄悄绑错。

/// 上游**跨 workspace** daemon 请求的 provenance。
///
/// 判据：该 `(test, line)` 对应的上游请求用的是一个**不属于被访问资源**的
/// workspace 身份。落在 `daemon_token` 表上时，对应「令牌登记在某个 outsider
/// workspace」。
#[derive(Debug, Clone, Copy)]
pub struct ForeignDaemonRequest {
    /// 上游测试名（= fixture 的分组键 `Fixture.source.test`）。
    pub test: &'static str,
    /// fixture 的 `Source.line`（上游 `testutil.Call` / `testPool` 断言那一行）。
    pub line: u64,
    /// 上游证据：`文件:行` + 那一行写了什么。写在这里是为了让下一个读代码的人
    /// **不必重新去 clone 上游**就能核对。
    pub evidence: &'static str,
}

/// 上游跨空间 daemon 探针（6 条；第 7 条见 [`NOT_DECLARED_FOREIGN`]）。
///
/// 上游惯用写法是把「attacker 令牌 → 404」放在同一条测试的**第一次** daemon 请求，
/// 随后是「本空间令牌 → 200」的对照 —— 但**本表不依赖那个顺序**：它逐条点名
/// `source.line`，顺序变了这条表就不匹配（而不是静默错绑）。
pub const FOREIGN_DAEMON_REQUESTS: &[ForeignDaemonRequest] = &[
    ForeignDaemonRequest {
        test: "TestGetTaskStatus_ForeignWorkspace_Returns404",
        line: 133,
        evidence: "daemon_task_lookup_test.go:130-133 `otherWorkspace := uuid.NewString()` \
                   + `newDaemonTokenRequest(..., otherWorkspace, \"other-daemon\")`",
    },
    ForeignDaemonRequest {
        test: "TestGetIssueGCCheck_WithDaemonToken_CrossWorkspace",
        line: 1180,
        evidence: "daemon_test.go:1179-1180 `newDaemonTokenRequest(..., \
                   \"00000000-0000-0000-0000-000000000000\", \"attacker-daemon\")`",
    },
    ForeignDaemonRequest {
        test: "TestGetDaemonWorkspaceRepos_WithDaemonToken_WorkspaceMismatch",
        line: 1586,
        evidence: "daemon_test.go:1584-1586 `newDaemonTokenRequest(..., \
                   \"00000000-0000-0000-0000-000000000000\", \"test-daemon-mdt\")`",
    },
    ForeignDaemonRequest {
        test: "TestGetChatSessionGCCheck",
        line: 3815,
        evidence: "daemon_test.go:3812-3815 `newDaemonTokenRequest(..., \
                   \"00000000-0000-0000-0000-000000000000\", \"attacker-daemon\")`",
    },
    ForeignDaemonRequest {
        test: "TestGetTaskGCCheck",
        line: 3931,
        evidence: "daemon_test.go:3928-3931 `newDaemonTokenRequest(..., \
                   \"00000000-0000-0000-0000-000000000000\", \"attacker-daemon\")`",
    },
    ForeignDaemonRequest {
        test: "TestAckTaskCancelled",
        line: 4482,
        evidence: "daemon_test.go:4479-4482 `newDaemonTokenRequest(..., \
                   \"00000000-0000-0000-0000-000000000000\", \"attacker-daemon\")`",
    },
];

/// 本片 7 条 daemon 里**明确不登记**的那一条，连同理由。
///
/// `daemon/TestGetChatSessionGCCheck@…:3840#13` 期望 404，但它的 404 **不是身份**给的：
/// 上游在 `daemon_test.go:3836` 先 `DELETE FROM chat_session WHERE id = $1`，再用
/// **本空间**令牌访问同一个 id（`:3837-3840`）。抽取器只抽 HTTP 调用 ⇒ 那次删除
/// 从未被记下 ⇒ 本仓无论如何绑定身份都表达不出「行已被删掉」这个前置。
/// 登记成 by-design 不修，好过为凑数字去放宽 `daemon/gc.rs` 的 404（那会破坏
/// 「workspace 不匹配与行不存在返回同一个 404」这条反枚举设计，见 `daemon/gc.rs:15-17`）。
pub const NOT_DECLARED_FOREIGN: &[(&str, u64, &str)] = &[(
    "TestGetChatSessionGCCheck",
    3840,
    "上游 daemon_test.go:3836 先 DELETE 掉 chat session 行，再用本空间令牌访问同一 id；\
     抽取器未记录该副作用 ⇒ 装置面表达不出，登记为 by-design",
)];

/// 某条 fixture 的上游请求是否用了**跨 workspace** 的 daemon 身份。
#[must_use]
pub fn is_foreign_daemon_request(test: &str, line: u64) -> bool {
    FOREIGN_DAEMON_REQUESTS
        .iter()
        .any(|r| r.test == test && r.line == line)
}

/// `groups` 里是否至少有一个分组需要 outsider 身份。
///
/// 用来决定要不要**建**那个 outsider workspace：不需要时连建都不建，避免给
/// 每一次回放都白加一个 workspace。
#[must_use]
pub fn needs_outsider(groups: &[String]) -> bool {
    FOREIGN_DAEMON_REQUESTS
        .iter()
        .any(|r| groups.iter().any(|g| g == r.test))
}

/// 上游某条测试在目录里**建过**的一行自定义 status。
#[derive(Debug, Clone, Copy)]
pub struct CustomStatusRow {
    /// 上游测试名（= 分组键）。
    pub test: &'static str,
    /// `issue_status.key` —— 也是 fixture `body.status` 里出现的那个值。
    pub key: &'static str,
    /// `issue_status.name`（上游给什么就记什么；有一条是中文展示名）。
    pub name: &'static str,
    /// `issue_status.category`，取上游四值词汇。
    pub category: &'static str,
    /// 上游证据。
    pub evidence: &'static str,
}

/// 本片 A 组 4 条：上游测试**先建目录项、再 `POST /api/issues {"status": "<key>"}`**。
///
/// `category` 一律 `"started"`：上游 `issuestatus.go:140` 把 `in_progress` / `in_review`
/// 都归到 `CategoryStarted`，另外两条直接写 `issuestatus.CategoryStarted`。
pub const CUSTOM_STATUS_ROWS: &[CustomStatusRow] = &[
    CustomStatusRow {
        test: "TestArchiveRejectsAfterACommittedWrite",
        key: "race_b_writer",
        name: "race_b_writer",
        category: "started",
        evidence: "issue_status_test.go:805 `createTestCustomStatus(t, \"race_b_writer\", \
                   issuestatus.InProgress)`（`InProgress` ⇒ `started`，issuestatus.go:140）",
    },
    CustomStatusRow {
        test: "TestCreateEventCarriesCustomStatusCategory",
        key: "human_review_ev",
        name: "human_review_ev",
        category: "started",
        evidence: "issue_status_test.go:1245 `createTestCustomStatus(t, \"human_review_ev\", \
                   issuestatus.InReview)`（`InReview` ⇒ `started`）",
    },
    CustomStatusRow {
        test: "TestIssueResponseCarriesCustomStatusName",
        key: "in_review_8",
        name: "客户确认",
        category: "started",
        evidence: "issue_status_test.go:1655 `CreateIssueStatusEntry{Key: \"in_review_8\", \
                   Name: \"客户确认\", Category: issuestatus.CategoryStarted}`",
    },
    CustomStatusRow {
        test: "TestCustomStatusPayloadsAgreeAcrossRenderings",
        key: "in_review_7",
        name: "客户确认",
        category: "started",
        evidence: "issue_status_test.go:1905 `CreateIssueStatusEntry{Key: \"in_review_7\", \
                   Name: \"客户确认\", Category: issuestatus.CategoryStarted}`",
    },
];

/// 某个分组要建的自定义 status 目录项（0..n 行）。
pub fn custom_statuses_for(test: &str) -> impl Iterator<Item = &'static CustomStatusRow> + '_ {
    CUSTOM_STATUS_ROWS.iter().filter(move |r| r.test == test)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// 重复登记会让「一个 `(test, line)` 落进两张不同的表」变成静默行为。
    #[test]
    fn declared_tables_have_no_duplicate_keys() {
        let mut seen = BTreeSet::new();
        for r in FOREIGN_DAEMON_REQUESTS {
            assert!(
                seen.insert((r.test, r.line)),
                "重复登记 {}:{}",
                r.test,
                r.line
            );
            assert!(!r.evidence.is_empty(), "{}:{} 没有上游证据", r.test, r.line);
        }
        let mut keys = BTreeSet::new();
        for r in CUSTOM_STATUS_ROWS {
            assert!(
                keys.insert((r.test, r.key)),
                "重复登记 {}:{}",
                r.test,
                r.key
            );
            assert!(!r.evidence.is_empty(), "{}:{} 没有上游证据", r.test, r.key);
        }
    }

    /// key / name / category 必须过得上本仓 `POST /api/issue-statuses` 的校验，
    /// 否则种子会在回放中途才报错。
    #[test]
    fn custom_status_rows_satisfy_the_catalog_constraints() {
        for r in CUSTOM_STATUS_ROWS {
            assert!(
                mc_repos::issue_status::validate_key(r.key).is_some(),
                "{}: key `{}` 不是合法 key",
                r.test,
                r.key
            );
            // DB CHECK：`char_length(name) BETWEEN 1 AND 64`。
            assert!(
                !r.name.is_empty() && r.name.chars().count() <= 64,
                "{}: name 不合法",
                r.test
            );
            assert!(
                mc_repos::issue_status::parse_category(r.category).is_some(),
                "{}: category `{}` 不在词表内",
                r.test,
                r.category
            );
            // 内置 key 不能当自定义 status 建第二遍（唯一索引会冲突）。
            assert!(
                mc_repos::issue_status::DEFAULT_STATUSES
                    .iter()
                    .all(|(k, _, _, _)| *k != r.key),
                "{}: key `{}` 撞内置目录",
                r.test,
                r.key
            );
        }
    }

    /// 不登记的那一条必须**明确**不落在跨空间表里 —— 否则「by-design」与
    /// 「已修」两个状态会同时成立。
    #[test]
    fn the_not_declared_fixture_is_not_also_declared_foreign() {
        for (test, line, why) in NOT_DECLARED_FOREIGN {
            assert!(
                !is_foreign_daemon_request(test, *line),
                "{test}:{line} 同时出现在两张表里"
            );
            assert!(!why.is_empty(), "{test}:{line} 没写理由");
        }
    }

    #[test]
    fn needs_outsider_tracks_the_declared_table() {
        let hit = vec![
            "TestGetChatSessionGCCheck".to_string(),
            "TestOther".to_string(),
        ];
        assert!(needs_outsider(&hit));
        assert!(!needs_outsider(&["TestOther".to_string()]));
        assert!(!needs_outsider(&[]));
    }

    #[test]
    fn custom_statuses_for_selects_exactly_the_declared_rows() {
        let rows: Vec<_> =
            custom_statuses_for("TestIssueResponseCarriesCustomStatusName").collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key, "in_review_8");
        assert_eq!(rows[0].name, "客户确认");
        assert_eq!(custom_statuses_for("TestNothingDeclared").count(), 0);
    }
}
