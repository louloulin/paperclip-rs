# Playwright E2E — Section 5 Infrastructure

Playwright 浏览器级 UI happy path 测试基础设施，对应 `tasks.md` Section 5。

## 目录结构

```
tests/e2e/playwright/
├── playwright.config.ts     # Playwright 配置（env var 驱动端口）
├── package.json            # @playwright/test 依赖
├── tests/
│   ├── _csrf-helper.ts    # CSRF token 辅助函数（从 ../tests/ 复制）
│   └── happy-path.spec.ts  # 示例模板测试（各 client 测试的起点）
└── test-results/          # Playwright 自动保存截图/录像于此
```

## 快速开始

### 前置条件

```bash
# 1. 安装 Playwright 浏览器
cd tests/e2e/playwright
npx playwright install --with-deps chromium

# 2. 安装 Node 依赖
pnpm install
```

### 方式一：用 harness 脚本（推荐）

```bash
# 启动完整环境（PG + pc-server + Vite）并自动跑测试
../../scripts/run-ui-e2e.sh

# 仅启动服务器，不跑测试（调试用）
SKIP_PLAYWRIGHT=1 ../../scripts/run-ui-e2e.sh

# 有头模式（打开浏览器窗口）
E2E_HEADLESS=0 ../../scripts/run-ui-e2e.sh
```

### 方式二：手动启动服务后单独跑测试

```bash
# 终端 1：启动后端（参考 scripts/e2e-full-stack.sh 的端口分配）
PAPERCLIP_E2E_HTTP_PORT=53100 PAPERCLIP_E2E_UI_PORT=51800 \
  ../../scripts/run-ui-e2e.sh SKIP_PLAYWRIGHT=1

# 终端 2：跑测试（端口与上面一致）
E2E_SERVER_URL=http://127.0.0.1:53100 \
E2E_UI_URL=http://127.0.0.1:51800 \
  npx playwright test
```

## 环境变量

| 变量 | 说明 | 默认值 |
|------|------|--------|
| `E2E_SERVER_URL` | pc-server 地址 | `http://localhost:53100` |
| `E2E_UI_URL` | Vite dev server 地址 | `http://localhost:51800` |
| `E2E_HEADLESS` | `0` = 有头模式 | `1`（无头） |
| `PAPERCLIP_E2E_PG_PORT` | PostgreSQL 端口 | 随机 |
| `PAPERCLIP_E2E_HTTP_PORT` | pc-server 端口 | 随机 |
| `PAPERCLIP_E2E_UI_PORT` | Vite 端口 | 随机 |
| `SKIP_PLAYWRIGHT` | 设为 `1` 跳过测试 | `0` |

## 添加新测试

参考 `tests/happy-path.spec.ts` 中的三种风格：

1. **Pure browser** — 纯 UI 交互（签到 → 导航 → DOM 断言）
2. **API + browser** — 通过 API 创建数据，再用浏览器验证显示
3. **API smoke** — 纯 API 快速冒烟测试

每个新 client 对应一个 `test.describe` block，参考现有测试文件：
`../tests/api-flow.spec.ts`、`../tests/v12-full-flow.spec.ts`。

## 查看报告

```bash
npx playwright show-report
# 或直接打开 test-results/index.html
```

## 故障排查

- pc-server /health 不是 200：检查 `LOG_DIR/.s5-e2e-logs/server.log`
- Vite 无法启动：检查 `LOG_DIR/.s5-e2e-logs/vite.log`
- CSRF 错误：确认请求带了 `x-csrf-token` header（参考 `_csrf-helper.ts`）
