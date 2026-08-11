# R493 — pc-cli `onboard --non-interactive` 真实路径

> 配套: `proposal.md` V2 + `design.md`。
> 目标: 把 `onboard` 从"只 print 步骤"升级为"非交互模式真生成 master key + 写 .env"。

## 改动

### 1. `Cargo.toml` (workspace)
- 新增 `rand = "0.8"` 作为 workspace 依赖（之前 pc-secrets 间接用，Cargo.lock 已有 0.8.7）。

### 2. `apps/pc-cli/Cargo.toml`
- 新增 `rand = { workspace = true }` + `base64 = { workspace = true }`。

### 3. `apps/pc-cli/src/main.rs`

**`Onboard` 变体扩展**（在原有 `--config` 上加 3 个 flag）：
- `--non-interactive` (bool): 触发真实路径
- `--output` / `-o` (Option<String>): 写到哪里（默认 stdout）
- `--force` (bool): 允许覆盖已有文件

**新增 3 个 helper**（高内聚低耦合）：

| Helper | 作用 | 依赖 |
|---|---|---|
| `render_onboard_env(master_key_b64, port, host) -> BTreeMap<String, String>` | 纯函数：按 key 排序构造 .env 内容 | 无 IO |
| `generate_master_key_b64() -> String` | `OsRng.fill_bytes(32)` + base64 (44 chars) | `rand`, `base64` |
| `onboard_command(config, non_interactive, output, force) -> Result<()>` | 调度：交互模式 print 步骤；非交互模式生成 + 写文件 | 上面两个 helper |

**修改 `run_command` 调用点**：旧 `onboard_command(config.clone())?` → `onboard_command(config.clone(), false, None, false)?;`（保留旧的交互行为）。

**新增 8 个测试**（覆盖纯函数 + 三种交互模式）：

| 测试 | 验证 |
|---|---|
| `r493_render_onboard_env_is_key_sorted_and_complete` | 6 个 key 全部出现，顺序按字母排序，host/port/master_key 透传 |
| `r493_render_onboard_env_honors_explicit_host_port` | 非默认 host="0.0.0.0" + port=8080 正确写入 |
| `r493_generate_master_key_b64_is_44_chars_and_decodes_to_32_bytes` | base64 长度 = 44，反解码得到 32 字节 |
| `r493_generate_master_key_b64_returns_distinct_values` | 两次调用值不同（32 字节熵，碰撞概率 ≈ 2^-256）|
| `r493_onboard_non_interactive_writes_env_file` | `--output /tmp/...env` 真写文件，含 `PAPERCLIP_SECRETS_MASTER_KEY=` |
| `r493_onboard_non_interactive_refuses_to_overwrite_without_force` | 目标文件已存在 → 返回错误 `"refusing to overwrite"` |
| `r493_onboard_non_interactive_force_overwrites` | `--force` 允许覆盖，旧内容被替换 |
| `r493_onboard_interactive_is_unchanged` | 交互模式不报错（旧行为兼容）|

## 设计要点
- **高内聚**：`render_onboard_env` / `generate_master_key_b64` 都是纯函数，单测不依赖文件系统或网络。
- **低耦合**：`onboard_command` 只调 2 个 helper + `std::fs::write`；不引入 `pc-config` / `pc-secrets` 的类型（避免在 CLI 层再拉一坨依赖图）。
- **Node 1:1 对齐**：`generate_master_key_b64` 等价于上游 `randomBytes(32).toString("base64")`（`loadOrCreateGeneratedSecret` 路径）；输出走 `STANDARD` base64，与 `pc-secrets::key_store` 编码一致。
- **安全默认**：默认拒绝覆盖已有 `.env`（防止误操作覆盖密钥）；`--force` 必须显式传。
- **可脚本化**：`--non-interactive --output /path/.env` 单行完成"生成 master key + 落盘"，便于 CI / Docker entrypoint / 文档脚本。

## 验证
```
cargo test -p pc-cli --bin paperclipai
test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
cargo check --workspace                    0 errors
cargo fmt -p pc-cli --check                no diff (本轮改动)
```

8 个新 R493 测试 + 11 个旧 CLI parse 测试 + 2 个 env_lab 测试 = 19 total。

## CLI 真实做事率更新
- R492 末：5/18 (≈ 30%) 真实做事
- **R493 末：6/18 (≈ 33%) 真实做事** — `onboard --non-interactive` 升为真

### V2 CLI 子命令真实状态（截至 R493）

| 子命令 | 状态 |
|---|---|
| `doctor` | ✅ 真实 (HTTP /health) |
| `db-backup` | ✅ 真实 (HTTP /api/instance/database-backups) |
| `configure` | ✅ 真实 (HTTP /api/instance/settings) |
| `allowed-hostname` | ✅ 真实 (HTTP /api/allowed-hostnames) |
| `pipelines` | ✅ 真实 (HTTP) |
| `routines` | ✅ 真实 (HTTP) |
| `client {whoami, live-events, companies, agents, issues, get, post}` | ✅ 真实 (HTTP) |
| **`onboard --non-interactive`** | ✅ 真实 (本轮) |
| `heartbeat run` | ✅ 真实 (HTTP) |
| `auth bootstrap-ceo` | ✅ 真实 (HTTP /api/auth/bootstrap-ceo) |
| `worktree list/current` | ✅ 真实 (`git worktree list` / `git rev-parse`) |
| `env-lab` | ✅ 真实 (process env + 文件写) |
| `service install-hint` | ✅ 真实 (生成 launchd plist / systemd unit) |
| `install` | 🔶 stub (本轮未动) |
| `uninstall` | 🔶 stub (本轮未动) |
| `update` | 🔶 stub (本轮未动) |
| `env` | 🔶 stub (本轮未动) |
| `run` | 🔶 stub (调 onboard + doctor，print "would now run") |

## 下一步候选
1. **R494 — V2 CLI 深化剩余 4 个 stub**（`install` / `uninstall` / `update` / `run`）— V2 完成度 33% → 60%。
2. **R494 — R492 接入**：`pc-decisions::DecisionService.create` 扩签名接 options/inputs/expiresAt → 接入 R492 pure helpers。
3. **R494 — `pc-decision-training` 复刻 `findCommitSha`**：用 R492 新增的 `find_commit_sha` 复刻 decision-training.ts 的 1:1 行为。
