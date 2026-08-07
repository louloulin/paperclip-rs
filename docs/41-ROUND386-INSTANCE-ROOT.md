# R386 — Instance-Root Resolver (Node parity surface)

## 目标

按 `comet-open` + `RTK` 思路,精确镜像 Node `server-utils.ts` 中
`resolvePaperclipInstanceRootForAdapter` 的签名/语义,新增独立模块
`pc-acpx::instance_root`,保持高内聚低耦合 — 一个模块一个文件,
零 I/O,纯函数。

`paths::resolve_paperclip_instance_root`(R369)是早期实现的低层
版本,API 略不同 (`PathBuf` + `AcpxError`);本模块是 Node 兼容
适配层 (`String` + `ResolvePaperclipInstanceRootError`)。两者并存,
通过 `instance_root_agrees_with_paths_resolver` 集成测试保证语义
一致。

## 范围

- 新增 `crates/pc-acpx/src/instance_root.rs`(562 行含 25 单测)
- `crates/pc-acpx/src/lib.rs` 增加模块声明 + re-export
- 新增 `crates/pc-acpx/tests/round386_instance_root.rs`(233 行 12 集成测试)
- 跟 Node `paperclip/packages/adapter-utils/src/server-utils.ts`
  L106 / L107 / L133-137 / L139-149 行精确对齐

## 复刻的 Node 函数

### `DEFAULT_PAPERCLIP_INSTANCE_ID` (L106)
```rust
pub const DEFAULT_PAPERCLIP_INSTANCE_ID: &str = "default";
```

### `PATH_SEGMENT_RE` (L107)
Node regex `/^[a-zA-Z0-9_-]+$/`,Rust 等价:
```rust
pub fn is_valid_paperclip_instance_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}
```

### `expandHomePrefix` (L133-137)
```rust
pub fn expand_home_prefix(value: &str, home: &Path) -> String
```
完整镜像 Node 行为:`~` → home,`~/x` → home/x,其他 verbatim。
注意:`log_redaction::expand_home_prefix` 已是等价实现,但签名是
`fn(value, home: &Path) -> String`,与 `instance_root` 模块**不冲突**
— 通过 `lib.rs` re-export 共享(`expand_home_prefix` 由 `log_redaction`
导出,`instance_root` 不重复导出)。

### `resolvePaperclipInstanceRootForAdapter` (L139-149)

签名 Node:
```ts
function resolvePaperclipInstanceRootForAdapter(input: {
  homeDir?: string;
  instanceId?: string;
  env?: NodeJS.ProcessEnv;
} = {}): string
```

Rust 镜像:
```rust
pub struct ResolvePaperclipInstanceRootInput {
    pub home_dir: Option<String>,
    pub instance_id: Option<String>,
    pub env: Option<BTreeMap<String, String>>, // None = std::env
}

pub enum ResolvePaperclipInstanceRootError {
    InvalidInstanceId(String),
}

pub fn resolve_paperclip_instance_root_for_adapter(
    input: &ResolvePaperclipInstanceRootInput,
) -> Result<String, ResolvePaperclipInstanceRootError>

pub fn default_resolve_paperclip_instance_root_for_adapter(
) -> Result<String, ResolvePaperclipInstanceRootError> // 镜像 `{}` 输入
```

### 行为细节
1. **home_raw 优先级**:`homeDir.trim()`(非空)> `env.PAPERCLIP_HOME.trim()`(非空)> `~/.paperclip`
2. **home 解析**:raw → `expandHomePrefix` → `path.resolve(cwd, expanded)`(lexical);无 raw → `path.resolve(cwd, home, ".paperclip")`
3. **instance_id 优先级**:`instanceId.trim()` > `env.PAPERCLIP_INSTANCE_ID.trim()` > `"default"`
4. **trim 后空白视为 absent**(`""` 在 JS 中是 falsy,等价于 `Option::None`)
5. **校验失败抛 typed error**:`Invalid PAPERCLIP_INSTANCE_ID '<value>'.`
6. **返回路径**:`path.resolve(homeDir, "instances", instanceId)` — lexical normalize

### 关键设计决策

