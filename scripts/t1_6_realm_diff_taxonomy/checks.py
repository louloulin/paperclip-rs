# --------------------------------------------------------------------------- #
# 判别式双向验证（正例 + 反例）—— 本脚本最重要的自检段
# --------------------------------------------------------------------------- #

from __future__ import annotations

from .constants import OTHER_FAMILY_OBSERVED
from .fields import obs
from .rules import classify

#: 已知正例：必须被指定规则命中（每条一个代表）。
#:
#: ⚠️ 维护纪律（2026-09-29 05:00 cycle 实测）：**本表只许在「该族确有残余成员」时成立。**
#: 若某一族的成员被前序切片**真的修好了**（成员转 `pass`、该族在 report.json 里 0 命中），
#: 那它的正例会变成**永远满足不了的一条**（`classify` 返 `None`，`ok` 恒 `False`）——
#: 也就是一条没人看守的雷：分类器自检永远红，下一个接的人只会以为分类器坏了。
#: 处置**不是删掉这条断言**，而是把它**移到 `KNOWN_NEGATIVE`** 并注明「该族已被消灭」，
#: 这样断言反而**变强**：从「必须命中 X」升级成「必须一条都不命中」（防修复回退）。
KNOWN_POSITIVE = [
    ("issues/TestListIssuesStatusSortCountsCustomStatuses"
     "@server/internal/handler/issue_sort_test.go:19#71",
     "EXTRACT_QUERY_LITERAL_MISBOUND"),
    ("tokens/TestRenewPAT_RejectsTokenBelongingToDifferentUser"
     "@server/internal/handler/personal_access_token_test.go:363#9",
     "BEHAVIOR_STAMPING_CHAIN_UNWIRED"),
]

#: 已知反例：**必须一条规则都不命中**。这几条就是被「天真判别式」吃掉过的 —— 坑 ① 与 ②，
# 外加两条已知的别族成员（装置面 daemon / AUTH_401）。
KNOWN_NEGATIVE = [
    # 该族（`EXTRACT_IDENTITY_WORKSPACE_HEADER_ABSENT`）的 4 条成员已被 PR #173（LUM-2578）
    # **真的修好**：抽取器补上 `X-Workspace-ID` 绑定后它们全部转 `pass`（本条 observed=204），
    # 且该族名在 `report.json` 里 **0 命中** ⇒ 规则已无残余成员。
    # 所以它从正例降级为反例：**必须一条子族都不命中** —— 锁定「已消灭的族不会复活」。
    # （`SUB_RULES` 里那条规则保留不动：它判的是**形状**，将来若有新 fixture 命中同一形状，
    #   仍应被归到这一族；有成员时再把代表加回 `KNOWN_POSITIVE` 即可。）
    ("chat/TestDeleteChatSession_PrunesChannelRows"
     "@server/internal/handler/chat_test.go:772#27", None),
    ("chat/TestListChatMessagesPage_RejectsInvalidLimit"
     "@server/internal/handler/chat_test.go:709#26", None),
    ("daemon/TestGetChatSessionGCCheck@server/internal/handler/daemon_test.go:3815#11", None),
    ("daemon/TestGetTaskStatus_ForeignWorkspace_Returns404"
     "@server/internal/handler/daemon_task_lookup_test.go:133#2", None),
    ("issues/TestPluginInstallTokenEnforcesGrantedScope"
     "@server/internal/handler/plugin_action_test.go:160#98", None),
    # 该族（`DEVICE_CHAT_AGENT_RUNTIME_STATE`）的 3 条成员已被 PR（本片，LUM-2588）
    # **真的修好**：装置按上游 provenance 造出「agent 未绑定 / 已归档 / runtime 不可达」
    # 三种形态后它们全部转 `pass`（observed 409），且该族名在 `report.json` 里 **0 命中**
    # ⇒ 规则已无残余成员。所以它从正例降级为反例：**必须一条子族都不命中** ——
    # 锁定「已消灭的族不会复活」（`rules.py` 里那条 `DEVICE_CHAT_AGENT_RUNTIME_STATE`
    # 规则保留不动：它判的是**形状**，将来若有新 fixture 命中同一形状，仍应归到这一族）。
    ("chat/TestSendChatMessage_ArchivedAgent"
     "@server/internal/handler/chat_test.go:249#21", None),
]

#: by-design 审计：逐条回答「这条判据在什么实现下会 FAIL」。
BY_DESIGN_AUDIT = {
    "criterion": "问「这条判据在什么实现下会 FAIL」——答不上来的多半是该删的空条。",
    "verdict": (
        "22 条里 **0 条**判成 by-design：每一条都答得出「什么实现下它就不红了」"
        "（抽取器修绑定 / 装置造前置 / handler 补判定）⇒ 都是**真工作量**，不该记成空条。"
    ),
    "answered": [
        {"subfamily": "EXTRACT_*（5 族 9 条）",
         "would_fail_under": "抽取器把身份/查询值/请求形状/路径绑定记对之后（重抽 golden）。"},
        {"subfamily": "DEVICE_*（3 族 3 条：queued-task 行 / 逐字段基线 / DB 故障注入）",
         "would_fail_under": (
             "**新增装置能力**而不是新增种子行：① 抽取器把 queued-task 的字面 UUID 绑成 "
             "`$testTaskID`（现路径里那个 id 是常量，没有任何路由允许装置指定入队行的 id）；"
             "② handler 补上 `title_base`/`description_base` 的**逐字段** CAS（本仓只有聚合 "
             "`expected_revision`；用它凑 409 等于断言该测试的反面）；③ harness 加一档"
             "**可按 fixture 注入的 DB 故障**（现在唯一能让读失败的手段是打死共享连接池，"
             "那会污染同一次回放的其余 fixture）。"
         )},
        {"subfamily": "BEHAVIOR_*（6 族 6 条）",
         "would_fail_under": "对应 handler 的判定顺序与状态码词汇对齐上游。"},
    ],
}


def discriminant_checks(by_id: dict, static_only: bool) -> dict:
    if static_only:
        return {
            "applies": False,
            "why": (
                "双向验证验的是 **db-mode 判别式**（族边界与真值都要求 `status_observed`）。"
                "静态投影只能给超集（实测 R1 静态 10 vs 真值 4）⇒ 在这一层跑正例/反例只会"
                "把「超集」误报成「判别式错」。要验证就先取 db 读数。"),
            "known_positive": [], "known_negative": [],
            "all_positive_pass": None, "all_negative_pass": None,
        }
    pos, neg = [], []
    for fid, want in KNOWN_POSITIVE:
        row = by_id.get(fid)
        got = classify(row) if row else "MISSING"
        pos.append({"id": fid, "want": want, "got": got, "ok": got == want})
    for fid, want in KNOWN_NEGATIVE:
        row = by_id.get(fid)
        if row is None:
            neg.append({"id": fid, "want": want, "got": "MISSING", "ok": False})
            continue
        got = classify(row)
        neg.append({
            "id": fid,
            "want": want or "不命中任何剩余子族",
            "got": got,
            "observed": obs(row),
            # 族边界（observed 401/404）本身也是一条反例判据。
            "excluded_by_family_boundary": obs(row) in OTHER_FAMILY_OBSERVED,
            "ok": (got == want) and (want is not None or got is None),
        })
    return {
        "applies": True, "static_only": False,
        "known_positive": pos, "known_negative": neg,
        "all_positive_pass": all(p["ok"] for p in pos),
        "all_negative_pass": all(n["ok"] for n in neg),
    }
