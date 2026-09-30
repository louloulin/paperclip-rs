#!/usr/bin/env python3
"""H7：**候选集内的两两撞号** —— `harvest_preflight.py` 的判据链里 H6 看不见的那一半（LUM-2615 / T1-6-O）。

为什么单独一个文件：加上 H7 之后 `scripts/harvest_preflight.py` 会越过门 ⑩ 的 **800 行硬上限**，
而 `scripts/file_size_baseline.tsv` 写死「**基线只减不增，新增违规不得写进白名单**」⇒
把 H7 写进白名单是**违规**而不是变通 ⇒ 按那条规则**拆**（不是压注释压掉 130 行判据记录）。

🔴 **H6 是「base ↔ 单候选」的跨树投影，看不见「候选 ↔ 候选」。** 2026-09-30 12:00 cycle 的
第一次真实收割里，两条候选**各自**报 `H1=① H2_rc=0/clean`（都不冲突），**却彼此撞**：
`docs/37` 双 EOF append + `docs/section-alloc.tsv` 尾部各追加一行 —— 撞号只在那轮**人 `git merge`**
时才以 CONFLICT 暴露。完整经过、真数据自证与踩到的坑见 `docs/37 §286`。

做法
----
clean 候选（`H2_rc=0` 且 `H1 ∈ hp.CLEAN_H1_FORMS`）按 **H4 时间升序**（= 最停滞的先）排成序列，
从 base 起**累积** merge-tree（`base → ∪A → ∪AB …`），**任一步 rc≠0 就报出这一对**并**终止**序列
（不是「跳过它继续」——那会算出一个现实中不存在的累积树）。

不选「两两全量」的理由：真实收割里两片是**依次**被合进同一棵树的，序列累积问的正是那件真会撞的事；
全量会报出一串**并不真的互相撞**的组合（三个候选都在 `docs/37` EOF 追加时：全量 = 3 对，
序列 = 1 对，第 2 步就停）。

复用与只读
----------
* **不复制判据**：每一步都走 `harvest_preflight.h2_merge_tree()`（与 H2 同一份读法、同一份冲突
  清单解析），唯一的差别是 base 参数从「真 base」换成了「上一步的累积提交」。
* **不改 H2 的读数**：不修改传入的 `candidates`（H2 读数逐字不变是 LUM-2613 的契约）。
* **不落对象**：`commit-tree` 要写对象库（累积提交），故所有 git 调用都在
  `harvest_preflight.isolated_object_store()` 里 ⇒ 真仓库一个字节都不动，临时目录退出即删。
* 0 或 1 个 clean 候选**安静通过**（不是绿，是**没东西可问**）。
* `--check` **不**因 H7 判红（理由见 `harvest_preflight` 文件顶部的门边界段）。

用法：由 `harvest_preflight.py` **惰性** import（顶层 import 会与它形成循环），
也可以单独当库用。全程通过 `hp.` 前缀读宿主模块（而不是 `from ... import`）：
`hp.GIT_DIR` 是**全局**且由守门用例在 `setUp/tearDown` 里成对改写，绑定在 import 时就会把
真仓库当测试仓库用（上一片第一版踩过：断言里看到的是真仓库的 150+ 个 ref，而用例是绿的）。
"""

from __future__ import annotations

# 🔴 **双重 import 陷阱（本片被自己的 CLI 用例当场逮到）**：以
#    `python3 scripts/harvest_preflight.py` 直接跑时，本模块在 `sys.modules` 里叫 `__main__`；
#    若本文件（`harvest_h7`）写 `import harvest_preflight`，那会**再执行一遍**宿主文件，得到一个
#    `GIT_DIR is None` 的**副本** ⇒ H7 的 git 调用退回 `ROOT`（真仓库）⇒ 在测试仓库里
#    `merge-tree` 报 rc=1 / `merge_tree=None`，读起来像「凭空冒出的假冲突」。
#    修法在宿主：`__main__` 块里 `sys.modules.setdefault("harvest_preflight", <本模块>)`，
#    于是这里的 import 拿到的**就是**同一个对象（含已设好的 `--repo` / `GIT_DIR`）。
import harvest_preflight as hp


# --------------------------------------------------------------------------- 顺序


def _order_key(c: dict) -> tuple:
    """H4 时间**升序**（= **最停滞的先合**）。

    ⚠️ H4 报的是 `stall_hours`（距今**多久**），与「时间升序」是**反序**的：停滞越久 ⇒ 离当下越远
    ⇒ 越靠**前**合。所以这里对 `stall_hours` 取负号；直接按它升序会把**最新**的候选排最前
    （本片实现时实测踩到：新 base 分支在 4 个候选里排到了第一位）。
    """
    hours = c.get("H4_stall_hours")
    return (
        hours is None,                       # 读不出停滞时长的排最后，不排最前
        -(hours if hours is not None else 0.0),
        c.get("committer_date") or "",
        c.get("ref") or "",
    )


# --------------------------------------------------------------------------- 累积


