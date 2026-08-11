#![forbid(unsafe_code)]

//! `OpenAPI` 3.1 spec builder.
//!
//! 与原 paperclip `server/src/routes/openapi.ts` 等价：
//! - 生成 `/openapi.json` 与 `/openapi.yaml` 的 JSON/YAML 序列化
//! - 通过 `OpenApiRegistry::builder()` 注册 paths + schemas
//! - 最小可用：先覆盖 metadata + 已注册 paths 的最小 schema
//!
//! 设计目标：保持与原 UI 期望的 schema 字段名一致，避免破坏前端类型生成。

pub mod builder;
pub mod path;
pub mod schema;
pub mod serializers;
pub mod spec;

pub use builder::OpenApiRegistry;
pub use path::{HttpMethod, OpenApiPath};
pub use schema::SchemaRef;
pub use spec::{Contact, ExternalDocs, Info, License, OpenApiSpec, Server};
