#!/usr/bin/env python3
"""`scripts/slash_alias_audit.py` 的 `unittest`（LUM-2620 / T1-6-Q1，`docs/37 §290`）。

门 ⑦ 的**第二条**命令（`scripts/gates.sh:699`）每天都在跑这个 376 行的判定器，而本文件落地
之前它 **0 用例、0 门执行**（`scripts/tests.manifest` 里没有对应行）。同族第三片
（`LUM-2606` / `LUM-2608` / `LUM-2617`）。

**只测不改**：缺陷一律登记在 `KNOWN_DEFECTS` + `@unittest.expectedFailure` ⇒ 门 ⑫ 仍绿但
用例每天都跑；谁修好实现 ⇒ `UNEXPECTED SUCCESS` ⇒ 门红 ⇒ 必须同时删装饰器并改本表。

读数纪律（`§283`：**用例数可以是绿的**）：每条断言都先实测过，含「今天是什么行为」。
`EXTRA_ALIAS` 按 docstring 是 **warning 不是 defect**（实测 rc=0）—— 钉成 rc=0；若照字面
当 defect 断言，就会写出一个「钉住错误行为」的绿用例。

零 Rust / 零 cargo / 零真库 / 零磁盘，纯标准库。
Run: ``python3 scripts/test_slash_alias_audit.py``
"""

import contextlib
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
import warnings

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import slash_alias_audit as sa  # noqa: E402

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
REAL_UPSTREAM = os.path.join(ROOT, "docs", "fixtures", "upstream-routes.tsv")
REAL_ALLOWLIST = os.path.join(ROOT, "docs", "fixtures", "slash-alias-allowlist.tsv")

#: 缺陷号 → 钉住它的用例、它声明的契约、当轮实测到的现象、收口方式。**四条字段都必填**。
KNOWN_DEFECTS = {
    "KD-1": {
        "case": "TestKnownDefects.test_kd1_no_allowlist_with_explicit_allowlist_is_strict_not_missing",
        "claim": "`--no-allowlist` 的 help 写「run strict: every finding is a defect」⇒ 与 `--allowlist` 同时给时应当**忽略**该路径并严格判词，不是报「文件不存在」",
        "observed": "`allowlist_path = None if args.no_allowlist else …` 之后 `elif args.allowlist:` 仍然成立 ⇒ rc=2 + `error: allowlist not found: <真实存在的路径>`；同一棵树上单给 `--allowlist` 是 rc=0",
        "close": "把存在性检查绑到 `allowlist_path`（而不是 `args.allowlist`）上，然后删掉装饰器",
    },
    "KD-2": {
        "case": "TestKnownDefects.test_kd2_declared_mode_also_judges_stale_allowlist_rows",
        "claim": "docstring 把「key 不再触发的名单行本身就是缺陷（exit 1）」写成**全局**规则 ⇒ `--declared` 也该判",
        "observed": "`--declared` 分支从头到尾没有 `stale` 变量、`--json` 报告里也没有 `stale_allowlist` 键 ⇒ 同一份带陈旧行的名单，tree 模式 rc=1、declared 模式 rc=0",
        "close": "两个分支共用同一段 stale 计算，然后删掉装饰器",
    },
    "KD-3": {
        "case": "TestKnownDefects.test_kd3_duplicate_upstream_spellings_are_not_silently_collapsed",
        "claim": "`load_upstream` 的 docstring 说它保留「尾斜杠」这个区分 ⇒ 同一个 folded key 的两种写法不该被**静默**合并成一条",
        "observed": "`out[(meth, fold(raw))] = raw` 后写覆盖先写 ⇒ 2 行输入得 1 条，**顺序还决定判词**（`/x` 在前得 MISSING_ALIAS 侧，`/x/` 在前得 MISSING_EXACT 侧）",
        "close": "同一 folded key 出现两种尾斜杠形态时报错（或至少计数），然后删掉装饰器",
    },
    "KD-4": {
        "case": "TestKnownDefects.test_kd4_an_empty_owner_column_does_not_shift_why_into_owner",
        "claim": "名单格式是 `METHOD<TAB>path<TAB>owner<TAB>why` 四列 ⇒ 某列为空时其余列**就地**为空，不该整体左移",
        "observed": "`[p.strip() for p in line.split(\"\\t\") if p.strip()]` 先丢掉空字段 ⇒ `GET\\t/x/\\t\\twhy` 解析成 owner=`why`、why=`\"\"`；空格分隔行则把整段尾巴塞进 owner（`line.split(None, 2)`）",
        "close": "改成 `line.split(\"\\t\")` 保留空位（仅在分隔符不是 TAB 时才走空格回退），然后删掉装饰器",
    },
    "KD-5": {
        "case": "TestKnownDefects.test_kd5_upstream_method_case_is_normalised_like_the_other_loaders",
        "claim": "`load_allowlist` 与 `--declared` 都对 METHOD 做 `.upper()` ⇒ `load_upstream` 也该做，同一个键的大小写不该决定它能不能匹配",
        "observed": "`load_upstream` 原样存 `parts[0].strip()` ⇒ 上游 fixture 里一行 `get\\t/x/` 产出键 `('get', '/x')`，与任何 `GET` 注册永不相交（静默漏审，而不是报错）",
        "close": "`meth = parts[0].strip().upper()`，然后删掉装饰器",
    },
    "KD-6": {
        "case": "TestKnownDefects.test_kd6_an_unknown_base_ref_is_an_error_not_a_whole_tree_scan",
        "claim": "`--base-ref` 的 help 说「only consider routes added relative to this git ref」⇒ ref 不存在时应当报错",
        "observed": "`w3b_premerge_audit.git()` 在 returncode≠0 时返回 `\"\"` ⇒ 基准清单为空 ⇒ `routes = 全树 - ∅` ⇒ **静默变成审全树**（实测 2 处缺陷，而同一个 ref 存在时是 1 处），无任何提示",
        "close": "在 `routes_at` 里把 git 失败与「空清单」区分开（或在 main 里先 `rev-parse --verify`），然后删掉装饰器",
    },
    "KD-7": {
        "case": "TestKnownDefects.test_kd7_an_empty_path_does_not_fold_onto_the_root",
        "claim": "空的 path 列是坏数据 ⇒ 不该与合法的根路径 `/` 折叠成同一个键",
        "observed": "`fold` 结尾是 `… .rstrip(\"/\") or \"/\"` ⇒ `fold(\"\") == fold(\"/\") == \"/\"`；上游 fixture 里一行 `GET\\t` 会与 `GET\\t/` 抢同一个键（`len(parts) < 2` 只挡得住少列，挡不住空列）",
        "close": "`fold` 对空串返回空串（并在 `load_upstream` 里报错），然后删掉装饰器",
    },
    "KD-8": {
        "case": "TestKnownDefects.test_kd8_scanning_a_tree_leaks_no_file_handles",
        "claim": "树扫描不该每读一个 `.rs` 就漏一个文件句柄 ⇒ 任何开启 warning 的 runner 下都**不该**往 stderr 写东西",
        "observed": "抽取器 `w3b_premerge_audit.py:177` 写的是 `inv |= extract_routes(open(p, …).read())`（无 `with`）⇒ 一次全树扫描对每个 `.rs` 漏一个句柄；unittest 默认开启 warning，于是**每一次树扫描**都往 stderr 写一段 `ResourceWarning` 块 ⇒ 「干净且静默」这句话不成立（本片 4 条用例的 stderr 断言就是被它顶红的）",
        "close": "改成 `with open(p, …) as fh: inv |= extract_routes(fh.read())`，然后删掉装饰器",
    },
}


