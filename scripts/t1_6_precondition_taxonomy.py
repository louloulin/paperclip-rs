#!/usr/bin/env python3
"""把 T1-6 的 `PRECONDITION` 族（`outcome == unevaluable`）再拆成**可逐条派工**的子族。

## 它与 `t1_6_taxonomy.py` 的分工

`scripts/t1_6_taxonomy.py` 把所有未绿 fixture 分成 **5 个根因族**（`UNMOUNTED` /
`PRECONDITION` / `AUTH_401` / `SEED_404` / `REALM_DIFF`）。那是**停止条件那一层**的粒度 ——
够跑 `--db-url`，**不够派工**：`PRECONDITION` 一族内部跨 9 个域，缺的是**互不相干**的机制
（任务令牌 / 云面替身 / DB 故障注入 / 签名 cookie / 限流器替身 …）。

本脚本只做第二刀：**把 `PRECONDITION` 这一族按「未满足的前置机制」再分一次**，
每个子族给出 **条数 / 域分布 / `requires` 键集 / 统一的 `detail` 文本 / 逐条清单**，
使每一条都能被单独写进工单。本脚本**不判断**任何一条该不该修 —— 那是 `docs/37` 里
逐个打开上游 setup 之后的结论，机械判别式给不出那种东西。

## 判据（只用 fixture 自带字段，**不许**引入任何「跑一遍才有」的数据）

分组键 = **`requires` 的键集合**（空 ⇒ 哨兵 `NO_REQUIRES`）。选它而不是 `detail` 的理由
只有一个，但很硬：**`contracts/golden/**` 里有 `requires`，没有 `detail`** ——
`detail` 是 `mc-conformance` 回放时在 Rust 侧按机读键现拼的说明文字，**不在契约里**。
所以 `requires` 是**唯一在两种输入下都存在、且划分完全相同**的判据。

`detail` 的条目键（`bare_handler_no_wiring: …`）与 `requires` **不是同一套**
（daemon 那条的 `requires` 有 `daemon_token` 而 `detail` 没有它的条目），两者都输出，
**别混用**。

三个坑，**先读再改**：

1. **最大的一族 `requires` 是空的**。`actor == agent` 的 fixture 的 `detail` 是一整句散文
   （`actor kind Agent needs a real credential; …`），`requires` 是空列表。
   ⇒ 「按 `requires` 分族」对最大的那族只能给出一个 `NO_REQUIRES` 组。
   对这一组，本脚本**再加一刀机械判据**：按 **`source` 文件**分桶
   （`chat_history_test.go` 11 条 / `issue_agent_create_origin_test.go` 1 条）——
   这两组的装置需求确实不同，逐个打开上游 setup 核过（`docs/37` 的派工单里逐条列了）。
2. **`detail` 里的 `||` 之后是同一段文本的 tier 重复**。`external_oauth` 那条 fixture 的
   `external_oauth: …` 出现了**两次**。按「出现次数」计数会把 `external_oauth` 记成 2、
   `bare_handler_no_wiring` 记成 3、`webhook_rate_limiter_denying` 记成 2 —— **三个都虚高**。
   本脚本按「**每个 fixture 每个键只计一次**」出键频次表。
3. **折叠是判断，不是判别式**。本脚本**不**对重叠的键做优先级取首 ——
   `{bare_handler_no_wiring, daemon_token, db_fault_injection}` 就是它自己的一组。
   一个 fixture 有几个未满足的前置，它就属于几个键的并集那一组。

## 两种输入（都能零编译跑完，`--golden` 那条**不需要真库、不需要 target/**）

    # A. db-mode 回放 json（族计数与 `stop_condition.sh` 的 T1-6 同源；对账到 unevaluable 总数）
    mc-conformance --golden contracts/golden --db-url "$DSN" --json > /tmp/db.json
    python3 scripts/t1_6_precondition_taxonomy.py /tmp/db.json

    # B. 纯静态扫描：直接读 `contracts/golden/**`，**零 Rust / 零 cargo / 零真库 / 零磁盘**
    #    （分族判据不许依赖「跑一遍才有」的数据 —— 这一条是本脚本能活过磁盘紧张的原因）
    python3 scripts/t1_6_precondition_taxonomy.py --golden contracts/golden

⚠️ **输入 B 重建的是一个超集，不是那 27 条。** `outcome` 是回放产物、不在契约里，
所以静态面只能给「**可能**落在本族」的候选集，判据（纯机械）：

    候选 = (`requires` 非空) ∪ (`actor.kind == "agent"`)

实测（`--golden`）：候选 **46** 条 / 子族 **10** 个，而 db-mode 实测是 **27** / **9**。
差出来的 19 条**当前不是 `unevaluable`**：其中 **18 条是 `requires == ["daemon_token"]`
单键**（就是 §239.3 那个「跨 workspace 反枚举簇」，它们落在 `REALM_DIFF` 族里，不是本族），
另 1 条是 `webhooks/TestStripeWebhookDisabledReturnsForbidden`
（带了 `cloud_runtime_configured` 却已被步 1 的 403 判掉）。
⇒ **B 模式的价值是「0 编译复现子族划分与逐条清单」，不是「算出 27」**；
`--json` 里用 `reconcilable: false` 标明，B 模式不谎报 27。
🔴 正因为如此，**族计数只有一个权威来源：db-mode json**。`docs/37` 里报 27 的地方
一律来自 A 模式，不要拿 B 模式的 29 去对账。

退出码恒为 0：它是**报告器**，不是门。绿红权威只在 `stop_condition.sh` 的 T1-6。
"""

