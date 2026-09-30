#!/usr/bin/env python3
"""收割前置判据链（harvest preflight）—— 把「合不合 / 让不让号 / 有没有漏片」变成**受检动作**。

这片关掉的风险（LUM-2613 / T1-6-N）
----------------------------------
`docs/37` §277.3 / §280 / §281 连着三轮记的是同一件事：**收割的判据本身没有任何受检动作，
全靠人记**。具体地，PR #168 从 §243 到 §282 连续七轮，每轮都人肉重跑同一条**六步判据链**，
其中至少三步**零编译、零磁盘、纯 git 算术**（`merge-tree --write-tree` 的 rc、base 是否 head
祖先、head 停滞轮数），每轮约 1 秒，但**每轮的历史读数只存在于 `docs/37` 的散文里** ——
没有任何一条命令会重新算它，也没有任何一条命令会问「你这次算的和上次一致吗」。

本脚本把那条判据链的六个读数做成**只读**命令。

六条判据
--------
H1 **base 是否 head 祖先** —— 三种形态，逐字区分：
    ① base 是 head 的祖先 ⇒ 可快进（fast-forward，无合并提交）；
    ② head 是 base 的祖先 ⇒ head 已被 base 包含，**没有未合内容**（该走的是「丢弃/关闭」）；
    ③ 双向都不是 ⇒ 需真合（合并提交），冲突面见 H2/H3。
   🔴 形态②与形态①都表现为「`git merge-base --is-ancestor A B` 一次调用」，只问一个方向
   就会把「已包含」误读成「可快进」—— 那是**相反的处置**（该合的没合 / 该关的合了）。
   所以本判据**同时**问两个方向。

H2 **`git merge-tree --write-tree <base> <head>` 的 rc** —— 零编译、约 1 秒。
   rc=0 ⇒ 给出合并树哈希，无冲突；rc≠0 ⇒ stdout 第一行仍是合并树哈希，其后是
   `<mode> <sha> <stage>\\t<path>` 的冲突条目，**这些 path 就是冲突文件清单**。
   ⚠️ `refs/pull/N/merge^{tree}` **不是**本判据：GitHub 那个 ref 是按**当时的** base 算的
   （`docs/37 §181` 的「三读数等式」必须补前提「base 未前进」）。要判就现算。

H3 **冲突面分类** —— 把 H2 的冲突清单按面切开：
   * **号段面**：`docs/37-M3-W3C-PREFLIGHT.md`、`docs/section-alloc.tsv`
     ⇒ 可机械解（重排段号 + 同步台账），**不需要仲裁**；
   * **代码面**：`crates/**`、`migrations/**`、`contracts/**`、`apps/**`、`Cargo.*`
     ⇒ 需要人仲裁（`docs/37 §239` / §281 证明这两类处置完全不同）；
   * 其余（其它文档 / 脚本）单列，不混进上面两类。
   #168 的形状断言就是这一条：**号段面 1 / 代码面 0**。

H4 **head 停滞** —— 停滞时长 + **停滞轮数**。
   时长 = `--now`（默认当下 UTC）− head 提交的 committer time。
   轮数 = base 树 `docs/37` 里**正文提到该 head 的 sha(短) 或分支名**的 `## §NNN` 段数。
   🔴 这个数与散文里的「N 轮未动」**不是同一个量**（本文件实测：#168 散文说「七轮未动」，
   本判据说 14 段提到它）—— 散文数的是「连续几轮维持不合并裁定」，本判据数的是
   「有几段谈过它」。它是**超集**，用来看门，别拿它去对齐散文。

H5 **远端分支枚举**（§277.3 的机械化）—— 列出**未被 base 包含**的
   `origin/agent/*`、`origin/feat/*`，按**起手 commit 时间**排序，并对每条同时给出
   H1 / H2 / H4。`pulls?state=open` 会漏掉「已交付但没开 PR」的片（`§275.3` 的第二个
   漏片分支、`§277` 补扫救回 `LUM-2604`），这条判据就是那次补扫的固化。

H6 **号段台账交叉**（§278 的机械化 + 前移）—— 在**候选树**上跑
   `scripts/section_alloc_check.py` 的**同一套** R1–R4（`import` 它的 `check()`，
   **不复制判据**），并额外做一件 R1–R4 做不到的事：**合并撞号投影** ——
   逐个段号比「候选树上的持有者」与「base 台账登记的持有者」，不等即**撞号预警**。
   R1–R4 是**树内**判据，#168 的撞号是**两棵树之间**的（候选 §243 = `LUM-2570`，
   base 台账 §243 = `LUM-2573`）⇒ 树内判据全绿，撞号只在 merge 时以 CONFLICT 暴露。
   投影让它在**提交前**就红。

`--check`（门用）与「分支健康」的边界
------------------------------------
本脚本**只读**：没有 `--write`、不 merge、不开 PR、不改任何分支。
`--check` 判红的是**工具级不变量**（base 可解析、发现集合非空、每条候选的读数都能算出来），
**不**判红「某个候选有冲突」—— 后者依赖当下的远端状态，是 §275 那一族「平台完成态读数
≠ 状态健康」：拿它当 CI 门会在 base 上恒红，于是门只会被关掉。冲突与撞号以
`--json` 的 `conflicts` / `section_collisions` 字段与明细行**报出**，由收割的人读。

用法
----
    # 单个候选（PR head 分支或远端分支）
    python3 scripts/harvest_preflight.py --base origin/feat/multica-rs-initial \\
        --head origin/agent/devbox5/8e61c45406b5
    # 扫全部未包含的远端分支（§277.3）
    python3 scripts/harvest_preflight.py --base origin/feat/multica-rs-initial --list-remote
    # 两者一起 + 机器可读
    python3 scripts/harvest_preflight.py --base ... --head ... --list-remote --json
    # 门用法（工具级不变量）
    python3 scripts/harvest_preflight.py --check --base origin/feat/multica-rs-initial
"""

