#!/usr/bin/env python3
r"""判定器测试覆盖门：`ci.yml` / `gates.sh` 真正执行的每个 `scripts/**/<name>.py`
必须存在 `scripts/**/test_<name>.py`（门 ⑭；`LUM-2626` / `T1-6-G1`，`docs/37 §296`）。

这片关掉的风险
--------------
过去 6 个 cycle（`LUM-2606` / `2608` / `2617` / `2620` / `2621` / `2623` / `2624`）
做的事**只有一个**：给 `ci.yml` 直接引用的判定器补测试。那一族今天收官了 ——
cycle 14:30 实测四条引用面（`file_size_check.py` / `route_parity.py` /
`schema_drift.py` / `section_alloc_check.py`）**全部**都有 `test_<同名>.py`。
**但那是运气，不是门。** 下一个新判定器只要被加进 `ci.yml` 或 `ALL_GATES`，
就会**静默地**重演这一族：`docs/37 §292.2` / `§293.4` 实测过这种形态 ——
「它每天在 `ci.yml:90` 的 `fast` 必过 job 里跑生产判词，却既不在 `scripts/tests.manifest`
（没有任何东西执行它的代码）也不在 `scripts/file_size_baseline.tsv`
（门 ⑩ 只能按行数判它）」。本脚本把那两句文档变成一条会红的门。

判据（一条；任何一条不满足 ⇒ exit 1）
------------------------------------
C1 **凡是被引用面真正执行**的 `scripts/**/<name>.py`（`<name>` 不以 `test_` 开头），
   **必须**存在 `scripts/**/test_<name>.py`（任意一层子目录都行）。
C1' **包入口是同一个判据的包形态**：`scripts/<pkg>/__main__.py` 与 `scripts/<pkg>/__init__.py`
   的期望测试名是 **`<pkg>`**（即 `test_<pkg>.py`），**不是** `__main__` / `__init__`。
   （`LUM-2629` / `T1-6-R1`，`docs/37 §300`；修的是一个**假红**。）
C2 **`python3 -m <module>` 形态也是引用**：`python3 -m scripts.foo.bar` 解析成磁盘上那个
   真正被执行的实体（优先 `scripts/foo/bar/__main__.py`，否则 `scripts/foo/bar.py`），
   于是它**进读数、也要有测试**。**被门执行却没进读数** = 假绿，比假红贵。

引用面 = 两个，且**只认真正被执行的那几行**
------------------------------------------
A. `.github/workflows/*.yml`
B. `scripts/gates.sh`（`ALL_GATES` 的调度实现所在文件）

🔴 **本脚本里不许写死那四个文件名**（`ci.yml:82` 的注释明写「本仓纪律：命令唯一实现」）。
输入必须**从上面两个面解析出来**。实测过一次的反例（`LUM-2602`）：
`assertIn("find scripts", gates.sh 的源码)` 这类断言**会被一行注释满足**
（`ci.yml:89` 那行注释提到 `python3 scripts/section_alloc_check.py`，
但那个脚本是经 `gates.sh` 执行的，不是经这行）。所以：

* **yml**：只收 `name:` / `run:` 两种键的**值行**，以及 `run: |` / `run: >`
  这类块标量之后的**更深缩进行**；以 `#` 开头的行**整行丢弃**。
  （本仓约定把命令写在 step 的 `name:` 里 —— `ci.yml:75`
  `- name: "⑩ file-size — python3 scripts/file_size_check.py --quiet"` ——
  真正的执行在下一行 `run: bash scripts/gates.sh --only file-size`。
  两种形态都收，收的是**会进到日志 / CI 面板里的那行文本**。）
* **gates.sh**：收**非注释、非空行**；**跳过 `printf` 开头的行**（那是把命令
  *显示*出来给人看，不是执行 —— `gates.sh:439` 就是这种）。

包形态的判词（`__init__.py` 单独被引用要不要豁免？—— **不豁免**）
------------------------------------------------------------------
`python3 scripts/foo/__main__.py` 和 `python3 -m scripts.foo` 描述的是**同一个包**，
`python3 scripts/foo/__init__.py` 也是。把它们判成三种不同的期望名（`test___main__.py` /
`test_foo.py` / `test___init__.py`）等于给同一个东西开三扇门，而其中两扇**没人会满足** ——
`test___main__.py` 这个文件名**没有人类会写**，于是**按正确约定接线的人永远判红**。
所以本门只保留**一条**规则：**包入口 ⇒ 期望测试名 = 包名**。判词（`--quiet` 之外的读数面
逐条打出来，`gaps` 里也逐条点名）：

* `scripts/<pkg>/__main__.py`  ⇒ 期望 `scripts/**/test_<pkg>.py`
* `scripts/<pkg>/__init__.py` ⇒ 期望 `scripts/**/test_<pkg>.py`（**不豁免**）

**不豁免的代价**是刻意的：豁免它就等于开一个洞 —— 有人只写 `python3 scripts/foo/__init__.py`
就能让整个包逃出这道门。豁免的**唯一**情形是「它根本不是包入口」：顶层 `scripts/__init__.py`
的父目录就是 `scripts` 本身（没有包名可取）⇒ 不判红（也不进读数）。

排除项
------
* `gates.sh` 自身（它不是 `.py`，正则天然进不来；仍显式排除以防将来放宽正则）。
* `test_*.py` 自身 —— 测试文件不需要自己的测试。
* glob（`scripts/test_*.py`、`scripts/**/test_*.py`）—— 正则的字符类里没有 `*`，
  所以它们**不可能**被当成一条具体的脚本引用收进来（这是 `ci.yml:83` 那条 step
  不会被误当成「一个没有测试的判定器」的原因）。

前置判据（`§276` 的教训）
-------------------------
「没有东西可校验」**不许**读成绿。两个前置件都是判据：

* 引用面文件不存在（`gates.sh` / `.github/workflows/` 缺）⇒ 红。
* 解析结果**一个脚本都没引用到** ⇒ 红（否则「解析规则写坏了」与「没人引用」同形）。

用法
----
    python3 scripts/judge_test_coverage_check.py            # 逐条读数
    python3 scripts/judge_test_coverage_check.py --quiet    # 只打一行总结（门 ⑭ 用）
    bash scripts/gates.sh --only judge-test-coverage        # 门

本脚本是**只读**的：它不改任何文件，只判红 / 判绿。

已知边界（**会漏、也会误报**的地方，见 `docs/37 §296.4`）
--------------------------------------------------------
会**漏**（真缺口但门绿）：
1. 只经**变量间接**执行的判定器（`X="scripts/foo.py"` 那行字面量仍在 ⇒ 能收；
   但若路径是 `python3 "$DIR/foo.py"` 拼出来的 ⇒ 一个字面量都没有 ⇒ 漏）。
2. 经**第三个**包装器（`Makefile` / `justfile` / 另一个 `*.sh`）执行的判定器。
3. 判定器 A 用 `subprocess` 调判定器 B（`gates.sh` / `ci.yml` 里没有 B 的字面量）。
4. 非 `.py` 的判定器（`.sh` / 无扩展名）—— 本门只按 `scripts/**/<name>.py` 收。
5. 真的叫 `test_foo.py` 但**不是** unittest 的判定器（被 `test_` 前缀豁免掉）。
6. `run:` 块标量里用 `\` 续行、路径被拆成 `"$SCRIPTS"/foo.py` 的写法。
7. `python3 -m <module>` 里 `<module>` **不是** `scripts.` 开头的（`python3 -m pytest`
   / `-m unittest`）—— 本门只收 `scripts/**`，别的包树不在本仓的管辖面里。
8. `python3 -m` 与包名被**换行 / 变量**拆开（`python3 -m \\\n  scripts.foo`）——
   `MODULE_REF_RE` 是单行正则。

会**误报**（无缺口但门红）：
7. 一个只在 `gates.sh` 的**非 printf 非注释行**里以字面量出现、但其实从未执行的脚本
   （例如某个 `if false` 分支里的路径）会被要求有测试。
9. 判定器被**改名**而旧 `test_<旧名>.py` 还在 ⇒ 旧名进不了引用面、不判红，
   但新名会判红 —— 这是**设计意图**（就是要你同步改名），不算误报。
10. 一行**同时**被当成两种形态（`python3 scripts/foo/bar.py -m scripts.foo`）会记两条
    引用。它们解析到**同一个**路径，缺口按**实体**去重（只点名一次），所以不是双计数。
"""

