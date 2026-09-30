#!/usr/bin/env python3
"""把 T1-6（⑨ conformance --db-url）的失败 fixture 按**根因**分族。

## 为什么要有这个脚本

T1-6 是本仓停止条件里**唯一**还在红的一条（`stop_condition.sh --db-url`：`pass 15 /
fail 1 / T1-6`）。它的判词是「`unevaluable 0 ∧ mismatch 0`」，也就是说剩余工作量
被压成一个数字（N 条 fixture 没绿）。**一个总数不能直接派工** —— 必须能回答
「这 N 条各自缺什么、该谁补」。本脚本就是那个「总数 → 派工单」的转换器。

## 判据来源（不要凭印象改这里的族）

分族只用 fixture 自带的字段，**不猜**。判据的**先后顺序是有语义的**，别调换：

1. **`outcome` 优先**。`unevaluable` = 装置面缺口（前置没满足，**这条根本没被
   判定过**）；`mismatch` / `unmounted` = 行为面缺口（**判过了且不符**）。
   🔴 这条顺序是踩过坑才定下来的：`requires` 非空**不能**当第一判据 —— 实测
   58 条 `mismatch` 里有 **8 条**也带 `requires`（daemon 面的 `daemon_token`
   /`db_fault_injection`），它们的前置**是满足的**，只是行为不符。先按 `requires`
   分会把那 8 条误记成「装置缺口」，从而把工派给装置面的人 —— 而那些缺口
   恰恰是**行为面**的。判据：`requires` 只在 `outcome == unevaluable` 时**印证**
   归因，不参与分族。

2. 🔴 **`status_observed == 404` 这一刀必须读 body，不能只读状态码。**
   上一版的 docstring 写的是「404 ⇒ 一定是已挂载的 handler 主动返回的」，依据是
   `crates/mc-conformance/src/verdict.rs:149` 的
   `if observed.status == 404 && empty_body && !json { return Unmounted }`。
   **那是非续接推理**：`verdict.rs` 只说了「404 + 空 body ⇒ unmounted」，
   反过来推不出来 —— 404 + 非空 body 完全可能是**鉴权中间件**返回的，handler
   根本没被执行到。本仓现成的反例（`crates/mc-http/src/routes/agents.rs:204-218`）：
   `workspace_role()` 对非成员返回 `not_found("workspace")`，走统一错误体
   （`crates/mc-errors/src/http.rs:17-36`）⇒ **body 非空的 404，而路由是挂着的**。
   `classify()` 当时**从头到尾没有读过 body**，于是把 13 条鉴权面缺口记成
   `SEED_404`（负责面 = `mc-conformance::seed`）派了出去。
   ⇒ 本仓的 404 至少三种来源，只有第三种才是 `unmounted`：

   | 来源 | 形态 | 族 |
   |---|---|---|
   | ① handler 查实体落空 | 404 + 非空 body，body 指向**业务实体** | `SEED_404` |
   | ② 鉴权面先答 | 404 + 非空 body，body 指向 **workspace 作用域** | `AUTHZ_404` |
   | ③ axum fallback | 404 + **空 body**（或 405） | `UNMOUNTED` |

3. `status_observed == 401` ⇒ 鉴权层在**认证**阶段就拒了，而上游这些用例期望的是
   **认证通过后的授权判定**（403/200/201）⇒ 身份注入面缺口。

## 🔴 body 证据从哪来（以及为什么它现在多半是「缺失」）

本仓**当前**的 `mc-conformance::report::Row`（`crates/mc-conformance/src/report.rs:23-38`）
**没有 observed body 字段** —— 它只有 `status_observed` 和一句 `detail`；而 404 的
mismatch 分支写的 `detail` 是 `status 404 != expected N`（`verdict.rs:177-180`），
**不含 body**。因此：

* 本脚本认三类 body 证据（按优先级）：`body_observed`（原始文本）→ `body_empty`
  （bool）→ `detail` 里出现 `404 with empty body`（只有 `unmounted` 行会有）。
* **证据缺失时不得假装判据跑过**：该条仍落 `SEED_404`，但整族被标
  `authoritative: false` + `unverified: N`，并往 stderr 打一条告警。
  这是本脚本最重要的一条纪律 —— 宁可承认「这一刀没跑成」，也不把症状当根因
  再派一次工（这正是 LUM-2597 的成因）。
* 要让 `SEED_404` / `AUTHZ_404` 变成**权威**分族读数，前置是 `Row` 带上 body
  （Rust 面，另一片）+ 用 `--with-db` 重跑。**本脚本自己的合成单测不是读数。**

## ① / ② 怎么分（两个方向都要钉住）

`workspace` 作用域的 404 有两种可能来源，body 文本**完全一样**
（`not_found("workspace")` ⇒ `{"code":"not_found","message":"not found: workspace"}`）：
`workspace_role()` 的鉴权先答，和「workspace 这行不存在」。所以单看 body 分不开，
判据必须再加一刀 —— **`status_expected`**：

* body 指向 workspace 作用域 **且** `status_expected != 404` ⇒ fixture 期望的是
  200/201/204/403，拿到 workspace 作用域 404 ⇒ **调用者非成员**，handler 未执行
  ⇒ `AUTHZ_404`。
* 否则 ⇒ `SEED_404`。其中 `status_expected == 404` 的那批是 fixture **在测**
  「实体/作用域缺失」，那是装置面或 handler 面，不是「非成员」。

🔴 `AUTHZ_404` **只排除一个根因，不指定另一个**：症状是「调用者不是该 workspace 的成员」，
而造成它的可以是**装置少种了第二个成员**（docs/37 §267.2 实测：LUM-2591 修好的 5 条
就是这个形态，而修法是往 seed 里加一行成员），也可以是**身份注入 / 鉴权中间件**。
这两种要逐条读 `seed.rs` + handler 才能分辨，**body 分不出来** ⇒ 本族是二者的并集，
派工时必须先拆，**不许整族当成「装置少种一行」**（那就是本片要拆掉的旧病复发）。

## 用法

    mc-conformance --golden contracts/golden --db-url "$DSN" --json > /tmp/db.json
    python3 scripts/t1_6_taxonomy.py /tmp/db.json          # 人读
    python3 scripts/t1_6_taxonomy.py /tmp/db.json --json   # 机器读
    python3 scripts/test_t1_6_taxonomy.py                   # 判据单测（无 cargo/无库）

退出码恒为 0：本脚本是**报告器**，不是门。它不判定通过与否 —— 那件事只由
`stop_condition.sh` 的 T1-6 做，避免出现第二个「红绿权威」。
"""

