#!/usr/bin/env python3
"""号段台账校验：`docs/37` 的 `## §NNN` 段号必须与 `docs/section-alloc.tsv` 双向一致，且不得撞号。

这片关掉的风险（LUM-2607 / T1-6-L）
----------------------------------
`docs/37-M3-W3C-PREFLIGHT.md` 是**四份东西的合流点**：号段台账 + 预飞结论 + 门读数 +
失败判别式汇总，而且**每一片都要往里追加**。可是「分配一个空号」是**纯手工**的、
先到先得、**无校验**的动作：两片都可以合法地拿到同一个空号，各自都以为对方不存在。
撞号只在**合并时**以 `docs/37` 的 CONFLICT 暴露 —— 已经连续三轮产生真实合并成本
（`LUM-2596` 的 PR #168 至今被号段冲突卡住、不可合并）。本脚本把那一步前移：
**撞号在提交前就红**（不再靠冲突去发现）。

判据（四条；任何一条不满足 ⇒ exit 1）
------------------------------------
R1 文件 → 台账：`docs/37` 里出现的**每个** `## §NNN` 段号，台账必须有对应行。
R2 台账 → 文件：台账**每一行**的段号，必须在 `docs/37` 里真实存在
   （**双向** —— 只有单向的话，台账自己会漂成散文）。
R3 台账段号唯一：同一段号在台账里出现两次 ⇒ 红。**这是「撞号」的判据**：
   两个片各自分配同一个空号，就表现为两行同号。
R4 出现次数一致：段号在 `docs/37` 里的实际出现次数，必须等于台账登记的次数
   （第 4 列，缺省 1）⇒ **任何新增的文件内重复段号都红**。

第 4 列为什么存在（本文件唯一的格式偏离）
----------------------------------------
base `a5facad4` 里**已经有一个真实撞号**：`## §226` 出现两次（`docs/37:20250` 与
`docs/37:20884`，后者被插在 §230 与 §231 之间）。本片的范围明确**不改号**
（不删 / 不重排 / 不合并既有段），所以只能**如实登记**：台账那一行记 `2`。

没有第 4 列会怎样：R1–R3 对一个「文件里出现两次、台账只登记一次」的段号**全部判绿**
—— 也就是说，门会对它自己的**历史失败模式**失明。而 §226 的成因正是那个失败模式：
一次**直推**（未走分配）里重用了已在用的号。有第 4 列，既有撞号被显式登记，
而**任何新增**撞号（文件里多出一次、台账没跟着改）立刻判红。

用法
----
    python3 scripts/section_alloc_check.py            # 逐条读数 + 总结
    python3 scripts/section_alloc_check.py --quiet    # 只打一行总结（门 ⑬ 用）
    bash scripts/gates.sh --only section-alloc        # 门（命令唯一实现仍在本脚本）

本脚本是**只读**的：没有 `--write-*` 模式。台账是**分配动作**的产物，必须由分配者
写下；「自动重生成台账」＝「自动把撞号合法化」，那正是本片要关掉的东西。
"""

from __future__ import annotations

import argparse
import os
import re
import sys
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DOC = ROOT / "docs" / "37-M3-W3C-PREFLIGHT.md"
LEDGER = ROOT / "docs" / "section-alloc.tsv"

# `## §173`、`## §173. 标题`、`## §173 标题` 都是本仓真实存在的写法（三种都实测过）。
# §NNN 之后必须是空白 / `.` / `、` / `:` 或行尾 —— 否则 `## §1730` 会被误读成 `§173`。
SECTION_RE = re.compile(r"^##\s*§\s*(\d+)(?=[\s.、:：]|$)")


def rel(path: Path) -> str:
    try:
        return str(path.relative_to(ROOT))
    except ValueError:
        return str(path)


def read_doc_sections(path: Path) -> list[tuple[int, str]]:
    """返回 [(行号, 段号), ...] —— **含重复出现**，R4 要用到次数。"""
    out: list[tuple[int, str]] = []
    for lineno, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        m = SECTION_RE.match(line)
        if m:
            out.append((lineno, m.group(1)))
    return out


def read_ledger(path: Path) -> tuple[list[dict], list[str]]:
    """返回 (行, 格式缺陷)。行 = {line, num, holder, summary, count}。

    格式：`<段号>\\t<持有者 issue>\\t<一句话摘要>[\\t<出现次数>]`，`#` 开头为注释，
    空行忽略。字段数不是 3 或 4、段号不是十进制数字、次数 < 1 —— 一律判**缺陷**，
    而不是静默忽略：台账漂了必须看得见。
    """
    rows: list[dict] = []
    defects: list[str] = []
    for lineno, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        fields = line.split("\t")
        if len(fields) not in (3, 4):
            defects.append(
                f"{rel(path)}:{lineno}: 需要 3 或 4 个 TAB 分隔字段，实得 {len(fields)} 个: {line!r}"
            )
            continue
        num, holder, summary = fields[0].strip(), fields[1].strip(), fields[2].strip()
        count_s = fields[3].strip() if len(fields) == 4 else "1"
        if not num.isdigit():
            defects.append(f"{rel(path)}:{lineno}: 段号必须是十进制数字，实得 {num!r}")
            continue
        if not count_s.isdigit() or int(count_s) < 1:
            defects.append(f"{rel(path)}:{lineno}: 出现次数必须是 >=1 的十进制数字，实得 {count_s!r}")
            continue
        rows.append(
            {"line": lineno, "num": num, "holder": holder, "summary": summary, "count": int(count_s)}
        )
    return rows, defects