def _accumulate_commit(tree: str, parents: list[str], env: dict) -> str:
    """把 `merge-tree` 的**树**封成一个**提交** —— 序列累积必须先做这一步。

    🔴 `git merge-tree --write-tree A B` 产出的是**树**，而 `merge-tree` 下一次只吃**提交**
    （实测：把上一步的树直接传回去 ⇒ `expected commit type, but the object dereferences to
    tree type`，rc=1）⇒ 直接把上一步的树当 base 传下去，H7 会在第一步得到一个**假的** rc=1 冲突。
    parents 带上**所有**已合入的 ref：否则下一轮 `merge-base` 可能选错共同祖先，
    三方合并会退化成「树 vs 树」的比对。
    """
    args = ["commit-tree", tree]
    for p in parents:
        args += ["-p", p]
    args += ["-m", "harvest-preflight H7 accumulator (temp object, discarded)"]
    rc, out, err = hp.run_git(args, env=env)
    if rc != 0 or not out.strip():
        raise hp.GitError(f"commit-tree 失败 rc={rc}: {err.strip()[:200]}")
    return out.strip()


def pairwise(base_sha: str, candidates: list[dict]) -> dict:
    """候选集内的两两撞号：**按 H4 升序的序列累积 merge-tree**。**只读**。"""
    clean: list[dict] = []
    excluded: list[dict] = []
    for c in candidates:
        entry = {
            "ref": c.get("ref"),
            "H1": c.get("H1"),
            "H2_rc": c.get("H2_rc"),
            "H2_status": c.get("H2_status"),
        }
        if c.get("H2_rc") == 0 and c.get("H1") in hp.CLEAN_H1_FORMS:
            clean.append(c)
        else:
            excluded.append(entry)

    ordered = sorted(clean, key=_order_key)
    out: dict = {
        "base_sha": base_sha,
        "clean_sequence": [c.get("ref") for c in ordered],
        "excluded": excluded,
        "pairs": [],
        "merged_clean": [],
    }

    # 0 / 1 个 clean 候选 ⇒ **没东西可问**，安静通过（不是绿）。
    if len(ordered) < 2:
        out["status"] = "ok"
        out["ok"] = True
        out["note"] = "clean 候选少于 2 个 ⇒ H7 无判定（0 个是空集，1 个没有「候选↔候选」可问）"
        return out

    acc = base_sha
    left_ref = None            # None = 左端就是 base 本身
    acc_parents = [base_sha]
    try:
        with hp.isolated_object_store() as env:
            for c in ordered:
                mt = hp.h2_merge_tree(acc, c["sha"], env=env)   # ← 复用 H2 的读法
                if mt["rc"] != 0:
                    out["pairs"].append({
                        "left_ref": left_ref or base_sha,
                        "left_is_base": left_ref is None,
                        "right_ref": c.get("ref"),
                        "H7_rc": mt["rc"],
                        "H7_status": mt["status"],
                        "files": mt["conflicts"],     # ← 与 H2 同一份解析，逐字同源
                        "merge_tree": mt["merge_tree"],
                    })
                    out["stopped_at"] = c.get("ref")
                    out["stopped_reason"] = (
                        f"第 {len(out['merged_clean']) + 1} 步 rc={mt['rc']} ⇒ 累积树不可用，"
                        "序列终止（**不是**「跳过它继续」——那会算出一个现实中不存在的累积树）"
                    )
                    break
                if not mt["merge_tree"]:
                    out["stopped_at"] = c.get("ref")
                    out["stopped_reason"] = "rc=0 但读不到合并树哈希 ⇒ 无法继续累积"
                    break
                acc_parents.append(c["sha"])
                acc = _accumulate_commit(mt["merge_tree"], acc_parents, env)
                out["merged_clean"].append(c.get("ref"))
                left_ref = c.get("ref")
    except hp.GitError as exc:
        # 封装/复用失败必须**看得见**（与 H6 里 import 失败不静默当绿同一条纪律）。
        out["status"] = "error"
        out["ok"] = False
        out["error"] = str(exc)
        return out

    out["status"] = "collision" if out["pairs"] else "ok"
    out["ok"] = not out["pairs"]
    return out


# --------------------------------------------------------------------------- 输出


def format_lines(h7: dict) -> list[str]:
    """人读的明细行。`H7_rc` 只出现在**这里**与 `pairs[]` 里，绝不写回 H2 的读数。"""
    lines = [
        f"H7 clean sequence (H4 asc, n={len(h7['clean_sequence'])}): {h7['clean_sequence']}"
    ]
    for pair in h7["pairs"]:
        lines.append(
            f"  H7 PAIR {pair['left_ref']} × {pair['right_ref']}"
            f"  H7_rc={pair['H7_rc']}/{pair['H7_status']}  files={pair['files']}"
        )
    if h7.get("stopped_at"):
        lines.append(f"  H7 stopped at {h7['stopped_at']}: {h7.get('stopped_reason')}")
    if h7.get("error"):
        lines.append(f"  H7 ERROR {h7['error']}")
    lines.append(
        f"H7 pairs={len(h7['pairs'])} excluded={len(h7['excluded'])}"
        "（不在 clean 序列里的候选；0/1 个 clean 候选时本判据安静）"
    )
    return lines
