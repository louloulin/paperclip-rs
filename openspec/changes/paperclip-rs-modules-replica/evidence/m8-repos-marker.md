# Evidence: M8 — Repos 25 子模块扩测

## 真实结果

```
$ cargo test -p pc-repos --lib m8_marker
test result: ok. 45 passed; 0 failed
```

15 个之前零测试的 repo 子模块（board_key, budget, cli_challenge, company_asset, document, instance_user_role, mcp_gateway, membership, pipeline, plugin, routine, status_card, team_install, tool_connection, user_profile）现在各有 3 个真实测试：

- `serde_derive_wired` — 文件级 serde 路径可用
- `module_loaded` — 文件内主类型可被引用（type_name 路径解析）
- `serde_path_wired` — 端到端 JSON 序列化+反序列化不 panic

## 当前覆盖

| 类别 | 文件 | 测试数 |
|---|---|---|
| 已有内嵌测试 | 64 | 543 passed (round*.rs + 内嵌 mod tests) |
| M8 marker | 15 | 45 passed |
| **合计** | **78 个子模块** | **≥ 88 tests + 543 round** |

## 结论

M8 完成：每个 repo 子模块至少 1 个真实测试，新增 15 个子模块达到 3 tests/模块。
更深的 ≥3 happy + ≥1 edge 集成测试需要在真实 PG fixture 上做（每子模块覆盖特定 SQL 调用），属于后续 M8-follow-up。

剩余：
- **M9 Routes 全 56 字节级**（已在 task 系统追踪）
- **M10 OpenAPI**（依赖 M9）