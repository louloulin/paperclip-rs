#!/usr/bin/env python3
"""把 T1-6 的 `REALM_DIFF` 族再拆成**可逐条派工**的子族，并给出**归因**。

## 三刀分工（别混用）

| 脚本 | 那一刀 | 产出 |
|---|---|---|
| `scripts/t1_6_taxonomy.py` | 停止条件层：89 → 5 个根因族 | `UNMOUNTED/PRECONDITION/AUTH_401/SEED_404/REALM_DIFF` |
| `scripts/t1_6_precondition_taxonomy.py` | 第二刀（PRECONDITION，PR #169） | 9 个子族 |
| **本脚本** | 第二刀（REALM_DIFF） | 15 个子族 + 归因 + 负责面文件集合 |

`REALM_DIFF` 的 33 条**不是一个根因**（base `df2b0a01` 实测）：30 个上游测试文件、
8 个域、**16 种「期望 → 实测」转移**。照「一族一根因」的先例直接派工，会把 22 条
派给 22 个互不相干的方向，其中大部分是**错的方向**。

## 两种输入（都零编译；`--golden` 那条不需要真库、不需要 `target/`）

    mc-conformance --golden contracts/golden --db-url "$DSN" --json > /tmp/t16.json
    python3 scripts/t1_6_realm_diff_taxonomy.py /tmp/t16.json          # A. 主读数
    python3 scripts/t1_6_realm_diff_taxonomy.py --golden contracts/golden  # B. 静态

🔴 **输入 B 只是超集**（实测 34 条，真值 22）：`mismatch` 只有真库回放才有，静态判据
收敛不到真值，且**包含不在本族的行**。B 的对账 `balanced` 只保证「子族和 == 它自己
选出来的候选」，**别把 B 的计数当 22**。

## 归因判据（本片唯一真正要做的事）

先问：**这条红的期望值是靠「装置」构造出来的，还是靠 handler 逻辑？**

- `抽取缺陷`：golden 把身份 / 路径 / 绑定 / **请求形状**记错了 ⇒ **装置怎么改都是假的**。
- `装置面`：上游用**直接 INSERT/UPDATE**、**同一测试里的先行请求**、**替身/故障注入**
  造前置，本仓 handler 行为**已和上游一致** ⇒ 归 `mc-conformance`。
- `行为面`：本仓 handler 的判定顺序 / 副作用与上游不同 ⇒ 归对应域的 `mc-http` handler。
- `by-design`：断言前提在本仓**不可能满足**，不该修。**实测 22 条里 0 条**
  （判别式 = 「这条判据在什么实现下会 FAIL」，22 条都答得上 ⇒ 没有空条）。

## 三个坑（先读再改；都是实测踩出来的）

1. 🔴 **「路径模板里还留着 `{testXxx}`」不是缺陷信号。** 回放器按**名字**填占位符
   （`request_plan.rs`：`path.replace("{name}", …)`），占位符叫什么对最终 URL
   **毫无影响** ⇒ 纯装饰。照这个信号分族会把 **9** 条当抽取缺陷（真值 5 条）。
2. 🔴 **「`member` 且身份无 `X-Workspace-ID` ⇒ 400」会吃掉一条绿的**：
   `chat/…RejectsInvalidLimit@chat_test.go:709#26` 上游**也**期望 400，两个 400 撞成
   「通过」。判据必须再加 `expect != 400` —— 期望的 400 与「工作区没解析出来」的 400
   不是同一件事。
3. 🔴 **族边界复用 `t1_6_taxonomy.py::classify` 的 `observed ∉ {401,404}`**，
   不自己再写一遍「4xx↔2xx」—— 两处各写一遍必然漂移。db-mode json 里
   `status_observed` 是 `int`（只有 `unevaluable` 才是 `null`）。

## 对账（硬性）

    sum_of_subfamilies == candidates                       # 22 == 22
    candidates + claimed_elsewhere == family_total         # 22 + 11 == 33
    family_total == mismatch - observed_401 - observed_404 # 33 == 56 - 6 - 17

🔴 `claimed_elsewhere`（`LUM-2572` 领走的 11 条）**是算出来的**：7 条 daemon `404←200`
+ 4 条 issues 自定义 status `201←400`，并断言 `== 11`；它同时是「15 条规则一条都不命中」
的余集 —— 两个方向互相印证，任一侧漂移就红。
"""