from __future__ import annotations

import argparse
import datetime as _dt
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
# git 命令的工作目录。默认 = 本仓根；`--repo`（门/测试用）可指向别处。
# 🔴 它是**全局**而不是每次传参：H1–H6 一共十几次 git 调用，逐层传同一个 cwd 只是噪声；
#    代价是「改它必须复原」，本仓守门用例在 setUp/tearDown 里成对处理（见 test_harvest_preflight）。
GIT_DIR: Path | None = None
DOC_REL = "docs/37-M3-W3C-PREFLIGHT.md"
LEDGER_REL = "docs/section-alloc.tsv"

# `## §173`、`## §173. 标题`、`## §173 标题` 三种写法本仓都实测过（与门 ⑬ 同一份正则）。
SECTION_RE = re.compile(r"(?m)^##\s*§\s*(\d+)(?=[\s.、:：]|$)")
SECTION_HEAD_RE = re.compile(r"^##\s*§\s*(\d+)(?P<title>[^\n]*)$", re.M)
HOLDER_RE = re.compile(r"LUM-(\d+)")

# 号段面 / 代码面的划分（判据 H3）。列在这里而不是散在函数里，是为了让「什么是号段面」
# 成为一个可被 grep 的**声明**，与 `docs/37` 的号段台账同一性质。
SECTION_PLANE_FILES = (DOC_REL, LEDGER_REL)
CODE_PLANE_PREFIXES = ("crates/", "migrations/", "contracts/", "apps/")
CODE_PLANE_NAMES = ("Cargo.toml", "Cargo.lock")


# --------------------------------------------------------------------------- git


class GitError(RuntimeError):
    pass


def run_git(args: list[str], cwd: Path | None = None) -> tuple[int, str, str]:
    """跑一条 git 命令，返回 (rc, stdout, stderr)。**不抛**，调用方自己判 rc。"""
    proc = subprocess.run(
        ["git", *args],
        cwd=str(cwd or GIT_DIR or ROOT),
        capture_output=True,
        text=True,
        check=False,
    )
    return proc.returncode, proc.stdout, proc.stderr


def resolve(ref: str) -> str | None:
    rc, out, _ = run_git(["rev-parse", "--verify", "--quiet", f"{ref}^{{commit}}"])
    return out.strip() if rc == 0 and out.strip() else None


def show_text(ref: str, path: str) -> str | None:
    """读某棵树上的一个文件；不存在返回 None（区别于「存在但空」）。"""
    rc, out, _ = run_git(["show", f"{ref}:{path}"])
    return out if rc == 0 else None


# --------------------------------------------------------------------------- H1