from __future__ import annotations

import argparse
import os
import re
import sys
from pathlib import Path

TEST_PREFIX = "test_"

# 一条**具体的** `scripts/**/<name>.py` 引用。字符类里刻意没有 `*`、`$`、`"`：
# => `scripts/test_*.py`、`scripts/**/test_*.py`、`python3 "$DIR/foo.py"` 都进不来。
SCRIPT_REF_RE = re.compile(r"scripts/[A-Za-z0-9_./-]+\.py")

# `python3 -m scripts.foo.bar` —— **模块形态**的引用（`LUM-2629` / `§300` 缺陷 2）。
# 必须以 `scripts.` 开头，否则 `python3 -m pytest` / `-m unittest` 会被收进来。
# 前导 `(?:^|\s)` 是为了不把 `--rootdir` / `-mtime` 之类的尾巴切下来。
MODULE_REF_RE = re.compile(r"(?:^|\s)-m\s+(scripts(?:\.[A-Za-z_][A-Za-z0-9_]*)+)")

# `--dry` 行 = **试跑**，不要求有测试（工单硬约束）。但它**进读数并被逐条打出来**
# （`[DRY]` 标记）—— 不静默丢弃，否则「`--dry`」就成了绕过这道门的开关。
DRY_FLAG_RE = re.compile(r"(?:^|\s)--dry(?:\s|$)")

