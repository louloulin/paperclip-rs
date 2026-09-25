//! M8 的**部署密钥读取口**（github / vcs / composio）—— 从 `state.rs` 拆出的独立文件
//! （`LUM-1797` / `docs/61-M8-PLAN.md` §6.3 的 R-M8-7）。
//!
//! # 为什么单独一个文件（门 ⑩ 的预飞）
//!
//! `state.rs` 是**唯一被 M7-0 与 M8-0 两个 anchor 追加**的文件（M6-0 也追加过）。
//! M7-0 落地后它已 **640 行**（> 620 的预设阈值）⇒ 按 `docs/61` §6.3 / R-M8-7 的约定，
//! M8-0 把「读 env + 构造」下放到本文件，`state.rs` 只加三个字段与三行构造。
//!
//! # 三条与 `PluginSecretKey` / `ChannelKeys` 同款的纪律（别开后门）
//!
//! 1. **未配置就是未配置**：缺 / 空 / 非法 ⇒ `None` / `false`，**绝不**用零值兑底、
//!    **绝不** panic（缺密钥时就**不装配**该面，`docs/61` §2.4 / §2.5）；
//! 2. **不新增 `AppState::new` 参数**：这些结构在 `AppState::new` 的构造体里读 env，
//!    所以全部调用点不动；测试要注入就手写字面量 / 用各自的 `from_env_with`；
//! 3. **密钥不进 `Debug`**：手写脱敏实现，只暴露**存在性**（运维需要判断哪面没配）。
//!
//! # 逐面的 env（`docs/61` §2.4 的四类部署密钥）
//!
//! | 面 | env | 缺失语义 |
//! | --- | --- | --- |
//! | GitHub App | `GITHUB_APP_ID` + `GITHUB_APP_PRIVATE_KEY` | 「能浏览仓库」关闭 |
//! | GitHub 连接/webhook | `GITHUB_WEBHOOK_SECRET` + `GITHUB_APP_SLUG` | connect 给「未配置」；webhook 401 |
//! | VCS | `MULTICA_VCS_SECRET_KEY`（base64 32B）+ `MULTICA_VCS_INTEGRATION_ENABLED` | connect 503（**绝不落明文**） |
//! | composio | `COMPOSIO_API_KEY` + `COMPOSIO_STATE_SECRET`\|`JWT_SECRET` + `COMPOSIO_CALLBACK_BASE_URL`\|`MULTICA_PUBLIC_URL` | 4 条会话路由 503 |

use mc_secrets::SecretBox;

/// GitHub App 的四类部署密钥 / 标识（**唯一**读取口是 [`GithubKeys::from_env`]）。
///
/// ⚠️ 与 `mc-vcs-github::port::GithubAppConfig` 的分工：本结构是**进程 env 的读取口**
/// （生产装配），那边是**装配参数的载体**（交给 `ghsnapshot::Manager`）。两者形状相近但
/// 用途不同，**不要**互相替代（env 读取口只有这一个）。
#[derive(Clone, Default)]
pub struct GithubKeys {
    pub app_id: Option<String>,
    pub private_key_pem: Option<String>,
    pub webhook_secret: Option<String>,
    pub app_slug: Option<String>,
}

impl GithubKeys {
    /// 从进程环境读（生产装配点：`AppState::new`）。
    pub fn from_env() -> Self {
        Self::from_env_with(|name| std::env::var(name).ok())
    }

    /// 从任意「名字 → 值」查询函数读（单测注入，不碰进程全局 env）。
    pub fn from_env_with<F>(get: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        Self {
            app_id: non_empty(get("GITHUB_APP_ID")),
            private_key_pem: non_empty(get("GITHUB_APP_PRIVATE_KEY")),
            webhook_secret: non_empty(get("GITHUB_WEBHOOK_SECRET")),
            app_slug: non_empty(get("GITHUB_APP_SLUG")),
        }
    }

    /// 「能换 installation token / 浏览仓库」的判据（App id + 私钥）。
    pub fn is_app_configured(&self) -> bool {
        self.app_id.is_some() && self.private_key_pem.is_some()
    }

    /// 「webhook 可验签 / state 可签」的判据（上游**刻意复用**同一个 secret）。
    pub fn is_webhook_configured(&self) -> bool {
        self.webhook_secret.is_some()
    }

    /// 「可给前端安装引导 URL」的判据。
    pub fn is_connectable(&self) -> bool {
        self.app_slug.is_some() && self.webhook_secret.is_some()
    }