def _marked_expected_failures():
    """Every `test_*` in this module carrying `@unittest.expectedFailure`, as `Class.method`."""
    marked = set()
    for name, obj in sorted(globals().items()):
        if not (isinstance(obj, type) and issubclass(obj, unittest.TestCase)):
            continue
        for attr in sorted(dir(obj)):
            if attr.startswith("test") and getattr(obj, attr).__dict__.get(
                "__unittest_expecting_failure__", False
            ):
                marked.add(f"{name}.{attr}")
    return marked


# ── 夹具助手 ────────────────────────────────────────────────────────────────────
# 树扫描只读 `<tree>/crates/mc-http/src/**.rs` ⇒ 每棵合成树都必须造出这个目录。

def _write(path, text):
    with open(path, "w", encoding="utf-8") as fh:
        fh.write(text)


def _make_tree(root, routes):
    """A tree registering `routes` (list of (METHOD, path)); returns the tree root."""
    src = os.path.join(root, "crates", "mc-http", "src")
    os.makedirs(src, exist_ok=True)
    body = "\n".join(
        'pub fn r%d() -> Router { Router::new().route("%s", any(get(h%d)))\n}'
        % (i, path, i)
        for i, (_meth, path) in enumerate(routes)
    )
    _write(os.path.join(src, "routes.rs"), body)
    return root


def _tsv(tmp, name, rows):
    path = os.path.join(tmp, name)
    _write(path, "".join(r + "\n" for r in rows))
    return path


def _upstream_of(tmp, rows, name="up.tsv"):
    """An upstream fixture file; the *loader* folds it, so callers pass raw literals."""
    return _tsv(tmp, name, rows)


def _run(argv):
    """`main(argv)` with stdout/stderr captured; SystemExit folded into a return code."""
    bo, be = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(bo), contextlib.redirect_stderr(be):
        try:
            rc = sa.main(argv)
        except SystemExit as exc:
            rc = exc.code
    return rc, bo.getvalue(), be.getvalue()


class _TreeCase(unittest.TestCase):
    """Base: a temp dir per test, plus a tree holding one defective route by default."""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.tmp = self._tmp.name

    def tree_with(self, routes):
        return _make_tree(os.path.join(self.tmp, "tree"), routes)

    def empty_allowlist(self, name="al.tsv"):
        return _tsv(self.tmp, name, ["# no rows"])


# ── 登记表 ──────────────────────────────────────────────────────────────────────


class TestKnownDefectRegistry(unittest.TestCase):
    """登记表与装饰器**双向**校验：只查一个方向，删掉登记就没人再看它。"""
    def test_every_registered_defect_has_a_standing_xfail_case(self):
        registered = {d["case"] for d in KNOWN_DEFECTS.values()}
        self.assertEqual(registered - _marked_expected_failures(), set())
    def test_nothing_is_marked_xfail_without_being_registered(self):
        registered = {d["case"] for d in KNOWN_DEFECTS.values()}
        self.assertEqual(_marked_expected_failures() - registered, set())
    def test_each_entry_names_a_claim_an_observation_and_a_way_out(self):
        for defect_id, entry in sorted(KNOWN_DEFECTS.items()):
            for field in ("case", "claim", "observed", "close"):
                self.assertTrue(entry.get(field), f"{defect_id}.{field} is empty")
    def test_the_registry_is_json_serialisable_so_it_can_be_cited_by_docs(self):
        self.assertEqual(json.loads(json.dumps(KNOWN_DEFECTS)), KNOWN_DEFECTS)
        for defect_id in KNOWN_DEFECTS:
            self.assertRegex(defect_id, r"^KD-\d+$")


# ── 缺陷：xfail（正确的那一侧） ─────────────────────────────────────────────────


