# R430 — Codex 配额探针（quota probe）复刻

> 目标：复刻 Node `packages/adapters/codex-local/src/server/quota.ts`，提供
> 可独立测试、与 IO 解耦的 Codex 配额探测能力。

## 背景

R428 之前，Codex 只有「执行时错误分类」`isCodexProviderQuotaError`，
没有「主动探测当前配额」的能力。Node 侧有完整的 `quota.ts`：

- `readCodexAuthInfo`：解析 `~/.codex/auth.json`（legacy + modern 两种结构）；
- `fetchCodexQuota`：调用 ChatGPT WHAM API 获取 5h / weekly / credits 窗口；
- `fetchCodexRpcQuota`：通过 `codex app-server` RPC 读取 `account/rateLimits/read`；
- `getQuotaWindows`：RPC 优先，失败后回退 WHAM，最终返回 `ProviderQuotaResult`。

## 本次新增

独立 crate `crates/pc-adapter-quota`（高内聚低耦合，无跨 adapter 依赖）：

| 模块 | 说明 |
|---|---|
| `base64_url_decode` / `decode_jwt_payload` | JWT payload 解析（email / planType） |
| `parse_codex_auth_json` | auth.json 双结构解析 |
| `normalize_codex_used_percent` | <1 视为百分比小数，封顶 100 |
| `unix_seconds_to_iso` / `seconds_to_window_label` | 时间与窗口标签 |
| `map_wham_usage` | WHAM 响应 → `QuotaWindow` |
| `map_codex_rpc_quota` | RPC rateLimits → `CodexRpcQuotaSnapshot` |
| `classify_quota_error_family` | 401 响应体 → auth 错误族 |
| `fetch_codex_quota_with` | 依赖注入版 WHAM 探测（HTTP 可 mock） |
| `truncate_body` | 响应体截断（≤4000B，防 token 泄露） |

关键安全约束（对齐 Node 测试）：
- 401 响应体仅用于错误分类，不写入最终 `error`（防 token 泄露）；
- 响应体截断 4000 字节，避免超大 body 放大错误消息；
- `normalize_codex_used_percent(0.5)` → `50`（Node 把 <1 当百分比小数）。

## 单测验证

```sh
cargo test -p pc-adapter-quota
```

12 项单测全部通过，覆盖：base64/JWT、legacy/modern auth、percent 规范化、
ISO 时间、窗口标签、WHAM 三窗口、RPC 映射、错误族分类、响应体截断。

## 与 Node 差距（此模块）

| 维度 | Node | Rust | 差距 |
|---|---|---|---|
| 纯函数（解析/映射） | ✅ | ✅ 全部复刻 | 已对齐 |
| WHAM HTTP 探测 | ✅ fetch | ✅ `fetch_codex_quota_with`（注入式） | 已对齐 |
| `codex app-server` RPC 客户端 | ✅ | ⏳ 未实现（需要子进程 JSON-Lines 协议） | **中** |
| `getQuotaWindows` 组合回退 | ✅ RPC→WHAM | ⏳ 未实现（依赖 RPC 客户端） | **中** |

## 后续

- R431：实现 `CodexRpcClient`（spawn `codex -s read-only app-server`，JSON-Lines 往返），
  并组合出 `get_quota_windows`（RPC 优先 → WHAM 回退 → ProviderQuotaResult）。
- R432：把配额窗口接入 server 层 UI / API（对齐 Node `ProviderQuotaResult` 聚合）。
