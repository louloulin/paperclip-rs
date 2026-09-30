"""T1-6 `REALM_DIFF` 族的常量：族名 / 族判据 / 归因标签 / 负责面文件集合。

纯常量模块 —— 不导入任何兄弟模块，避免循环依赖。
"""

from __future__ import annotations

FAMILY = "REALM_DIFF"
FAMILY_CRITERION = (
    "判过了（outcome == mismatch）且 observed 既不是 401 也不是 404 ⇒ "
    "已挂载的 handler 主动返回了一个与上游不同的状态码"
)
#: 被 `t1_6_taxonomy.py` 分到别的族的 observed 值（本脚本据此划边界）。
OTHER_FAMILY_OBSERVED = (401, 404)

EXTRACTION, FIXTURE, BEHAVIOR, BY_DESIGN = "抽取缺陷", "装置面", "行为面", "by-design"

#: 四个归因的「负责面」文件集合。**列的是文件，不是组织** —— 只有文件集合能判并行。
OWNER_FILES = {
    EXTRACTION: [
        "scripts/extract_upstream_fixtures.py",
        "scripts/extract_requirements.py",
        "contracts/golden/**",
    ],
    FIXTURE: [
        "crates/mc-conformance/src/seed.rs",
        "crates/mc-conformance/src/harness.rs",
        "crates/mc-conformance/src/requirements.rs",
        "crates/mc-conformance/src/request_plan.rs",
    ],
    BY_DESIGN: [],
}

#: **子族级**负责面。归因级那张表对 `行为面` **故意留空**：一个归因横跨 5 个互不相交的域，
#: 归因级只能给出一个并集，而并集非空**不代表**可并行（那是 §240/§257 之后的老坑形状：
#: 结论所依赖的字段是空的，屏幕却已经打出了结论）。
#:
#: ⚠️ 维护纪律：**这张表缺条目 = 该子族不可派工**，不是「该子族不存在负责面」。
#: `report.py` 据此把 lane 的并行结论从「是」自动降级为「未定」，并点名缺哪几条
#: —— 不要在 `__main__` 里手工把结论写死成「是」。
#:
#: 每条都必须**读代码得到**（`docs/37` §258 §「行为面仍不可派」第 4 轮的处置是「不替它猜」）：
#: 下面 4 条是 2026-09-29 22:00 cycle 逐个打开上游测试站位 + 本仓 handler 定位后填的。
BEHAVIOR_OWNER_FILES = {
    # 12 条成员 = chat 11（`/api/chat/history` / `/api/chat/thread`）＋ properties 1。
    # 上游 `resolveActor` 直接信 `X-Actor-Source: task_token` 盖章头；本仓的闸在
    # `actor_guard.rs`，且只挂在账户级路由上。
    "BEHAVIOR_MACHINE_ACTOR_GATE": [
        "crates/mc-http/src/routes/chat/task/history.rs",
        "crates/mc-http/src/routes/properties.rs",
        "crates/mc-http/src/actor_guard.rs",
    ],
    # `parse_metadata_filter`（`routes/issues/query.rs`）不拒畸形 metadata 过滤串 ⇒ 200。
    "BEHAVIOR_METADATA_FILTER_PARSE": [
        "crates/mc-http/src/routes/issues/query.rs",
        "crates/mc-http/src/routes/issues/list.rs",
    ],
    # `renew_current_pat`（`routes/pats.rs`）只从 `Authorization` 取身份，不读 `X-User-ID`。
    "BEHAVIOR_STAMPING_CHAIN_UNWIRED": [
        "crates/mc-http/src/routes/pats.rs",
        "crates/mc-http/src/middleware/authn.rs",
    ],
    # `update_workspace`（`routes/workspaces.rs`）静默忽略 `issue_prefix`。
    "BEHAVIOR_ISSUE_PREFIX_UNPORTED": [
        "crates/mc-http/src/routes/workspaces.rs",
    ],
    # `query_issues`（`routes/issues/list.rs`）用 axum 的 `Json<HashMap<..>>` 提取器，
    # 解码失败被 `JsonRejection` 短路成 **422**；上游 `QueryIssues`（`issue.go:1153`）
    # 是 `json.NewDecoder(..).Decode(&params)` 失败即 **400**。
    # `list.rs` 是**唯一**落点（提取器就在那儿）；`query.rs` 是**同请求的另一半** ——
    # 它持有 `ListIssuesQuery::from_pairs`，畸形值与未知键的逐键语义都在那里判定，
    # 「只把畸形值挪到 400、不动良构路径」这条双向判据要同时读这两个文件才验得了。
    "BEHAVIOR_JSON_DECODE_STATUS": [
        "crates/mc-http/src/routes/issues/list.rs",
        "crates/mc-http/src/routes/issues/query.rs",
    ],
    # `BEHAVIOR_NUL_PAYLOAD`（NUL 字节）**仍然不填**：本轮只派了 JSON 解码这一族，
    # NUL 那条要读的是 `routes/chat/**` 的 task-messages 回调 handler，与上面 4 条
    # 无文件交集；没读就填等于编（同段开头的纪律）。`report.py` 会把 lane 判成「未定」
    # 并点名这一条。
}