from __future__ import annotations

import argparse
import collections
import json
import sys

# ── 族定义 ────────────────────────────────────────────────────────────────────
# 键 = 族名，值 = (一句话判据, 负责面)。判据**互斥且穷尽**：按顺序取第一个成立的。
# 第一刀是 `outcome`（判过没有），第二刀是症状（错在哪）。理由见模块 docstring。
FAMILIES: list[tuple[str, str, str]] = [
    (
        "UNMOUNTED",
        "路由未挂载（404 空 body / 405）⇒ 属「未实现」，⑦ known_gap=0 时本仓不许加路由",
        "先裁定：真缺口还是抽取器错认（见 docs/37 的 compat 折叠先例）",
    ),
    (
        "PRECONDITION",
        "outcome=unevaluable：前置没满足，这条**没有被判定过**（云面 / 令牌 / 故障注入 / cookie）",
        "mc-conformance 装置（TierRouters / daemon_token / 故障注入档位）",
    ),
    (
        "AUTH_401",
        "判过了，observed 401：认证阶段就拒了，上游期望的是认证后的授权判定",
        "harness 身份注入 + 鉴权中间件",
    ),
    (
        "AUTHZ_404",
        "判过了，observed 404 + **body 非空且指向 workspace 作用域**，而 fixture 期望的不是 404"
        " ⇒ 症状是「调用者不是该 workspace 的成员」，handler 未执行。"
        "本族**只排除「实体缺失」这一个根因**：成因可能是装置少种成员，也可能是身份注入/鉴权，"
        "body 分不出来 ⇒ 派工前必须逐条拆",
        "先拆：mc-conformance 装置少种成员 **或** mc-http 鉴权面 / 身份注入（**不是**「实体缺失」）",
    ),
    (
        "SEED_404",
        "判过了，observed 404 + **body 非空**且指向业务实体（或 fixture 期望的就是 404）"
        " ⇒ 路由已挂，实体查落空。body 证据缺失时本族 `authoritative=false`，**不可直接派工**",
        "mc-conformance::seed 按 fixture 种实体",
    ),
    (
        "REALM_DIFF",
        "判过了，同域 CRUD 链的状态码/时序差异（4xx↔2xx、409/403 判定顺序）",
        "各域 handler 的判定顺序与副作用",
    ),
]