# 包入口文件：basename 是这两个时，期望测试名取**包名**（父目录名），不是 basename。
PACKAGE_ENTRY_NAMES = frozenset({"__main__", "__init__"})

# yml 里只有这两种键的**值行**是「会进到 CI 面板 / 被执行」的那几行。
YML_STEP_KEY_RE = re.compile(r"^(\s*)(?:-\s+)?(?:name|run)\s*:\s*(.*)$")
YML_BLOCK_VALUES = frozenset({"|", ">", "|-", ">-", "|+", ">+"})

# `printf ...` 是把命令**显示**出来，不是执行（`gates.sh:439` 的形态）。
SH_PRINTF_RE = re.compile(r"^\s*printf\b")


def _surfaces(root: Path) -> tuple[Path, Path, Path]:
    scripts_dir = root / "scripts"
    return scripts_dir, scripts_dir / "gates.sh", root / ".github" / "workflows"


def _rel(path: Path, root: Path) -> str:
    try:
        return str(path.relative_to(root))
    except ValueError:
        return str(path)


def yml_executable_lines(path: Path) -> list[tuple[int, str]]:
    """返回 [(行号, 行文本), ...]：yml 里**会被执行 / 会出现在 CI 面板上**的行。

    丢弃：空行、以 `#` 开头的整行注释。
    保留：`name:` / `run:` 的值行，以及 `run: |` 块标量之后更深缩进的行。
    """
    out: list[tuple[int, str]] = []
    block_indent: int | None = None
    for lineno, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        body = line.strip()
        if not body or body.startswith("#"):
            # 注释里的 `scripts/x.py` 是**散文**，不是引用（`ci.yml:89` 实测）。
            continue
        indent = len(line) - len(line.lstrip())
        if block_indent is not None:
            if indent > block_indent:
                out.append((lineno, line))
                continue
            block_indent = None
        m = YML_STEP_KEY_RE.match(line)
        if not m:
            continue
        out.append((lineno, line))
        if m.group(2).strip() in YML_BLOCK_VALUES:
            block_indent = indent
    return out


def sh_executable_lines(path: Path) -> list[tuple[int, str]]:
    """返回 [(行号, 行文本), ...]：gates.sh 里**非注释、非空、非纯 printf 显示**的行。"""
    out: list[tuple[int, str]] = []
    for lineno, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        body = line.strip()
        if not body or body.startswith("#"):
            continue
        if SH_PRINTF_RE.match(line):
            continue
        out.append((lineno, line))
    return out


def reference_lines(root: Path) -> list[tuple[str, int, str]]:
    """返回 [(引用面, 行号, 行文本), ...]，跨全部引用面合并（面内保持行序）。"""
    scripts_dir, gates_sh, workflow_dir = _surfaces(root)
    out: list[tuple[str, int, str]] = []
    if workflow_dir.is_dir():
        for yml in sorted(workflow_dir.glob("*.yml")):
            for lineno, line in yml_executable_lines(yml):
                out.append((_rel(yml, root), lineno, line))
    if gates_sh.is_file():
        for lineno, line in sh_executable_lines(gates_sh):
            out.append((_rel(gates_sh, root), lineno, line))
    return out


