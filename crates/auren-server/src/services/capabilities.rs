//! Host capability detection.
//!
//! Some tools need a program that may not exist on the deployment host: a
//! headless browser for `web_navigate`, an interpreter for `exec_python`, a
//! client for `exec_ssh`. Advertising those tools when the program is absent
//! produces a silent failure — the model calls the tool, the call fails, and
//! the user has no idea why.
//!
//! Detection runs once per process (the host does not change while the server
//! is up) and the agent's toolset is filtered accordingly, so the model only
//! ever sees tools that can actually run.

use std::sync::OnceLock;

/// A program or facility a tool may require.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    /// A Chromium-compatible browser for `web_navigate`.
    Browser,
    /// A POSIX shell (`sh`) or `cmd.exe` on Windows.
    Shell,
    /// A Python interpreter.
    Python,
    /// The OpenSSH client.
    Ssh,
    /// The git CLI.
    Git,
}

impl Capability {
    /// Stable identifier used in API responses and logs.
    pub fn id(&self) -> &'static str {
        match self {
            Capability::Browser => "browser",
            Capability::Shell => "shell",
            Capability::Python => "python",
            Capability::Ssh => "ssh",
            Capability::Git => "git",
        }
    }

    /// What a user needs to install to make this available.
    pub fn install_hint(&self) -> &'static str {
        match self {
            Capability::Browser => {
                "install Chromium (or set AUREN_BROWSER_PATH); the Docker image enables it with --build-arg WITH_BROWSER=true"
            }
            Capability::Shell => "this platform has no shell available",
            Capability::Python => "install Python 3 and make `python3` (or `python`) available on PATH",
            Capability::Ssh => "install the OpenSSH client (`ssh`)",
            Capability::Git => "install the `git` CLI",
        }
    }

    /// Whether the requirement is met on this host.
    pub fn is_available(&self) -> bool {
        match self {
            Capability::Browser => crate::services::browser::is_available(),
            Capability::Shell => program_available(if cfg!(windows) { "cmd" } else { "sh" }),
            Capability::Python => python_available(),
            Capability::Ssh => program_available("ssh"),
            Capability::Git => program_available("git"),
        }
    }
}

/// All capabilities, for the status endpoint.
pub const ALL: [Capability; 5] = [
    Capability::Browser,
    Capability::Shell,
    Capability::Python,
    Capability::Ssh,
    Capability::Git,
];

/// Snapshot of every capability, computed once.
pub fn snapshot() -> &'static [(Capability, bool)] {
    static SNAPSHOT: OnceLock<Vec<(Capability, bool)>> = OnceLock::new();
    SNAPSHOT.get_or_init(|| ALL.iter().map(|c| (*c, c.is_available())).collect())
}

/// Whether a capability is available, cached.
pub fn available(capability: Capability) -> bool {
    snapshot()
        .iter()
        .find(|(c, _)| *c == capability)
        .map(|(_, ok)| *ok)
        .unwrap_or(false)
}

/// Capabilities that are missing on this host.
pub fn missing() -> Vec<&'static str> {
    snapshot()
        .iter()
        .filter(|(_, ok)| !ok)
        .map(|(c, _)| c.id())
        .collect()
}

/// The capability a tool implementation needs, if any.
///
/// Keyed on the implementation reference (or the tool name for integration
/// operations) so both declarative skills and builtins are covered.
pub fn required_for(reference: &str) -> Option<Capability> {
    // Normalize `builtin.x` and bare `x` to the same lookup key.
    let bare = reference.strip_prefix("builtin.").unwrap_or(reference);
    match bare {
        "web_navigate" => Some(Capability::Browser),
        "exec_shell" => Some(Capability::Shell),
        "exec_python" => Some(Capability::Python),
        "exec_ssh" | "ssh_connect" => Some(Capability::Ssh),
        // The whole `git_*` family shells out to the git CLI.
        other if other.starts_with("git_") => Some(Capability::Git),
        _ => None,
    }
}

/// Whether a tool with this implementation reference can run here.
pub fn tool_is_available(reference: &str) -> bool {
    match required_for(reference) {
        Some(capability) => available(capability),
        None => true,
    }
}

/// Explain why a tool cannot run, for an error message.
pub fn unavailable_reason(reference: &str) -> Option<String> {
    let capability = required_for(reference)?;
    if capability.is_available() {
        return None;
    }
    Some(format!(
        "this tool requires {} which is not available on this host: {}",
        capability.id(),
        capability.install_hint()
    ))
}