#: `not_found("workspace")` 的 message 前缀（`mc-errors/src/lib.rs:25` 的
#: `#[error("not found: {resource}")]`）。带 workspace 作用域的 404 命中这里。
_NOT_FOUND_PREFIX = "not found:"

#: 独立的 404 业务码（`mc-errors/src/lib.rs:118`），出现即鉴权/作用域面。
_WORKSPACE_SCOPE_CODES = frozenset({"workspace_not_found"})


def _body_evidence(fx: dict) -> tuple[str, str, dict | None]:
    """取这条 fixture 的 observed body 证据。

    返回 `(kind, body_text, parsed_json)`，其中
    `kind ∈ {"body", "empty", "detail_empty", "absent"}`：

    * `body` —— 拿到了原始 body 文本（`body_observed` / `observed_body` / `body`）；
    * `empty` —— 显式的 `body_empty: true`；
    * `detail_empty` —— body 字段没有，但 `detail` 写了 `404 with empty body`
      （只有 `verdict.rs` 判成 `unmounted` 的行会这样，所以它同时说明
      `outcome` 与 body 判据**同源**，不是独立证据）；
    * `absent` —— 没有 body 证据（**本仓当前 report.json 就是这种**）。

    `parsed_json` 是能解析成 JSON 对象时的 dict，否则 None。
    """
    for key in ("body_observed", "observed_body", "body"):
        raw = fx.get(key)
        if isinstance(raw, str):
            if not raw.strip():
                return "empty", raw, None
            try:
                parsed = json.loads(raw)
            except (ValueError, TypeError):
                parsed = None
            return "body", raw, parsed if isinstance(parsed, dict) else None
        if isinstance(raw, dict):
            return "body", json.dumps(raw, ensure_ascii=False), raw

    flag = fx.get("body_empty")
    if isinstance(flag, bool):
        return ("empty", "", None) if flag else ("absent", "", None)

    detail = fx.get("detail")
    if isinstance(detail, str) and "404 with empty body" in detail:
        return "detail_empty", "", None

    return "absent", "", None


def _is_workspace_scope(parsed: dict | None) -> bool:
    """body（**必须能解析成 JSON 对象**）是否指向 **workspace 作用域**。

    两种形态都算：
    * `{"code": "workspace_not_found", ...}`（独立业务码，无歧义）；
    * `{"code": "not_found", "message": "not found: workspace"}`（`not_found("workspace")`）。

    非 JSON 的 body 一律**不算** —— 对着裸文本做子串匹配就是「猜」，
    而本脚本的分族只允许用能读出结构的证据。
    """
    if parsed is None:
        return False
    code = parsed.get("code")
    if isinstance(code, str) and code in _WORKSPACE_SCOPE_CODES:
        return True
    message = parsed.get("message")
    if isinstance(message, str) and message.startswith(_NOT_FOUND_PREFIX):
        return message[len(_NOT_FOUND_PREFIX) :].strip() == "workspace"
    return False