def h1_relation(base_sha: str, head_sha: str) -> dict:
    """形态 ①/②/③。**两个方向都问** —— 单方向会把 ② 误读成 ①（处置相反）。"""
    rc_bh, _, _ = run_git(["merge-base", "--is-ancestor", base_sha, head_sha])
    rc_hb, _, _ = run_git(["merge-base", "--is-ancestor", head_sha, base_sha])
    base_is_anc = rc_bh == 0
    head_is_anc = rc_hb == 0
    if base_is_anc and head_is_anc:
        form, verdict = "=", "①/② 同一 commit（空候选）"
    elif base_is_anc:
        form, verdict = "①", "base 是 head 的祖先 ⇒ 可快进"
    elif head_is_anc:
        form, verdict = "②", "head 已被 base 包含 ⇒ 没有未合内容（关闭/丢弃，不是合）"
    else:
        form, verdict = "③", "双向都不是祖先 ⇒ 需真合（合并提交）；冲突面见 H2/H3"
    lr_rc, lr_out, _ = run_git(["rev-list", "--left-right", "--count", f"{base_sha}...{head_sha}"])
    behind = ahead = -1
    if lr_rc == 0:
        parts = lr_out.split()
        if len(parts) == 2 and all(p.isdigit() for p in parts):
            behind, ahead = int(parts[0]), int(parts[1])
    return {
        "form": form,
        "verdict": verdict,
        "base_is_ancestor_of_head": base_is_anc,
        "head_is_ancestor_of_base": head_is_anc,
        "base_only_commits": behind,
        "head_only_commits": ahead,
    }


# --------------------------------------------------------------------------- H2


def h2_merge_tree(base_sha: str, head_sha: str) -> dict:
    rc, out, err = run_git(["merge-tree", "--write-tree", base_sha, head_sha])
    lines = out.splitlines()
    tree = lines[0].strip() if lines and re.fullmatch(r"[0-9a-f]{40,64}", lines[0].strip()) else None
    conflicts: list[str] = []
    automerged: list[str] = []
    for line in lines[1:]:
        m = re.match(r"^(?:\d{6}) [0-9a-f]+ [0-3]\t(?P<p>.+)$", line)
        if m:
            p = m.group("p")
            if p not in conflicts:
                conflicts.append(p)
            continue
        m = re.match(r"^Auto-merging (?P<p>.+)$", line)
        if m:
            automerged.append(m.group("p"))
            continue
        m = re.match(r"^CONFLICT \([^)]*\): .*?(?:in|at) (?P<p>\S+)\s*$", line)
        if m and m.group("p") not in conflicts:
            conflicts.append(m.group("p"))
    # rc 128 不是「冲突」：本仓实测有两条远古分支与 base **unrelated histories**，
    # git 直接拒绝合并。把它读成「rc=128 的冲突」就是**读不出病因的读数**（§278 的形状），
    # 所以单列一档 status。
    if rc == 0:
        status = "clean"
    elif rc == 1:
        status = "conflict"
    elif "unrelated histories" in err:
        status = "unrelated-histories"
    else:
        status = "error"
    return {
        "rc": rc,
        "status": status,
        "merge_tree": tree,
        "conflicts": sorted(conflicts),
        "auto_merged": sorted(set(automerged)),
        "stderr": err.strip()[:400],
    }


# --------------------------------------------------------------------------- H3


def h3_classify(conflicts: list[str]) -> dict:
    section_plane, code_plane, other = [], [], []
    for p in conflicts:
        if p in SECTION_PLANE_FILES:
            section_plane.append(p)
        elif p in CODE_PLANE_NAMES or p.startswith(CODE_PLANE_PREFIXES):
            code_plane.append(p)
        else:
            other.append(p)
    return {
        "section_plane": sorted(section_plane),
        "code_plane": sorted(code_plane),
        "other": sorted(other),
        "section_plane_count": len(section_plane),
        "code_plane_count": len(code_plane),
        "other_count": len(other),
        # 「可机械解」vs「要仲裁」的分界就这一个数（§239 / §281）。
        "needs_arbitration": bool(code_plane) or bool(
            [p for p in other if not p.startswith("docs/")]
        ),
    }


# --------------------------------------------------------------------------- H4


def _parse_dt(value: str) -> _dt.datetime | None:
    try:
        return _dt.datetime.fromisoformat(value.strip())
    except ValueError:
        return None


