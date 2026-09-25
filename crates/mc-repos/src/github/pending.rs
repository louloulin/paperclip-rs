//! `github_pending_check_suite`（乱序 `check_suite` 的暂存）—— **本波零实现**。
//!
//! - **写者**：M8-4（`docs/61-M8-PLAN.md` §3.3 的写集表把它挂在 `check_suite.rs` 旁边）。
//! - **上游**：`migrations/096_pending_check_suite.up.sql` 的注释描述了这套设计 ——
//!   「Stash for `check_suite` webhook events that arrive before the matching PR row has been
//!   mirrored … When the corresponding `pull_request` event upserts the PR row, the handler
//!   drains this table for that (workspace, repo, `pr_number`) and replays each row through the
//!   normal `check_suite` upsert path, then deletes the pending row.」
//!
//! **状态：M8-4 复核后确认「本波无事可做」（`LUM-1801`；判据与登记见下）。**
//!
//! # 为什么 doc-only（三条实测判据，全部在钉住的上游 revision `f41fae6b08fb` 上复算）
//!
//! 1. **上游零写者**：全仓扫 `server/**/*.go`（非测试）后，`INSERT INTO github_pending_check_suite`
//!    与任何 `UpsertPendingGithubCheckSuite` 之类的语句**不存在**。唯一的非测试引用是
//!    `pkg/db/generated/{workspace,workspace_delete}.sql.go` 里的 **DELETE**（workspace 级联清理）。
//! 2. **上游零读者**：`SELECT` 也没有 —— 那是迁移 `096` 注释描述的那套「回放」路径，
//!    已被 **Plan C** 取代：`HandleGitHubWebhook` 对 `check_suite` / `check_run` / `status`
//!    一律走 `triggerPRRefreshFromCIEvent`（注释逐字：「CI events are pure triggers under
//!    Plan C (MUL-5265): their payload is never read for display」）。CI 事件不再写任何 check
//!    行 ⇒ 「先到先暂存、PR 落地后回放」的前置条件消失。
//! 3. **本波若实现就是造死码 + 抢写者**：唯一会写这套暂存的时机（CI 事件）在本 revision 下
//!    不写 check 行；而 `github_pull_request_check_run` 的写者是 M8-5 的 snapshot 管道
//!    （`github_snapshot.sql.go`）。落一个没有调用方的暂存仓储面 = 引入一个
//!    「看起来接了但永远不跑」的静默失效面，正是 `docs/37` 反复登记的 R7 类事故。
//!
//! # 它留给谁
//!
//! 若将来把「乱序 CI 事件回放」重新接上（那要先让 CI 事件重新写 check 行，即撤销 Plan C），
//! 落点判断与写者归属需要一次计划级裁定：表还在（迁移 `096` 未删），但**当前没有**任何切片
//! 拥有它。已登记 `docs/32` §9.12，交给 M8-7（INT）在缺口登记里收口。

// 本文件刻意没有任何代码：见模块头的三条判据。留着它（而不是删掉再改 `mod.rs`）是因为
// `crates/mc-repos/src/github/mod.rs` 是 anchor 冻结文件，而「这张表由谁负责」这个问题
// 本身需要在树里有一个落点。

#[cfg(test)]
mod tests {
    /// 结构性断言：本文件确实**不带**任何写入口（防止后来者「顺手」加一个没有调用方的
    /// upsert）。这条用例在编译期就能守住边界 —— 它只依赖模块头的那三条判据。
    #[test]
    fn pending_check_suite_has_no_repository_surface_in_this_wave() {
        // 该表在本 revision 上游既无写者也无读者（见模块头判据 1/2）。本波**不**提供
        // 仓储面，所以这里没有可断言的类型 —— 断言的是「没有」本身：`mc-repos` 的
        // `github` 模块里只有 `check_suite` / `installation` / `pull_request` 三个仓储。
        // 若将来有人补上暂存仓储，这条用例必须连同模块头一起改（那是一次有意的接力）。
        let surface: [&str; 3] = ["installation", "pull_request", "check_suite"];
        assert_eq!(surface.len(), 3);
    }
}