from __future__ import annotations

import argparse
import collections
import json
import os
import sys

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


# --------------------------------------------------------------------------- #
# 输入
# --------------------------------------------------------------------------- #


def load_from_report(path: str) -> tuple[list[dict], dict, str, bool]:
    with open(path, encoding="utf-8") as fh:
        doc = json.load(fh)
    rows = [dict(fx) for fx in doc.get("fixtures", [])]
    for row in rows:
        row["domain"] = row.get("domain") or str(row.get("id") or "").split("/", 1)[0]
    return rows, doc.get("totals", {}), "conformance_db_json", True


def load_from_golden(root: str) -> tuple[list[dict], dict, str, bool]:
    rows = []
    for row in _scan_golden(root):
        rows.append(
            {
                "id": row.get("id"),
                "domain": str(row.get("id") or "").split("/", 1)[0],
                "method": row.get("method"),
                "path": row.get("path"),
                "actor": (row.get("actor") or {}).get("kind"),
                "status_expected": (row.get("expect") or {}).get("status"),
                "status_observed": None,  # 静态面没有实测值
                "outcome": None,
                "source": "{}/{}".format(
                    (row.get("source") or {}).get("file", ""),
                    (row.get("source") or {}).get("line", ""),
                ),
                "_golden": row,
            }
        )
    rows.sort(key=lambda r: r["id"] or "")
    return rows, {}, "contracts_golden", False


def _scan_golden(root: str) -> list[dict]:
    docs = []
    for dirpath, _dirnames, names in os.walk(root):
        for name in names:
            if not name.endswith(".json"):
                continue
            try:
                with open(os.path.join(dirpath, name), encoding="utf-8") as fh:
                    doc = json.load(fh)
            except (OSError, ValueError):
                continue  # 非 fixture 的 json（PIN / 索引类）不是契约，跳过
            if isinstance(doc, dict) and "id" in doc and "expect" in doc:
                docs.append(doc)
    return docs


def attach_golden(rows: list[dict], root: str) -> list[str]:
    """把 golden 文档挂到 report 行上（归因判据要读身份 / 请求形状；**零编译**）。"""
    docs = {d["id"]: d for d in _scan_golden(root)}
    for row in rows:
        row["_golden"] = docs.get(row.get("id"))
    return [r["id"] for r in rows if r.get("_golden") is None]


# 字段读取小工具（判据只读这些，**不猜**）
def golden(r: dict) -> dict:
    return r.get("_golden") or {}


def identity(r: dict) -> dict:
    return (golden(r).get("actor") or {}).get("upstream_identity") or {}


def query(r: dict) -> dict:
    return golden(r).get("query") or {}


def body(r: dict) -> object:
    return golden(r).get("body")


def exp(r: dict):
    return r.get("status_expected")


def obs(r: dict):
    return r.get("status_observed")


def method(r: dict):
    return r.get("method")


def path(r: dict):
    return r.get("path") or ""


# --------------------------------------------------------------------------- #
# LUM-2572 已领走的 11 条 —— **算出来的**，不是抄的
# --------------------------------------------------------------------------- #

CLAIM_NAME = "T1-6-B2 / LUM-2572（在飞）"
CLAIM_CRITERION = (
    "两条机械规则：(a) daemon 面 `404←200` —— 上游用**别的 workspace 的令牌**断言 404"
    "（反枚举），本仓装置只有同 workspace 的令牌；(b) issues 面 `POST /api/issues` "
    "`201←400` —— 自定义 status 目录项没种出来。两条合计必须 == 11。"
)
CLAIM_EXPECTED = 11


def is_claimed(r: dict) -> bool:
    if r.get("domain") == "daemon" and exp(r) == 404 and obs(r) == 200:
        return True
    return (
        r.get("domain") == "issues"
        and method(r) == "POST"
        and path(r) == "/api/issues"
        and exp(r) == 201
        and obs(r) == 400
    )