def h4_stall(head_sha: str, head_ref: str, doc_text: str | None, now: _dt.datetime) -> dict:
    rc, out, _ = run_git(["show", "-s", "--format=%cI%x1f%s", head_sha])
    commit_time = subject = ""
    if rc == 0 and "\x1f" in out:
        commit_time, subject = out.split("\x1f", 1)
        subject = subject.strip()
    dt = _parse_dt(commit_time)
    stall_hours = None
    if dt is not None:
        if dt.tzinfo is None:
            dt = dt.replace(tzinfo=_dt.timezone.utc)
        stall_hours = round((now - dt).total_seconds() / 3600.0, 2)

    seen: list[int] = []
    if doc_text:
        # 短 sha 与分支短名都算「提到它」—— 只认 sha 会漏掉只写分支名的段。
        needles = [head_sha[:7], head_ref.rsplit("/", 1)[-1]] if head_ref else [head_sha[:7]]
        needles = [n for n in needles if n]
        parts = SECTION_RE.split(doc_text)
        # parts = [preamble, num, body, num, body, ...]
        for i in range(1, len(parts), 2):
            if any(n in parts[i + 1] for n in needles):
                seen.append(int(parts[i]))
    seen_sorted = sorted(set(seen))
    return {
        "head_sha": head_sha,
        "commit_time": commit_time,
        "subject": subject,
        "stall_hours": stall_hours,
        # 🔴 与散文「N 轮未动」不是同一个量：本判据数的是「有几**段**谈过它」，
        #    散文数的是「连续几轮维持裁定」。本值是超集（见文件顶部 H4）。
        "sections_seen": seen_sorted,
        "sections_seen_count": len(seen_sorted),
        "last_section_seen": max(seen_sorted) if seen_sorted else None,
        "stale": bool(stall_hours is not None and stall_hours >= 1.0),
    }


# --------------------------------------------------------------------------- H6


def _section_holders(doc_text: str) -> dict[int, str | None]:
    """段号 → 该段标题里第一个 `LUM-####`（没有就 None）。这是「持有者」的**可机械**近似。"""
    holders: dict[int, str | None] = {}
    for m in SECTION_HEAD_RE.finditer(doc_text):
        num = int(m.group(1))
        h = HOLDER_RE.search(m.group("title"))
        holders.setdefault(num, f"LUM-{h.group(1)}" if h else None)
    return holders


