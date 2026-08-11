#!/usr/bin/env python3
"""v11_endpoint_count.py — 验证 v11-ui-happy-path.sh 包含 ≥60 endpoint。

作为 R582+ 回归保护：避免未来误删 endpoint。
退出码：0 = 通过（≥60），1 = 失败（< 60）。
"""
import re
import sys
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent.parent / "v11-ui-happy-path.sh"

if not SCRIPT.exists():
    print(f"FAIL: script not found: {SCRIPT}")
    sys.exit(1)

content = SCRIPT.read_text()
# ENDPOINTS 数组内每行格式: "name|method|path|expected"
rx = re.compile(r'^\s*"([^|]+)\|([A-Z]+)\|([^|]+)\|(\d+(?:,\d+)*)"\s*$', re.MULTILINE)
endpoints = rx.findall(content)

count = len(endpoints)
unique_count = len(set((n, m, p) for n, m, p, _ in endpoints))

print(f"V11 endpoints: total={count} unique={unique_count}")
if count < 60:
    print(f"FAIL: expected ≥60 endpoints, got {count}")
    sys.exit(1)
if count != unique_count:
    print(f"FAIL: duplicate endpoints: total={count} unique={unique_count}")
    sys.exit(1)

print(f"PASS: {count} unique endpoints (≥60 target)")