#### `lexically_normalize` 路径解析
Node `path.resolve` 是 lexical(不解析 symlink),Rust 端用 `PathBuf::components()`
手写 `.` / `..` 解析,零文件系统 I/O,完全等价于 Node 语义。R385 的 `workspace_env.rs`
有相同实现,本模块独立副本(避免跨模块耦合)。

#### `path_resolve(cwd, base, segments)` 通用 helper
镜像 `path.resolve(base, ...segments)`:
- joined path 是 absolute → 直接 lexically_normalize
- joined path 是 relative → anchor 到 cwd 后 lexically_normalize

#### `home_dir_or_default` fallback
- 优先 `std::env::var_os("HOME")`(Node 等价 `os.homedir()`)
- 无 HOME → fallback 到 cwd(匹配 Node 在 `$HOME` 未设置时的行为)

#### `BTreeMap<String, String>` 作为 env 类型
与 Node `NodeJS.ProcessEnv`(`Record<string, string>`)语义等价,
但用 `BTreeMap` 替代 `HashMap` 以保证 `env` 在测试和跨调用中的稳定迭代顺序。
`paths::resolve_paperclip_instance_root` 仍用 `HashMap`,两者通过集成测试
`instance_root_agrees_with_paths_resolver` 验证 lexical 结果一致。

#### Error: `ResolvePaperclipInstanceRootError`
独立于 `AcpxError::InvalidInstanceId`,专门为 Node 适配层服务。
实现 `std::error::Error` + `Display`,显示消息与 Node `throw` 完全一致。

#### `unsafe_code = "forbid"` 兼容
纯字符串拼接 + `PathBuf` 操作,无任何 unsafe。

## 测试

- 25 个新单元测试注入 `instance_root::tests`:
  - 4 个 `DEFAULT_PAPERCLIP_INSTANCE_ID` / env 常量稳定性
  - 6 个 `is_valid_paperclip_instance_id`(接受 alphanum / 接受 _- / 拒绝 empty / 拒绝 path sep / 拒绝 whitespace&punct / 拒绝 unicode)
  - 1 个 `expand_home_prefix` tilde 形式
  - 10 个 `resolve_paperclip_instance_root_for_adapter` 行为(default / 优先 home_dir / trim / trim instance / env fallback / blank input / instance_id override / tilde expand / relative anchor / 校验失败)
  - 1 个 error display 与 Node 消息一致
  - 1 个 input default 字段一致性
  - 1 个 `path_resolve` dot-dot 归一化
  - 1 个 `home_dir_or_default` 绝对路径保证
- 12 个新集成测试在 `tests/round386_instance_root.rs`:
  - 2 个 default helper / 常量验证
  - 2 个 Node trim / null-guard parity(empty input / whitespace)
  - 3 个 precedence(home_dir beats env / instance_id beats env / env falls back to default)
  - 3 个 validator(rejects / accepts / typed error)
  - 2 个 invalid via input / env
  - 1 个 `instance_root_agrees_with_paths_resolver` 跨模块等价

合计 R386 新增 **37 个测试**,全部绿色。

## 验证

```
cd paperclip-rs && cargo test -p pc-acpx
```

结果:**656 个 pc-acpx tests 通过** (R385 是 619,+37),0 失败 0 回归。

```
cd paperclip-rs && cargo fmt --check
```

clean。

## 下一步

完成 R386 后,`adapter-utils/src/server-utils.ts` 的**纯函数模块**
已全部移植到 `pc-acpx`,共 8 模块 31+ 个函数。剩余 module 待复刻:

### R387 候选(skill sync prefs)
- `readPaperclipSkillSyncPreference` (L2794-2834)
- `writePaperclipSkillSyncPreference` (L2870-3002)
- `resolvePaperclipDesiredSkillNames` (L2858-2869)

### R388 候选(skill snapshot)
- `buildRuntimeMountedSkillSnapshot` (L2491-2608)
- `buildPersistentSkillSnapshot` (L2609-2734)

### R389 候选(async skill materialize)
- `materializePaperclipSkillCopy` (L3038+)

后续轮次将按 R385/R386 同等风格,逐模块新建独立文件 + 单测 + 集成测试,
保持高内聚低耦合。
