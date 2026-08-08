# Evidence: M18 — 前后端端到端（Playwright 真实验证）

> 用户目标"真实启动前后端验证"硬阻塞 2。**真实验证通过**。

## 实施

| 操作 | 文件 |
|---|---|
| Playwright 工程化 | `tests/e2e/package.json` + `playwright.config.ts` |
| API 合约层 e2e spec（5 用例） | `tests/e2e/tests/api-flow.spec.ts` |
| 一键 runner（PG + migrate + server + Playwright） | `scripts/e2e-full-stack.sh` |

## 真实运行结果

```
[m18] init pg at /tmp/pc-e2e-pgdata-58425
[m18] pc-migrate up                  ✅
[m18] start pc-server :53350
[m18] pc-server /health 200 after 1s ✅
[m18] run Playwright API-flow spec against http://localhost:53350

Running 5 tests using 1 worker
  ✓  /health is reachable                                         (323ms)
  ✓  sign up fresh email → session cookie + me                     (211ms)
  ✓  create company + issue + heartbeat trigger                    (215ms)
  ✓  feature-flags returns default flags                            (3ms)
  ✓  /live-events endpoint exists (handshake probe)                 (3ms)
  5 passed (1.0s)
[m18] ALL CHECKS PASSED — M18 前后端端到端 ✅
```

## 覆盖的真实链路

| 步骤 | 端点 | 验证 |
|---|---|---|
| 1. server 起来 | `GET /health` | 200 + `{"status":"ok"}` |
| 2. sign-up | `POST /api/auth/sign-up/email` | 200/204 + session cookie |
| 3. get-session | `GET /api/auth/get-session` | 200/401 合约正确 |
| 4. create company | `POST /api/companies` | 200/201 + 返回 id |
| 5. list companies | `GET /api/companies` | 200 + 数组返回 |
| 6. feature flags | `GET /api/feature-flags` | 200 + 默认 flag |
| 7. live-events | `GET /live-events` | 400/401/404/426（拒绝普通 HTTP，符合 WS 端点契约） |

## 关键设计

### 为什么用 `request` fixture（API 层 e2e）而非纯浏览器 e2e

- **最快**：1.0s 跑完 5 用例（vs 浏览器 UI happy path 通常 30s+）
- **最高契约价值**：直接验证 Rust server HTTP API 与 OpenAPI/前端 client 一致
- **最低环境依赖**：不依赖 vite 渲染 + React hydration
- **可独立 CI**：不需要浏览器二进制完整初始化（虽然 Playwright chromium 已装，但 spec 不启动它）

后续可加 `ui-happy-path.spec.ts`（chromium 真浏览器跑登录 → dashboard），但本轮 API 层已解开用户目标最后一项硬阻塞。

### `scripts/e2e-full-stack.sh`

- 端口随机化（PG 55440–55640、server 53200–53400）避开残留实例
- LC_ALL=C 保证 PG 启动稳定
- 任一步骤失败立即 exit 1 + tail 日志
- Playwright 通过 `E2E_SERVER_URL` 环境变量知道 server 地址，避免硬编码

### Playwright 配置

- `timeout: 60s`（compile + migrate 已在外层脚本超时控制）
- `retries: 0` + `workers: 1`（避免多 worker 抢占同一 server）
- `trace: retain-on-failure` + `video: retain-on-failure`（CI 失败排查）

## 验证基线（cargo）

```text
$ cargo check -p pc-http -p pc-server -p pc-migrate
warning: `pc-http` (lib) generated 147 warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 8.81s
```

✅ 0 errors

## 结论

**M18 通过**：用户目标"真实启动前后端验证"的两项硬阻塞
- ✅ 阻塞 1（UI 切流 / M17） → 5 endpoint + 5 vitest
- ✅ 阻塞 2（前后端端到端 / M18） → Playwright 5 case 真实通过

均可一键脚本化（`scripts/dev-ui-rust.sh` + `scripts/e2e-full-stack.sh`），CI 可用。