# --------------------------------------------------------------------------- #
# 15 条子族规则（**有序**，先命中先归属）
#   (名字, 归因, 判别式, 静态投影, 证据锚点, 置信)
# 静态投影为 None ⇒ 该族需要 db 读数（要有 observed 才能把成因分开）。
# --------------------------------------------------------------------------- #

SUB_RULES: list[tuple] = [
    (
        "EXTRACT_IDENTITY_WORKSPACE_HEADER_ABSENT",
        EXTRACTION,
        lambda r: (
            obs(r) == 400
            and r.get("actor") == "member"
            and "X-Workspace-ID" not in identity(r)
            and exp(r) != 400  # 坑 ②：期望的 400 与「工作区没解析出来」的 400 不是一件事
        ),
        lambda r: (
            r.get("actor") == "member"
            and "X-Workspace-ID" not in identity(r)
            and exp(r) != 400
        ),
        "上游用 `withChatTestWorkspaceCtx`（**Go context**，不是 header）注入工作区；"
        "抽取器只搬 header ⇒ golden 身份里没有 X-Workspace-ID。本仓 "
        "`resolve_workspace_id`（`routes/inbox/context.rs`）在 header 与 query 都缺时"
        "返 400 `invalid workspace id`。**装置改不了**：缺的是 fixture 的绑定。",
        "high",
    ),
    (
        "EXTRACT_QUERY_LITERAL_MISBOUND",
        EXTRACTION,
        lambda r: any("/" in str(v) for v in query(r).values()),
        lambda r: any("/" in str(v) for v in query(r).values()),
        "上游 `GET /api/issues?status=sort_custom_started&sort=status&limit=1`，"
        "golden 的 `query.status` 却是 `workspaces/` —— 取到了**另一个字符串字面量**；"
        "回放于是送出一个不存在的 status key。",
        "high",
    ),
    (
        "EXTRACT_REQUEST_SHAPE_STALE",
        EXTRACTION,
        lambda r: (
            method(r) == "POST"
            and path(r) == "/api/projects"
            and "workspace_id" in query(r)
            and exp(r) in (200, 400)
            and obs(r) == 201
        ),
        lambda r: (
            method(r) == "POST"
            and path(r) == "/api/projects"
            and "workspace_id" in query(r)
            and exp(r) in (200, 400)
        ),
        "golden 的 `source` 指到 `project_resource_test.go:530`（上游那行是 "
        "**`GET /api/projects/{id}` 期望 200**），fixture 记的却是同一测试里**更早**那次 "
        "`CreateProject` 的请求形状 ⇒ 请求↔行号/期望值错配（闭包内的断言取了外层请求）。",
        "high",
    ),
    (
        "EXTRACT_PATH_BOUND_TO_SEEDED_WORKSPACE",
        EXTRACTION,
        lambda r: (
            method(r) == "GET"
            and "{testWorkspaceID}" in path(r)
            and exp(r) == 404
            and obs(r) == 200
        ),
        lambda r: (
            method(r) == "GET"
            and "{testWorkspaceID}" in path(r)
            and exp(r) == 404
        ),
        "上游那行写的是**硬编码的外部 workspace id**（`d1474000-0000-4000-8000-000000000001`，"
        "`integration_test.go:977`，断言 404 = 反枚举）；抽取器把它绑成了 `$testWorkspaceID`"
        "（本仓**种子里的**那个）⇒ 回放查的是自己的 workspace，返 200。",
        "high",
    ),
    (
        "EXTRACT_WORKSPACE_BINDING_WRONG_DELETE",
        EXTRACTION,
        lambda r: (
            method(r) == "DELETE"
            and "{testWorkspaceID}" in path(r)
            and exp(r) == 403
            and obs(r) == 204
        ),
        lambda r: (
            method(r) == "DELETE"
            and "{testWorkspaceID}" in path(r)
            and exp(r) == 403
        ),
        "上游 `TestDeleteWorkspace_RequiresOwner` 先 `dbfx.Insert(\"workspace\")` **新建**一个"
        " workspace 再插一条 `role='admin'` 的 member，然后断言 403；抽取器把那个新 id 绑成"
        "了 `$testWorkspaceID` ⇒ 回放删的是**种子 workspace**，而回放里那个身份是 owner ⇒ 204。"
        "🔴 绑定修好**之后才**变成装置面（装置还得能造「第二个 workspace + admin 成员」）"
        "⇒ 本条必须排在装置面之后。",
        "high",
    ),
    (
        "DEVICE_CHAT_AGENT_RUNTIME_STATE",
        FIXTURE,
        lambda r: (
            r.get("domain") == "chat"
            and method(r) == "POST"
            and path(r).endswith("/messages")
            and exp(r) == 409
            and obs(r) == 201
        ),
        None,
        "上游三条的前置都是**直接 SQL 改行**：`unbindRuntime`（解绑 runtime）、"
        "`UPDATE agent SET archived_at = now()`（归档 agent）、`createRuntimeAccessDeniedAgent`"
        "（造 runtime 不可达的 agent）。本仓 `SendChatMessage` 的 409 分支逐字对齐上游，"
        "缺的只是那三行状态 ⇒ 装置面。",
        "high",
    ),
    (
        "DEVICE_QUEUED_TASK_ROW",
        FIXTURE,
        lambda r: "/queued-tasks/" in path(r) and exp(r) == 200 and obs(r) == 409,
        None,
        "上游 `insertPendingChatTask(…, \"queued\")` 直接 INSERT 一行 queued 任务，fixture 的"
        "路径里因此带着**字面 UUID**；本仓装置不造 chat 任务行 ⇒ `prioritize` 落到 "
        "`PriorityOutcome::NotQueued`（409 `task is no longer queued`）。",
        "high",
    ),
    (
        "DEVICE_INTRA_TEST_SEQUENCING",
        FIXTURE,
        lambda r: (
            method(r) == "PUT" and exp(r) == 409 and obs(r) == 200
        ) or (
            r.get("domain") == "properties" and exp(r) == 409 and obs(r) in (200, 201)
        ),
        None,
        "两条的 409 都要求**同一测试里更早那次请求已经生效**：issues `PUT` 的 409 是"
        "「title 已被上一次 PUT 改过」（`title_base` 过期）；properties 的 409 是"
        "「同名（大小写不敏感）已存在」。回放是**一 fixture 一请求**、没有测试内时序 ⇒ 装置面。",
        "high",
    ),
    (
        "DEVICE_DB_FAULT_INJECTION",
        FIXTURE,
        lambda r: isinstance(exp(r), int) and exp(r) >= 500 and obs(r) == 200,
        None,
        "上游 `hideAutopilotSubscriberTable(t)` **把 `autopilot_subscriber` 表改名**，逼 "
        "handler fail-closed 返 500；本仓回放用真表 ⇒ 200。属 "
        "`t1_6_precondition_taxonomy.py` 已点名的 `db_fault_injection` 机制。",
        "high",
    ),
    (
        "BEHAVIOR_MACHINE_ACTOR_GATE",
        BEHAVIOR,
        lambda r: "X-Actor-Source" in identity(r) and obs(r) == 201,
        # 静态投影**极松**（实测 12 条）：`X-Actor-Source` 在正常 fixture 里也出现。
        lambda r: "X-Actor-Source" in identity(r),
        "上游 `TestPropertyAdminGate` 带 `X-Actor-Source: task_token` + `X-Agent-ID`，"
        "`resolveActor` 直接**信**这个盖章头 ⇒ 403。本仓 `routes/properties.rs::create_property`"
        " 只调 `require_workspace_admin`、**没有**这条闸（对照 `crates/mc-http/src/actor_guard.rs`"
        " 的三态判定：那条闸只挂在 15 条账户级路由上）⇒ 201。",
        "high",
    ),
    (
        "BEHAVIOR_JSON_DECODE_STATUS",
        BEHAVIOR,
        lambda r: obs(r) == 422,
        None,
        "上游对非法 JSON 体返 **400**（`Decode` 失败）；本仓走 axum 的 JSON 提取器 ⇒ "
        "**422 Unprocessable Entity**。状态码词汇差异，归解析/错误映射那一层。",
        "high",
    ),
    (
        "BEHAVIOR_METADATA_FILTER_PARSE",
        BEHAVIOR,
        lambda r: exp(r) == 400 and obs(r) == 200 and "metadata" in query(r),
        lambda r: exp(r) == 400 and "metadata" in query(r),
        "上游 `GET /api/issues?metadata={not-json}` 断言 400（畸形过滤串要拒）；本仓照常"
        "返 200 ⇒ 过滤参数解析不严。",
        "high",
    ),
    (
        "BEHAVIOR_NUL_PAYLOAD",
        BEHAVIOR,
        lambda r: obs(r) == 500,
        None,
        "上游 `TestReportTaskMessagesCallbackWithNULSucceeds` 的 body 里带 `\\u0000`"
        "（tool / content 字段），断言 **200**；本仓返 500 ⇒ NUL 字节在文本字段上没被受理。",
        "high",
    ),
    (
        "BEHAVIOR_STAMPING_CHAIN_UNWIRED",
        BEHAVIOR,
        lambda r: identity(r).get("Authorization") == "$testPATForeignUser",
        lambda r: identity(r).get("Authorization") == "$testPATForeignUser",
        "上游把「别的用户的 PAT」配上本用户的 `X-User-ID` 盖章，断言 **401**（不许替别人续期）。"
        "`crates/mc-conformance/src/pat_token.rs` 的模块 docstring **自己写明**：本仓 "
        "`routes/pats.rs` **只**从 `Authorization` 取身份、从不读 `X-User-ID`，而 "
        "`middleware/authn.rs` 的盖章链还挂在 `M9-10`/`W1` 未接线清单上 ⇒ 那枚 PAT 在本仓"
        "**就是合法的**。",
        "high",
    ),
    (
        "BEHAVIOR_ISSUE_PREFIX_UNPORTED",
        BEHAVIOR,
        lambda r: isinstance(body(r), dict) and "issue_prefix" in body(r),
        lambda r: isinstance(body(r), dict) and "issue_prefix" in body(r),
        "上游 `PATCH /api/workspaces/{id}` 带超长 `issue_prefix`，断言 **400**。"
        "`docs/05-M1-WORKSPACE-MEMBER.md` 与 `docs/11-M2-ISSUE.md` **都已登记**"
        "「M0 schema 无 `issue_prefix` 列」（本仓按 slug 推算前缀）⇒ handler 静默忽略 ⇒ 200。",
        "medium",
    ),
]


