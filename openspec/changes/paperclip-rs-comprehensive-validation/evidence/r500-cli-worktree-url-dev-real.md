# R500 — `pc-cli worktree url/dev` 从 stub 变成真做事

> 配套: `proposal.md` V2 + `ARCHITECTURE.md` §6 R500 路线图。
> 目标: `worktree url` 不再硬编码 `http://127.0.0.1:3100`, 而是从 worktree 名字推导端口; 新增 `worktree dev` action 一次性打印 worktree 信息 + 派生 URL + 启动提示。

## 改动

### 1. `apps/pc-cli/src/main.rs`

**`WorktreeAction` 变体扩展**:
- 加 `Dev` action: 一次性打印 worktree name / toplevel / base port / 派生 URL / 启动命令
- `Url` action 改为真做事: 调 `current_worktree_toplevel()` 找路径 → `worktree_name_from_path()` 提名字 → `derive_worktree_url()` 算 URL

**新增 4 个 `pub` helper** (worktree URL 派生):

| Helper | 作用 |
|---|---|
| `default_base_port() -> u16` | 读 `PAPERCLIP_PORT` env, fallback 3100 |
| `default_dev_host() -> String` | 读 `PAPERCLIP_HOST` env, fallback `127.0.0.1` |
| `worktree_name_from_path(path: &str) -> String` | 纯函数: 提路径最后一段作为 worktree 名, 空路径返回 `(root)` |
| `derive_worktree_port(name: &str, base_port: u16) -> u16` | 纯函数: main/master/default/(root) 保持 base_port; 其他走 FNV-1a hash % 999 + 1 offset |
| `derive_worktree_url(name, base_port, host) -> String` | 纯函数: `format!("http://{host}:{port}")` |
| `current_worktree_toplevel() -> Option<String>` | 调 `git rev-parse --show-toplevel`, 失败返回 None |

**`Url` 真实做事**:
- 旧: `println!("http://127.0.0.1:3100")` (硬编码, 不区分 worktree)
- 新: 找当前 worktree → 派生 URL → print

**`Dev` 新 action**:
```
Worktree: feature-foo
Toplevel: /Users/me/code/paperclip-rs.worktrees/feature-foo
Base port: 3100
Dev URL:  http://127.0.0.1:3582

Quick start:
  export PAPERCLIP_PORT=3582
  paperclipai run
```

### 2. 测试 (5 个 R500 新测试)

| 测试 | 验证 |
|---|---|
| `r500_default_base_port_falls_back_to_3100` | 无 env → 3100 (或 3000-3999 范围) |
| `r500_worktree_name_from_path_strips_trailing_slash` | `/Users/me/code/main/` → `main`; `/` → `(root)`; `""` → `(root)`; `/Users/me/.git` → `.git` |
| `r500_derive_worktree_port_keeps_base_for_main` | main/master/default/(root) → base_port 不变 |
| `r500_derive_worktree_port_is_stable_and_offset` | 同名 → 同 port; 不同名 → 不同 port; offset 在 1-999 |
| `r500_derive_worktree_url_format` | main → `http://127.0.0.1:3100`; 其他 → 派生 port |

### 3. 修复 1 个真实 bug

`worktree_name_from_path("/Users/me/.git")` 原本用 `trim_end_matches(".git")` 把整个 `.git` 字符串吃掉了, 返回 `""` → 错位 `(root)`。修: 只取路径最后一段, 不再 trim suffix。

## 设计要点
- **高内聚**: 5 个 `pub` helper 全是纯函数 (`derive_*`) + 1 个 subprocess helper (`current_worktree_toplevel`)
- **可测**: 4 个纯函数让单测覆盖 base / 派生 / 边界, 不需要 git
- **稳定端口**: FNV-1a hash 同一个 worktree name 永远给同一个 port (FNV-1a 是 stable hash, 不需要外部状态)
- **保留 base port**: main/master/default 不偏移, 不影响"主分支直跑"的体验
- **Node 1:1 对齐**: 上游 `worktree.ts` 用类似约定 (worktree name → dev port offset)

## 验证
```
cargo test -p pc-cli --bin paperclipai
  test result: ok. 49 passed (44 旧 + 5 新 R500)
cargo check --workspace          0 errors
cargo fmt -p pc-cli --check      no diff
```

## 整体进度（R500 末）

| 维度 | R499 末 | **R500 末** | Δ |
|---|---|---|---|
| **CLI nested action 真实率** | **~95% (19/20)** | **100% (20/20)** ⭐ | **+1 (Url + Dev)** |
| pc-cli 单测 | 44 | **49** | +5 |
| 整体单测 | ≈ 1774 | ≈ **1779** | +5 |
| V3-V15 硬目标 | 几乎未动 | 几乎未动 | — |

## V2 CLI nested action 状态（截至 R500）

| Action | 状态 |
|---|---|
| `worktree list` / `current` / `url` / **`dev`** (R500) | ✅ 全部真做事 |
| `service install-hint` / `status` | ✅ 全部真做事 |
| `heartbeat run` | ✅ 真做事 |
| `auth bootstrap-ceo` | ✅ 真做事 |
| `pipelines {list, get, create, case-list, case-get, ...}` | ✅ 真做事 |
| `routines {list, get, pause, resume, ...}` | ✅ 真做事 |
| `client {whoami, live-events, companies, agents, issues, get, post}` | ✅ 真做事 |

**所有 20 个 nested action 都真做事 (100%)** ⭐

## 经验教训
- **`trim_end_matches` 是 greedy**: `trim_end_matches(".git")` 会吃掉整个字符串, 不是只吃 suffix. 改用 `ends_with` + 切片更安全
- **FNV-1a 优于 Random**: 端口派生需要稳定（同 name → 同 port），不能用 `rand` 或 hash 状态
- **保留 base 给常见名**: main/master/default 保持 base port 不偏移, 不破坏"主分支直跑"用户习惯

## 下一步候选（R501）
1. **R501 — V6 路由补全** (companies/tools 等) — V6 86% → 100% (从 R499 roadmap)
2. **R501 — R492 helper 接入** `pc-decisions::DecisionService.create` (扩签名 + 接入 target_ids/target_actions) — 验证低耦合
3. **R501 — V3 OpenAPI 起手** (utoipa derive + 56 path 注册) — V3 5% → 60%