def h6_section_ledger(head_sha: str, base_doc: str | None, base_ledger: str | None) -> dict:
    """在**候选树**上跑 R1–R4（复用门 ⑬ 的 `check()`）+ 合并撞号投影。

    为什么必须投影：R1–R4 全是**树内**判据，而 #168 的撞号是**两棵树之间**的
    （候选 §243 持有者 `LUM-2570` vs base 台账 §243 持有者 `LUM-2573`）⇒ 树内全绿。
    """
    result: dict = {
        "candidate_doc_present": False,
        "candidate_ledger_present": False,
        "r1_r4": None,
        "section_collisions": [],
        "new_sections": [],
        "candidate_internal_dups": [],
    }
    cand_doc = show_text(head_sha, DOC_REL)
    if cand_doc is None:
        result["status"] = "skipped"
        result["reason"] = f"候选树上没有 {DOC_REL}"
        return result
    result["candidate_doc_present"] = True
    cand_ledger = show_text(head_sha, LEDGER_REL)
    result["candidate_ledger_present"] = cand_ledger is not None

    cand_nums = [int(m.group(1)) for m in SECTION_RE.finditer(cand_doc)]
    from collections import Counter

    result["candidate_internal_dups"] = sorted(
        n for n, c in Counter(cand_nums).items() if c > 1
    )

    # ---- R1–R4：复用门 ⑬ 的实现（import，不是复制）----
    if cand_ledger is not None:
        try:
            sys.path.insert(0, str(Path(__file__).resolve().parent))
            import section_alloc_check  # noqa: E402  (路径刚插好)

            with tempfile.TemporaryDirectory(prefix="harvest-preflight-") as td:
                tmp_doc = Path(td) / Path(DOC_REL).name
                tmp_led = Path(td) / Path(LEDGER_REL).name
                tmp_doc.write_text(cand_doc, encoding="utf-8")
                tmp_led.write_text(cand_ledger, encoding="utf-8")
                old_doc, old_led = section_alloc_check.DOC, section_alloc_check.LEDGER
                try:
                    section_alloc_check.DOC = tmp_doc
                    section_alloc_check.LEDGER = tmp_led
                    defects, readings = section_alloc_check.check()
                finally:
                    section_alloc_check.DOC, section_alloc_check.LEDGER = old_doc, old_led
            result["r1_r4"] = {
                "ok": not defects,
                "defects": defects,
                "readings": readings,
            }
        except Exception as exc:  # noqa: BLE001 —— 复用失败必须看得见，不能静默当绿
            result["r1_r4"] = {"ok": False, "defects": [f"复用 section_alloc_check 失败: {exc!r}"]}
    else:
        result["r1_r4"] = {
            "ok": None,
            "defects": [],
            "reason": f"候选树上没有 {LEDGER_REL}（台账在该候选起手之后才引入）"
            "⇒ 树内 R1–R4 不适用；这**不是绿**，下面的投影仍然要跑",
        }

    # ---- 合并撞号投影（跨树；台账与 docs/37 都缺时才跳过）----
    if base_doc is not None and base_ledger is not None:
        base_holders = _section_holders(base_doc)
        ledger_holders: dict[int, str] = {}
        for line in base_ledger.splitlines():
            if not line.strip() or line.lstrip().startswith("#"):
                continue
            f = line.split("\t")
            if len(f) in (3, 4) and f[0].strip().isdigit():
                ledger_holders.setdefault(int(f[0]), f[1].strip())
        cand_holders = _section_holders(cand_doc)
        base_nums = {int(m.group(1)) for m in SECTION_RE.finditer(base_doc)}
        for num in sorted(set(cand_holders) & base_nums):
            cand_holder = cand_holders[num]
            base_holder = ledger_holders.get(num, base_holders.get(num))
            if cand_holder is not None and base_holder is not None and cand_holder != base_holder:
                result["section_collisions"].append(
                    {
                        "section": num,
                        "candidate_holder": cand_holder,
                        "base_holder": base_holder,
                        "base_source": "ledger" if num in ledger_holders else "docs/37",
                    }
                )
        result["new_sections"] = sorted(set(cand_holders) - base_nums)
    else:
        result["projection"] = f"base 侧缺 {DOC_REL if base_doc is None else LEDGER_REL} ⇒ 投影不适用"

    result["status"] = "ok" if not result["section_collisions"] else "collision"
    return result


# --------------------------------------------------------------------------- 组装


def inspect_candidate(base_sha: str, head_ref: str, base_doc: str | None,
                      base_ledger: str | None, now: _dt.datetime) -> dict:
    head_sha = resolve(head_ref)
    if head_sha is None:
        return {"ref": head_ref, "error": f"ref 不可解析: {head_ref}"}
    h1 = h1_relation(base_sha, head_sha)
    h2 = h2_merge_tree(base_sha, head_sha)
    h3 = h3_classify(h2["conflicts"])
    h4 = h4_stall(head_sha, head_ref, base_doc, now)
    h6 = h6_section_ledger(head_sha, base_doc, base_ledger)
    return {
        "ref": head_ref,
        "head_sha": head_sha,
        "H1": h1,
        "H2": h2,
        "H3": h3,
        "H4": h4,
        "H6": h6,
        "actionable_conflicts": h2["rc"] != 0,
    }


def list_remote_candidates(base_sha: str, base_ref: str, base_doc, base_ledger,
                           now: _dt.datetime, prefixes: tuple[str, ...],
                           limit: int) -> dict:
    """H5：未被 base 包含的 origin 分支，按**起手 commit 时间**（新→旧）排序。"""
    out: dict = {"excluded_self": base_ref, "prefixes": list(prefixes), "candidates": []}
    for pref in prefixes:
        rc, listing, _ = run_git(
            ["for-each-ref", "--format=%(refname:short)\t%(committerdate:iso-strict)", pref]
        )
        if rc != 0:
            out.setdefault("errors", []).append(f"for-each-ref {pref} rc={rc}")
            continue
        for line in listing.splitlines():
            if not line.strip():
                continue
            ref, _, when = line.partition("\t")
            if ref == base_ref:
                continue
            sha = resolve(ref)
            if sha is None:
                continue
            rc_anc, _, _ = run_git(["merge-base", "--is-ancestor", sha, base_sha])
            if rc_anc == 0:
                continue  # 已被 base 包含 —— §277.3 的补扫只关心「没被包含的」
            out["candidates"].append({"ref": ref, "sha": sha, "committer_date": when})
    out["candidates"].sort(key=lambda c: (c["committer_date"], c["ref"]), reverse=True)
    total = len(out["candidates"])
    if limit and total > limit:
        out["candidates"] = out["candidates"][:limit]
        out["truncated"] = {"found": total, "shown": limit}
    out["found"] = total
    for c in out["candidates"]:
        c["H1"] = h1_relation(base_sha, c["sha"])["form"]
        mt = h2_merge_tree(base_sha, c["sha"])
        c["H2_rc"] = mt["rc"]
        c["H2_status"] = mt["status"]
        cls = h3_classify(mt["conflicts"])
        c["H3"] = {
            "section_plane_count": cls["section_plane_count"],
            "code_plane_count": cls["code_plane_count"],
        }
        c["H4_stall_hours"] = h4_stall(c["sha"], c["ref"], base_doc, now)["stall_hours"]
    return out