class TestKnownDefects(unittest.TestCase):
    """KD-1…KD-7 —— 断言在**正确**的那一侧；每条都有 `test_kdN_today_…` 绿用例作对照。"""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.tmp = self._tmp.name

    def _strict_tree(self):
        """A tree whose single route is a MISSING_EXACT (upstream serves `/k` plain)."""
        tree = _make_tree(os.path.join(self.tmp, "t"), [("GET", "/k/")])
        up = _upstream_of(self.tmp, ["GET\t/k"])
        return tree, up, _tsv(self.tmp, "al.tsv", ["GET\t/k/\towner\twhy"])
    # KD-1 -------------------------------------------------------------------
    @unittest.expectedFailure
    def test_kd1_no_allowlist_with_explicit_allowlist_is_strict_not_missing(self):
        """🔴 KD-1：`gates.sh:82` 把 `--no-allowlist` 写成**查缺口的正规手段**，而它与一个**真实存在**的 `--allowlist` 同给时走 `elif args.allowlist:` ⇒ rc=2「文件不存在」。"""
        tree, up, al = self._strict_tree()
        rc, _out, err = _run(["--tree", tree, "--upstream", up,
                              "--allowlist", al, "--no-allowlist"])
        self.assertEqual(rc, 1, f"strict mode should report the finding, not a missing file: {err}")
        self.assertIn("FAIL: 1 trailing-slash shape defect(s)", err)
    def test_kd1_today_the_flag_pair_exits_2_with_a_bogus_missing_file_message(self):
        """KD-1 今天行为（绿）：存在的文件被报成不存在。"""
        tree, up, al = self._strict_tree()
        self.assertTrue(os.path.exists(al))
        rc, _out, err = _run(["--tree", tree, "--upstream", up,
                              "--allowlist", al, "--no-allowlist"])
        self.assertEqual(rc, 2)
        self.assertIn("allowlist not found", err)
        # 对照：单给 --allowlist（不加 --no-allowlist）时同一棵树是 rc=0。
        self.assertEqual(_run(["--tree", tree, "--upstream", up, "--allowlist", al])[0], 0)
        # 对照：单给 --no-allowlist 时**不**报「不存在」—— 缺陷在两个 flag 的组合上。
        self.assertEqual(_run(["--tree", tree, "--upstream", up, "--no-allowlist"])[0], 1)
    # KD-2 -------------------------------------------------------------------
    @unittest.expectedFailure
    def test_kd2_declared_mode_also_judges_stale_allowlist_rows(self):
        """🔴 KD-2：docstring 把「key 不再触发的名单行本身就是缺陷」写成**全局**规则，tree 模式照做，`--declared` 分支里根本没有 `stale` 变量 ⇒ rc=0。"""
        up = _upstream_of(self.tmp, ["GET\t/k"])
        al = _tsv(self.tmp, "al.tsv", ["GET\t/vanished/\towner\twhy"])
        dec = _tsv(self.tmp, "dec.tsv", ["GET\t/k"])
        rc, out, err = _run(["--declared", dec, "--upstream", up, "--allowlist", al])
        self.assertEqual(rc, 1, f"a stale allowlist row must be a defect here too: {err}")
        self.assertIn("STALE", out)
    def test_kd2_today_declared_mode_ignores_a_stale_allowlist_row(self):
        """KD-2 今天行为（绿）：同一份名单，tree 模式红、declared 模式绿。"""
        up = _upstream_of(self.tmp, ["GET\t/k"])
        al = _tsv(self.tmp, "al.tsv", ["GET\t/vanished/\towner\twhy"])
        dec = _tsv(self.tmp, "dec.tsv", ["GET\t/k"])
        tree = _make_tree(os.path.join(self.tmp, "t"), [("GET", "/k")])
        self.assertEqual(_run(["--tree", tree, "--upstream", up, "--allowlist", al])[0], 1)
        rc, out, _err = _run(["--declared", dec, "--upstream", up, "--allowlist", al])
        self.assertEqual(rc, 0)
        self.assertNotIn("STALE", out)
        # --declared 的 --json 报告里连 stale 字段都没有 ⇒ 机器读数同样看不见。
        rc, out, _err = _run(["--declared", dec, "--upstream", up,
                              "--allowlist", al, "--json"])
        self.assertNotIn("stale_allowlist", json.loads(out))
    # KD-3 -------------------------------------------------------------------
    @unittest.expectedFailure
    def test_kd3_duplicate_upstream_spellings_are_not_silently_collapsed(self):
        """🔴 KD-3：`load_upstream` 自称保留「trailing slash preserved」，可 `out[(meth, fold(raw))] = raw` 在**折叠之后**做覆盖 ⇒ 2 行输入得 1 条，顺序决定判词。"""
        both = _upstream_of(self.tmp, ["GET\t/x/", "GET\t/x"])
        self.assertEqual(len(sa.load_upstream(both)), 2,
                         "both spellings must survive as distinct data")
    def test_kd3_today_the_second_spelling_wins_and_the_count_drops(self):
        """KD-3 今天行为（绿）：2 行输入 → 1 条，且**顺序决定判词**。"""
        slash_first = _upstream_of(self.tmp, ["GET\t/x/", "GET\t/x"], "a.tsv")
        plain_first = _upstream_of(self.tmp, ["GET\t/x", "GET\t/x/"], "b.tsv")
        # 后写覆盖先写 ⇒ 活下来的字面量正好相反。
        self.assertEqual(sa.load_upstream(slash_first), {("GET", "/x"): "/x"})
        self.assertEqual(sa.load_upstream(plain_first), {("GET", "/x"): "/x/"})
        # 于是**同一棵注册树**（只注册了平写）在两份上游表下得到相反的判词：
        # 留 `/x` ⇒ 上游只服务平写 ⇒ 注册正确 ⇒ 无发现；留 `/x/` ⇒ 上游服务双形态 ⇒ 缺别名。
        self.assertEqual(sa.audit({("GET", "/x")}, sa.load_upstream(slash_first)), [])
        self.assertEqual([f["kind"] for f in sa.audit({("GET", "/x")}, sa.load_upstream(plain_first))],
                         ["MISSING_ALIAS"])
    # KD-4 -------------------------------------------------------------------
    @unittest.expectedFailure
    def test_kd4_an_empty_owner_column_does_not_shift_why_into_owner(self):
        """🔴 KD-4：四列格式里某列为空时 `[… if p.strip()]` 先丢掉空字段 ⇒ 整行左移，`why` 顶上来当 owner。判红判绿不变 ⇒ 门是绿的，而名单不再自解释。"""
        al = _tsv(self.tmp, "al.tsv", ["GET\t/x/\t\tbecause upstream serves both"])
        self.assertEqual(sa.load_allowlist(al), {"GET\t/x": ("?", "because upstream serves both")})
    def test_kd4_today_an_empty_owner_column_shifts_the_row_left(self):
        """KD-4 今天行为（绿）：左移后的读数，以及空格分隔行的另一种错法。"""
        al = _tsv(self.tmp, "al.tsv", ["GET\t/x/\t\tbecause upstream serves both"])
        self.assertEqual(sa.load_allowlist(al), {"GET\t/x": ("because upstream serves both", "")})
        # 两列行（没有 owner / why）才落到 ("?", "") 这个设计中的缺省。
        two = _tsv(self.tmp, "two.tsv", ["GET\t/x/"])
        self.assertEqual(sa.load_allowlist(two), {"GET\t/x": ("?", "")})
        # 空格分隔回退 `split(None, 2)` 把整段尾巴塞进 owner。
        sp = _tsv(self.tmp, "sp.tsv", ["GET /x/ owner why"])
        self.assertEqual(sa.load_allowlist(sp), {"GET\t/x": ("owner why", "")})
        # 四列齐全时是对的 —— 缺陷只在**缺列**时发作。
        full = _tsv(self.tmp, "full.tsv", ["GET\t/x/\towner\twhy"])
        self.assertEqual(sa.load_allowlist(full), {"GET\t/x": ("owner", "why")})
    # KD-5 -------------------------------------------------------------------
    @unittest.expectedFailure
    def test_kd5_upstream_method_case_is_normalised_like_the_other_loaders(self):
        """🔴 KD-5：`load_upstream` 是三个 loader 里唯一不 `.upper()` METHOD 的：名单与 `--declared` 都做了，于是上游 fixture 里一行小写 method 产出永不相交的键 ⇒ 静默漏审。"""
        up = _upstream_of(self.tmp, ["get\t/x/"])
        self.assertIn(("GET", "/x"), sa.load_upstream(up),
                      "upstream METHOD must fold to upper case like the other two loaders")
    def test_kd5_today_only_the_allowlist_normalises_method_case(self):
        """KD-5 今天行为（绿）：同样的 `get`，名单认、上游不认。"""
        up = _upstream_of(self.tmp, ["get\t/x/"])
        al = _tsv(self.tmp, "al.tsv", ["get\t/x/\towner\twhy"])
        self.assertEqual(list(sa.load_upstream(up)), [("get", "/x")])
        self.assertEqual(list(sa.load_allowlist(al)), ["GET\t/x"])
        # 后果：上游写小写 ⇒ 同一键的注册被判成「不是上游契约」而完全不上报。
        self.assertEqual(sa.audit({("GET", "/x")}, sa.load_upstream(up)), [])
    # KD-6 -------------------------------------------------------------------
    @unittest.expectedFailure
    def test_kd6_an_unknown_base_ref_is_an_error_not_a_whole_tree_scan(self):
        """🔴 KD-6：`git()` 在 returncode≠0 时返回 `""` ⇒ 基准清单为空 ⇒ `routes = 全树 − ∅`，help 承诺的「only added」变成**相反**的集合且无任何提示。"""
        tree = self._git_tree()
        up = _upstream_of(self.tmp, ["GET\t/old", "GET\t/new"])
        base = self._base_sha
        self.assertEqual(_run(["--tree", tree, "--upstream", up, "--base-ref", base])[0], 1)
        rc, _out, err = _run(["--tree", tree, "--upstream", up,
                              "--base-ref", "0" * 40])
        self.assertIn("FAIL", err)
        self.assertNotEqual(rc, 1, "a nonexistent ref must not produce the same verdict as a real one")
    def test_kd6_today_an_unknown_base_ref_silently_scans_the_whole_tree(self):
        """KD-6 今天行为（绿）：ref 不存在时**不报错**，缺陷数 1 → 2。"""
        tree = self._git_tree()
        up = _upstream_of(self.tmp, ["GET\t/old", "GET\t/new"])
        good = _run(["--tree", tree, "--upstream", up, "--base-ref", self._base_sha])
        bad = _run(["--tree", tree, "--upstream", up, "--base-ref", "0" * 40])
        # stderr 里只有「2 处缺陷」这个**判词**，没有任何一句提到那个 ref 不存在。
        self.assertIn("FAIL: 2 trailing-slash shape defect(s)", bad[2])
        self.assertNotIn("0" * 40, bad[2])
        self.assertNotIn("bad ref", bad[2].lower())
        self.assertIn("=> 1 defect(s)", good[1])
        self.assertIn("=> 2 defect(s)", bad[1])
    def _git_tree(self):
        """A git tree whose base commit registers `/old/` and whose worktree adds `/new/`."""
        tree = os.path.join(self.tmp, "gt")
        src = os.path.join(tree, "crates", "mc-http", "src")
        os.makedirs(src)
        rel = os.path.join(src, "r.rs")
        old = 'pub fn a() -> Router { Router::new().route("/old/", any(get(h)))\n}\n'
        new = 'pub fn b() -> Router { Router::new().route("/new/", any(get(h)))\n}\n'

        def git(*a):
            return subprocess.run(["git", "-C", tree] + list(a),
                                  capture_output=True, text=True, check=False)

        git("init", "-q")
        git("config", "user.email", "probe@local")
        git("config", "user.name", "probe")
        _write(rel, old)
        git("add", "-A")
        git("commit", "-qm", "base")
        self._base_sha = git("rev-parse", "HEAD").stdout.strip()
        _write(rel, old + new)
        return tree
    # KD-8 -------------------------------------------------------------------
    @unittest.expectedFailure
    def test_kd8_scanning_a_tree_leaks_no_file_handles(self):
        """🔴 KD-8：`w3b_premerge_audit.py:177` 无 `with` ⇒ 每个 `.rs` 漏一个句柄、抛一次 `ResourceWarning`。门 ⑦ 跑子进程（默认不打印）所以门绿，但库内调用并不静默。"""
        with tempfile.TemporaryDirectory() as tmp:
            tree = _make_tree(os.path.join(tmp, "t"), [("GET", "/k")])
            up = _upstream_of(tmp, ["GET\t/k"])
            with warnings.catch_warnings(record=True) as caught:
                warnings.simplefilter("always")
                rc, _out, _err = _run(["--tree", tree, "--upstream", up])
            leaks = [w for w in caught if issubclass(w.category, ResourceWarning)]
        self.assertEqual(rc, 0)
        self.assertEqual(leaks, [], "the scanner must not leak a file handle per .rs file")
    def test_kd8_today_every_tree_scan_writes_a_resource_warning_to_stderr(self):
        """KD-8 今天行为（绿）：警告块逐个 `.rs` 出现，句柄数 = 文件数。"""
        with tempfile.TemporaryDirectory() as tmp:
            tree = _make_tree(os.path.join(tmp, "t"), [("GET", "/k"), ("GET", "/k2")])
            src = os.path.join(tree, "crates", "mc-http", "src")
            _write(os.path.join(src, "second.rs"), 'pub fn s() -> Router { Router::new() }\n')
            up = _upstream_of(tmp, ["GET\t/k"])
            with warnings.catch_warnings(record=True) as caught:
                warnings.simplefilter("always")
                rc, _out, err = _run(["--tree", tree, "--upstream", up])
            leaks = [w for w in caught if issubclass(w.category, ResourceWarning)]
        self.assertEqual(rc, 0, "警告不影响退出码 —— 这正是它能潜伏的原因")
        self.assertGreaterEqual(len(leaks), 2, "两个 .rs ⇒ 两个泄漏句柄")
        # 泄漏点在共享抽取器里，不在本脚本 —— 所以这是一条**跨文件**的读数。
        self.assertTrue(all("w3b_premerge_audit.py" in w.filename for w in leaks),
                        [w.filename for w in leaks])
        self.assertEqual(err, "", "catch_warnings 捕获时 stderr 是干净的 ⇒ 判词与警告可分离")
    # KD-7 -------------------------------------------------------------------
    @unittest.expectedFailure
    def test_kd7_an_empty_path_does_not_fold_onto_the_root(self):
        """🔴 KD-7：`fold` 把空串救成 `"/"`，而 `load_upstream` 只挡少列、**挡不住空列** ⇒ 空 path 与 `GET\t/` 抢同一个键（与 KD-3 相乘）。"""
        self.assertNotEqual(sa.fold(""), sa.fold("/"))
        up = _upstream_of(self.tmp, ["GET\t/k", "GET\t"])
        self.assertNotIn(("GET", "/k"), sa.load_upstream(up),
                         "an empty path column must not be folded onto the root key")
    def test_kd7_today_fold_maps_the_empty_path_onto_the_root(self):
        """KD-7 今天行为（绿）：空串折叠成 `/`，与根键**相撞**（后写覆盖）。"""
        self.assertEqual(sa.fold(""), "/")
        self.assertEqual(sa.fold(""), sa.fold("/"))
        # 一行真的 `GET	/` 加一行空的 `GET	` ⇒ 同一个折叠键 ⇒ 2 行输入得 1 条。
        up = _upstream_of(self.tmp, ["GET\t/", "GET\t"])
        self.assertEqual(sa.load_upstream(up), {("GET", "/"): ""})
        # 相撞的方向同样由顺序决定（与 KD-3 相乘）。
        rev = _upstream_of(self.tmp, ["GET\t", "GET\t/"], "rev.tsv")
        self.assertEqual(sa.load_upstream(rev), {("GET", "/"): "/"})
        # 不含根路径时空列只是多出一条与业务无关的键，不影响别处。
        k = _upstream_of(self.tmp, ["GET\t/k", "GET\t"], "k.tsv")
        self.assertEqual(sorted(sa.load_upstream(k)), [("GET", "/"), ("GET", "/k")])


