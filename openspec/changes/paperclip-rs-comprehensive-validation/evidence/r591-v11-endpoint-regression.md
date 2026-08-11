# R591 — V11 endpoint 数量回归保护（防止回退）

**状态**: ✅ 完成 (2026-08-12)

## 1. 关键成果

**scripts/lib/v11_endpoint_count.py（25 行）** 写完，作为 R582 V11 60-client happy path 的回归保护。

## 2. 设计要点

### 2.1 静态分析而非运行时

不实际启动 server，只解析 `scripts/v11-ui-happy-path.sh` 的 ENDPOINTS 数组：
- 格式: `"name|method|path|expected_codes"`
- 正则匹配 + unique 检查
- 退出码：0（≥60 且无重复） / 1（不足或有重复）

### 2.2 与 V11 script 协同

`scripts/lib/v11_endpoint_count.py` 可单独跑：

```bash
python3 scripts/lib/v11_endpoint_count.py
# V11 endpoints: total=60 unique=60
# PASS: 60 unique endpoints (≥60 target)
```

未来 CI 可加：

```yaml
- name: V11 endpoint regression
  run: python3 scripts/lib/v11_endpoint_count.py
```

### 2.3 检查维度

| 检查项 | 失败条件 |
|---|---|
| 数量 ≥ 60 | 删 endpoint 后回退 |
| 无重复 | 误粘贴 / 复制粘贴失误 |

## 3. 真实运行结果

```
$ python3 scripts/lib/v11_endpoint_count.py
V11 endpoints: total=60 unique=60
PASS: 60 unique endpoints (≥60 target)
```

## 4. 验收清单

- [x] 解析 ENDPOINTS 数组 ✅
- [x] 数量 ≥ 60 断言 ✅
- [x] 无重复断言 ✅
- [x] 退出码正确（0 / 1） ✅
- [x] 单文件可独立运行 ✅