# --------------------------------------------------------------------------- 输出


def _fmt_candidate(c: dict) -> list[str]:
    if "error" in c:
        return [f"  {c['ref']}: ERROR {c['error']}"]
    h1, h2, h3, h4, h6 = c["H1"], c["H2"], c["H3"], c["H4"], c["H6"]
    out = [
        f"candidate {c['ref']} @ {c['head_sha'][:9]}",
        f"  H1 form={h1['form']} {h1['verdict']}"
        f"  (base_only={h1['base_only_commits']} head_only={h1['head_only_commits']})",
        f"  H2 rc={h2['rc']} status={h2['status']} merge_tree={h2['merge_tree']}"
        + (f" conflicts={h2['conflicts']}" if h2["conflicts"] else " conflicts=[]"),
        f"  H3 section_plane={h3['section_plane_count']}{h3['section_plane']}"
        f" code_plane={h3['code_plane_count']}{h3['code_plane']} other={h3['other_count']}"
        f" needs_arbitration={h3['needs_arbitration']}",
        f"  H4 stall_hours={h4['stall_hours']} sections_seen={h4['sections_seen_count']}"
        f" last_section={h4['last_section_seen']} commit_time={h4['commit_time']}",
    ]
    if h2["auto_merged"]:
        out.append(f"     auto-merged (clean, NOT conflicts): {h2['auto_merged']}")
    r = h6.get("r1_r4")
    if r and r.get("ok") is None:
        out.append(f"  H6 status={h6['status']} R1-R4 N/A: {r.get('reason')}")
    elif r:
        out.append(
            f"  H6 R1-R4 ok={r['ok']} defects={len(r.get('defects', []))}"
            + (f" first={r['defects'][0]}" if r.get("defects") else "")
        )
    if h6.get("candidate_internal_dups"):
        out.append(f"     candidate internal dups: {h6['candidate_internal_dups']}")
    if h6.get("new_sections"):
        out.append(f"     sections new vs base: {h6['new_sections']}")
    for c_ in h6.get("section_collisions", []):
        out.append(
            f"  H6 COLLISION §{c_['section']}: candidate={c_['candidate_holder']}"
            f" base={c_['base_holder']} (base source: {c_['base_source']})"
        )
    return out


def check_invariants(base_ref: str, base_sha: str | None, remote: dict) -> list[str]:
    """门用的**工具级**不变量。分支健康（冲突/撞号）不在这里判红 —— 理由见文件顶部。"""
    bad: list[str] = []
    if base_sha is None:
        bad.append(f"base ref 不可解析: {base_ref}")
    found = remote.get("found", 0)
    if base_sha is not None and found == 0:
        # 空发现集合绝不能读成绿（与门 ⑫ 空 glob / 门 ⑬ 空台账同族）。
        bad.append("H5 发现集合为空（没有任何未被 base 包含的远端分支）")
    if base_sha is not None and found > 0 and not remote.get("candidates"):
        bad.append(f"H5 found={found} 但候选清单为空（limit/打印面把发现吞了）")
    for err in remote.get("errors", []):
        bad.append(f"H5 {err}")
    for c in remote.get("candidates", []):
        if not isinstance(c.get("H2_rc"), int) or c.get("H1") not in "①②③=":
            bad.append(f"H5 候选 {c['ref']} 的读数算不出来（H1/H2 缺失）")
    return bad