def existing_test_names(root: Path) -> set[str]:
    """`scripts/**/test_<name>.py` 的 `<name>` 集合（递归，排除 `__pycache__`）。"""
    scripts_dir, _, _ = _surfaces(root)
    names: set[str] = set()
    if not scripts_dir.is_dir():
        return names
    for p in scripts_dir.rglob("test_*.py"):
        if "__pycache__" in p.parts or not p.is_file():
            continue
        # `.stem` 而非切片：`p.name[len(TEST_PREFIX):]` 会把 `.py` 一起留下
        # （`'test_x.py'[5:]` == `'x.py'`）⇒ 与 `judge_stem()` 的返回值永不相等
        # ⇒ 全部判成 GAP。两条路径必须产出**同一个** stem。
        names.add(p.stem[len(TEST_PREFIX):])
    return names


def module_ref_to_path(root: Path, module: str) -> str:
    """`-m scripts.foo.bar` → 磁盘上真正被执行的实体的仓库相对路径。

    包优先（`scripts/foo/bar/__main__.py`），否则按普通模块（`scripts/foo/bar.py`）。
    两者都不存在时**不猜**：仍按普通模块形态返回，让它照常进读数并在缺测试时判红 ——
    静默丢弃一条引用就是缺陷 2 那种假绿。
    """
    rel = module.replace(".", "/")
    if (root / rel / "__main__.py").is_file():
        return f"{rel}/__main__.py"
    return f"{rel}.py"


def judge_stem(ref: str) -> str | None:
    """`scripts/foo/bar.py` → `bar`；`scripts/foo/__main__.py` → `foo`（**包名**）；
    `scripts/test_x.py` → None（豁免）；glob → 永不进来。

    🔴 包入口这一支是 `LUM-2629` / `§300` 缺陷 1 的修复。旧版只取 basename ⇒
    `scripts/t1_6_realm_diff_taxonomy/__main__.py` 被要求 `scripts/**/test___main__.py`
    —— **一个没人会写的文件名** ⇒ 按本仓正确约定（`test_<包名>.py`）接线的人**永远判红**。
    一个**不可满足**的判词比没有判词更糟：它训练所有人忽略这道门。
    """
    name = ref.rsplit("/", 1)[-1]
    if not name.endswith(".py"):
        return None
    stem = name[:-3]
    if stem.startswith(TEST_PREFIX):
        return None
    if name == "gates.sh" or stem == "gates":
        return None
    if stem not in PACKAGE_ENTRY_NAMES:
        return stem
    # 包入口 ⇒ 期望测试名 = **包名**。顶层 `scripts/__init__.py` 没有包名可取 ⇒ 豁免
    # （它不是包入口，是「scripts 本身是包」这个前提没成立）。
    parts = ref.rsplit("/", 2)
    if len(parts) < 3:
        return None
    pkg = parts[-2]
    return pkg if pkg.isidentifier() else None


