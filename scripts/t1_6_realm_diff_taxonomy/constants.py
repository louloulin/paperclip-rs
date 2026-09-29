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
