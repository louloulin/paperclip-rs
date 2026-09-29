"""T1-6 `REALM_DIFF` 族归因脚本（由单文件 `t1_6_realm_diff_taxonomy.py` 拆出，零行为变更）。

- 入口：`python3 -m scripts.t1_6_realm_diff_taxonomy <report.json>` 或
  `python3 -m scripts.t1_6_realm_diff_taxonomy --golden contracts/golden`
- 各模块的职责见 `__main__.py` 顶部的「模块切分」一节。

本文件只做再导出，方便 `from scripts.t1_6_realm_diff_taxonomy import build` 这类用法。
"""

from __future__ import annotations

from .checks import (
    BY_DESIGN_AUDIT,
    KNOWN_NEGATIVE,
    KNOWN_POSITIVE,
    discriminant_checks,
)
from .claims import CLAIM_CRITERION, CLAIM_EXPECTED, CLAIM_NAME, is_claimed
from .constants import (
    BEHAVIOR,
    BY_DESIGN,
    EXTRACTION,
    FAMILY,
    FAMILY_CRITERION,
    FIXTURE,
    OTHER_FAMILY_OBSERVED,
    OWNER_FILES,
)
from .fields import body, exp, golden, identity, method, obs, path, query
from .report import build, summarise
from .rules import (
    STATIC_DECIDABLE,
    SUB_RULES,
    classify,
    transition,
    upstream_file,
)
from .sources import attach_golden, load_from_golden, load_from_report

__all__ = [
    # constants
    "FAMILY",
    "FAMILY_CRITERION",
    "OTHER_FAMILY_OBSERVED",
    "EXTRACTION",
    "FIXTURE",
    "BEHAVIOR",
    "BY_DESIGN",
    "OWNER_FILES",
    # fields
    "golden",
    "identity",
    "query",
    "body",
    "exp",
    "obs",
    "method",
    "path",
    # sources
    "load_from_report",
    "load_from_golden",
    "attach_golden",
    # claims
    "CLAIM_NAME",
    "CLAIM_CRITERION",
    "CLAIM_EXPECTED",
    "is_claimed",
    # rules
    "SUB_RULES",
    "STATIC_DECIDABLE",
    "classify",
    "transition",
    "upstream_file",
    # checks
    "KNOWN_POSITIVE",
    "KNOWN_NEGATIVE",
    "BY_DESIGN_AUDIT",
    "discriminant_checks",
    # report
    "summarise",
    "build",
]
