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

会**误报**（无缺口但门红）：
7. 一个只在 `gates.sh` 的**非 printf 非注释行**里以字面量出现、但其实从未执行的脚本
   （例如某个 `if false` 分支里的路径）会被要求有测试。
8. 判定器被**改名**而旧 `test_<旧名>.py` 还在 ⇒ 旧名进不了引用面、不判红，
   但新名会判红 —— 这是**设计意图**（就是要你同步改名），不算误报。
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


def judge_stem(ref: str) -> str | None:
    """`scripts/foo/bar.py` → `bar`；`scripts/test_x.py` → None（豁免）；glob → 永不进来。"""
    name = ref.rsplit("/", 1)[-1]
    if not name.endswith(".py"):
        return None
    stem = name[:-3]
    if stem.startswith(TEST_PREFIX):
        return None
    if name == "gates.sh" or stem == "gates":
        return None
    return stem


def read_judgement(root: Path) -> dict:
    """跑一遍判据，返回结构化结果（测试与 `--quiet` 共用这一份唯一实现）。"""
    scripts_dir, gates_sh, workflow_dir = _surfaces(root)
    defects: list[str] = []

    if not gates_sh.is_file():
        defects.append(f"reference surface missing: {_rel(gates_sh, root)}")
    if not workflow_dir.is_dir():
        defects.append(f"reference surface missing: {_rel(workflow_dir, root)}/")

    tests = existing_test_names(root)

    # (脚本引用, 引用面, 行号) —— 一个脚本被多处引用就都记，缺陷逐条点名。
    refs: list[tuple[str, str, int]] = []
    seen: set[tuple[str, str, int]] = set()
    for surface, lineno, line in reference_lines(root):
        for m in SCRIPT_REF_RE.finditer(line):
            key = (m.group(0), surface, lineno)
            if key not in seen:
                seen.add(key)
                refs.append(key)

    gaps: list[dict] = []
    judged: set[str] = set()
    for ref, surface, lineno in refs:
        stem = judge_stem(ref)
        if stem is None:
            continue
        judged.add(ref)
        if stem in tests:
            continue
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

    # 🔴 前置判据：解析结果为空 ⇒ 红。否则「解析规则写坏了」与「没人引用」同形。
    if not refs:
        defects.append(
            "no scripts/**/<name>.py reference was parsed from the reference surfaces "
            "(the parser is broken, or nothing is wired up)"
        )

    gap_refs = {g["ref"] for g in gaps}
    surfaces = sorted({surface for _, surface, _ in refs})
    return {
        "root": root,
        "defects": defects,
        "gaps": gaps,
        "refs": refs,
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
        for ref, surface, lineno in rd["refs"]:
            print(f"  {surface}:{lineno}: 引用 {ref}")
        print(f"  ⇒ 被执行且非 test_* 的判定器: {rd['numbers']} 个")
        for ref in rd["judged"]:
            stem = ref.rsplit("/", 1)[-1][:-3]
            mark = "GAP" if ref in {g["ref"] for g in rd["gaps"]} else "OK "
            print(f"    [{mark}] {ref}  ->  test_{stem}.py")
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
        "judge-test-coverage: OK — surfaces=%d judges=%d covered=%d gaps=0"
        % (rd["sections"], rd["numbers"], len(rd["covered"])),
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