# ── 纯函数读数 ──────────────────────────────────────────────────────────────────


class TestFoldAndCanon(unittest.TestCase):
    """`fold` 是**折叠键**，`canon` 是**报告用**的归一（保留尾斜杠）。"""
    def test_fold_strips_the_trailing_slash_because_that_is_the_thing_under_audit(self):
        self.assertEqual(sa.fold("/x"), sa.fold("/x/"))
        self.assertEqual(sa.fold("/x"), "/x")
        self.assertEqual(sa.fold("/x/"), "/x")
    def test_fold_unifies_upstream_and_axum_param_spellings(self):
        self.assertEqual(sa.fold("/a/{id}"), sa.fold("/a/:id"))
        self.assertEqual(sa.fold("/a/{id}"), "/a/:param")
        self.assertEqual(sa.fold("/a/{id}/b/{name}"), "/a/:param/b/:param")
    def test_fold_unifies_the_two_spellings_of_a_catch_all(self):
        """上游 chi 写 `/uploads/*`（无名），matchit 0.7 要求 `*key`（M10-B2 / `LUM-2113`）。"""
        self.assertEqual(sa.fold("/uploads/*"), sa.fold("/uploads/*key"))
        self.assertEqual(sa.fold("/uploads/*key/"), "/uploads/*")
    def test_fold_keeps_the_root_reachable(self):
        self.assertEqual(sa.fold("/"), "/")
        self.assertEqual(sa.fold("//"), "/")
    def test_canon_preserves_the_trailing_slash_the_report_has_to_show(self):
        """`canon` 若也折叠掉尾斜杠，报告里 `/x` 与 `/x/` 会打印成同一个字符串。"""
        self.assertEqual(sa.canon("/a/{id}/"), "/a/:param/")
        self.assertEqual(sa.canon("/a/:id"), "/a/:param")
        self.assertNotEqual(sa.canon("/x"), sa.canon("/x/"))


