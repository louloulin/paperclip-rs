#!/usr/bin/env python3
"""Workspace 依赖审计（plan1 §3.2 / LUM-1389 任务 3）。

只读脚本，不改任何文件、不判红（永远 exit 0，方便随时手工跑）。五类检查：

A. 成员里**直接写 version** 的依赖（版本真相不在 workspace 层），并区分
   - A1: `[workspace.dependencies]` 已有同名条目（改一行引用即可）
   - A2: 不在 `[workspace.dependencies]`（需要先 hoist）
   - A3: `package =` 改名引用（如 tower04 → tower 0.4），改名后无法直接继承
B. `Cargo.lock` 里同名 crate 的多版本（重复版本）。含大量传递依赖，属正常现象；
   判定"是否本仓可控"要用 `cargo tree -d --invert <crate>` 看是谁拉进来的。
C. feature / default-features 分歧：
   - C1（真分歧）：**normal / build** 依赖上成员额外加 features，或成员之间 features 不一致
   - C2（信息性）：**dev-dependencies** 上额外加 features —— 这是 `{ workspace = true, features = [...] }`
     的正常用法（测试专用 feature 不该污染整个 workspace），登记但不修
D. `[workspace.dependencies]` 里没有任何成员使用的条目（死条目）
E. `[lints]` 统一：E1 = 成员缺 `[lints] workspace = true`；E2 = 成员自带 `[lints.*]` 覆盖
   （cargo 里两者不能并存：`cannot override workspace lints`）

用法：
    python3 scripts/audit_workspace_deps.py [--repo-root .]
    python3 scripts/audit_workspace_deps.py --json      # 机器可读输出
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys
import tomllib

NORMAL_TABLES = ("dependencies", "build-dependencies")
DEV_TABLES = ("dev-dependencies",)


def load(path: pathlib.Path) -> dict:
    with path.open("rb") as fh:
        return tomllib.load(fh)


def as_spec(raw: object) -> dict:
    """`foo = "1"` 与 `foo = { version = "1" }` 都归一成 dict。"""
    return raw if isinstance(raw, dict) else {"version": raw}


def member_manifests(root: pathlib.Path, members: list[str]) -> list[pathlib.Path]:
    """展开 members 里的 glob / 字面路径 → 每个成员的 Cargo.toml。"""
    out: list[pathlib.Path] = []
    for pattern in members:
        for hit in sorted(root.glob(pattern)):
            manifest = hit / "Cargo.toml"
            if manifest.is_file():
                out.append(manifest)
    return out


def lock_duplicates(lock: dict) -> dict[str, list[str]]:
    versions: dict[str, list[str]] = {}
    for pkg in lock.get("package", []):
        versions.setdefault(pkg["name"], []).append(pkg.get("version", "?"))
    return {name: sorted(vs) for name, vs in versions.items() if len(vs) > 1}


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo-root", default=".")
    ap.add_argument("--json", action="store_true", help="输出 JSON（给 CI / 后续脚本消费）")
    args = ap.parse_args()
    root = pathlib.Path(args.repo_root).resolve()

    root_manifest = load(root / "Cargo.toml")
    ws = root_manifest["workspace"]
    ws_deps: dict[str, object] = ws.get("dependencies", {})
    members: list[str] = ws["members"]
    manifests = member_manifests(root, members)

    a1: list[str] = []
    a2: list[str] = []
    a3: list[str] = []
    c1: list[str] = []
    c2: list[str] = []
    used_ws: set[str] = set()

    for manifest in manifests:
        doc = load(manifest)
        pkg = doc.get("package", {}).get("name", manifest.parent.name)
        for table in NORMAL_TABLES + DEV_TABLES:
            for name, raw in doc.get(table, {}).items():
                spec = as_spec(raw)
                renamed = spec.get("package")
                version = spec.get("version")
                if version is not None:
                    where = f"{pkg}: {name} = {json.dumps(raw, ensure_ascii=False)} ({table})"
                    if renamed is not None:
                        a3.append(
                            f"{where} → 改名继承自 [{renamed}]（workspace 已有该库，但别名需单独决策）"
                            if renamed in ws_deps
                            else f"{where} → 改名依赖，需先 hoist [{renamed}]"
                        )
                    elif name in ws_deps:
                        a1.append(f"{where} → workspace = true")
                    else:
                        a2.append(f"{where} → hoist 到 [workspace.dependencies]")
                    continue

                if spec.get("workspace") is not True:
                    continue
                used_ws.add(name)
                base_key = renamed or name
                base = as_spec(ws_deps.get(base_key, {}))
                base_feats = set(base.get("features") or [])
                extra = sorted(set(spec.get("features") or []) - base_feats)
                bits = []
                if extra:
                    bits.append(f"+{','.join(extra)}")
                if spec.get("default-features") is not None:
                    bits.append(f"default-features = {spec['default-features']}")
                if not bits:
                    continue
                row = f"{base_key}: {pkg} ({table}) {' '.join(bits)}"
                (c1 if table in NORMAL_TABLES else c2).append(row)

    dead = [f"{name} = {ws_deps[name]!r}" for name in sorted(ws_deps) if name not in used_ws]
    dupes = lock_duplicates(load(root / "Cargo.lock")) if (root / "Cargo.lock").is_file() else {}
    e1, e2 = [], []
    for manifest in manifests:
        doc = load(manifest)
        pkg = doc.get("package", {}).get("name", manifest.parent.name)
        rel = manifest.parent.relative_to(root)
        lints = doc.get("lints")
        if not isinstance(lints, dict) or lints.get("workspace") is not True:
            e1.append(f"{pkg} ({rel}) 缺 `[lints] workspace = true`")
            continue
        extra = sorted(key for key in lints if key != "workspace")
        if extra:
            e2.append(f"{pkg} ({rel}) 成员级 lints 覆盖: {extra}")

    result = {
        "repo_root": str(root),
        "members_pattern": members,
        "member_manifest_count": len(manifests),
        "workspace_dependency_count": len(ws_deps),
        "A1_explicit_version_already_in_workspace": a1,
        "A2_explicit_version_needs_hoist": a2,
        "A3_renamed_dependency": a3,
        "B_lock_duplicate_versions": {k: v for k, v in sorted(dupes.items())},
        "C1_feature_divergence_normal_deps": c1,
        "C2_feature_extras_dev_deps": c2,
        "D_unused_workspace_entries": dead,
        "E1_missing_lints_workspace": e1,
        "E2_member_lint_overrides": e2,
    }

    if args.json:
        print(json.dumps(result, indent=2, ensure_ascii=False))
        return 0

    def section(title: str, rows: list[str]) -> None:
        print(f"\n## {title} ({len(rows)})")
        if rows:
            for row in rows:
                print(f"  - {row}")
        else:
            print("  （空）")

    print(f"# workspace 依赖审计 — {root}")
    print(f"members = {members}")
    print(f"成员 manifest: {len(manifests)} 个；[workspace.dependencies]: {len(ws_deps)} 条")
    section("A1 直写 version，workspace 已有同名条目", a1)
    section("A2 直写 version，需 hoist", a2)
    section("A3 改名引用（package = ...）", a3)
    section("B Cargo.lock 同名多版本", [f"{n}: {', '.join(v)}" for n, v in sorted(dupes.items())])
    section("C1 feature 分歧（normal/build 依赖 — 应修）", c1)
    section("C2 dev-dependencies 额外 features（登记，不修）", c2)
    section("D workspace.dependencies 死条目", dead)
    section("E1 成员缺 [lints] workspace = true", e1)
    section("E2 成员级 lints 覆盖（与 workspace = true 互斥）", e2)
    print(
        f"\nFINDINGS: A1={len(a1)} A2={len(a2)} A3={len(a3)} "
        f"B={len(dupes)} C1={len(c1)} C2={len(c2)} D={len(dead)} "
        f"E1={len(e1)} E2={len(e2)}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
