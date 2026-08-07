# R388 — Skill Snapshot Builders (Node parity surface)

## 目标

按 `comet-open` + `RTK` 思路,精确镜像 Node `server-utils.ts` 中
`buildRuntimeMountedSkillSnapshot` (L2491-2608) 和
`buildPersistentSkillSnapshot` (L2609-2734),以及相关的内部 helpers
(`skillLocationLabel` L294-298 / `buildManagedSkillOrigin` L300-309 /
`isPaperclipSkillSourceMissing` L311-313 /
`resolvePaperclipSkillMissingDetail` L315-320 /
`resolveSkillDetail` L322-330),新建独立模块
`pc-acpx::skill_snapshot`,保持高内聚低耦合 — 一个模块一个文件,
纯函数 / 零 I/O / 零 unsafe。

## 范围

- 新增 `crates/pc-acpx/src/skill_snapshot.rs`(1386 行含 29 单测)
- `crates/pc-acpx/src/lib.rs` 增加模块声明 + 18 个 re-export
- 新增 `crates/pc-acpx/tests/round388_skill_snapshot.rs`(541 行 19 集成测试)
- 跟 Node `paperclip/packages/adapter-utils/src/server-utils.ts`
  L294-330 / L2491-2734 行 + `types.ts` L236-275 行精确对齐

## 复刻的 Node 函数 / 类型

### 枚举
- `AdapterSkillOrigin`(Node L246-249)— `CompanyManaged` / `UserInstalled` / `ExternalUnknown`
- `AdapterSkillState`(L238-244)— `Available` / `Configured` / `Installed` / `Missing` / `Stale` / `External`
- `AdapterSkillSyncMode`(L236)— `Unsupported` / `Persistent` / `Ephemeral`
- `InstalledSkillTargetKind`— `Symlink` / `Directory` / `File`
- `PaperclipSkillSourceStatus`— `Available` / `Missing`

### 结构
- `PaperclipSkillEntry`(L231-238)— Node `PaperclipSkillEntry` 接口镜像
- `InstalledSkillTarget`(L246-249)— `target_path` + `kind`
- `AdapterSkillEntry`(types.ts L251-264)— 输出条目,含所有 Optional 字段
- `AdapterSkillSnapshot`(types.ts L268-275)— 顶层快照
- `AdapterDesiredSkillEntry`— `desired_skill_entries` 元素

### 选项
- `RuntimeMountedSkillSnapshotOptions`(L270-284)— runtime builder 输入
- `PersistentSkillSnapshotOptions`(L256-268)— persistent builder 输入

### 辅助函数
- `skill_location_label`(L294-298)— trim 空白字符串,空则 None
- `build_managed_skill_origin`(L300-309)— `(CompanyManaged, "Managed by Paperclip", false)` 三元组
- `is_paperclip_skill_source_missing`(L311-313)
- `resolve_paperclip_skill_missing_detail`(L315-320)— 优先 entry 自带 detail,空则 fallback
- `resolve_skill_detail`(L322-330)— `SkillDetail` 三态(None / Static / Dynamic closure)

### 主函数
- `build_runtime_mounted_skill_snapshot`(L2491-2608)
- `build_persistent_skill_snapshot`(L2609-2734)

## 关键设计决策

### `SkillDetail` 三态 enum
镜像 Node `string | ((entry) => string | null) | null | undefined`:
- `None`:`null` / `undefined`
- `Static(String)`:`"static detail"`
- `Dynamic(Arc<dyn Fn(&PaperclipSkillEntry) -> Option<String> + Send + Sync>)`:closure

`Debug` 手动实现,因为 `dyn Fn` 不实现 `Debug`。
`From<&'static str>` / `From<String>` / `From<Option<String>>` impl 让
caller 用 `.into()` 转换常见字符串。

### `PaperclipSkillEntry` 不通过 lib.rs re-export
`pc-acpx::skill_materialize` 已有同名类型(serde-friendly,字段为
`PathBuf` 等),服务于不同的 materialize 上下文。skill_snapshot 中的
`PaperclipSkillEntry` 是 Node 接口的简单镜像(字段为 `String`),通过
`pc_acpx::skill_snapshot::PaperclipSkillEntry` 显式路径访问,避免
命名冲突并保持两个模块独立。

### `BTreeMap` / `BTreeSet` 而非 `HashMap`
保持确定性迭代顺序,与 Node `Map`/`Set` 一致。`HashMap` 会破坏
`byKey` 查找的稳定性,虽然当前用法不依赖,但作为防御性选择。

### 三个 pass 的 entries 构造
镜像 Node:
1. **Pass 1**: 每个 available entry → managed entry(missing/available/configured)
2. **Pass 2**: 每个 desired 但 unavailable → missing + warning
3. **Pass 3**: 每个 external_installed 但 runtimeName 不冲突 → external entry

末尾 `sort_by(|left, right| left.key.cmp(&right.key))` 镜像
`entries.sort((left, right) => left.key.localeCompare(right.key))`。

### `unsafe_code = "forbid"` 兼容
纯数据结构 + `Arc<dyn Fn>` 操作,无任何 unsafe。

## 测试

- 29 个新单元测试注入 `skill_snapshot::tests`:
  - 1 个 origin labels 表
  - 2 个 `skill_location_label`(空白 / trim)
  - 1 个 `build_managed_skill_origin` 三元组稳定性
  - 1 个 `is_paperclip_skill_source_missing`
  - 2 个 `resolve_paperclip_skill_missing_detail`
  - 1 个 `resolve_skill_detail`(三态)
  - 11 个 `build_runtime_mounted_skill_snapshot`(available / desired configured / unsupported / missing / unavailable warning / external installed / collision skip / sort / order preserve / warnings extend / closure detail)
  - 10 个 `build_persistent_skill_snapshot`(available / installed / stale / external conflict / external non-desired / missing desired / unavailable warning / missing source with target_path / external installed fallback / sort)
- 19 个新集成测试在 `tests/round388_skill_snapshot.rs`:
  - 4 个 cross-module parity(origin labels / location label / source missing / missing detail fallback)
  - 8 个 runtime builder end-to-end(supported ephemeral / dynamic detail / unavailable warning / external with trimmed label / target fallback / collision skip / desired entries order / version id)
  - 6 个 persistent builder end-to-end(installed / stale / external conflict / external non-desired / missing desired / external installed fallback)
  - 2 个 `resolve_skill_detail` 跨变体 + default 构造

合计 R388 新增 **48 个测试**,全部绿色。

## 验证

```
cd paperclip-rs && cargo test -p pc-acpx
```

结果:**742 个 pc-acpx tests 通过** (R387 是 694,+48),0 失败 0 回归。

```
cd paperclip-rs && cargo fmt --check
```

clean。

## 下一步

adapter-utils 中除了 `materialize_paperclip_skill_copy` (async,极复杂)
外,其他纯函数模块已全部移植。剩余模块:

### R389 候选(async skill materialize,极复杂)
- `materializePaperclipSkillCopy` (L3038+,async I/O,hash,lock)

### 后续路线
- R390+: 13 个 adapter stubs(`pc-adapter-gemini-local` /
  `pc-adapter-grok-local` / `pc-adapter-opencode-local` /
  `pc-adapter-pi-local` / `pc-adapter-cursor-cloud` / 等)的实质实现
- R400+: secrets AWS/GCP/Vault 真实解密 + plugin worker→host 回调