from __future__ import annotations

import argparse
import collections
import json
import os
import re
import sys

# `- <key>: <text>` —— 只认行首一个短横线，缩进不限。
BULLET = re.compile(r"^\s*-\s+([A-Za-z_][A-Za-z_0-9]*):\s*(.+?)\s*$")

# `detail` 散文兜底：没有任何 `- ` 条目时用这句前缀判别（逐字抄自实测文本，不做模糊匹配）。
SENTINELS: tuple[tuple[str, str], ...] = (
    ("actor kind Agent needs a real credential", "AGENT_CREDENTIAL"),
)

NO_REQUIRES = "NO_REQUIRES"
NO_BULLET = "NO_BULLET"


def mechanism_keys(detail: str) -> list[str]:
    """`detail` → **去重后**的机制键列表（先切 `||`，再取行，再去重）。见坑 ②。"""
    keys: list[str] = []
    for segment in detail.split("||"):
        for line in segment.splitlines():
            m = BULLET.match(line)
            if m and m.group(1) not in keys:
                keys.append(m.group(1))
    return keys


def detail_sentinel(detail: str) -> str:
    if mechanism_keys(detail):
        return "+".join(sorted(set(mechanism_keys(detail))))
    for prefix, sentinel in SENTINELS:
        if detail.startswith(prefix):
            return sentinel
    return NO_BULLET


def group_key(requires: list[str]) -> str:
    return "+".join(sorted(set(requires))) if requires else NO_REQUIRES


def source_file(source: str) -> str:
    return (source or "").rsplit(":", 1)[0] if source else ""


# --------------------------------------------------------------------------- #
# 输入 A：mc-conformance --db-url --json
# --------------------------------------------------------------------------- #
def load_from_report(path: str) -> tuple[list[dict], dict, str, bool]:
    with open(path, encoding="utf-8") as fh:
        doc = json.load(fh)
    rows = []
    for fx in doc.get("fixtures", []):
        if fx.get("outcome") != "unevaluable":
            continue
        rows.append(
            {
                "id": fx.get("id"),
                "domain": fx.get("domain"),
                "method": fx.get("method"),
                "path": fx.get("path"),
                "actor": fx.get("actor"),
                "status_expected": fx.get("status_expected"),
                "requires": sorted(set(fx.get("requires") or [])),
                "detail": fx.get("detail") or "",
                "source": fx.get("source"),
            }
        )
    return rows, doc.get("totals", {}), "conformance_db_json", True


