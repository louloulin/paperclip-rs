# --------------------------------------------------------------------------- #
# 组装
# --------------------------------------------------------------------------- #

from __future__ import annotations

import collections

from .checks import BY_DESIGN_AUDIT, discriminant_checks
from .claims import CLAIM_CRITERION, CLAIM_EXPECTED, CLAIM_NAME, is_claimed
from .constants import (
    BEHAVIOR,
    BEHAVIOR_OWNER_FILES,
    EXTRACTION,
    FAMILY,
    FAMILY_CRITERION,
    FIXTURE,
    OTHER_FAMILY_OBSERVED,
    OWNER_FILES,
)
from .fields import obs
from .rules import STATIC_DECIDABLE, SUB_RULES, classify, transition, upstream_file


def owner_files_for(subfamily: str, attribution: str) -> list[str]:
    """子族负责面：**归因级**表优先（抽取面 / 装置面按归因聚合），否则查**子族级**表。

    两级都查不到 ⇒ 返回 `[]`（空串），并由 `verdict()` 把该 lane 降级为「未定」。
    """
    lane_files = OWNER_FILES.get(attribution, [])
    if lane_files:
        return lane_files
    if attribution == BEHAVIOR:
        return BEHAVIOR_OWNER_FILES.get(subfamily, [])
    return []


def verdict(entry: dict) -> None:
    """就地写入 lane 的并行结论（`parallel` / `parallel_why`）——**不手工写死**。

    三档：
      · `serial`     —— 已知同写集（`serial_with` 非空）；
      · `parallel`   —— 每个子族的负责面都非空，且子族之间**两两不相交**；
      · `undetermined` —— 有子族负责面为空串 ⇒ 证据缺列，**不许**报「可并行：是」。

    这条是本模块 2026-09-29 22:00 cycle 补的：此前 `__main__` 无条件打
    「行为面 —— 可并行：是（各域 handler 互不相交）」，而那一列当时是空串。
    """
    if entry["serial_with"]:
        entry["parallel"] = "serial"
        entry["parallel_why"] = "同写集，必须串行：{}".format("；".join(entry["serial_with"]))
        return
    empty = [n for n, f in entry["subfamily_owner_files"].items() if not f]
    if empty:
        entry["parallel"] = "undetermined"
        entry["parallel_why"] = (
            "子族 {} 的负责面是**空串** ⇒ 结论所依赖的证据缺列（读代码补 `constants.py` "
            "的 `BEHAVIOR_OWNER_FILES`，不要在 `__main__` 里手写「可并行：是」）".format(
                "、".join(sorted(empty)))
        )
        return
    seen: dict[str, str] = {}
    for name, files in sorted(entry["subfamily_owner_files"].items()):
        for f in files:
            if f in seen and seen[f] != name:
                entry["parallel"] = "serial"
                entry["parallel_why"] = (
                    "{} 与 {} 都写 `{}` ⇒ 写集相交，不能并行".format(seen[f], name, f))
                return
            seen.setdefault(f, name)
    entry["parallel"] = "parallel"
    entry["parallel_why"] = (
        "{} 个子族的负责面均已填写，且文件集合两两不相交".format(len(entry["subfamily_owner_files"]))
    )


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
                "owner_files": owner_files_for(name, attribution),
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
            "subfamily_owner_files": {},
        })
        entry["count"] += s["count"]
        entry["subfamilies"].append(s["name"])
        entry["subfamily_owner_files"][s["name"]] = s["owner_files"]
    for lane, entry in lanes.items():
        verdict(entry)
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