    /// 已配的项（**字典序**，诊断/测试要确定性；**不**回显值）。
    pub fn configured(&self) -> Vec<&'static str> {
        let mut names = Vec::new();
        if self.app_id.is_some() {
            names.push("GITHUB_APP_ID");
        }
        if self.private_key_pem.is_some() {
            names.push("GITHUB_APP_PRIVATE_KEY");
        }
        if self.webhook_secret.is_some() {
            names.push("GITHUB_WEBHOOK_SECRET");
        }
        if self.app_slug.is_some() {
            names.push("GITHUB_APP_SLUG");
        }
        names
    }
}

impl std::fmt::Debug for GithubKeys {
    /// 手写脱敏：**绝不**打印私钥或 webhook secret。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GithubKeys")
            .field("configured", &self.configured())
            .finish()
    }
}

/// VCS 面的部署密钥与**产品边界**开关。
///
/// 两个开关语义**独立**（`docs/61` §2.4）：`is_enabled()` 是产品边界
/// （`MULTICA_VCS_INTEGRATION_ENABLED`，自建版才有、云端关掉）；`is_configured()` 是密钥存在性。
#[derive(Clone, Default)]
pub struct VcsKeys {
    /// `MULTICA_VCS_SECRET_KEY`（base64 32B）解出的封装盒。
    secret_box: Option<SecretBox>,
    /// `MULTICA_VCS_INTEGRATION_ENABLED`（缺省 `false`：**产品边界默认关闭**）。
    integration_enabled: bool,
}

impl VcsKeys {
    /// 部署密钥的 env 名（**只有一份**，别在别处内联字面量）。
    pub const SECRET_KEY_ENV: &'static str = "MULTICA_VCS_SECRET_KEY";
    /// 产品边界的 env 名。
    pub const ENABLED_ENV: &'static str = "MULTICA_VCS_INTEGRATION_ENABLED";

    /// 从进程环境读（生产装配点）。
    pub fn from_env() -> Self {
        Self::from_env_with(|name| std::env::var(name).ok())
    }

    /// 从任意「名字 → 值」查询函数读。
    pub fn from_env_with<F>(get: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        let secret_box =
            mc_secrets::secretbox::load_key_with(Self::SECRET_KEY_ENV, |name| get(name));
        // 开关的解析口径照上游：只有显式 `true` / `1` / `yes` 才算开（**不** trim 后再判，
        // 与 `PluginSecretKey` 的「没有 trim」同款——上游只在精确值上判）。
        let integration_enabled = matches!(
            get(Self::ENABLED_ENV).as_deref(),
            Some("true" | "1" | "yes")
        );
        Self {
            secret_box,
            integration_enabled,
        }
    }

    /// 密钥是否配置（⇒ connect 可 503 vs 落明文；缺密钥**绝不**落明文）。
    pub fn is_configured(&self) -> bool {
        self.secret_box.is_some()
    }

    /// 产品边界是否开启。
    pub fn is_enabled(&self) -> bool {
        self.integration_enabled
    }

    /// 封装盒（`None` = 未配置）。
    pub fn secret_box(&self) -> Option<&SecretBox> {
        self.secret_box.as_ref()
    }
}

impl std::fmt::Debug for VcsKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VcsKeys")
            .field(
                "secret_key",
                &self.secret_box.as_ref().map(|_| "<redacted>"),
            )
            .field("integration_enabled", &self.integration_enabled)
            .finish()
    }
}

/// composio 的三类部署配置（**四个条件**里的三个；第四个是 feature flag，由
/// `mc-feature-flags` 提供，不在这里）。
#[derive(Clone, Default)]
pub struct ComposioKeys {
    pub api_key: Option<String>,
    pub state_secret: Option<String>,
    pub callback_base_url: Option<String>,
}

impl ComposioKeys {
    /// 从进程环境读（生产装配点）。
    ///
    /// `state_secret` 的回落是 `JWT_SECRET`；`callback_base_url` 的回落是 `MULTICA_PUBLIC_URL`
    /// （`docs/61` §2.4 的 composio 行，逐字）。
    pub fn from_env() -> Self {
        Self::from_env_with(|name| std::env::var(name).ok())
    }

    /// 从任意「名字 → 值」查询函数读。
    pub fn from_env_with<F>(get: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        Self {
            api_key: non_empty(get("COMPOSIO_API_KEY")),
            state_secret: non_empty(get("COMPOSIO_STATE_SECRET"))
                .or_else(|| non_empty(get("JWT_SECRET"))),
            callback_base_url: non_empty(get("COMPOSIO_CALLBACK_BASE_URL"))
                .or_else(|| non_empty(get("MULTICA_PUBLIC_URL"))),
        }
    }