# --------------------------------------------------------------------------- #
# 输入 B：contracts/golden/** 的纯静态扫描
# --------------------------------------------------------------------------- #
def is_candidate(requires: list[str], actor: str) -> bool:
    """静态面的「可能落在本族」判据（见模块 docstring 的输入 B 段）。"""
    return bool(requires) or actor == "agent"


def load_from_golden(root: str) -> tuple[list[dict], dict, str, bool]:
    rows: list[dict] = []
    for dirpath, _dirnames, filenames in os.walk(root):
        for name in sorted(filenames):
            if not name.endswith(".json"):
                continue
            try:
                with open(os.path.join(dirpath, name), encoding="utf-8") as fh:
                    doc = json.load(fh)
            except (OSError, ValueError):
                continue  # 非 fixture 的 json（baseline / 索引类）不是契约，跳过
            if not isinstance(doc, dict) or "id" not in doc or "expect" not in doc:
                continue
            extraction = doc.get("extraction") or {}
            src = doc.get("source") or {}
            requires = sorted(set(extraction.get("requires") or []))
            actor = (doc.get("actor") or {}).get("kind")
            if not is_candidate(requires, str(actor)):
                continue
            # golden 不带 `detail`（见模块 docstring）。`NO_REQUIRES` 桶靠散文兜底不可用，
            # 于是这里按 `actor` 记一条等价的机械标注，供对拍时辨认。
            detail = "" if requires else "{}: (golden 未记录 detail)".format(actor)
            rows.append(
                {
                    "id": doc.get("id"),
                    "domain": (doc.get("id") or "").split("/", 1)[0],
                    "method": doc.get("method"),
                    "path": doc.get("path"),
                    "actor": actor,
                    "status_expected": (doc.get("expect") or {}).get("status"),
                    "requires": requires,
                    "detail": detail,
                    "source": "{}/{}".format(src.get("file", ""), src.get("line", "")),
                }
            )
    rows.sort(key=lambda r: r["id"] or "")
    return rows, {}, "contracts_golden", False


# --------------------------------------------------------------------------- #


def build(rows: list[dict], totals: dict, input_kind: str, reconcilable: bool) -> dict:
    groups: dict[str, list[dict]] = collections.defaultdict(list)
    for row in rows:
        groups[group_key(row["requires"])].append(row)

    subfamilies = []
    for key, items in sorted(groups.items(), key=lambda kv: (-len(kv[1]), kv[0])):
        texts = {f["detail"] for f in items if f["detail"]}
        uniform = len(texts) == 1
        # `NO_REQUIRES` 桶按上游测试文件再切一刀（模块 docstring 坑 ①）。
        buckets: dict[str, list[dict]] = collections.defaultdict(list)
        for f in items:
            buckets[source_file(f["source"])].append(f)
        subfamilies.append(
            {
                "name": key,
                "mechanism_keys": [] if key == NO_REQUIRES else key.split("+"),
                "count": len(items),
                "by_domain": dict(
                    collections.Counter(f["domain"] for f in items).most_common()
                ),
                "by_actor": dict(
                    collections.Counter(str(f["actor"]) for f in items).most_common()
                ),
                "requires_union": sorted({k for f in items for k in f["requires"]}),
                "detail_keys": sorted({detail_sentinel(f["detail"]) for f in items}),
                "detail_uniform": uniform,
                "detail": max(texts, key=len) if texts else "",
                "by_source_file": {
                    name: len(bucket)
                    for name, bucket in sorted(buckets.items(), key=lambda kv: -len(kv[1]))
                },
                "fixtures": [
                    {
                        "id": f["id"],
                        "domain": f["domain"],
                        "method": f["method"],
                        "path": f["path"],
                        "actor": f["actor"],
                        "status_expected": f["status_expected"],
                        "requires": f["requires"],
                        "source": f["source"],
                    }
                    for f in sorted(items, key=lambda x: (x["domain"], x["id"] or ""))
                ],
            }
        )

    # 键频次：按 **fixture** 计（不是按 `detail` 里的出现次数）—— 见坑 ②。
    freq: collections.Counter = collections.Counter()
    for row in rows:
        for k in set(mechanism_keys(row["detail"])) or {NO_BULLET}:
            freq[k] += 1

    accounted = sum(s["count"] for s in subfamilies)
    return {
        "input_kind": input_kind,
        "totals": totals,
        "reconcilable": reconcilable,
        "candidates": len(rows),
        "subfamily_count": len(subfamilies),
        "reconciliation": {
            "sum_of_subfamilies": accounted,
            "candidates": len(rows),
            "balanced": accounted == len(rows),
            "equals_unevaluable": accounted == totals.get("unevaluable")
            if reconcilable
            else None,
        },
        "mechanism_key_frequency_by_fixture": dict(freq.most_common()),
        "subfamilies": subfamilies,
    }


