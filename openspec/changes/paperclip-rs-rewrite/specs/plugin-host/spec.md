## ADDED Requirements

### Requirement: 插件 Worker 池与生命周期 SHALL
The system SHALL satisfy the following behavior.

`pc-plugin-host` 维护 stdio 子进程池，每个 worker 对应一个插件；按需启动/停止；健康探针每 30s 一次。

#### Scenario: 加载插件
- **WHEN** `PluginLoader::activate(plugin_id)` 且能力声明有效
- **THEN** 启动子进程，注册 host↔worker RPC handlers，订阅事件总线

### Requirement: JSON-RPC over stdio 协议稳定 SHALL
The system SHALL satisfy the following behavior.

RPC 方法名与消息类型与 `@paperclipai/plugin-sdk/src/protocol.ts` 一致；`pc-plugin-protocol` crate 共享给 host 与未来 Rust worker。

#### Scenario: 兼容旧插件
- **WHEN** 加载 npm 上的 `paperclip-plugin-*-1.x.x`
- **THEN** host 正确响应其所有 RPC 调用
