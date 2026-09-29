# --------------------------------------------------------------------------- #
# 15 条子族规则（**有序**，先命中先归属）
#   (名字, 归因, 判别式, 静态投影, 证据锚点, 置信)
# 静态投影为 None ⇒ 该族需要 db 读数（要有 observed 才能把成因分开）。
# --------------------------------------------------------------------------- #

from __future__ import annotations

from .constants import BEHAVIOR, EXTRACTION, FIXTURE
from .fields import body, exp, identity, method, obs, path, query

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
