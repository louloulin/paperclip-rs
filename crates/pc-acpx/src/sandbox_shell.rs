//! `pc-acpx::sandbox_shell` — port of `sandbox-shell.ts` from Node
//! `paperclip/packages/adapter-utils/src/`.
//!
//! Mirrors exactly the two pure helpers used by the runtime-target
//! layer to select a default shell and to translate a script into the
//! argv that invokes it:
//!
//! - `preferredShellForSandbox` → [`preferred_shell_for_sandbox`]
//! - `shellCommandArgs` → [`shell_command_args`]
//!
//! Both helpers are total and side-effect free; they accept an optional
//! `shellCommand` and an optional script body, so they compose cleanly
//! with the runtime-target callers without dragging any I/O into the
//! crate.

/// Resolve the shell to invoke for a sandbox-launched process.
///
/// Mirrors Node `preferredShellForSandbox`: returns `"bash"` only when
/// the caller explicitly asks for it; everything else (including
/// `null`, `undefined`, `"sh"`, or any other value) collapses to `"sh"`
/// so the runtime always has a usable shell token.
#[must_use]
pub fn preferred_shell_for_sandbox(shell_command: Option<&str>) -> &'static str {
    match shell_command {
        Some("bash") => "bash",
        _ => "sh",
    }
}

/// Build the argv vector that invokes `script` through the shell
/// selected by [`preferred_shell_for_sandbox`].
///
/// Mirrors Node `shellCommandArgs`: the result is always
/// `["-c", script]`. The shell token itself is supplied by the caller,
/// which means the runtime-target layer composes the final command as
/// `shell + shell_command_args(script)`.
#[must_use]
pub fn shell_command_args(script: &str) -> [String; 2] {
    ["-c".to_string(), script.to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferred_shell_for_sandbox_accepts_bash() {
        assert_eq!(preferred_shell_for_sandbox(Some("bash")), "bash");
    }

    #[test]
    fn preferred_shell_for_sandbox_collapses_other_values_to_sh() {
        assert_eq!(preferred_shell_for_sandbox(Some("sh")), "sh");
        assert_eq!(preferred_shell_for_sandbox(Some("zsh")), "sh");
        assert_eq!(preferred_shell_for_sandbox(None), "sh");
    }

    #[test]
    fn shell_command_args_returns_dash_c_with_script() {
        assert_eq!(
            shell_command_args("echo hello"),
            ["-c".to_string(), "echo hello".to_string()]
        );
    }

    #[test]
    fn shell_command_args_round_trips_complex_scripts() {
        let script = "if [ -d /tmp ]; then echo yes; fi";
        let args = shell_command_args(script);
        assert_eq!(args[0], "-c");
        assert_eq!(args[1], script);
    }
}
