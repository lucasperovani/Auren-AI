//! Headless browser navigation for pages that need JavaScript.
//!
//! Plain HTTP fetching cannot see pages rendered on the client (SPAs, content
//! behind infinite scroll, anything hydrated after load). This module drives a
//! locally installed Chromium instead of shipping one: the binary is expected
//! on the host — the Docker image installs it, and desktop installs can add it
//! or run in the `--no-browser` mode.
//!
//! Design notes:
//!
//! - **One-shot, not a daemon.** `chromium --headless --dump-dom` navigates,
//!   runs scripts until the page settles, prints the rendered DOM, and exits.
//!   There is no long-lived browser to supervise, no WebSocket to keep alive,
//!   and no state leaking between requests.
//! - **Bounded.** A virtual time budget and a wall-clock timeout cap every run;
//!   the process is killed if it overruns.
//! - **Sandboxed.** Runs with `--no-sandbox` only inside a container (where the
//!   kernel sandbox is unavailable); otherwise the sandbox stays on.
//! - **Same SSRF rules as the HTTP fetcher.** The URL is validated before the
//!   browser ever starts.

use std::process::Stdio;
use std::time::Duration;

use crate::error::ApiError;
use crate::services::web_research::html_to_text;

/// Wall-clock ceiling for one navigation. Chromium startup alone takes ~1s.
const NAV_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the page may run scripts before the DOM is captured.
const DEFAULT_VIRTUAL_TIME_MS: u32 = 8_000;
/// Cap on the rendered DOM handed back, in characters.
const MAX_DOM_CHARS: usize = 500_000;