def render_human(out: dict) -> None:
    rec = out["reconciliation"]
    print(
        "T1-6 PRECONDITION 子族  input={}  候选 {}  子族 {}".format(
            out["input_kind"], out["candidates"], out["subfamily_count"]
        )
    )
    if out["reconcilable"]:
        print(
            "  对账：子族条数之和 {} == unevaluable {}  ⇒ {}".format(
                rec["sum_of_subfamilies"],
                out["totals"].get("unevaluable"),
                "平" if rec["equals_unevaluable"] else "**不平**",
            )
        )
    else:
        print(
            "  对账：子族条数之和 {} == 候选 {}（{}）—— **本模式是超集，不与 unevaluable "
            "对账**：`outcome` 是回放产物、不在契约里。判据 = `requires` 非空 ∪ "
            "`actor.kind == agent`。族计数只有一个权威来源 = db-mode json。".format(
                rec["sum_of_subfamilies"], rec["candidates"],
                "平" if rec["balanced"] else "**不平**"
            )
        )
    print("  机制键频次（按 fixture 计，已去重）：{}".format(
        "  ".join("{}={}".format(k, v)
                  for k, v in out["mechanism_key_frequency_by_fixture"].items())))
    print()
    for i, s in enumerate(out["subfamilies"], 1):
        print("── [{}] {}  {} 条".format(i, s["name"], s["count"]))
        print("   域分布：{}".format(
            "  ".join("{}={}".format(d, c) for d, c in s["by_domain"].items())))
        print("   actor ：{}".format(
            "  ".join("{}={}".format(a, c) for a, c in s["by_actor"].items())))
        print("   detail 条目键：{}".format(", ".join(s["detail_keys"])))
        print("   detail 逐字统一：{}".format("是" if s["detail_uniform"] else "**否**"))
        print("   上游文件：{}".format(
            "  ".join("{}={}".format(k or "?", v) for k, v in s["by_source_file"].items())))
        for f in s["fixtures"]:
            print("      - {:<10} {:<6} {:<4} {} → {}".format(
                f["domain"], f["method"], f["actor"], f["id"], f["status_expected"]))
        print()


def main() -> int:
    ap = argparse.ArgumentParser(description=(__doc__ or "").splitlines()[0])
    src = ap.add_mutually_exclusive_group(required=True)
    src.add_argument("report", nargs="?", help="mc-conformance --db-url --json 的输出")
    src.add_argument(
        "--golden",
        help="contracts/golden 目录（纯静态扫描，零编译 / 零真库 / 零磁盘）",
    )
    ap.add_argument("--json", action="store_true", help="输出机器可读 JSON")
    args = ap.parse_args()

    if args.golden:
        rows, totals, kind, ok = load_from_golden(args.golden)
    else:
        rows, totals, kind, ok = load_from_report(args.report)

    out = build(rows, totals, kind, ok)
    if args.json:
        print(json.dumps(out, ensure_ascii=False, indent=2))
    else:
        render_human(out)
    return 0


if __name__ == "__main__":
    sys.exit(main())