class TestAuditVerdicts(unittest.TestCase):
    """三条判词各自的方向；上游表的键**必须**是折叠过的。"""

    def _one(self, routes, up_key, up_raw):
        return [(f["kind"], f["id"]) for f in sa.audit(set(routes), {up_key: up_raw})]
    def test_upstream_serves_both_forms_but_only_the_plain_one_is_registered(self):
        self.assertEqual(self._one([("GET", "/x")], ("GET", "/x"), "/x/"),
                         [("MISSING_ALIAS", "GET\t/x")])
    def test_upstream_serves_both_forms_but_only_the_slashed_one_is_registered(self):
        self.assertEqual(self._one([("GET", "/x/")], ("GET", "/x"), "/x/"),
                         [("MISSING_ALIAS", "GET\t/x")])
    def test_both_forms_registered_against_a_dual_form_upstream_is_clean(self):
        self.assertEqual(self._one([("GET", "/x"), ("GET", "/x/")], ("GET", "/x"), "/x/"), [])
    def test_upstream_serves_only_the_plain_form_but_the_slashed_one_is_registered(self):
        self.assertEqual(self._one([("GET", "/x/")], ("GET", "/x"), "/x"),
                         [("MISSING_EXACT", "GET\t/x")])
    def test_upstream_serves_only_the_plain_form_and_both_are_registered_is_a_warning(self):
        """🔴 `EXTRA_ALIAS` 是 **warning**，不是 defect（docstring 明写，实测 rc=0）。"""
        self.assertEqual(self._one([("GET", "/x"), ("GET", "/x/")], ("GET", "/x"), "/x"),
                         [("EXTRA_ALIAS", "GET\t/x")])
    def test_plain_registration_against_a_plain_upstream_is_clean(self):
        self.assertEqual(self._one([("GET", "/x")], ("GET", "/x"), "/x"), [])
    def test_a_key_the_upstream_fixture_does_not_know_is_left_to_gate_sevens_first_command(self):
        """`audit` 对不存在的上游键 `continue` —— 本地独占键归 `route_parity.py` 的 local_only。"""
        self.assertEqual(self._one([("GET", "/z")], ("GET", "/other"), "/other"), [])
    def test_the_method_part_of_the_key_is_matched_exactly(self):
        self.assertEqual(self._one([("get", "/x")], ("GET", "/x"), "/x/"), [])
    def test_both_raw_spellings_of_one_folded_key_are_carried_in_the_finding(self):
        f = sa.audit({("GET", "/x"), ("GET", "/x/")}, {("GET", "/x"): "/x"})[0]
        self.assertEqual(f["registered"], ["/x", "/x/"])
        self.assertEqual(f["upstream"], "/x")
        self.assertEqual(f["key"], "GET /x")
        self.assertEqual(f["id"], "GET\t/x")
        self.assertTrue(f["why"])


