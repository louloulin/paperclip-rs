# R432：跨 Provider 配额聚合（fetchAllQuotaWindows）

## 目标
复刻 Node `server/src/services/quota-windows.ts` 的 `fetchAllQuotaWindows()`，
将 codex_local / claude_local 的配额探测结果按 provider 聚合，
单 provider 失败或超时不影响整体响应。

## 改动

### `crates/pc-adapter-quota`
- 新增 Claude 配额纯函数（对齐 `packages/adapters/claude-local/src/server/quota.ts`）：
  - `claude_to_percent` / `format_currency_amount` / `format_extra_usage_label`
  - `map_anthropic_oauth_usage`：OAuth usage API 响应 → `QuotaWindow`
  - `claude_clean_terminal_text` / `claude_normalize_for_label_search` / `claude_trim_to_latest_usage_panel`
  - `claude_extract_usage_error` / `claude_percent_from_line` / `claude_is_quota_label` / `claude_canonical_quota_label` / `claude_format_cli_detail`
  - `parse_claude_cli_usage_text`：CLI `/usage` 面板 → `QuotaWindow`（必须含 Current session）
- 新增探测层：
  - `codex_home_dir()` / `claude_config_dir()` / `read_codex_auth_info()` / `read_claude_token()`
  - `fetch_codex_wham_quota()` / `fetch_claude_oauth_quota()`
  - `probe_codex_local()`：RPC 优先 → WHAM 回退（对齐 Node `getQuotaWindows`）
  - `probe_claude_local()`：Bedrock 短路 → OAuth → CLI `/usage`（12s 超时）
  - `capture_claude_cli_quota()`：构建 `script -q` 探测命令并解析（对齐 Node 12s 超时）
- 新增聚合：
  - `provider_slug_for_adapter_type()`：`claude_local→anthropic`、`codex_local→openai`
  - `fetch_all_quota_windows()`：每个任务 20s 超时，超时返回 `ok=false` 错误结果
- 修复 `CodexRpcClient`：stdout 改用持久 `BufReader`，避免每次 take 丢弃缓冲数据

### `crates/pc-http`
- `routes/costs.rs` 的 `quota_windows` handler 由空列表改为真实聚合：
  并行 `probe_codex_local` + `probe_claude_local`，经 `fetch_all_quota_windows` 返回
  `ProviderQuotaResult[]` 的 camelCase JSON。

## 测试
- `pc-adapter-quota`：39 项单测全绿（新增 Claude OAuth/CLI 解析、聚合超时、目录解析、Bedrock 短路等）
- 真实验证（本机无 provider 登录）：
  - codex：`codex app-server` JSON-Lines 握手成功，返回 `chatgpt authentication required` → 降级错误
  - claude：CLI 探测 12s 超时返回明确错误，聚合不再被 20s 兜底截断
- 修复真实 bug：`printf '\033'` 在 Rust 字符串中产生 NUL 字节（八进制转义陷阱），
  导致 `sh -c` 拒绝执行；已改为 `\\033`。

## 待办
- claude_local 探测在无登录时耗时 12s（对齐 Node 行为）；后续可考虑缓存 auth 状态缩短路径。
