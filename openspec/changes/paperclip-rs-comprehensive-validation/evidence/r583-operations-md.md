# R583 — OPERATIONS.md 中文运维手册（P2 文档补齐）

**状态**: ✅ 完成 (2026-08-12)

## 1. 关键成果

**OPERATIONS.md（416 行中文）** 写完，覆盖：

1. **生产部署** — 系统要求 / 编译 / systemd / 环境变量 / 反向代理
2. **数据库运维** — 迁移 / 备份 / 恢复 / 保留策略 / VACUUM
3. **监控告警** — 健康端点 / Prometheus / 关键告警阈值 / 排查清单
4. **启动性能基线** — R579 实测数据
5. **水平扩展** — 无状态 server / WS sticky session / 心跳分片
6. **安全** — 关键配置 / 防火墙 / 审计
7. **升级流程** — 蓝绿 / 回滚
8. **常见问题** — 5 个真实场景

## 2. R582 → R583 完成度提升

| 维度 | R580 末 | R583 末 |
|---|---|---|
| V15 中文文档 | ~20% | **~50%** ↑ |
| 用户可投产文档完整度 | ~70% | **~85%** ↑ |
| 综合完成度 | ~68% | **~70%** ↑ |

## 3. OPERATIONS.md 结构

```
OPERATIONS.md (416 行)
├── 1. 生产部署 (8.1%)
│   ├── 1.1 系统要求
│   ├── 1.2 编译部署
│   ├── 1.3 systemd 单元
│   ├── 1.4 环境变量（12 个）
│   ├── 1.5 反向代理（Nginx + WebSocket）
├── 2. 数据库运维 (10.6%)
│   ├── 2.1 Schema 迁移
│   ├── 2.2 备份
│   ├── 2.3 恢复
│   ├── 2.4 备份保留策略
│   ├── 2.5 真空与重建索引
├── 3. 监控告警 (12.0%)
│   ├── 3.1 健康检查端点
│   ├── 3.2 Prometheus 指标
│   ├── 3.3 关键告警（7 个）
│   ├── 3.4 故障排查清单（7 个 SQL）
├── 4. 启动性能基线 (R579)
├── 5. 水平扩展 (12.0%)
│   ├── 5.1 无状态 server
│   ├── 5.2 WebSocket 限制
│   ├── 5.3 心跳调度
├── 6. 安全 (16.8%)
│   ├── 6.1 关键配置
│   ├── 6.2 防火墙
│   ├── 6.3 审计
├── 7. 升级流程 (16.8%)
│   ├── 7.1 升级步骤
│   ├── 7.2 回滚步骤
└── 8. 常见问题 (5 个 Q&A)
```

## 4. 关键决策

### 4.1 systemd Notify 类型

用 `Type=notify` 而非 `simple`：sd_notify protocol 让 pc-server 启动后通知 systemd「已就绪」，避免 systemd 假设服务立即可用导致的 healthcheck race。

### 4.2 systemd 安全加固

`NoNewPrivileges + ProtectSystem=strict + ProtectHome + PrivateTmp + ReadWritePaths`：标准 systemd hardening 配置，比 rootless container 更轻量。

### 4.3 Nginx WebSocket 单独 location

`/live-events` 单独 location 块，配置 `Upgrade` / `Connection` 头。其他路径用普通 HTTP proxy。

### 4.4 备份三层保留

每日（7天）+ 每周（4周）+ 每月（12月）+ WAL 归档（7天）：标准 3-2-1 备份策略简化版。

### 4.5 心跳分片推荐

多副本时用 `--heartbeat-shard=X/N` 显式分片；接受重复（DB 唯一约束兜底）作为备选方案。

## 5. 与其他文档的关系

| 文档 | 关注点 |
|---|---|
| `README.md` | 快速上手 / 仓库结构 |
| `ARCHITECTURE.md` | 当前状态 / crate 拓扑 / 设计决策 |
| `ARCHITECTURE-DIAGRAMS.md` | 底层图 / 数据流图 |
| `MODULE-MAPPING.md` | Node→Rust 模块映射 |
| `PROJECT-PLAN.md` | v1.0 执行计划 |
| **`OPERATIONS.md` (新)** | **生产部署 / 备份 / 监控 / 故障恢复** |

## 6. 剩余 G15 文档缺口

| 文档 | 状态 | 估计工作量 |
|---|---|---|
| OPERATIONS.md | ✅ R583 完成 | — |
| PLUGIN_AUTHORING.md | ❌ 待写 | 0.5 轮 |
| MIGRATION_FROM_NODE.md | ❌ 待写 | 0.5 轮 |
| AGENTS.md（中文） | ❌ 待写 | 0.3 轮 |

## 7. 验收清单

- [x] 生产部署完整覆盖（systemd + nginx + env）✅
- [x] 数据库运维完整（迁移 / 备份 / 恢复 / 索引）✅
- [x] 监控告警完整（端点 / 指标 / 阈值 / 排查）✅
- [x] 启动性能基线（R579 实测数据）✅
- [x] 水平扩展方案（无状态 + WS sticky + 心跳分片）✅
- [x] 安全（关键配置 + 防火墙 + 审计）✅
- [x] 升级 / 回滚流程 ✅
- [x] 常见问题 5 个 ✅
