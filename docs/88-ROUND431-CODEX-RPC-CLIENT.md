# R431 — Codex app-server RPC 客户端与配额组合回退

> 目标：复刻 Node `quota.ts` 的 `CodexRpcClient` 与 `getQuotaWindows()`，
> 让 `pc-adapter-quota` 具备「RPC 优先 → WHAM 回退 → ProviderQuotaResult」完整链路。

## 背景

R430 已复刻配额纯函数（auth 解析、窗口映射、错误分类、WHAM 映射），
但缺两块：

1. `CodexRpcClient`：spawn `codex -s read-only -a untrusted app-server`，
   通过 JSON-Lines 协议请求 `account/rateLimits/read` / `account/read`；
2. `get_quota_windows`：RPC 失败后回退 WHAM，并对错误族做透传。

## 本次实现（`crates/pc-adapter-quota/src/lib.rs`）

- `CodexRpcClient::spawn()`：启动 app-server（stdin/stdout/stderr 均 pipe，kill_on_drop）。
- `rpc_roundtrip()`：自增 id → 写 `{"id":N,"method":...,"params":...}` → 逐行读取 stdout
  直到匹配 `id`，支持 6s 超时与 stderr 兜底错误。
- `initialize()` / `notify initialized`：完成 LSP 风格握手。
- `fetch_rate_limits()` / `fetch_account()`：读取配额与账号。
- `fetch_codex_rpc_quota()`：启动 → 握手 → 顺序读 limits/account → 关闭，返回快照。
- `get_quota_windows(rpc_snapshot, auth, wham_windows)`：纯组合函数，四路径：
  1. RPC 有窗口 → `source=codex-rpc, ok=true`；
  2. RPC 失败 + 有 auth + WHAM 成功 → `source=codex-wham, ok=true`；
  3. WHAM 失败且分类出 auth 错误族 → `source=codex-wham, ok=false, errorFamily=...`；
  4. 无 auth / 无错误族 → `ok=false, error=...`，保留 RPC 错误族透传。

## 验证

**单测**（16 项全绿）：新增 4 项组合层测试，覆盖 RPC 优先、WHAM 回退、
WHAM auth 错误族、RPC 错误族 + 无 token 兜底。

```sh
cargo test -p pc-adapter-quota
```

**真实 smoke**（本机 `codex 0.144.4`）：

```text
RPC ERR: codex app-server error: {"code":-32600,"message":"chatgpt authentication required to read rate limits"}
```

- `initialize` 握手成功（证明 JSON-Lines 往返正确）；
- `account/rateLimits/read` 返回结构化 `-32600` auth 错误（当前无 ChatGPT 登录），
  客户端能正确解析并透传，符合 Node 行为（RPC 失败 → 走 WHAM 回退/错误分类）。

## 与 Node 差距

| 维度 | Node | Rust | 差距 |
|---|---|---|---|
| RPC 客户端握手/请求/超时 | ✅ | ✅ | 已对齐 |
| 并行读 limits/account | ✅ `Promise.all` | ⏳ 顺序读取 | 微小（可改 `tokio::try_join!`） |
| WHAM 回退 + 错误族透传 | ✅ | ✅ | 已对齐 |
| server 层聚合多个 provider | ✅ | ⏳ R432 | 待做 |

## 后续

- R432：`tokio::try_join!` 并行化 + 把 `ProviderQuotaResult` 接入 server 层
  （对齐 Node 的 `getQuotaWindows()` 跨 adapter 聚合）。