    /// 三个**本结构负责的**条件是否齐（第四个 feature flag 由调用侧 `and` 上）。
    pub fn is_configured(&self) -> bool {
        self.api_key.is_some() && self.state_secret.is_some() && self.callback_base_url.is_some()
    }

    /// 已配的项（诊断；**不**回显值）。
    pub fn configured(&self) -> Vec<&'static str> {
        let mut names = Vec::new();
        if self.api_key.is_some() {
            names.push("COMPOSIO_API_KEY");
        }
        if self.state_secret.is_some() {
            names.push("COMPOSIO_STATE_SECRET|JWT_SECRET");
        }
        if self.callback_base_url.is_some() {
            names.push("COMPOSIO_CALLBACK_BASE_URL|MULTICA_PUBLIC_URL");
        }
        names
    }
}

impl std::fmt::Debug for ComposioKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComposioKeys")
            .field("configured", &self.configured())
            .finish()
    }
}

/// 空串即 `None`（上游 `if raw == ""` 的口径；**不** trim —— 与 `PluginSecretKey` 同款）。
fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|raw| !raw.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_keys_default_to_unconfigured_without_panicking() {
        let none = |_: &str| None;
        let github = GithubKeys::from_env_with(none);
        assert!(!github.is_app_configured());
        assert!(!github.is_webhook_configured());
        assert!(!github.is_connectable());
        assert!(github.configured().is_empty());

        let vcs = VcsKeys::from_env_with(none);
        assert!(!vcs.is_configured());
        assert!(!vcs.is_enabled());
        assert!(vcs.secret_box().is_none());

        let composio = ComposioKeys::from_env_with(none);
        assert!(!composio.is_configured());
        assert!(composio.configured().is_empty());
    }

    #[test]
    fn empty_strings_are_treated_as_unset() {
        let empty = |_: &str| Some(String::new());
        assert!(!GithubKeys::from_env_with(empty).is_app_configured());
        assert!(!VcsKeys::from_env_with(empty).is_configured());
        assert!(!ComposioKeys::from_env_with(empty).is_configured());
    }

    #[test]
    fn composio_falls_back_to_jwt_secret_and_public_url() {
        let get = |name: &str| match name {
            "JWT_SECRET" => Some("jwt-secret".to_string()),
            "MULTICA_PUBLIC_URL" => Some("https://public.test".to_string()),
            _ => None,
        };
        let keys = ComposioKeys::from_env_with(get);
        assert_eq!(keys.state_secret.as_deref(), Some("jwt-secret"));
        assert_eq!(
            keys.callback_base_url.as_deref(),
            Some("https://public.test")
        );
        // 缺 api key ⇒ 整体仍未配置。
        assert!(!keys.is_configured());
    }

    #[test]
    fn debug_never_echoes_secret_values() {
        let github = GithubKeys {
            app_id: Some("1".into()),
            private_key_pem: Some("-----BEGIN PRIVATE KEY-----\nsecret\n".into()),
            webhook_secret: Some("shh".into()),
            app_slug: Some("slug".into()),
        };
        let rendered = format!("{github:?}");
        assert!(!rendered.contains("secret"));
        assert!(!rendered.contains("shh"));

        let composio = ComposioKeys {
            api_key: Some("ak_live_xxx".into()),
            state_secret: Some("state-secret".into()),
            callback_base_url: Some("https://x.test".into()),
        };
        let rendered = format!("{composio:?}");
        assert!(!rendered.contains("ak_live_xxx"));
        assert!(!rendered.contains("state-secret"));
    }

    #[test]
    fn vcs_enabled_flag_accepts_only_explicit_truthy_values() {
        let get = |name: &str| match name {
            "MULTICA_VCS_INTEGRATION_ENABLED" => Some("true".to_string()),
            _ => None,
        };
        assert!(VcsKeys::from_env_with(get).is_enabled());
        let get = |name: &str| match name {
            "MULTICA_VCS_INTEGRATION_ENABLED" => Some("yes".to_string()),
            _ => None,
        };
        assert!(VcsKeys::from_env_with(get).is_enabled());
        let get = |name: &str| match name {
            "MULTICA_VCS_INTEGRATION_ENABLED" => Some("false".to_string()),
            _ => None,
        };
        assert!(!VcsKeys::from_env_with(get).is_enabled());
    }
}