class TestUpstreamLoading(unittest.TestCase):
    """`load_upstream` 的输入纪律：注释、空行、缺列。"""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.tmp = self._tmp.name
    def test_comments_blank_lines_and_short_rows_are_skipped(self):
        up = _upstream_of(self.tmp, ["# a comment", "", "   ", "GET\t/x/", "ONECOLUMN", "GET"])
        self.assertEqual(sa.load_upstream(up), {("GET", "/x"): "/x/"})
    def test_a_row_with_two_columns_but_an_empty_path_still_enters_the_table(self):
        """🔴 空列**进得来**（KD-7 的入口）：`len(parts) < 2` 挡得住少列，挡不住空列。"""
        up = _upstream_of(self.tmp, ["GET\t"])
        self.assertEqual(sa.load_upstream(up), {("GET", "/"): ""})
    def test_surrounding_whitespace_on_the_method_and_path_is_stripped(self):
        up = _upstream_of(self.tmp, ["  GET  \t  /x/  "])
        self.assertEqual(sa.load_upstream(up), {("GET", "/x"): "/x/"})


class TestAllowlistLoading(unittest.TestCase):
    """`load_allowlist` 的输入纪律与缺省值。"""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.tmp = self._tmp.name
    def test_the_method_header_row_of_the_checked_in_table_is_skipped(self):
        al = _tsv(self.tmp, "al.tsv", ["METHOD\tPATH\tOWNER\tWHY", "GET\t/x/\towner\twhy"])
        self.assertEqual(sa.load_allowlist(al), {"GET\t/x": ("owner", "why")})
    def test_a_trailing_comment_is_stripped_and_blank_lines_are_skipped(self):
        al = _tsv(self.tmp, "al.tsv", ["GET\t/x/\towner\twhy  # inline note", "", "  "])
        self.assertEqual(sa.load_allowlist(al), {"GET\t/x": ("owner", "why")})
    def test_a_two_column_row_gets_the_documented_defaults(self):
        al = _tsv(self.tmp, "al.tsv", ["GET\t/x/"])
        self.assertEqual(sa.load_allowlist(al), {"GET\t/x": ("?", "")})
    def test_a_row_with_only_the_method_is_a_hard_error_exit_2(self):
        """坏名单行不是「跳过」而是 **rc=2**：读错名单等于没读，而跳过会静默放行。"""
        al = _tsv(self.tmp, "al.tsv", ["GET", "G3T\t/x/\towner\twhy"])
        with self.assertRaises(SystemExit) as ctx:
            sa.load_allowlist(al)
        self.assertEqual(ctx.exception.code, 2)
        # 通过 main() 时它同样是 rc=2，不是 traceback。
        up = _upstream_of(self.tmp, ["GET\t/k"])
        tree = _make_tree(os.path.join(self.tmp, "t"), [("GET", "/k")])
        rc, _out, err = _run(["--tree", tree, "--upstream", up, "--allowlist", al])
        self.assertEqual(rc, 2)
        self.assertIn("bad allowlist line", err)
    def test_the_key_is_matched_against_the_folded_finding_id(self):
        """名单键与 `audit` 的 `f["id"]` 同为 `METHOD<TAB>折叠路径` ⇒ 大小写与尾斜杠都归一。"""
        al = _tsv(self.tmp, "al.tsv", ["get\t/a/{id}/\towner\twhy"])
        self.assertEqual(list(sa.load_allowlist(al)), ["GET\t/a/:param"])


# ── main() 的判词 ───────────────────────────────────────────────────────────────