def classify(fx: dict) -> str:
    if fx.get("outcome") == "unmounted":
        return "UNMOUNTED"
    if fx.get("outcome") == "unevaluable":
        return "PRECONDITION"
    if fx.get("status_observed") == 401:
        return "AUTH_401"
    if fx.get("status_observed") == 404:
        kind, _, parsed = _body_evidence(fx)
        if kind in ("empty", "detail_empty"):
            # 空 body 的 404 就是 axum fallback（verdict.rs:149）⇒ 未挂载。
            return "UNMOUNTED"
        if kind == "body" and _is_workspace_scope(parsed) and fx.get("status_expected") != 404:
            # fixture 期望的是 200/201/204/403，却拿到 workspace 作用域 404
            # ⇒ 调用者不是成员，鉴权面先答，handler 没执行。
            return "AUTHZ_404"
        return "SEED_404"
    return "REALM_DIFF"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("report", help="mc-conformance --db-url --json 的输出")
    ap.add_argument("--json", action="store_true", help="输出机器可读 JSON")
    args = ap.parse_args()

    with open(args.report, encoding="utf-8") as fh:
        doc = json.load(fh)

    fixtures = doc["fixtures"]
    bad = [f for f in fixtures if f.get("outcome") != "pass"]
    totals = doc["totals"]

    by_family: dict[str, list[dict]] = collections.defaultdict(list)
    evidence_mix: dict[str, collections.Counter] = collections.defaultdict(collections.Counter)
    for f in bad:
        name = classify(f)
        by_family[name].append(f)
        kind, _, _ = _body_evidence(f)
        evidence_mix[name][kind] += 1

    out = {
        "totals": totals,
        "bad_total": len(bad),
        "contract_equivalence_rate": doc.get("contract_equivalence_rate"),
        "mounted_equivalence_rate": doc.get("mounted_equivalence_rate"),
        "families": {},
        "caveats": [],
    }
    for name, crit, owner in FAMILIES:
        items = by_family.get(name, [])
        domains = collections.Counter(f["domain"] for f in items)
        unverified = evidence_mix[name]["absent"] if name in ("SEED_404", "AUTHZ_404") else 0
        # 族判据依赖 body，而 body 可能整族缺失 ⇒ 缺了就必须**说出来**，
        # 否则这一族的计数会像上一版那样被当成派工依据（分母静默搬家）。
        authoritative = unverified == 0
        if unverified:
            out["caveats"].append(
                "{name}: {n} 条没有 observed body 证据（report.json 的 Row 无 body 字段）"
                " ⇒ 本族计数**不可作为派工依据**；需 Row 带 body 后 --with-db 重跑。".format(
                    name=name, n=unverified
                )
            )
        out["families"][name] = {
            "criterion": crit,
            "owner": owner,
            "count": len(items),
            "authoritative": authoritative,
            "unverified": unverified,
            "body_evidence": dict(evidence_mix[name]),
            "by_domain": dict(domains.most_common()),
            "fixtures": [
                {
                    "id": f["id"],
                    "domain": f["domain"],
                    "method": f["method"],
                    "path": f["path"],
                    "expected": f["status_expected"],
                    "observed": f.get("status_observed"),
                    "outcome": f["outcome"],
                    "requires": f.get("requires") or [],
                    "body_evidence": _body_evidence(f)[0],
                }
                for f in sorted(items, key=lambda x: (x["domain"], x["path"]))
            ],
        }

    # 「outcome=mismatch 但 body 空」= verdict.rs 与本脚本判据打架。搬家必须留痕。
    inconsistent = [
        f["id"]
        for f in bad
        if f.get("status_observed") == 404
        and f.get("outcome") not in ("unmounted",)
        and _body_evidence(f)[0] in ("empty", "detail_empty")
    ]
    if inconsistent:
        out["caveats"].append(
            "{} 条 404 的 outcome 不是 unmounted 但 body 为空 ⇒ 已按 UNMOUNTED 归族，"
            "verdict.rs:149 需复核。".format(len(inconsistent))
        )
    out["verdict_inconsistent"] = inconsistent

    if args.json:
        print(json.dumps(out, ensure_ascii=False, indent=2))
        for c in out["caveats"]:
            print("WARN: " + c, file=sys.stderr)
        return 0

    t = totals
    print(
        "T1-6 根因分族  fixtures {fixtures}  pass {pass}  mismatch {mismatch}  "
        "unmounted {unmounted}  unevaluable {unevaluable}  ⇒  待清 {bad}".format(
            bad=len(bad), **t
        )
    )
    print(
        "  contract_equivalence_rate={}  mounted_equivalence_rate={}".format(
            out["contract_equivalence_rate"], out["mounted_equivalence_rate"]
        )
    )
    print()
    for name, crit, owner in FAMILIES:
        info = out["families"][name]
        print("── {}  {} 条{}".format(name, info["count"], "" if info["authoritative"] else "  ⚠ 不可派工"))
        print("   判据：{}".format(crit))
        print("   负责面：{}".format(owner))
        if not info["authoritative"]:
            print(
                "   ⚠ 无 body 证据 {} 条（mc-conformance::report::Row 尚无 body 字段）"
                " ⇒ 需 Row 带 body 后 --with-db 重跑".format(info["unverified"])
            )
        if info["by_domain"]:
            print(
                "   域分布：{}".format(
                    "  ".join("{}={}".format(d, c) for d, c in info["by_domain"].items())
                )
            )
        print()
    for c in out["caveats"]:
        print("WARN: " + c, file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