/// Look for a program on `PATH`, honouring Windows executable extensions.
fn program_available(program: &str) -> bool {
    let Some(path_var) = std::env::var_os("PATH") else {
        return false;
    };
    let exts: &[&str] = if cfg!(windows) {
        &["", ".exe", ".cmd", ".bat"]
    } else {
        &[""]
    };
    for dir in std::env::split_paths(&path_var) {
        for ext in exts {
            if dir.join(format!("{program}{ext}")).is_file() {
                return true;
            }
        }
    }
    false
}

/// Whether any supported Python interpreter name is on `PATH`.
fn python_available() -> bool {
    let names: &[&str] = if cfg!(windows) {
        &["python", "python3", "py"]
    } else {
        &["python3", "python"]
    };
    names.iter().any(|n| program_available(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_implementations_to_capabilities() {
        // `builtin.` prefixes and bare names resolve identically.
        assert_eq!(required_for("web_navigate"), Some(Capability::Browser));
        assert_eq!(required_for("builtin.web_navigate"), Some(Capability::Browser));
        assert_eq!(required_for("exec_shell"), Some(Capability::Shell));
        assert_eq!(required_for("exec_python"), Some(Capability::Python));
        assert_eq!(required_for("exec_ssh"), Some(Capability::Ssh));
        assert_eq!(required_for("ssh_connect"), Some(Capability::Ssh));
        // The git family is matched by prefix.
        assert_eq!(required_for("git_status"), Some(Capability::Git));
        assert_eq!(required_for("git_push"), Some(Capability::Git));
        // Tools with no host requirement are always usable.
        assert_eq!(required_for("web_search"), None);
        assert_eq!(required_for("web_fetch"), None);
        assert_eq!(required_for("task_create"), None);
        assert_eq!(required_for("github_list_repos"), None);
    }

    #[test]
    fn shell_is_always_available_on_a_build_host() {
        // CI machines and dev boxes both have a shell; if this fails the
        // environment is unusual rather than the mapping being wrong.
        assert!(available(Capability::Shell));
        assert!(tool_is_available("exec_shell"));
    }

    #[test]
    fn available_tools_have_no_reason() {
        // web_search needs nothing, so it can never be reported unavailable.
        assert!(unavailable_reason("web_search").is_none());
        for (capability, ok) in snapshot() {
            if *ok {
                // A satisfied capability never blocks its tools.
                let any_tool = match capability {
                    Capability::Browser => "web_navigate",
                    Capability::Shell => "exec_shell",
                    Capability::Python => "exec_python",
                    Capability::Ssh => "exec_ssh",
                    Capability::Git => "git_status",
                };
                assert!(unavailable_reason(any_tool).is_none());
            }
        }
    }

    #[test]
    fn snapshot_covers_every_capability() {
        assert_eq!(snapshot().len(), ALL.len());
        // The missing list only contains disabled capabilities.
        for id in missing() {
            assert!(ALL.iter().any(|c| c.id() == id));
        }
    }

    #[test]
    fn browser_detection_follows_the_configured_path() {
        // `AUREN_BROWSER_PATH` wins over PATH discovery, so a bogus value is a
        // deterministic way to simulate "no browser installed".
        //
        // The capability *snapshot* is cached per process (the host does not
        // change while the server runs), so this test checks the underlying
        // probe rather than the cache to stay order-independent.
        //
        // Safety: the variable is process-global; this test owns it.
        unsafe { std::env::set_var("AUREN_BROWSER_PATH", "/nonexistent/definitely-not-a-browser") };
        assert!(
            !crate::services::browser::is_available(),
            "a nonexistent AUREN_BROWSER_PATH must disable the browser"
        );
        // The mapping is what makes the filter hide the tool.
        assert_eq!(required_for("web_navigate"), Some(Capability::Browser));

        unsafe { std::env::remove_var("AUREN_BROWSER_PATH") };
    }

    #[test]
    fn missing_capability_produces_an_actionable_reason() {
        // The reason must tell the user what to install, not just fail. This
        // is computed on demand (no cache), so it reflects the live host.
        let hint = Capability::Browser.install_hint();
        assert!(hint.contains("Chromium"));
        assert!(hint.contains("WITH_BROWSER"));
        assert!(Capability::Python.install_hint().contains("python3"));

        // A capability with no mapping never reports a reason.
        assert!(unavailable_reason("web_search").is_none());
        assert!(unavailable_reason("notify_user").is_none());
    }
}
