"""输入面：db-mode 的 report json 与静态面的 `contracts/golden` 两种读法。"""

from __future__ import annotations

import json
import os


def load_from_report(path: str) -> tuple[list[dict], dict, str, bool]:
    with open(path, encoding="utf-8") as fh:
        doc = json.load(fh)
    rows = [dict(fx) for fx in doc.get("fixtures", [])]
    for row in rows:
        row["domain"] = row.get("domain") or str(row.get("id") or "").split("/", 1)[0]
    return rows, doc.get("totals", {}), "conformance_db_json", True


def load_from_golden(root: str) -> tuple[list[dict], dict, str, bool]:
    rows = []
    for row in _scan_golden(root):
        rows.append(
            {
                "id": row.get("id"),
                "domain": str(row.get("id") or "").split("/", 1)[0],
                "method": row.get("method"),
                "path": row.get("path"),
                "actor": (row.get("actor") or {}).get("kind"),
                "status_expected": (row.get("expect") or {}).get("status"),
                "status_observed": None,  # 静态面没有实测值
                "outcome": None,
                "source": "{}/{}".format(
                    (row.get("source") or {}).get("file", ""),
                    (row.get("source") or {}).get("line", ""),
                ),
                "_golden": row,
            }
        )
    rows.sort(key=lambda r: r["id"] or "")
    return rows, {}, "contracts_golden", False


def _scan_golden(root: str) -> list[dict]:
    docs = []
    for dirpath, _dirnames, names in os.walk(root):
        for name in names:
            if not name.endswith(".json"):
                continue
            try:
                with open(os.path.join(dirpath, name), encoding="utf-8") as fh:
                    doc = json.load(fh)
            except (OSError, ValueError):
                continue  # 非 fixture 的 json（PIN / 索引类）不是契约，跳过
            if isinstance(doc, dict) and "id" in doc and "expect" in doc:
                docs.append(doc)
    return docs


def attach_golden(rows: list[dict], root: str) -> list[str]:
    """把 golden 文档挂到 report 行上（归因判据要读身份 / 请求形状；**零编译**）。"""
    docs = {d["id"]: d for d in _scan_golden(root)}
    for row in rows:
        row["_golden"] = docs.get(row.get("id"))
    return [r["id"] for r in rows if r.get("_golden") is None]