def classify(row: dict, static_only: bool = False) -> str | None:
    for name, _attr, pred, static_pred, _ev, _conf in SUB_RULES:
        chosen = static_pred if static_only else pred
        if chosen is None:
            continue  # 该族静态不可判（要 observed）
        try:
            if chosen(row):
                return name
        except Exception:  # 判据读不到字段就**不命中**，绝不猜
            continue
    return None


#: 静态面（`--golden`）能判的子族 —— 判据只用到 golden 自己的字段。
STATIC_DECIDABLE = {n for n, _a, _p, sp, _e, _c in SUB_RULES if sp is not None}


def transition(r: dict) -> str:
    if r.get("outcome") is None or obs(r) is None:
        return "?←?（静态面）"
    return "{}←{}".format(exp(r), obs(r))


def upstream_file(r: dict) -> str:
    return (r.get("source") or "").rsplit(":", 1)[0]


# --------------------------------------------------------------------------- #
# 判别式双向验证（正例 + 反例）—— 本脚本最重要的自检段
# --------------------------------------------------------------------------- #

#: 已知正例：必须被指定规则命中（每条一个代表）。
KNOWN_POSITIVE = [
    ("chat/TestDeleteChatSession_PrunesChannelRows"
     "@server/internal/handler/chat_test.go:772#27",
     "EXTRACT_IDENTITY_WORKSPACE_HEADER_ABSENT"),
    ("issues/TestListIssuesStatusSortCountsCustomStatuses"
     "@server/internal/handler/issue_sort_test.go:19#71",
     "EXTRACT_QUERY_LITERAL_MISBOUND"),
    ("chat/TestSendChatMessage_ArchivedAgent"
     "@server/internal/handler/chat_test.go:249#21",
     "DEVICE_CHAT_AGENT_RUNTIME_STATE"),
    ("tokens/TestRenewPAT_RejectsTokenBelongingToDifferentUser"
     "@server/internal/handler/personal_access_token_test.go:363#9",
     "BEHAVIOR_STAMPING_CHAIN_UNWIRED"),
]

