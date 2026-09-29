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
    python3 -m scripts.t1_6_realm_diff_taxonomy /tmp/t16.json          # A. 主读数
    python3 -m scripts.t1_6_realm_diff_taxonomy --golden contracts/golden  # B. 静态

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

## 模块切分（LUM-2583，纯搬代码，零行为变更）

    constants.py  族名/族判据/归因标签/负责面文件集合
    fields.py     9 个字段读取小工具（判据只读这些）
    sources.py    两种输入的读法（report json / contracts/golden 静态扫描）
    claims.py     LUM-2572 已领走的 11 条的机械判据
    rules.py      15 条子族规则（有序）+ classify + 转移形态
    checks.py     判别式双向验证（正例/反例）+ by-design 审计
    report.py     build（组装整棵 JSON）
    __main__.py   本文件：人读渲染 + CLI 入口
"""

from __future__ import annotations

import argparse
import json
import os
import sys

from .report import build
from .sources import attach_golden, load_from_golden, load_from_report


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