/// Locate a Chromium-compatible binary.
///
/// `AUREN_BROWSER_PATH` is authoritative: when it is set, that exact binary is
/// used and a missing file means "no browser". Silently falling back to a
/// different browser would ignore explicit configuration — an operator who
/// pinned a path needs to know it is wrong, and the capability check that
/// hides `web_navigate` depends on that answer being predictable.
///
/// When the variable is unset, the usual names are probed in `PATH`.
pub fn find_browser() -> Option<std::path::PathBuf> {
    if let Ok(explicit) = std::env::var("AUREN_BROWSER_PATH") {
        let trimmed = explicit.trim();
        if trimmed.is_empty() {
            return None;
        }
        let path = std::path::PathBuf::from(trimmed);
        return if path.is_file() { Some(path) } else { None };
    }
    const CANDIDATES: [&str; 8] = [
        "chromium",
        "chromium-browser",
        "google-chrome",
        "google-chrome-stable",
        "chrome",
        "msedge",
        // Desktop bundles sometimes ship a headless shell.
        "headless_shell",
        "brave-browser",
    ];
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        for name in CANDIDATES {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Whether navigation is available in this deployment.
pub fn is_available() -> bool {
    find_browser().is_some()
}

/// Is the process running inside a container? Determines whether the Chromium
/// sandbox can be used.
fn in_container() -> bool {
    std::path::Path::new("/.dockerenv").exists()
        || std::path::Path::new("/run/.containerenv").exists()
        || std::env::var("container").is_ok()
}

/// Render a page with the headless browser and return its text.
///
/// Returns the DOM-derived text, the final URL, and the page title, matching
/// the shape of the plain HTTP fetch so tools can treat both the same way.
pub async fn navigate(url: &str) -> Result<serde_json::Value, ApiError> {
    // The browser must never be pointed at an internal destination, even
    // though it resolves names itself.
    crate::services::web_research::validate_public_url(url)?;

    let Some(browser) = find_browser() else {
        return Err(ApiError::Conflict(
            "no headless browser is installed; set AUREN_BROWSER_PATH or install chromium".into(),
        ));
    };

    // Build the argument list explicitly: the URL must be the LAST argument.
    // Anything after it is parsed as a second navigation target, which headless
    // mode rejects with "Multiple targets are not supported".
    let profile_dir = temp_profile_dir();
    let mut args: Vec<String> = vec![
        "--headless=new".into(),
        "--disable-gpu".into(),
        "--disable-dev-shm-usage".into(),
        "--hide-scrollbars".into(),
        "--mute-audio".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--disable-extensions".into(),
        "--disable-background-networking".into(),
        "--disable-sync".into(),
        format!("--user-data-dir={profile_dir}"),
        // Let client-side scripts run before the DOM is captured.
        format!("--virtual-time-budget={DEFAULT_VIRTUAL_TIME_MS}"),
        "--dump-dom".into(),
    ];
    // Chromium cannot create its own sandbox inside most containers unless it
    // is privileged, so the flag is applied only there.
    if in_container() {
        args.push("--no-sandbox".into());
    }
    args.push(url.to_string());

    let output = tokio::time::timeout(
        NAV_TIMEOUT,
        tokio::process::Command::new(&browser)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| ApiError::Conflict("the page took too long to render".into()))?
    .map_err(|e| ApiError::Internal(anyhow::anyhow!("headless browser failed to start: {e}")))?;

    // Clean up the throwaway profile: a stale profile on the next run would
    // restore the previous session.
    let _ = std::fs::remove_dir_all(profile_dir_path());

    let dom = String::from_utf8_lossy(&output.stdout).to_string();
    if dom.trim().is_empty() {
        // Surface the browser's own explanation instead of a bare 503: a
        // missing sandbox, a bad profile, or a crash all look like "no output".
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail: String = stderr
            .lines()
            .filter(|l| !l.trim().is_empty())
            .take(3)
            .collect::<Vec<_>>()
            .join(" | ");
        return Err(ApiError::Conflict(format!(
            "the browser produced no page ({}): {detail}",
            output.status
        )));
    }

    let title = extract_title(&dom).unwrap_or_default();
    let text = html_to_text(&dom);
    let truncated = text.chars().count() > MAX_DOM_CHARS;
    let text: String = text.chars().take(MAX_DOM_CHARS).collect();

    Ok(serde_json::json!({
        "url": url,
        "status": 200,
        "content_type": "text/html",
        "title": title,
        "text": text,
        "rendered": true,
        "truncated": truncated,
    }))
}

/// A unique throwaway profile directory for one run.
fn profile_dir_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!("auren-browser-{}", uuid::Uuid::new_v4()))
}

fn temp_profile_dir() -> String {
    let path = profile_dir_path();
    let _ = std::fs::create_dir_all(&path);
    path.to_string_lossy().to_string()
}

/// Page title from the rendered DOM.
fn extract_title(dom: &str) -> Option<String> {
    let lower = dom.to_ascii_lowercase();
    let start = lower.find("<title")?;
    let open_end = dom[start..].find('>')? + start + 1;
    let close = lower[open_end..].find("</title>")? + open_end;
    let title = dom[open_end..close].trim();
    if title.is_empty() { None } else { Some(title.to_string()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_title_from_dom() {
        let dom = "<html><head><title>Rendered Page</title></head><body>x</body></html>";
        assert_eq!(extract_title(dom).as_deref(), Some("Rendered Page"));
        assert!(extract_title("<html><body>no title</body></html>").is_none());
    }

    #[test]
    fn browser_lookup_is_non_fatal_when_absent() {
        // The result depends on the host; the contract is that this never
        // panics and `is_available` agrees with `find_browser`.
        assert_eq!(is_available(), find_browser().is_some());
    }

    #[test]
    fn explicit_browser_path_wins_when_valid() {
        // Point at a path that certainly exists.
        let exe = std::env::current_exe().unwrap();
        // Safety: single-threaded test manipulating a process-global var.
        unsafe { std::env::set_var("AUREN_BROWSER_PATH", &exe) };
        assert_eq!(find_browser().as_deref(), Some(exe.as_path()));
        unsafe { std::env::remove_var("AUREN_BROWSER_PATH") };
    }
}
