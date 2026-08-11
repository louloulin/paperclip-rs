#![forbid(unsafe_code)]

//! URL key normalization for agents and projects.
//!
//! R530: Direct port of `paperclip/packages/shared/src/agent-url-key.ts` and
//! `paperclip/packages/shared/src/project-url-key.ts`.
//!
//! 设计原则:
//! - 所有 pub fn 都是纯函数 (无 IO, 无副作用)
//! - regex 编译成 `Lazy<Regex>` 一次, 后续零成本
//! - 全模块 API 不需要 `uuid` crate (Node 上游也是纯 regex)
//!
//! 范围 (本 crate):
//! - [`agent_url_key`]: [`is_uuid_like`], [`normalize_agent_url_key`], [`derive_agent_url_key`]
//! - [`project_url_key`]: [`normalize_project_url_key`], [`derive_project_url_key`], [`has_non_ascii_content`]
//!
//! **不** 范围 (留给集成层):
//! - `pc-agent` 业务层使用 ([`normalize_agent_url_key`] + UUID 检测 → agent 持久化 url_key)
//! - `pc-repos` / `pc-project` 业务层使用 (project URL key derivation)
//!
//! Node 上游这两个模块在 UI `search-query-parser.ts` / `utils.ts` /
//! `company-portability-sidebar.ts` / `server/src/routes/pipelines.ts` 等多处用;
//! Rust port 让 pc-agent 可以从内联实现 (R604) 切到独立 crate.

pub mod agent_url_key;
pub mod project_url_key;

pub use agent_url_key::{derive_agent_url_key, is_uuid_like, normalize_agent_url_key};
pub use project_url_key::{
    derive_project_url_key, has_non_ascii_content, normalize_project_url_key,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn r530_smoke_agent_url_key_reexports_work() {
        // Sanity check that all 3 agent functions are re-exported.
        assert_eq!(
            normalize_agent_url_key("Hello World"),
            Some("hello-world".to_string())
        );
        assert!(is_uuid_like("11111111-2222-3333-8444-555555555555"));
        assert_eq!(derive_agent_url_key(Some("My Agent"), None), "my-agent");
    }

    #[test]
    fn r530_smoke_project_url_key_reexports_work() {
        // Sanity check that all 3 project functions are re-exported.
        assert_eq!(
            normalize_project_url_key("My Project"),
            Some("my-project".to_string())
        );
        assert!(!has_non_ascii_content("hello"));
        assert!(has_non_ascii_content("héllo"));
        assert_eq!(
            derive_project_url_key(Some("My Project"), None),
            "my-project"
        );
    }
}