def read_judgement(root: Path) -> dict:
    """跑一遍判据，返回结构化结果（测试与 `--quiet` 共用这一份唯一实现）。"""
    scripts_dir, gates_sh, workflow_dir = _surfaces(root)
    defects: list[str] = []

    if not gates_sh.is_file():
        defects.append(f"reference surface missing: {_rel(gates_sh, root)}")
    if not workflow_dir.is_dir():
        defects.append(f"reference surface missing: {_rel(workflow_dir, root)}/")

    tests = existing_test_names(root)

    # (脚本引用, 引用面, 行号, 是否 --dry 试跑) —— 一个脚本被多处引用就都记，缺陷逐条点名。
    # 🔴 两种**路径形态**都必须收进来，否则「被门执行却没进读数」= 假绿（`§300` 缺陷 2）：
    #   1. `SCRIPT_REF_RE`  —— `python3 scripts/foo/bar.py`（含 `scripts/foo/__main__.py`）
    #   2. `MODULE_REF_RE`  —— `python3 -m scripts.foo.bar`（**必须**解析成磁盘上那个实体，
    #      否则它的期望测试名会算在 `bar` 而不是包/模块上）
    refs: list[tuple[str, str, int, bool]] = []
    seen: set[tuple[str, str, int, bool]] = set()

    def _add(ref: str, surface: str, lineno: int, dry: bool) -> None:
        key = (ref, surface, lineno, dry)
        if key not in seen:
            seen.add(key)
            refs.append(key)

    for surface, lineno, line in reference_lines(root):
        dry = bool(DRY_FLAG_RE.search(line))
        for m in SCRIPT_REF_RE.finditer(line):
            _add(m.group(0), surface, lineno, dry)
        for m in MODULE_REF_RE.finditer(line):
            _add(module_ref_to_path(root, m.group(1)), surface, lineno, dry)

    gaps: list[dict] = []
    judged: set[str] = set()
    dry_refs: list[tuple[str, str, int]] = []
    gap_refs: set[str] = set()
    candidates = 0
    for ref, surface, lineno, dry in refs:
        stem = judge_stem(ref)
        if stem is None:
            continue
        if dry:
            # `--dry` 试跑**不要求**有测试（工单硬约束），但**进读数**并逐条打出来 ——
            # 静默丢弃就等于给了「加个 --dry」这个绕过门的方式。
            dry_refs.append((ref, surface, lineno))
            continue
        candidates += 1
        judged.add(ref)
        if stem in tests or ref in gap_refs:
            # `ref in gap_refs`：**同一个实体**（两种引用形态、或两个引用面各写一次）只报
            # 一条缺口。refs 的去重键含行号（为了逐条点名），所以不靠它去重缺口。
            continue
        gap_refs.add(ref)
        gaps.append(
            {
                "ref": ref,
                "surface": surface,
                "line": lineno,
                "expects": f"scripts/**/test_{stem}.py",
            }
        )
        defects.append(
            f"{surface}:{lineno}: {ref} is executed but has no test — "
            f"expected scripts/**/test_{stem}.py"
        )

    # 🔴 前置判据：一条**可判的**引用都没解析到 ⇒ 红。否则「解析规则写坏了」与「没人引用」
    # 同形（`§276`）。数的是**非 dry 且 judge_stem 非 None** 的引用：一条 `--dry` 试跑
    # 证明了解析器活着，但它本身不构成「有东西要判」。
    if not candidates:
        defects.append(
            "no scripts/**/<name>.py reference was parsed from the reference surfaces "
            "(the parser is broken, or nothing is wired up)"
        )

    surfaces = sorted({surface for _, surface, _, _ in refs})
    return {
        "root": root,
        "defects": defects,
        "gaps": gaps,
        "refs": refs,
        "dry_refs": dry_refs,
        "judged": sorted(judged),
        "covered": sorted(judged - gap_refs),
        "surfaces": surfaces,
        "sections": len(surfaces),
        "numbers": len(judged),
        "tests": len(tests),
    }


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description="judge every ci.yml / gates.sh-executed scripts/**/<name>.py for a test_*.py"
    )
    ap.add_argument("--quiet", action="store_true", help="只打一行总结（门 ⑭ 的判词面）")
    ap.add_argument("--root", default=None, help="换一个仓库根目录（测试用）")
    args = ap.parse_args(argv)

    root = Path(args.root).resolve() if args.root else Path(__file__).resolve().parent.parent
    rd = read_judgement(root)

    if not args.quiet:
        print("judge-test-coverage: 引用面 = .github/workflows/*.yml + scripts/gates.sh")
        for ref, surface, lineno, dry in rd["refs"]:
            print(f"  {surface}:{lineno}: 引用 {ref}{'（--dry 试跑，不要求有测试）' if dry else ''}")
        print(f"  ⇒ 被执行且非 test_* 的判定器: {rd['numbers']} 个")
        gap_set = {g["ref"] for g in rd["gaps"]}
        for ref in rd["judged"]:
            # 报出的期望名必须与判词**同一个** `judge_stem()`：这一行自己就曾是缺陷 1 的
            # 第二个现场（读数面打 `test_<basename>`，判词面要 `test_<包名>`）。
            stem = judge_stem(ref)
            mark = "GAP" if ref in gap_set else "OK "
            print(f"    [{mark}] {ref}  ->  test_{stem}.py")
        for ref, surface, lineno in rd["dry_refs"]:
            print(f"    [DRY] {surface}:{lineno}: {ref}  ->  test_{judge_stem(ref)}.py（试跑，不判红）")
        print(f"  ⇒ 现有 scripts/**/test_*.py: {rd['tests']} 个")
        for d in rd["defects"]:
            print(f"error: {d}", file=sys.stderr)

    if rd["defects"]:
        print(
            "judge-test-coverage: FAIL — %d gap(s) / %d defect(s)"
            % (len(rd["gaps"]), len(rd["defects"])),
            flush=True,
        )
        return 1
    print(
        "judge-test-coverage: OK — surfaces=%d judges=%d covered=%d gaps=0 dry=%d"
        % (rd["sections"], rd["numbers"], len(rd["covered"]), len(rd["dry_refs"])),
        flush=True,
    )
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except BrokenPipeError:  # `| head` — the reader went away, that is not a failure
        devnull = os.open(os.devnull, os.O_WRONLY)
        os.dup2(devnull, 1)
        sys.exit(0)