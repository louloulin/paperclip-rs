# Paperclip Rust 多模块架构规范

## 调研依据

- Rust Book：复杂模块可由单文件演进为同名目录，通过父模块声明子模块并控制公开接口。
  - https://doc.rust-lang.org/book/ch07-05-separating-modules-into-different-files.html
- Tokio runtime：以 `mod.rs` 作为域入口，按 scheduler、driver、task、io、metrics 等职责拆分私有目录模块。
  - https://github.com/tokio-rs/tokio/tree/master/tokio/src/runtime
- Axum：crate 只暴露稳定 facade，复杂能力按 `extract`、`middleware`、`response`、`routing` 等目录聚合。
  - https://github.com/tokio-rs/axum/tree/main/axum/src
- rust-analyzer：简单功能保留单文件，复杂功能演进为同名目录；父模块负责 API，子模块承载算法和测试。
  - https://github.com/rust-lang/rust-analyzer/tree/master/crates/ide/src

## 分层原则

```text
pc-core / pc-config
        ↓
pc-db / pc-auth / pc-storage
        ↓
pc-repos（领域事务与持久化）
        ↓
pc-agent / pc-heartbeat / pc-workflow（运行时服务）
        ↓
pc-http / pc-cli（传输与组合入口）
```

- 下层 crate 不依赖上层传输层。
- HTTP handler 只负责解析身份、输入和状态码，不包含领域 SQL。
- 跨表一致性由 `pc-repos` 的领域事务入口负责。
- Kameo actor 负责并发、生命周期和消息编排，不替代数据库事务。

## 目录模块规则

当一个能力满足任一条件时，从 `name.rs` 演进为 `name/mod.rs`：

1. 超过约 300 行且仍在增长。
2. 同时包含模型、纯规则、数据库执行、外部适配或大量测试中的三类以上。
3. Node 来源由多个 service 文件共同构成一个 Rust 领域能力。
4. 需要隐藏内部 SQL/算法，只向调用方暴露少量稳定入口。

推荐结构：

```text
domain_name/
├── mod.rs       # facade、公共 DTO、pub use
├── models.rs    # 领域输入/输出；复杂时启用
├── rules.rs     # 无 I/O 纯规则、校验、文本生成
├── apply.rs     # transaction / side effects
├── repository.rs# 单表或聚合查询；复杂时启用
└── tests.rs     # 模块私有规则单测
```

不要求机械创建全部文件；只有真实职责存在时才新增。

## 可见性约束

- `mod.rs` 是唯一公共 facade。
- 子模块默认私有；跨子模块优先 `pub(super)`，crate 内复用才使用 `pub(crate)`。
- HTTP 层不得导入 `domain_name::apply`、`domain_name::rules` 等内部路径。
- 公共 DTO 使用明确领域名，禁止暴露数据库 tuple 或 `PgRow`。
- SQL 错误在领域边界转换为可判别错误枚举，HTTP 层只做状态码映射。

## 事务边界

- 一个用户可观察操作的所有数据库副作用应共享一个 transaction。
- 先锁定聚合根，再校验 company/actor/run，再写关系和主体，最后写 activity。
- realtime、WebSocket 和 actor 消息在 commit 后发送；数据库 activity 在 commit 前写入。
- 跨模块终态清理通过窄函数接收 `&mut Transaction<Postgres>`，不自行开启嵌套事务。

## 当前示范：Issue 终态副作用

```text
issue_terminal_effects/
├── mod.rs       # TerminalEffectIssue/Actor/Counts + facade
├── reasons.rs   # summary/status-card failure reason 与 interaction result
├── apply.rs     # summary slot、status card、interaction、tool action、activity 原子清理
└── tests.rs     # 纯规则单测
```

`issue.rs` 只调用 `apply_issue_terminal_effects`，不依赖内部表级实现。这保证高内聚、低耦合，并允许未来把该模块提升成独立 crate 而不改变调用方。

## 后续迁移要求

每个 Node 模块复刻必须包含：

1. Node 来源文件和行为清单。
2. Rust facade 与内部职责拆分。
3. 数据库 schema/FK/唯一约束核对。
4. 纯规则单测和可运行的数据库集成测试。
5. HTTP/actor 接线验证。
6. `docs/05-PROGRESS-AUDIT.md` 差距更新。

禁止以空返回、静态 JSON、`TODO`、无条件成功或 mock 持久化冒充模块完成。