def main(argv: list[str] | None = None) -> int:
    p = argparse.ArgumentParser(
        prog="harvest_preflight.py",
        description="收割前置判据链 H1–H6（只读；无 --write 模式）",
    )
    p.add_argument("--base", default="origin/feat/multica-rs-initial", help="base ref")
    p.add_argument("--repo", default=None,
                   help="git 工作目录（默认本仓根；测试/门可指向别的 clone）")
    p.add_argument("--head", help="候选 ref（PR head 分支或远端分支）")
    p.add_argument("--list-remote", action="store_true", help="H5：枚举未被 base 包含的远端分支")
    p.add_argument("--prefix", action="append", default=None,
                   help="H5 的分支前缀（可多次；默认 origin/agent, origin/feat）")
    p.add_argument("--limit", type=int, default=200, help="H5 最多列多少条候选（默认 200）")
    p.add_argument("--now", default=None, help="H4 的“现在”（ISO8601；默认系统 UTC）")
    p.add_argument("--check", action="store_true", help="门用法：只判工具级不变量")
    p.add_argument("--json", action="store_true", help="机器可读输出")
    args = p.parse_args(argv)

    if not args.head and not args.list_remote and not args.check:
        p.error("至少要给 --head / --list-remote / --check 之一（本脚本不做收割动作）")

    if args.repo:
        global GIT_DIR
        GIT_DIR = Path(args.repo).resolve()
        if not (GIT_DIR / ".git").exists() and not (GIT_DIR / "HEAD").exists():
            print(f"error: --repo 不是 git 仓库: {GIT_DIR}", file=sys.stderr)
            return 2

    now = _parse_dt(args.now) if args.now else _dt.datetime.now(_dt.timezone.utc)
    if now is None:
        print(f"error: --now 解析不了: {args.now!r}", file=sys.stderr)
        return 2
    if now.tzinfo is None:
        now = now.replace(tzinfo=_dt.timezone.utc)

    base_sha = resolve(args.base)
    base_doc = show_text(args.base, DOC_REL) if base_sha else None
    base_ledger = show_text(args.base, LEDGER_REL) if base_sha else None
    prefixes = tuple(args.prefix) if args.prefix else ("refs/remotes/origin/agent", "refs/remotes/origin/feat")

    report: dict = {
        "base_ref": args.base,
        "base_sha": base_sha,
        "now": now.isoformat(),
        "candidates": [],
    }
    lines: list[str] = [f"harvest-preflight base={args.base} @ {(base_sha or 'UNRESOLVED')[:9]}"]

    if args.head:
        c = inspect_candidate(base_sha, args.head, base_doc, base_ledger, now) if base_sha else \
            {"ref": args.head, "error": "base 不可解析"}
        report["candidates"].append(c)
        if not args.json:
            lines.extend(_fmt_candidate(c))

    if args.list_remote or args.check:
        if base_sha is None:
            remote = {"found": 0, "candidates": [], "errors": ["base 不可解析 ⇒ H5 未跑"]}
        else:
            remote = list_remote_candidates(base_sha, args.base, base_doc, base_ledger,
                                            now, prefixes, args.limit)
        report["H5"] = remote
        if not args.json:
            lines.append(
                f"H5 uncontained remote branches: found={remote.get('found', 0)}"
                f" prefixes={list(prefixes)}"
            )
            for c in remote.get("candidates", []):
                lines.append(
                    f"  {c['ref']} @ {c['sha'][:9]} {c['committer_date']}"
                    f" H1={c['H1']} H2_rc={c['H2_rc']}/{c['H2_status']}"
                    f" H3[sec={c['H3']['section_plane_count']} code={c['H3']['code_plane_count']}]"
                    f" H4_stall_h={c['H4_stall_hours']}"
                )

    if args.check:
        problems = check_invariants(args.base, base_sha, report.get("H5", {}))
        report["check"] = {"ok": not problems, "problems": problems}
        if not args.json:
            lines.append("check: " + ("OK" if not problems else f"FAIL ({len(problems)})"))
            for pb in problems:
                lines.append(f"  error: {pb}")

    if args.json:
        print(json.dumps(report, indent=2, ensure_ascii=False, sort_keys=True))
    else:
        print("\n".join(lines))

    if args.check and not report["check"]["ok"]:
        return 1
    return 2 if any("error" in c for c in report["candidates"]) else 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except BrokenPipeError:  # `| head` —— 读端走了不算失败
        devnull = os.open(os.devnull, os.O_WRONLY)
        os.dup2(devnull, sys.stdout.fileno())
        sys.exit(0)
