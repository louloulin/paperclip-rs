# Lexical.mjs ENOENT 真实根因（修正版）

> 日期：2026-08-03 · 状态：✅ 已修复（pnpm install --no-frozen-lockfile）
> 关系：纠正 `docs/01-VITE-ERROR-ROOT-CAUSE.md` 中"lexical 未在源码被引用"的错误结论。

## 一、错误复现

启动 paperclip UI（`pnpm --filter @paperclipai/ui dev`）时崩溃：

```
node:internal/fs/promises:640
Error: ENOENT: no such file or directory, open
  '/Users/.../paperclip/ui/node_modules/lexical/dist/Lexical.mjs'
  at async prepareEsbuildOptimizerRun (vite/dist/node/chunks/dep-D4NMHUTW.js:15089:3)
```

## 二、关键证据链（实地验证）

| 证据 | 实际值 | 之前文档的判断 | 真实结论 |
|---|---|---|---|
| `paperclip/ui/package.json` 声明 `"lexical": "0.46.0"` | ✅ 存在 | — | 正确 |
| `paperclip/ui/src/lib/` 中 `import ... from "lexical"` 的源文件数 | **4** 个（mention-deletion.ts / mention-aware-link-node.ts / paste-normalization.ts + 2 个测试） | "未引用" | **错误** |
| `paperclip/node_modules/lexical/dist/Lexical.mjs` | ✅ 存在（pnpm hoist 到根） | — | hoist 生效 |
| `paperclip/ui/node_modules/lexical/` | ❌ 不存在（修复前） | "pnpm orphan cleanup 删掉了" | 不是 orphan cleanup，是符号链接没建 |
| `paperclip/ui/node_modules/@lexical/link/` | ❌ 不存在（修复前） | "未在源码中引用" | 与源码无关，是 pnpm 链接没建 |
| `paperclip/node_modules/.pnpm/` 内容存储 | 极小/为空 | — | 实际是 **hoisted** 模式（不是 isolated） |
| `paperclip/.npmrc` | `auto-install-peers=true` | — | 未设 `node-linker=hoisted`，但实际行为就是 hoisted |

## 三、真实根因链

```
┌───────────────────────────────────────────────────────────────────────┐
│ 1. paperclip/ui/package.json 直接声明 "lexical": "0.46.0"           │
│    且 4 个源文件真的 import ... from "lexical"（不是孤立声明）。       │
│                                                                       │
│ 2. pnpm 解析依赖图时把 lexical 提升（hoist）到                        │
│    paperclip/node_modules/lexical（实物目录），                        │
│    因为这是 monorepo 根的"可被多个子包共用"的层。                      │
│                                                                       │
│ 3. 但 ui 子包的 package.json 是 lexical 的直接声明者，                │
│    pnpm 应该在 ui/node_modules/lexical 创建一个符号链接                │
│    指向根 node_modules/lexical —— 这一步没发生。                       │
│                                                                       │
│ 4. 可能原因（按可能性排序）：                                          │
│    a) 之前某次 pnpm install --force 或手工 rm -rf ui/node_modules    │
│       之后只跑了过滤安装，重建了根 hoisted 但漏建了子包链接；           │
│    b) pnpm-lock.yaml 与 ui/package.json 的 lexical 解析存在          │
│       peerDependenciesMeta 漂移（typescript peer），导致 hoist       │
│       但 child-link 阶段被跳过；                                       │
│    c) 仓库内 .npmrc 中 auto-install-peers=true 与 lockfile            │
│       同步逻辑产生竞态（paperclip 根有 patches/overrides 警告）。      │
│                                                                       │
│ 5. Vite 启动 → dep-scan → esbuild optimizer 阶段                      │
│    prepareEsbuildOptimizerRun() 并发 readFile 所有 ui/package.json    │
│    列出的依赖 → 读 ui/node_modules/lexical/dist/Lexical.mjs → ENOENT │
│    → 整个 dev server 崩溃（Vite 5+ 没有按需懒加载机制）。             │
└───────────────────────────────────────────────────────────────────────┘
```

## 四、已执行的修复（成功）

```bash
cd /Users/louloulin/Documents/lumosaipaperclip/paperclip
rtk pnpm install --filter @paperclipai/ui --no-frozen-lockfile
# 结果：ui/node_modules/lexical 与 ui/node_modules/@lexical/link 重建成功
$ ls paperclip/ui/node_modules/lexical/dist/Lexical.mjs
/Users/.../paperclip/ui/node_modules/lexical/dist/Lexical.mjs ✅
```

`--no-frozen-lockfile` 强制让 pnpm 重新走 hoist + child-link 阶段，
而不是直接信任 lockfile 的"已经安装"标记。

## 五、防御性建议（写入 paperclip CI）

### 5.1 加 pnpm 链接守卫脚本

在 `paperclip/scripts/check-pnpm-layout.mjs` 中检查所有 ui 子包
依赖的符号链接完整性：

```js
import { promises as fs } from 'node:fs';
import path from 'node:path';

const uiPkg = JSON.parse(await fs.readFile('ui/package.json', 'utf8'));
const deps = Object.keys(uiPkg.dependencies ?? {});

let failed = false;
for (const dep of deps) {
  if (dep.startsWith('@paperclipai/')) continue;          // workspace link
  const target = path.join('ui/node_modules', dep);
  try {
    const stat = await fs.stat(target);
    if (!stat.isSymbolicLink() && !stat.isDirectory()) {
      throw new Error(`${dep} missing in ui/node_modules`);
    }
  } catch (err) {
    console.error(`[check-pnpm-layout] ${dep}: ${err.message}`);
    failed = true;
  }
}
if (failed) process.exit(1);
```

加入 CI：`pnpm exec node scripts/check-pnpm-layout.mjs`。

### 5.2 改 vite.config.ts 兜底

即使符号链接再次缺失，让 Vite 不再一次性崩：

```ts
export default defineConfig({
  optimizeDeps: {
    include: ['lexical', '@lexical/link'],
  },
});
```

这样 Vite 在 dep-scan 阶段就强制读取 lexical，缺符号链接时会给出
明确的"无法解析模块"错误，而不是通用的 ENOENT 文件读取错误。

### 5.3 锁版本

把 `paperclip/.npmrc` 加上：

```
node-linker=isolated
prefer-frozen-lockfile=true
auto-install-peers=true
```

强制 pnpm 使用标准 isolated 模式（每个子包只看到自己声明的依赖），
并优先信任 lockfile，避免 hoist 阶段产生子包链接缺失。

## 六、与 paperclip-rs 的关系

paperclip-rs（Rust 后端）与 Vite 启动失败**无直接耦合**，但需要
`paperclip/ui` 能 dev 起来才能跑 E2E 冒烟脚本验证 Rust 后端。

Phase G（切流量）开始前必须保持：
- ui/ dev server 能稳定启动（避免 lexical 类诡异错误）
- VITE_API_BASE=http://127.0.0.1:3100（指向 pc-server）
