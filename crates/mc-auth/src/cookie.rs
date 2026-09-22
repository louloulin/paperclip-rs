//! Cookie 设置选项。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SameSite {
    Strict,
    Lax,
    None,
}

impl Default for SameSite {
    fn default() -> Self {
        Self::Lax
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CookieOptions {
    pub name: String,
    pub value: String,
    pub max_age_secs: Option<u64>,
    pub path: String,
    pub domain: Option<String>,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: SameSite,
}

impl CookieOptions {
    pub fn session_cookie(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
            max_age_secs: None,
            path: "/".into(),
            domain: None,
            secure: false,
            http_only: true,
            same_site: SameSite::Lax,
        }
    }

    pub fn render(&self) -> String {
        let mut out = format!("{}={}", self.name, self.value);
        if let Some(max_age) = self.max_age_secs {
            out.push_str(&format!("; Max-Age={max_age}"));
        }
        out.push_str(&format!("; Path={}", self.path));
        if let Some(domain) = &self.domain {
            out.push_str(&format!("; Domain={domain}"));
        }
        if self.secure {
            out.push_str("; Secure");
        }
        if self.http_only {
            out.push_str("; HttpOnly");
        }
        let ss = match self.same_site {
            SameSite::Strict => "Strict",
            SameSite::Lax => "Lax",
            SameSite::None => "None",
        };
        out.push_str(&format!("; SameSite={ss}"));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_includes_secure_attrs() {
        let c = CookieOptions::session_cookie("multica_session", "abc");
        let s = c.render();
        assert!(s.starts_with("multica_session=abc"));
        assert!(s.contains("SameSite=Lax"));
        assert!(s.contains("HttpOnly"));
        assert!(s.contains("Path=/"));
    }
}