def check() -> tuple[list[str], dict]:
    """跑完四条判据，返回 (缺陷清单, 读数)。只读，不写任何东西。"""
    defects: list[str] = []
    readings: dict = {}

    # 前置：被校验的两个文件必须存在。**「没有东西可校验」绝不允许读成绿** ——
    # 与门 ⑫ 的「空 glob ⇒ 判红」、⑥/⑧ 缺库 URL 同一族（`docs/37 §276` 的教训：
    # 判红条件要是「集合非空」，而集合本身是被检验的对象，丢文件就表现为绿）。
    missing = [p for p in (DOC, LEDGER) if not p.is_file()]
    if missing:
        for p in missing:
            defects.append(f"缺文件：{rel(p)} 不存在（被校验的文件没了只能判红，不能读成绿）")
        return defects, readings

    sections = read_doc_sections(DOC)
    rows, malformed = read_ledger(LEDGER)
    defects.extend(malformed)

    doc_counts = Counter(num for _, num in sections)
    # 台账一侧有两个**不同**的量，别混：
    #   * ledger_row_counts = 该段号有几**行**（R3 撞号判据看它）；
    #   * ledger_declared   = 该行**登记**的 docs/37 出现次数（第 4 列，R4 看它）。
    ledger_row_counts = Counter(r["num"] for r in rows)
    ledger_declared = {r["num"]: r["count"] for r in rows}

    readings["sections"] = len(sections)
    readings["distinct"] = len(doc_counts)
    readings["ledger_rows"] = len(rows)
    readings["doc_dups"] = sorted((n for n, c in doc_counts.items() if c > 1), key=int)

    # 空台账（0 行）必须判红：与「发现集合为空」同款处置。
    if not rows:
        defects.append(f"{rel(LEDGER)} 是空的（0 行）—— 空台账必须判红，不能读成绿")

    # R1 文件 → 台账（存在性）
    r1 = 0
    for n in sorted(set(doc_counts) - set(ledger_declared), key=int):
        r1 += 1
        defects.append(
            f"R1 段号 {n} 出现在 {rel(DOC)}（{doc_counts[n]} 处）但台账没有对应行"
            f"（分配动作没登记）"
        )

    # R2 台账 → 文件（存在性）
    r2 = 0
    for n in sorted(set(ledger_declared) - set(doc_counts), key=int):
        r2 += 1
        lines = ",".join(str(r["line"]) for r in rows if r["num"] == n)
        defects.append(f"R2 台账行 {lines} 登记了段号 {n}，但 {rel(DOC)} 里没有 `## §{n}` 段")

    # R3 台账段号唯一 —— 撞号判据
    r3 = 0
    for n, c in sorted(ledger_row_counts.items(), key=lambda kv: int(kv[0])):
        if c > 1:
            r3 += 1
            lines = ",".join(str(r["line"]) for r in rows if r["num"] == n)
            holders = " / ".join(sorted({r["holder"] for r in rows if r["num"] == n}))
            defects.append(f"R3 撞号：台账段号 {n} 出现 {c} 次（台账行 {lines}；持有者 {holders}）")

    # R4 出现次数一致（只报「两侧都有但次数不等」的情形；0 的情形由 R1/R2 报，避免重复刷屏）
    r4 = 0
    for n in sorted(set(doc_counts) & set(ledger_declared), key=int):
        d, l = doc_counts[n], ledger_declared[n]
        if d != l:
            r4 += 1
            lines = ",".join(str(r["line"]) for r in rows if r["num"] == n)
            defects.append(
                f"R4 段号 {n}：{rel(DOC)} 里出现 {d} 次，台账行 {lines} 登记 {l} 次"
                f"（新增撞号必须改台账登记次数，或换一个空号）"
            )

    readings.update({"R1": r1, "R2": r2, "R3": r3, "R4": r4})
    return defects, readings


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="section_alloc_check.py",
        description="docs/37 号段台账的双向校验 + 撞号判据（只读；无 --write-* 模式）。",
    )
    parser.add_argument("--quiet", action="store_true", help="只打一行总结（门 ⑬ 用）")
    args = parser.parse_args(argv)

    defects, rd = check()

    if not args.quiet:
        print(f"section-alloc: {rel(DOC)}  vs  {rel(LEDGER)}", flush=True)
        if rd:
            print(
                "  readings: sections=%d distinct=%d ledger_rows=%d"
                "  R1(file->ledger)=%d R2(ledger->file)=%d R3(ledger-dup)=%d R4(count)=%d"
                % (
                    rd["sections"], rd["distinct"], rd["ledger_rows"],
                    rd["R1"], rd["R2"], rd["R3"], rd["R4"],
                ),
                flush=True,
            )
            if rd["doc_dups"]:
                print(
                    "  既有重复段号（由台账第 4 列如实登记，不是缺陷）: "
                    + ", ".join("§" + n for n in rd["doc_dups"]),
                    flush=True,
                )
        for d in defects:
            print(f"error: {d}", file=sys.stderr)

    if defects:
        print(f"section-alloc: FAIL — {len(defects)} defect(s)", flush=True)
        return 1
    print(
        "section-alloc: OK — sections=%d numbers=%d ledger=%d defects=0"
        % (rd.get("sections", 0), rd.get("distinct", 0), rd.get("ledger_rows", 0)),
        flush=True,
    )
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except BrokenPipeError:  # `| head` — the reader went away, that is not a failure
        devnull = os.open(os.devnull, os.O_WRONLY)
        os.dup2(devnull, sys.stdout.fileno())
        sys.exit(0)
