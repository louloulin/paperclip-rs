//! `pc-acpx::sandbox_install_command` — port of
//! `sandbox-install-command.ts` from Node
//! `paperclip/packages/adapter-utils/src/`.
//!
//! The runtime-target layer uses this helper to build a one-shot shell
//! command that installs a global npm package on a sandbox image that
//! may or may not ship with `npm`. The command:
//!
//! 1. Bootstraps a portable Node tarball into `$HOME/.local` if `npm`
//!    is missing.
//! 2. Detects whether the user is `root`, has passwordless `sudo`,
//!    or neither, and runs `npm install -g <pkg>` accordingly.
//!
//! The Rust port is byte-identical to the Node output. The only
/// exported function is [`build_sandbox_npm_install_command`].

/// Build a single shell script that installs `package_name` globally
/// in a sandbox environment.
///
/// Mirrors Node `buildSandboxNpmInstallCommand`. The script:
/// - bootstraps Node v22.11.0 (linux x64 / arm64) into `$HOME/.local`
///   if no `npm` is on PATH,
/// - runs `npm install -g <pkg>` directly when running as `root`,
/// - falls back to `sudo -E npm install -g <pkg>` when passwordless
///   sudo is available,
/// - falls back to `npm install -g --prefix $HOME/.local <pkg>` when
///   neither is available.
#[must_use]
pub fn build_sandbox_npm_install_command(package_name: &str) -> String {
    let quoted_package_name = shell_single_quote(package_name);

    let ensure_npm_preamble = "PAPERCLIP_NPM_BOOTSTRAPPED=; \
        if ! command -v npm >/dev/null 2>&1; then \
            NODE_ARCH=\"$(uname -m)\"; \
            case \"$NODE_ARCH\" in \
                x86_64) NODE_ARCH=x64 ;; \
                aarch64|arm64) NODE_ARCH=arm64 ;; \
            esac; \
            NODE_VERSION=\"v22.11.0\"; \
            NODE_TARBALL=\"node-${NODE_VERSION}-linux-${NODE_ARCH}.tar.xz\"; \
            mkdir -p \"$HOME/.local\"; \
            curl -fsSL \"https://nodejs.org/dist/${NODE_VERSION}/${NODE_TARBALL}\" -o \"/tmp/${NODE_TARBALL}\" && \
            tar -xJf \"/tmp/${NODE_TARBALL}\" -C \"$HOME/.local\" --strip-components=1 && \
            rm -f \"/tmp/${NODE_TARBALL}\" && \
            export PATH=\"$HOME/.local/bin:$PATH\" && \
            PAPERCLIP_NPM_BOOTSTRAPPED=1; \
        fi;";

    let parts = [
        ensure_npm_preamble,
        "if [ -n \"$PAPERCLIP_NPM_BOOTSTRAPPED\" ]; then",
        &format!("npm install -g {quoted_package_name};"),
        "elif [ \"$(id -u)\" -eq 0 ]; then",
        &format!("npm install -g {quoted_package_name};"),
        "elif command -v sudo >/dev/null 2>&1 && sudo -n true >/dev/null 2>&1; then",
        &format!("sudo -E npm install -g {quoted_package_name};"),
        "else",
        &format!(
            "mkdir -p \"$HOME/.local\" && npm install -g --prefix \"$HOME/.local\" {quoted_package_name};"
        ),
        "fi",
    ];

    parts.join(" ")
}

/// POSIX shell single-quote a string. Mirrors the helper at the top
/// of the Node module.
fn shell_single_quote(value: &str) -> String {
    let escaped = value.replace('\'', r#"'"'"'"#);
    format!("'{escaped}'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_includes_npm_bootstrap_preamble() {
        let script = build_sandbox_npm_install_command("typescript");
        assert!(script.contains("PAPERCLIP_NPM_BOOTSTRAPPED"));
        assert!(script.contains("command -v npm"));
        assert!(script.contains("nodejs.org/dist/"));
        assert!(script.contains("NODE_VERSION=\"v22.11.0\""));
    }

    #[test]
    fn script_handles_root_user_branch() {
        let script = build_sandbox_npm_install_command("typescript");
        assert!(script.contains("$(id -u)\" -eq 0"));
    }

    #[test]
    fn script_handles_sudo_fallback_branch() {
        let script = build_sandbox_npm_install_command("typescript");
        assert!(script.contains("sudo -n true"));
        assert!(script.contains("sudo -E npm install -g"));
    }

    #[test]
    fn script_handles_user_local_fallback_branch() {
        let script = build_sandbox_npm_install_command("typescript");
        assert!(script.contains("--prefix \"$HOME/.local\""));
    }

    #[test]
    fn package_name_is_single_quoted() {
        let script = build_sandbox_npm_install_command("typescript");
        assert!(script.contains("npm install -g 'typescript';"));
        assert!(script.contains("npm install -g 'typescript';"));
        assert!(script.contains("npm install -g 'typescript';"));
    }

    #[test]
    fn single_quote_in_package_name_is_escaped() {
        let script = build_sandbox_npm_install_command("weird'pkg");
        assert!(script.contains(r#"'weird'"'"'pkg'"#));
    }

    #[test]
    fn different_package_names_produce_different_scripts() {
        let a = build_sandbox_npm_install_command("typescript");
        let b = build_sandbox_npm_install_command("@paperclipai/cli");
        assert_ne!(a, b);
        assert!(b.contains("@paperclipai/cli"));
    }
}
