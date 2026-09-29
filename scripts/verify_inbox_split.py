#!/usr/bin/env python3
"""LUM-2544：证明 `crates/mc-http/tests/inbox.rs` -> `inbox/` 子模块是**纯搬家**。

门禁全绿不能自证纯搬家（编译器抓漏，抓不到「顺手改了别的」）。本脚本按花括号配平
切出原文件的每一个顶层 item，逐个在拆分后的文件集合里按**重数**查找，命中不了就
报「未登记缺失」。

唯一允许的规范化：**拆分必需的可见性窄化**（`fn` -> `pub(crate) fn`、
`struct X` -> `pub(crate) struct X` 及其字段），逐条登记在 `REGISTRY`，
登记前用 `grep` 确认真实数量（第 4 批踩过「登记 4 处、实际 3 处」）。

`cargo fmt` 会重排长签名换行 => 比对有两层：先「逐字命中」，再「仅排版差异」
（token 序列一致）。两类**分开计数上报**。

用法:
    python3 scripts/verify_inbox_split.py                 # 自动回溯到原文件所在的提交
    python3 scripts/verify_inbox_split.py --ref <sha>     # 指定 git ref
    python3 scripts/verify_inbox_split.py --orig <path>   # 直接读一个原文件副本
退出码: 0 = 未登记缺失 = 0 且 impl 自洽且无 item 外的可执行行丢失且全部文件 <= 800 行
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
TEST_DIR = REPO / "crates" / "mc-http" / "tests" / "inbox"
ORIG = "crates/mc-http/tests/inbox.rs"

# ---------------------------------------------------------------------------
# 可见性窄化登记表：(标签, 期望出现次数, 归一化前的行首, 归一化后的行首)
# 每条都用 grep 核过真实数量；脚本运行时会再钉一次，数量不符直接 fail。
# ---------------------------------------------------------------------------
REGISTRY: list[tuple[str, int, str, str]] = [
    ("fn build_state", 1, "\nfn build_state(", "\npub(crate) fn build_state("),
    ("fn lazy_state", 1, "\nfn lazy_state(", "\npub(crate) fn lazy_state("),
    ("async fn connect", 1, "\nasync fn connect(", "\npub(crate) async fn connect("),
    ("async fn call", 1, "\nasync fn call(", "\npub(crate) async fn call("),
    ("fn message", 1, "\nfn message(", "\npub(crate) fn message("),
    ("async fn get", 1, "\nasync fn get(", "\npub(crate) async fn get("),
    ("async fn post", 1, "\nasync fn post(", "\npub(crate) async fn post("),
    ("async fn seed", 1, "\nasync fn seed(", "\npub(crate) async fn seed("),
    ("async fn cleanup", 1, "\nasync fn cleanup(", "\npub(crate) async fn cleanup("),
    ("async fn new_issue", 1, "\nasync fn new_issue(", "\npub(crate) async fn new_issue("),
    ("async fn new_item", 1, "\nasync fn new_item(", "\npub(crate) async fn new_item("),
    ("fn find", 1, "\nfn find(", "\npub(crate) fn find("),
    ("fn ids", 1, "\nfn ids(", "\npub(crate) fn ids("),
    ("struct Fx", 1, "\nstruct Fx {", "\npub(crate) struct Fx {"),
    # Fx 的字段：用**两行**针脚，`    ws: Uuid,` 单独一行会撞上 `new_item`/`new_issue`
    # 的多行签名（同名的 `ws: Uuid` 参数共 3 处）—— 第 4 批踩过「登记 4 处、实际 3 处」。
    (
        "Fx.ws",
        1,
        "\n    ws: Uuid,\n    owner: Uuid,",
        "\n    pub(crate) ws: Uuid,\n    pub(crate) owner: Uuid,",
    ),
    (
        "Fx.owner",
        1,
        "\n    owner: Uuid,\n    peer: Uuid,",
        "\n    pub(crate) owner: Uuid,\n    pub(crate) peer: Uuid,",
    ),
    (
        "Fx.peer",
        1,
        "\n    peer: Uuid,\n    ///",
        "\n    pub(crate) peer: Uuid,\n    ///",
    ),
    (
        "Fx.third",
        1,
        "\n    third: Uuid,\n    ///",
        "\n    pub(crate) third: Uuid,\n    ///",
    ),
    (
        "Fx.outsider",
        1,
        "\n    outsider: Uuid,\n}",
        "\n    pub(crate) outsider: Uuid,\n}",
    ),
]

# 顶层 item 的行首识别。**刻意不含 `use ` / `//!`**：use 块与模块文档会按职责被重新
# 分配到各子模块，由 [1b] 单独按「路径全集」核，而不是当 item 比。
ITEM_START = re.compile(
    r"^(#\[|///|struct |enum |impl |trait |fn |async fn |pub |const |static |type |mod )"
)
# `use` 路径（`use a::b::{c, d};` -> `a::b::c`、`a::b::d`）
USE = re.compile(r"^use\s+([^;]+);")
# 「可执行代码行」：排除空行、纯注释、外/内属性（`#[...]` / `#![...]`）、`use` 声明。
EXECUTABLE = re.compile(r"^(?!\s*$)(?!\s*//)(?!\s*#!?\[)(?!\s*use\s)")
# 分隔 token：注释 / 字符串 / 标识符 / 数字 / 标点（排版无关的 token 序列）
TOKEN = re.compile(
    r"//[^\n]*|r#*\"(?:[^\"\\]|\\.)*\"|\"(?:[^\"\\]|\\.)*\"|'(?:\\.|[^'\\])'|[A-Za-z_][A-Za-z0-9_]*|\d[\w.]*|\S"
)
NAME = re.compile(
    r"(?:async fn|fn|struct|enum|impl|trait|const|static|type|mod)\s+([A-Za-z_][A-Za-z0-9_]*)"
)


def strip_tokens(text: str) -> list[str]:
    """去掉注释，得到与排版无关的 token 序列（`cargo fmt` 重排换行的兜底）。"""
    return [t for t in TOKEN.findall(text) if not t.startswith("//")]


def token_hit(needle: list[str], haystacks: list[tuple[str, list[str]]]) -> str | None:
    """needle 的 token 序列是否在某个文件里作为**连续子序列**出现。返回文件名。"""
    if not needle:
        return None
    n = len(needle)
    for name, tk in haystacks:
        for i in range(len(tk) - n + 1):
            if tk[i : i + n] == needle:
                return name
    return None


def slice_items(lines: list[str]) -> list[tuple[int, int, str, str]]:
    """按花括号配平切出顶层 item，返回 (起, 止, 名字, 文本)。

    起点向上并入紧邻的属性 / `///` 文档 / `//!` 模块文档 —— 切片边界必须把文档注释
    一起带上（第 6 批 a 丢过两处 doc 注释，只有 item 级比对抓得到）。纯 `// ---`
    分隔 banner 不并入（它不属于任何一个 item）。
    """
    items: list[tuple[int, int, str, str]] = []
    i, n = 0, len(lines)
    while i < n:
        if not lines[i].strip() or not ITEM_START.match(lines[i]):
            i += 1
            continue
        start = i
        while start > 0 and lines[start - 1].strip():
            prev = lines[start - 1].lstrip()
            if prev.startswith("//") and not prev.startswith("///"):
                break  # 普通注释 / 分隔 banner：不并入
            if not ITEM_START.match(lines[start - 1]):
                break
            start -= 1
        depth, j, seen = 0, i, False
        while j < n:
            code = re.sub(r"//.*$", "", lines[j])
            code = re.sub(r'"(?:[^"\\]|\\.)*"', '""', code)
            depth += code.count("{") - code.count("}")
            seen = seen or "{" in code
            if seen and depth <= 0:
                break
            j += 1
        end = min(j, n - 1)
        m = NAME.search(lines[i])
        items.append((start, end, m.group(1) if m else lines[i].strip()[:40], "\n".join(lines[start : end + 1])))
        i = end + 1
    return items


def normalize(text: str) -> str:
    """把拆分必需的可见性窄化**还原**成原样，好让逐字比对能命中。"""
    for _, _, before, after in REGISTRY:
        text = text.replace(after, before)
    return text


def default_ref() -> str:
    """原文件所在的 ref —— **最近一次还存在 `tests/inbox.rs` 的提交**。

    提交之后 `HEAD:tests/inbox.rs` 已经不存在，所以不能写死 `HEAD`；这里从 `HEAD`
    往回找最后一个还含该路径的提交，合并前后都能直接跑。
    """
    out = subprocess.run(
        ["git", "log", "-1", "--format=%H", "--diff-filter=AM", "--", ORIG],
        cwd=REPO,
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    if not out:
        raise SystemExit(
            f"FAIL: git 历史里找不到仍含 {ORIG} 的提交（用 --ref <sha> 或 --orig <path> 指定）"
        )
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--orig", default=None, help="直接读一个原文件副本，跳过 git")
    ap.add_argument("--ref", default=None, help="原文件所在的 git ref（默认自动回溯）")
    args = ap.parse_args()

    if args.orig:
        ref = "(--orig)"
        src = Path(args.orig).read_text()
    else:
        ref = args.ref or default_ref()
        src = subprocess.run(
            ["git", "show", f"{ref}:{ORIG}"], cwd=REPO, capture_output=True, text=True, check=True
        ).stdout

    new_files = sorted(TEST_DIR.glob("*.rs"))
    if not new_files:
        print(f"FAIL: 找不到拆分后的文件 {TEST_DIR}/*.rs")
        return 1
    new_text = {f.name: f.read_text() for f in new_files}
    haystack = "\n".join(new_text.values())
    hay_norm = normalize(haystack)
    haystacks = [(n, strip_tokens(normalize(t))) for n, t in new_text.items()]

    print("=" * 78)
    print(
        "LUM-2544 纯搬家自证 —— %s @ %s (%d 行) -> %s/ (%d 文件, %d 行)"
        % (
            ORIG,
            ref[:12],
            src.count("\n"),
            TEST_DIR.name,
            len(new_files),
            sum(t.count("\n") for t in new_text.values()),
        )
    )
    print("=" * 78)

    # --- 0. 可见性登记表数量核实 --------------------------------------------
    print("\n[0] 可见性窄化登记表核实（%d 条）" % len(REGISTRY))
    reg_bad = 0
    for label, expect, before, after in REGISTRY:
        got_after, got_before = haystack.count(after), haystack.count(before)
        ok = got_after == expect and got_before == 0
        reg_bad += not ok
        print(
            "    %-22s 期望 %d 处 / 实得 %d 处，未窄化残留 %d 处  %s"
            % (label, expect, got_after, got_before, "OK" if ok else "<<< 不符")
        )
    if reg_bad:
        print(f"\nFAIL: 登记表与实际不符（{reg_bad} 条）")
        return 1

    # --- 1. item 级逐字比对（按重数） ---------------------------------------
    orig_lines = src.split("\n")
    items = slice_items(orig_lines)
    print(f"\n[1] item 级比对：原文件切出 {len(items)} 个顶层 item")

    # 同一段文本在原文件出现几次，就要求在新文件集合里至少出现几次（按重数对上）
    want: dict[str, list[tuple[int, int, str]]] = {}
    for start, end, name, text in items:
        want.setdefault(normalize(text), []).append((start, end, name))

    exact = fmt_only = 0
    missing: list[str] = []
    for needle, occ in want.items():
        need = len(occ)
        have = hay_norm.count(needle)
        if have >= need:
            exact += need
            continue
        where = token_hit(strip_tokens(needle), haystacks)
        if where:
            fmt_only += need
            print(f"    仅排版差异 {occ[0][2]} -> {where}")
        else:
            for start, end, name in occ:
                missing.append(f"{name} (原文件 L{start + 1}-L{end + 1})")

    print(f"    逐字命中            : {exact}")
    print(f"    仅排版差异(token 同): {fmt_only}")
    print(f"    未登记缺失          : {len(missing)}")
    for m in missing:
        print(f"        MISSING {m}")

    # --- 1b. use 块按职责重分配：核「路径全集」，不核位置 ---------------------
    # 模块文档（`//!`）与 use 块天然会随职责搬到各子模块，所以不按 item 逐字比，
    # 而是断言：**原文件用到的每个 use 路径，在新文件集合里仍然被导入**。
    print("\n[1b] use 块按职责重分配 —— 路径全集守恒")

    def use_paths(text: str) -> set[str]:
        out: set[str] = set()
        for line in text.split("\n"):
            m = USE.match(line.strip())
            if not m:
                continue
            body = m.group(1)
            if "{" in body:  # use a::{b, c};
                head, rest = body.split("{", 1)
                rest = rest.rsplit("}", 1)[0]
                for part in rest.split(","):
                    part = part.strip()
                    if part:
                        out.add(head.strip() + part.split(" as ")[0].strip())
            else:
                out.add(body.split(" as ")[0].strip())
        return out

    orig_uses = use_paths(src)
    new_uses = {p for t in new_text.values() for p in use_paths(t)}
    dropped = sorted(p for p in orig_uses if p not in new_uses)
    added = sorted(p for p in new_uses if p not in orig_uses)
    print(f"    原 use 路径 {len(orig_uses)} 条 / 新 {len(new_uses)} 条")
    print(f"    丢失（原文件有、新文件集合没有）: {len(dropped)} {dropped or ''}")
    print(f"    新增（子模块各自的显式导入）    : {len(added)}")
    for a in added:
        print(f"        + {a}")
    use_fail = len(dropped)

    # --- 2. 断言「没有任何可执行代码行在 item 之外丢失」 ---------------------
    print("\n[2] item 之外的可执行代码行")
    covered: set[int] = set()
    for start, end, _, _ in items:
        covered.update(range(start, end + 1))
    orphans = [
        (i + 1, orig_lines[i])
        for i in range(len(orig_lines))
        if i not in covered and EXECUTABLE.match(orig_lines[i]) and orig_lines[i].strip()
    ]
    print(f"    原文件 item 之外的可执行行 : {len(orphans)}")
    for ln, txt in orphans:
        print(f"        ORPHAN L{ln}: {txt}")
    orphan_fail = len(orphans)
    # crate 级闸门不是可执行行，但必须仍在 main.rs：丢了它，本 target 会在没开
    # test-util 的构建里也去编译（`docs/37` §221.4 的「阳性对照」思路：不靠 exit code，
    # 靠内容断言）。
    gate = '#![cfg(feature = "test-util")]'
    gate_ok = src.count(gate) == new_text.get("main.rs", "").count(gate) == 1
    print(f"    crate 级闸门 {gate!r} 仍在 main.rs: {'OK' if gate_ok else '<<< 丢失或重复'}")
    gate_fail = not gate_ok

    # --- 3. impl 块计数自洽（不许减少） -------------------------------------
    print("\n[3] impl 块计数自洽")
    orig_impls = len(re.findall(r"^impl\b", src, re.M))
    new_impls = len(re.findall(r"^impl\b", haystack, re.M))
    print(f"    原 {orig_impls} -> 新 {new_impls}（净增 {new_impls - orig_impls}）")
    impl_fail = new_impls < orig_impls

    # --- 4. 门 ⑩ 单文件 800 行上限 ------------------------------------------
    print("\n[4] 门 ⑩ 单文件 800 行上限")
    over = [n for n, t in new_text.items() if t.count("\n") > 800]
    for n, t in sorted(new_text.items()):
        print(f"    {n:<22} {t.count(chr(10)):>4} 行")
    print(f"    超限文件: {len(over)} {over or ''}")
    size_fail = len(over)

    # --- 5. 判据 ------------------------------------------------------------
    print("\n" + "=" * 78)
    ok = not (len(missing) or use_fail or orphan_fail or gate_fail or impl_fail or size_fail)
    print(
        "逐字命中 = %d | 仅排版差异 = %d | 未登记缺失 = %d | use 路径丢失 = %d | item 外可执行行 = %d"
        % (exact, fmt_only, len(missing), use_fail, orphan_fail)
    )
    print(f"判定: {'PASS' if ok else 'FAIL'}")
    print("=" * 78)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