class TestMainExitCodes(_TreeCase):
    """退出码是门 ⑦ 唯一的判据（`&&` 串联），所以每个码都要钉住。"""
    def test_missing_alias_exits_1(self):
        tree = self.tree_with([("GET", "/ma")])
        up = _upstream_of(self.tmp, ["GET\t/ma/"])
        rc, out, err = _run(["--tree", tree, "--upstream", up])
        self.assertEqual(rc, 1)
        self.assertIn("FAIL: 1 trailing-slash shape defect(s)", err)
        self.assertIn("MISSING_ALIAS", out)
    def test_missing_exact_exits_1(self):
        tree = self.tree_with([("GET", "/me/")])
        up = _upstream_of(self.tmp, ["GET\t/me"])
        rc, _out, err = _run(["--tree", tree, "--upstream", up])
        self.assertEqual(rc, 1)
        self.assertIn("FAIL: 1 trailing-slash shape defect(s)", err)
    def test_extra_alias_alone_exits_0_because_it_is_a_warning(self):
        """🔴 本片最容易踩的坑：把 `EXTRA_ALIAS` 当 defect 断言 ⇒ 钉住错误行为的绿用例。"""
        tree = self.tree_with([("GET", "/ea"), ("GET", "/ea/")])
        up = _upstream_of(self.tmp, ["GET\t/ea"])
        rc, out, err = _run(["--tree", tree, "--upstream", up])
        self.assertEqual(rc, 0, "EXTRA_ALIAS is a warning, not a defect")
        self.assertNotIn("FAIL", err)
        self.assertIn("EXTRA_ALIAS (1): extra alias (warning)", out)
        self.assertIn("=> 0 defect(s) from findings, 1 warning(s)", out)
    def test_a_clean_tree_exits_0(self):
        tree = self.tree_with([("GET", "/ok")])
        up = _upstream_of(self.tmp, ["GET\t/ok"])
        rc, out, err = _run(["--tree", tree, "--upstream", up])
        # stderr 上只允许有 KD-8 的警告块，**不许有判词**（`FAIL:` / `error:`）。
        self.assertEqual(rc, 0)
        self.assertNotIn("FAIL", err)
        self.assertNotIn("error:", err)
        self.assertIn("shapes OK", out)
    def test_an_allowlisted_finding_is_reported_but_not_a_defect(self):
        tree = self.tree_with([("GET", "/k/")])
        up = _upstream_of(self.tmp, ["GET\t/k"])
        al = _tsv(self.tmp, "al.tsv", ["GET\t/k/\towner\twhy"])
        rc, out, err = _run(["--tree", tree, "--upstream", up, "--allowlist", al])
        self.assertEqual(rc, 0)
        self.assertNotIn("FAIL", err)
        self.assertIn("MISSING_EXACT", out)
        self.assertIn("[allowlisted: owner]", out)
        self.assertIn("1 allowlisted (known debt", out)
    def test_no_allowlist_turns_every_finding_into_a_defect(self):
        tree = self.tree_with([("GET", "/k/")])
        up = _upstream_of(self.tmp, ["GET\t/k"])
        al = _tsv(self.tmp, "al.tsv", ["GET\t/k/\towner\twhy"])
        self.assertEqual(_run(["--tree", tree, "--upstream", up, "--allowlist", al])[0], 0)
        self.assertEqual(_run(["--tree", tree, "--upstream", up, "--no-allowlist"])[0], 1)
    def test_a_stale_allowlist_row_is_a_defect_rc1(self):
        """🔴 名单的自指负向判据（docstring）：key 不再触发的行**本身**判红（只减不增）。"""
        tree = self.tree_with([("GET", "/k")])
        up = _upstream_of(self.tmp, ["GET\t/k"])
        al = _tsv(self.tmp, "al.tsv", ["GET\t/vanished/\towner\twhy"])
        rc, out, err = _run(["--tree", tree, "--upstream", up, "--allowlist", al])
        self.assertEqual(rc, 1)
        self.assertIn("STALE allowlist row", out)
        # 缺陷数**单独**进 `=>` 行（§19.6 记的那次「stdout 绿 / stderr 红」）。
        self.assertIn("=> 0 defect(s) from findings, 1 stale allowlist row(s)", out)
        self.assertIn("FAIL: 1 trailing-slash shape defect(s)", err)
    def test_a_stale_row_named_in_the_report_points_at_the_file_that_owns_it(self):
        tree = self.tree_with([("GET", "/k")])
        up = _upstream_of(self.tmp, ["GET\t/k"])
        al = _tsv(self.tmp, "al.tsv", ["GET\t/vanished/\towner\twhy"])
        _rc, out, _err = _run(["--tree", tree, "--upstream", up, "--allowlist", al])
        self.assertIn(al, out)
    def test_an_explicit_allowlist_path_that_does_not_exist_exits_2(self):
        tree = self.tree_with([("GET", "/k")])
        up = _upstream_of(self.tmp, ["GET\t/k"])
        rc, _out, err = _run(["--tree", tree, "--upstream", up,
                              "--allowlist", os.path.join(self.tmp, "nope.tsv")])
        self.assertEqual(rc, 2)
        self.assertIn("allowlist not found", err)
    def test_quiet_prints_nothing_when_clean(self):
        tree = self.tree_with([("GET", "/ok")])
        up = _upstream_of(self.tmp, ["GET\t/ok"])
        rc, out, err = _run(["--tree", tree, "--upstream", up, "--quiet"])
        # stdout 必须**逐字节**为空（`--quiet` 的全部承诺），stderr 不许有判词。
        self.assertEqual((rc, out), (0, ""))
        self.assertNotIn("FAIL", err)
    def test_quiet_still_prints_when_there_is_a_defect(self):
        """`--quiet` 只压绿时的噪声；红时必须留得下判词。"""
        tree = self.tree_with([("GET", "/me/")])
        up = _upstream_of(self.tmp, ["GET\t/me"])
        rc, out, err = _run(["--tree", tree, "--upstream", up, "--quiet"])
        self.assertEqual(rc, 1)
        self.assertIn("MISSING_EXACT", out)
        self.assertIn("FAIL", err)
    def test_json_reports_the_machine_readable_findings(self):
        tree = self.tree_with([("GET", "/me/")])
        up = _upstream_of(self.tmp, ["GET\t/me"])
        rc, out, _err = _run(["--tree", tree, "--upstream", up, "--json"])
        rep = json.loads(out)
        self.assertEqual(rc, 1)
        self.assertEqual(rep["upstream_keys"], 1)
        self.assertEqual(rep["registered_keys"], 1)
        self.assertEqual([f["kind"] for f in rep["findings"]], ["MISSING_EXACT"])
        self.assertEqual(rep["stale_allowlist"], [])


class TestBaseRefMode(_TreeCase):
    """`--tree --base-ref`：**只报新增键**，旧键的缺陷被滤掉。"""

    def setUp(self):
        super().setUp()
        self.tree = os.path.join(self.tmp, "gt")
        src = os.path.join(self.tree, "crates", "mc-http", "src")
        os.makedirs(src)
        rel = os.path.join(src, "r.rs")
        old = 'pub fn a() -> Router { Router::new().route("/old/", any(get(h)))\n}\n'
        new = 'pub fn b() -> Router { Router::new().route("/new/", any(get(h)))\n}\n'

        def git(*a):
            return subprocess.run(["git", "-C", self.tree] + list(a),
                                  capture_output=True, text=True, check=False)

        git("init", "-q")
        git("config", "user.email", "probe@local")
        git("config", "user.name", "probe")
        _write(rel, old)
        git("add", "-A")
        git("commit", "-qm", "base")
        self.base = git("rev-parse", "HEAD").stdout.strip()
        _write(rel, old + new)

    def _up(self):
        return _upstream_of(self.tmp, ["GET\t/old", "GET\t/new"])
    def test_without_base_ref_both_old_and_new_keys_are_audited(self):
        rc, out, _err = _run(["--tree", self.tree, "--upstream", self._up()])
        self.assertEqual(rc, 1)
        self.assertIn("registered upstream-key literals: 2", out)
        self.assertIn("=> 2 defect(s)", out)
    def test_base_ref_filters_the_old_key_out(self):
        rc, out, _err = _run(["--tree", self.tree, "--upstream", self._up(),
                              "--base-ref", self.base])
        self.assertEqual(rc, 1)
        self.assertIn("registered upstream-key literals: 1", out)
        self.assertIn("=> 1 defect(s)", out)
        self.assertIn("added vs %s" % self.base, out)
        # 滤掉的正是旧键：整份报告里不再出现 /old。
        self.assertNotIn("/old", out)
        self.assertIn("/new", out)
    def test_base_ref_keeps_the_allowlist_and_stale_rules_in_force(self):
        """`--base-ref` 只改**集合**，不放松判词：一个陈旧名单行仍然判红。"""
        al = _tsv(self.tmp, "al.tsv", ["GET\t/vanished/\towner\twhy"])
        rc, out, _err = _run(["--tree", self.tree, "--upstream", self._up(),
                              "--base-ref", self.base, "--allowlist", al])
        self.assertEqual(rc, 1)
        self.assertIn("STALE allowlist row", out)