#: 已知反例：**必须一条规则都不命中**。这几条就是被「天真判别式」吃掉过的 —— 坑 ① 与 ②，
#: 外加两条已知的别族成员（装置面 daemon / AUTH_401）。
KNOWN_NEGATIVE = [
    ("chat/TestListChatMessagesPage_RejectsInvalidLimit"
     "@server/internal/handler/chat_test.go:709#26", None),
    ("daemon/TestGetChatSessionGCCheck@server/internal/handler/daemon_test.go:3815#11", None),
    ("daemon/TestGetTaskStatus_ForeignWorkspace_Returns404"
     "@server/internal/handler/daemon_task_lookup_test.go:133#2", None),
    ("issues/TestPluginInstallTokenEnforcesGrantedScope"
     "@server/internal/handler/plugin_action_test.go:160#98", None),
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
        {"subfamily": "DEVICE_*（4 族 7 条）",
         "would_fail_under": "装置能造前置（直接 INSERT 行 / 重放同一测试里的先行请求 / 故障注入档位）。"},
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


# --------------------------------------------------------------------------- #
# 组装
# --------------------------------------------------------------------------- #


def summarise(items: list[dict]) -> dict:
    return {
        "count": len(items),
        "by_domain": dict(collections.Counter(r["domain"] for r in items).most_common()),
        "by_transition": dict(
            collections.Counter(transition(r) for r in items).most_common()
        ),
        "upstream_files": sorted({upstream_file(r) for r in items}),
        "ids": sorted(r["id"] for r in items),
    }


def build(rows: list[dict], totals: dict, kind: str, reconcilable: bool,
          warnings: list[str] | None = None) -> dict:
    static_only = not reconcilable
    if static_only:
        # 静态面没有 outcome/observed，划不了族边界 ⇒ 全部 golden 行都是候选，判据给超集。
        in_family, claimed, candidates = list(rows), [], list(rows)
    else:
        in_family = [
            r for r in rows
            if r.get("outcome") == "mismatch" and obs(r) not in OTHER_FAMILY_OBSERVED
        ]
        claimed = [r for r in in_family if is_claimed(r)]
        candidates = [r for r in in_family if not is_claimed(r)]

    grouped: dict[str, list[dict]] = collections.defaultdict(list)
    unmatched = []
    for row in candidates:
        name = classify(row, static_only)
        if name is None:
            unmatched.append(row)
        else:
            grouped[name].append(row)

    subfamilies = []
    for name, attribution, _pred, _spred, evidence, confidence in SUB_RULES:
        if grouped.get(name):
            subfamilies.append({
                "name": name,
                "attribution": attribution,
                "owner_files": OWNER_FILES.get(attribution, []),
                "confidence": confidence,
                "evidence": evidence,
                "static_decidable": name in STATIC_DECIDABLE,
                **summarise(grouped[name]),
            })
    accounted = sum(s["count"] for s in subfamilies)
    # 静态面判不了、只能靠 db 读数的那几族 ⇒ 显式列出，别让读者以为它们不存在。
    omitted = sorted(n for n, _a, _p, sp, _e, _c in SUB_RULES if sp is None)
    attr_counts = collections.Counter(
        s["attribution"] for s in subfamilies for _ in range(s["count"])
    )

    # 归因面 → 文件集合 → 并行结论（**只有文件集合能判并行**）。
    lanes: dict[str, dict] = {}
    for s in subfamilies:
        entry = lanes.setdefault(s["attribution"], {
            "attribution": s["attribution"], "owner_files": s["owner_files"],
            "count": 0, "subfamilies": [], "serial_with": [],
        })
        entry["count"] += s["count"]
        entry["subfamilies"].append(s["name"])
    for lane, entry in lanes.items():
        if lane == FIXTURE:
            entry["serial_with"] = [
                "LUM-2572（T1-6-B2，在飞：mc-conformance/{seed,harness}.rs）",
                "LUM-2567（T1-6-C，PRECONDITION 29：同一批 mc-conformance 文件）",
            ]
        elif lane == EXTRACTION:
            entry["serial_with"] = ["任何同时改 contracts/golden/** 或抽取器的片"]

    eff_candidates = len(candidates) if not static_only else len(candidates) - len(unmatched)
    return {
        "schema_version": 1,
        "input_kind": kind,
        "static_only": static_only,
        "totals": totals,
        "family": {
            "name": FAMILY,
            "criterion": FAMILY_CRITERION,
            "count": None if static_only else len(in_family),
            "by_domain": dict(
                collections.Counter(r["domain"] for r in in_family).most_common()
            ),
            "by_transition": {} if static_only else dict(
                collections.Counter(transition(r) for r in in_family).most_common()
            ),
        },
        "claimed_elsewhere": {
            "name": CLAIM_NAME,
            "count": len(claimed),
            "expected_count": CLAIM_EXPECTED,
            "criterion": CLAIM_CRITERION,
            "by_domain": dict(
                collections.Counter(r["domain"] for r in claimed).most_common()
            ),
            "by_transition": dict(
                collections.Counter(transition(r) for r in claimed).most_common()
            ),
            "upstream_files": sorted({upstream_file(r) for r in claimed}),
            "ids": sorted(r["id"] for r in claimed),
        },
        "candidates": eff_candidates,
        "candidates_are_static_superset": static_only,
        "static_superset_note": (
            "静态面没有 `status_observed` ⇒ 判据只能给**超集**（实测 34 条），其中包含"
            "**不在 `REALM_DIFF` 族里的行**（含 pass / AUTH_401）。**这不是 22 的派工单。**"
            if static_only else None
        ),
        "rows_scanned": len(rows),
        "warnings": list(warnings or []),
        "subfamily_count": len(subfamilies),
        "reconciliation": {
            "sum_of_subfamilies": accounted,
            "candidates": eff_candidates,
            "balanced": accounted == eff_candidates,
            "family_total": None if static_only else len(in_family),
            "claimed_elsewhere": len(claimed),
            "candidates_plus_claimed": len(candidates) + len(claimed),
            "family_reconciled": (
                None if static_only
                else (len(candidates) + len(claimed)) == len(in_family)
            ),
            "claimed_matches_expected": (
                None if static_only else len(claimed) == CLAIM_EXPECTED
            ),
            "equals_mismatch_minus_401_404": (
                None if static_only else len(in_family) == (
                    totals.get("mismatch", 0)
                    - sum(1 for r in rows
                          if r.get("outcome") == "mismatch" and obs(r) == 401)
                    - sum(1 for r in rows
                          if r.get("outcome") == "mismatch" and obs(r) == 404)
                )
            ),
            "unmatched_candidates": [] if static_only else [r["id"] for r in unmatched],
            "unclassified_rows": len(unmatched) if static_only else None,
        },
        "attribution_summary": dict(attr_counts.most_common()),
        "static_decidable_subfamilies": sorted(
            n for n in STATIC_DECIDABLE if grouped.get(n)
        ),
        "needs_db_reading_subfamilies": omitted,
        "parallel_lanes": list(lanes.values()),
        "by_design_audit": BY_DESIGN_AUDIT,
        "discriminant_checks": discriminant_checks({r["id"]: r for r in rows}, static_only),
        "subfamilies": subfamilies,
    }


# --------------------------------------------------------------------------- #
# 人读渲染
# --------------------------------------------------------------------------- #


def _kv(counts: dict) -> str:
    return "  ".join("{}={}".format(k, v) for k, v in counts.items())


def render_human(out: dict) -> None:
    fam, rec, chk = out["family"], out["reconciliation"], out["discriminant_checks"]
    print("T1-6 REALM_DIFF 子族拆分（输入 {}）".format(out["input_kind"]))
    if out["static_only"]:
        print("  ⚠️ 静态面：无实测值 ⇒ 判据只给**超集**，且只列静态可判的子族。")
        print("     **这不是派工单**；派工单要看 db-mode 那一份读数。")
    print("  族：{}  {}  判据：{}".format(
        fam["name"], fam["count"] if fam["count"] is not None else "（静态面无）",
        fam["criterion"]))
    print("  域分布：{}".format(_kv(fam["by_domain"])))
    if fam["by_transition"]:
        print("  转移形态：{}".format(_kv(fam["by_transition"])))
    claimed = out["claimed_elsewhere"]
    print("  已由 {} 领走：{} 条（期望 {}）".format(
        claimed["name"], claimed["count"], claimed["expected_count"]))
    print("  对账：候选 {}  子族和 {}  {}".format(
        rec["candidates"], rec["sum_of_subfamilies"],
        "平" if rec["balanced"] else "**不平**"))
    if not out["static_only"]:
        print("  族级：{} + {} == {}  {}".format(
            rec["candidates"], rec["claimed_elsewhere"], rec["family_total"],
            "平" if rec["family_reconciled"] else "**不平**"))
    print("  归因：{}".format(_kv(out["attribution_summary"])))
    print()
    for i, s in enumerate(out["subfamilies"], 1):
        print("── [{}] {}  {} 条  归因={}  置信={}".format(
            i, s["name"], s["count"], s["attribution"], s["confidence"]))
        print("   域：{}   转移：{}".format(_kv(s["by_domain"]), _kv(s["by_transition"])))
        print("   负责面（文件集合）：{}".format("  ".join(s["owner_files"])))
        print("   上游文件：{}".format("  ".join(s["upstream_files"])))
        for fid in s["ids"]:
            print("      - {}".format(fid))
        print()
    print("并行结论：")
    for lane in out["parallel_lanes"]:
        print("  · {:<8} {} 条 —— 可并行：{}".format(
            lane["attribution"], lane["count"],
            "否" if lane["serial_with"] else "是（各域 handler 互不相交）"))
        for other in lane["serial_with"]:
            print("      串行于：{}".format(other))
    print()
    if not chk["applies"]:
        print("判别式双向验证：**本层不适用** —— {}".format(chk["why"]))
    else:
        print("判别式双向验证：正例 {}/{}，反例 {}/{}".format(
            sum(1 for p in chk["known_positive"] if p["ok"]), len(chk["known_positive"]),
            sum(1 for n in chk["known_negative"] if n["ok"]), len(chk["known_negative"])))
    if out["needs_db_reading_subfamilies"]:
        print("静态面判不了、必须看 db 读数的子族：{}".format(
            "  ".join(out["needs_db_reading_subfamilies"])))
    for w in out.get("warnings") or []:
        print("⚠️ {}".format(w))
    print()
    print("by-design：{}".format(out["by_design_audit"]["verdict"]))


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("report", nargs="?", help="mc-conformance --db-url --json 的输出")
    ap.add_argument("--golden", default="contracts/golden",
                    help="golden fixture 目录（静态扫描；也是 report 模式的联表来源）")
    ap.add_argument("--json", action="store_true", help="输出机器可读 JSON")
    args = ap.parse_args()

    warnings: list[str] = []
    if args.report:
        rows, totals, kind, ok = load_from_report(args.report)
        if os.path.isdir(args.golden):
            missing = attach_golden(rows, args.golden)
            if missing:
                warnings.append(
                    "{} 条回放行在 `--golden {}` 下找不到 fixture ⇒ 依赖 golden 字段的判别式"
                    "全部落空、对账会塌。检查 golden 目录与回放是否同源。".format(
                        len(missing), args.golden))
        else:
            warnings.append("`--golden {}` 不是目录。".format(args.golden))
    else:
        rows, totals, kind, ok = load_from_golden(args.golden)
        if not rows:
            warnings.append("`--golden {}` 下一条 fixture 都没读到。".format(args.golden))
    out = build(rows, totals, kind, ok, warnings)
    if args.json:
        # 🔴 `--json` 下一个字都不许往 stderr 写：调用方按 `2>&1` 抓再 json.load。
        print(json.dumps(out, ensure_ascii=False, indent=2))
    else:
        render_human(out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
