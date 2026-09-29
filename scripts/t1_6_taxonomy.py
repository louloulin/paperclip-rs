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

2. `status_observed == 404` ⇒ 一定是**已挂载的 handler** 主动返回的 404。
   依据 `crates/mc-conformance/src/verdict.rs:149`：只有「404 **且 body 为空**」
   （axum fallback）或 405 才判 `unmounted`。既然落在 `mismatch` 里，body 就
   必然非空 ⇒ 路由在、行为不对。

3. `status_observed == 401` ⇒ 鉴权层在**认证**阶段就拒了，而上游这些用例期望的是
   **认证通过后的授权判定**（403/200/201）⇒ 身份注入面缺口。

## 用法

    mc-conformance --golden contracts/golden --db-url "$DSN" --json > /tmp/db.json
    python3 scripts/t1_6_taxonomy.py /tmp/db.json          # 人读
    python3 scripts/t1_6_taxonomy.py /tmp/db.json --json   # 机器读

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
        "SEED_404",
        "判过了，observed 404 且 body 非空 ⇒ 路由已挂，handler 主动 404（实体缺失）",
        "mc-conformance::seed 按 fixture 种实体",
    ),
    (
        "REALM_DIFF",
        "判过了，同域 CRUD 链的状态码/时序差异（4xx↔2xx、409/403 判定顺序）",
        "各域 handler 的判定顺序与副作用",
    ),
]


def classify(fx: dict) -> str:
    if fx.get("outcome") == "unmounted":
        return "UNMOUNTED"
    if fx.get("outcome") == "unevaluable":
        return "PRECONDITION"
    if fx.get("status_observed") == 401:
        return "AUTH_401"
    if fx.get("status_observed") == 404:
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
    for f in bad:
        by_family[classify(f)].append(f)

    out = {
        "totals": totals,
        "bad_total": len(bad),
        "contract_equivalence_rate": doc.get("contract_equivalence_rate"),
        "mounted_equivalence_rate": doc.get("mounted_equivalence_rate"),
        "families": {},
    }
    for name, crit, owner in FAMILIES:
        items = by_family.get(name, [])
        domains = collections.Counter(f["domain"] for f in items)
        out["families"][name] = {
            "criterion": crit,
            "owner": owner,
            "count": len(items),
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
                }
                for f in sorted(items, key=lambda x: (x["domain"], x["path"]))
            ],
        }

    if args.json:
        print(json.dumps(out, ensure_ascii=False, indent=2))
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
        print("── {}  {} 条".format(name, info["count"]))
        print("   判据：{}".format(crit))
        print("   负责面：{}".format(owner))
        if info["by_domain"]:
            print(
                "   域分布：{}".format(
                    "  ".join("{}={}".format(d, c) for d, c in info["by_domain"].items())
                )
            )
        print()
    return 0


if __name__ == "__main__":
    sys.exit(main())