class TestDeclaredMode(_TreeCase):
    """`--declared`：代码还不存在时按声明预测。读入格式与 `--tree` 的上游表**不同构**。"""
    def test_the_declared_file_is_whitespace_separated_not_tab_separated(self):
        """🔴 读入格式差异（§290 的可交付读数）：`line.split()` ⇒ 空格或 TAB 都吃。"""
        up = _upstream_of(self.tmp, ["GET\t/k", "GET\t/dual/"])
        tabbed = _tsv(self.tmp, "tab.tsv", ["METHOD\tpath", "GET\t/dual/"])
        spaced = _tsv(self.tmp, "sp.tsv", ["METHOD path", "GET /dual/"])
        a = _run(["--declared", tabbed, "--upstream", up, "--json"])
        b = _run(["--declared", spaced, "--upstream", up, "--json"])
        self.assertEqual(json.loads(a[1])["declared_keys"], json.loads(b[1])["declared_keys"])
        self.assertEqual(json.loads(a[1])["predicted"], json.loads(b[1])["predicted"])
    def test_the_declared_table_has_the_same_header_skip_and_comment_rules(self):
        up = _upstream_of(self.tmp, ["GET\t/k"])
        dec = _tsv(self.tmp, "dec.tsv", ["METHOD\tpath", "# note", "", "  ", "GET\t/k"])
        rc, out, _err = _run(["--declared", dec, "--upstream", up, "--json"])
        self.assertEqual(rc, 0)
        self.assertEqual(json.loads(out)["declared_keys"], 1)
    def test_a_declared_path_dual_form_upstream_must_register_both_spellings(self):
        up = _upstream_of(self.tmp, ["GET\t/dual/"])
        dec = _tsv(self.tmp, "dec.tsv", ["GET\t/dual/"])
        rc, out, _err = _run(["--declared", dec, "--upstream", up, "--json"])
        rep = json.loads(out)
        self.assertEqual(rc, 1, "declaring only the slashed form of a dual-form key is a defect")
        self.assertEqual([f["kind"] for f in rep["findings"]], ["MISSING_ALIAS"])
        self.assertEqual(rep["predicted"][0]["forms"], ["/dual", "/dual/"])
        self.assertTrue(rep["predicted"][0]["dual"])
    def test_a_declared_path_single_form_upstream_must_be_declared_plain(self):
        up = _upstream_of(self.tmp, ["GET\t/single"])
        dec = _tsv(self.tmp, "dec.tsv", ["GET\t/single/"])
        rc, out, _err = _run(["--declared", dec, "--upstream", up, "--json"])
        self.assertEqual(rc, 1)
        self.assertEqual([f["kind"] for f in json.loads(out)["findings"]], ["MISSING_EXACT"])
    def test_declared_paths_absent_from_the_upstream_fixture_are_reported_as_unknown(self):
        up = _upstream_of(self.tmp, ["GET\t/k"])
        dec = _tsv(self.tmp, "dec.tsv", ["GET\t/k", "GET\t/nope/"])
        rc, out, _err = _run(["--declared", dec, "--upstream", up, "--json"])
        rep = json.loads(out)
        self.assertEqual(rc, 0)
        self.assertEqual(rep["unknown"], [["GET", "/nope/"]])
        self.assertEqual(len(rep["predicted"]), 1)
    def test_a_declared_line_with_no_path_exits_2(self):
        up = _upstream_of(self.tmp, ["GET\t/k"])
        dec = _tsv(self.tmp, "dec.tsv", ["onlyonecolumn"])
        rc, _out, err = _run(["--declared", dec, "--upstream", up])
        self.assertEqual(rc, 2)
        self.assertIn("bad declared line", err)
    def test_a_non_alpha_method_token_exits_2(self):
        up = _upstream_of(self.tmp, ["GET\t/k"])
        dec = _tsv(self.tmp, "dec.tsv", ["GET2\t/k"])
        rc, _out, err = _run(["--declared", dec, "--upstream", up])
        self.assertEqual(rc, 2)
        self.assertIn("bad declared line", err)
    def test_the_declared_human_report_prints_the_must_register_forms(self):
        up = _upstream_of(self.tmp, ["GET\t/dual/"])
        dec = _tsv(self.tmp, "dec.tsv", ["GET\t/dual/"])
        _rc, out, _err = _run(["--declared", dec, "--upstream", up])
        self.assertIn("declared 1 upstream key(s); dual-form required: 1", out)
        self.assertIn("DUAL   GET", out)
        self.assertIn("must register: /dual", out)


# ── 仓库里真实的那份读数 ────────────────────────────────────────────────────────


class TestCheckedInFixtures(unittest.TestCase):
    """`docs/fixtures/**` 是门 ⑦ 真正吃的那两份表。"""
    def test_the_checked_in_allowlist_is_header_only_so_every_finding_is_a_defect(self):
        """🔴 实测：那份名单**只有表头**，0 条数据行 ⇒ 今天门 ⑦ 的 allowlist 是空的。"""
        al = sa.load_allowlist(REAL_ALLOWLIST)
        self.assertEqual(al, {})
        rc, out, _err = _run(["--json"])
        self.assertEqual(json.loads(out)["allowlist_rows"], 0)
    def test_the_checked_in_upstream_fixture_preserves_the_dual_form_distinction(self):
        up = sa.load_upstream(REAL_UPSTREAM)
        self.assertGreater(len(up), 400)
        dual = [raw for raw in up.values() if raw.endswith("/")]
        self.assertGreater(len(dual), 0, "the fixture must still contain mounted-subrouter roots")
        # docstring 说「472 条里 82 条以 / 结尾」—— 数字会随上游变，形态不能变。
        self.assertTrue(all(not raw.endswith("//") for raw in up.values()))
    def test_the_gate_runs_this_script_quiet_and_it_is_green_on_this_tree(self):
        """门 ⑦ 第二条命令的实况：`python3 scripts/slash_alias_audit.py --quiet`。"""
        proc = subprocess.run([sys.executable, os.path.join(HERE, "slash_alias_audit.py"), "--quiet"],
                              capture_output=True, text=True, cwd=ROOT, check=False)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(proc.stdout, "")
        self.assertEqual(proc.stderr, "")


if __name__ == "__main__":
    unittest.main(verbosity=2)
