//! Import skills, tools, and MCP server definitions from a GitHub repository.
//!
//! Users point at a repository (optionally a subdirectory) and Auren discovers
//! what it provides: skill packages (`skill.json`), single tool definitions
//! (`tools/*.json`) and MCP server descriptors (`.mcp.json`, `mcp.json`, or a
//! `mcpServers` block in `package.json`). Discovery never installs anything by
//! itself: the caller reviews the catalog and installs explicitly, so an
//! arbitrary repository cannot silently register tools.
//!
//! Repositories are fetched through the GitHub API with the user's connected
//! account when available (private repos), falling back to anonymous access
//! for public ones.

use serde::Serialize;
use serde_json::{json, Value};

use crate::{error::ApiError, state::AppState};

/// Maximum listing depth when walking the repository tree.
const MAX_DEPTH: usize = 4;
/// Cap on discovered items so a huge repository cannot flood the UI.
const MAX_ITEMS: usize = 100;

/// A repository to read from.
#[derive(Debug, Clone)]
pub struct RepoRef {
    pub owner: String,
    pub repo: String,
    /// Branch, tag, or commit. Defaults to the repository's default branch.
    pub git_ref: Option<String>,
    /// Subdirectory to search in; empty means the repository root.
    pub path: Option<String>,
}

impl RepoRef {
    /// Parse `owner/repo`, `https://github.com/owner/repo`, or the `owner/repo`
    /// form with an optional `/tree/<ref>/<path>` suffix.
    pub fn parse(input: &str) -> Result<Self, ApiError> {
        let trimmed = input.trim();
        let without_scheme = trimmed
            .strip_prefix("https://github.com/")
            .or_else(|| trimmed.strip_prefix("http://github.com/"))
            .or_else(|| trimmed.strip_prefix("github.com/"))
            .unwrap_or(trimmed)
            .trim_end_matches('/');

        let mut parts = without_scheme.split('/');
        let owner = parts.next().unwrap_or_default();
        let repo = parts.next().unwrap_or_default();
        if owner.is_empty() || repo.is_empty() || !is_github_name(owner) || !is_github_name(repo) {
            return Err(ApiError::Unprocessable(
                "expected a repository as 'owner/repo' or a GitHub URL".into(),
            ));
        }

        // Optional /tree/<ref>/<path...>
        let mut git_ref = None;
        let mut path = None;
        if parts.next() == Some("tree") {
            git_ref = parts.next().map(|s| s.to_string());
            let rest: Vec<&str> = parts.collect();
            if !rest.is_empty() {
                path = Some(rest.join("/"));
            }
        }

        Ok(Self {
            owner: owner.to_string(),
            repo: repo.trim_end_matches(".git").to_string(),
            git_ref,
            path,
        })
    }
}

/// GitHub allows letters, digits, `.`, `-`, and `_` in these names. A name
/// must also contain at least one alphanumeric character, which rejects
/// path-ish inputs like `..` or `.`.
fn is_github_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value.chars().any(|c| c.is_ascii_alphanumeric())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// What a repository offers, grouped by kind.
#[derive(Debug, Serialize)]
pub struct Discovery {
    pub repository: String,
    pub reference: String,
    pub skills: Vec<DiscoveredSkill>,
    pub tools: Vec<DiscoveredTool>,
    pub mcp_servers: Vec<DiscoveredMcpServer>,
    /// Non-fatal notes (files skipped, unsupported manifests).
    pub notes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct DiscoveredSkill {
    /// Directory holding `skill.json`.
    pub path: String,
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
}

#[derive(Debug, Serialize)]
pub struct DiscoveredTool {
    pub path: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Serialize)]
pub struct DiscoveredMcpServer {
    pub path: String,
    pub name: String,
    pub url: Option<String>,
    /// `stdio` servers cannot be launched by the server-side MCP client.
    pub transport: String,
}

/// A resolved credential for GitHub access.
pub struct GithubAccess {
    pub token: Option<String>,
}

/// Discover everything importable in a repository.
pub async fn discover(
    state: &AppState,
    repo: &RepoRef,
    access: &GithubAccess,
) -> Result<Discovery, ApiError> {
    let client = reqwest::Client::new();
    let reference = match &repo.git_ref {
        Some(r) => r.clone(),
        None => default_branch(&client, repo, access).await?,
    };

    let tree = fetch_tree(&client, repo, &reference, access).await?;
    let entries = tree
        .get("tree")
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();

    let mut discovery = Discovery {
        repository: format!("{}/{}", repo.owner, repo.repo),
        reference: reference.clone(),
        skills: Vec::new(),
        tools: Vec::new(),
        mcp_servers: Vec::new(),
        notes: Vec::new(),
    };

    // Narrow to the requested subdirectory.
    let prefix = repo.path.as_deref().unwrap_or("").trim_matches('/').to_string();
    let in_scope = |path: &str| -> bool {
        if prefix.is_empty() {
            return true;
        }
        path == prefix || path.starts_with(&format!("{prefix}/"))
    };

    for entry in &entries {
        let Some(path) = entry.get("path").and_then(|p| p.as_str()) else { continue };
        let kind = entry.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if kind != "blob" || !in_scope(path) || depth(path) > MAX_DEPTH {
            continue;
        }
        let file = path.rsplit('/').next().unwrap_or(path);

        if file == "skill.json" {
            match fetch_json(&client, repo, &reference, path, access).await {
                Ok(manifest) => {
                    if let Some(skill) = skill_from_manifest(path, &manifest) {
                        discovery.skills.push(skill);
                    } else {
                        discovery.notes.push(format!("{path}: not a valid skill manifest"));
                    }
                }
                Err(_) => discovery.notes.push(format!("{path}: could not be read")),
            }
            continue;
        }

        let is_tool_file = (path.contains("/tools/") || path.starts_with("tools/"))
            && file.ends_with(".json");
        if is_tool_file {
            if let Ok(value) = fetch_json(&client, repo, &reference, path, access).await {
                if let Some(tool) = tool_from_definition(path, &value) {
                    discovery.tools.push(tool);
                }
            }
            continue;
        }

        if file == ".mcp.json" || file == "mcp.json" {
            if let Ok(value) = fetch_json(&client, repo, &reference, path, access).await {
                discovery
                    .mcp_servers
                    .extend(mcp_servers_from(path, &value));
            }
        }
    }

    if discovery.skills.len() + discovery.tools.len() + discovery.mcp_servers.len() > MAX_ITEMS {
        discovery.notes.push(format!(
            "Showing the first {MAX_ITEMS} items; narrow the path to see more."
        ));
        discovery.skills.truncate(MAX_ITEMS);
        discovery.tools.truncate(MAX_ITEMS);
        discovery.mcp_servers.truncate(MAX_ITEMS);
    }
    let _ = state;

    Ok(discovery)
}

fn depth(path: &str) -> usize {
    path.matches('/').count()
}

fn skill_from_manifest(path: &str, manifest: &Value) -> Option<DiscoveredSkill> {
    let id = manifest.get("id")?.as_str()?.to_string();
    let version = manifest.get("version")?.as_str()?.to_string();
    Some(DiscoveredSkill {
        path: path.trim_end_matches("/skill.json").to_string(),
        name: manifest
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or(&id)
            .to_string(),
        id,
        version,
        description: manifest
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    })
}

fn tool_from_definition(path: &str, value: &Value) -> Option<DiscoveredTool> {
    // Accept both a single tool object and a wrapper with a `tools` array.
    let name = value.get("name").and_then(|v| v.as_str()).map(|s| s.to_string());
    let name = name.or_else(|| {
        path.rsplit('/')
            .next()
            .map(|f| f.trim_end_matches(".json").to_string())
    })?;
    Some(DiscoveredTool {
        path: path.to_string(),
        description: value
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        name,
    })
}

/// Read MCP server definitions from a descriptor file.
fn mcp_servers_from(path: &str, value: &Value) -> Vec<DiscoveredMcpServer> {
    // Standard shape: { "mcpServers": { "name": { "url": "..." } } }
    let servers = value.get("mcpServers").and_then(|v| v.as_object());
    let Some(servers) = servers else { return Vec::new() };

    servers
        .iter()
        .map(|(name, config)| {
            let url = config.get("url").and_then(|v| v.as_str()).map(|s| s.to_string());
            // HTTP transports can be used by the server-side client; stdio
            // servers would need a local process, which is out of scope.
            let transport = if url.is_some() {
                "http"
            } else if config.get("command").is_some() {
                "stdio"
            } else {
                "unknown"
            };
            DiscoveredMcpServer {
                path: path.to_string(),
                name: name.clone(),
                url,
                transport: transport.to_string(),
            }
        })
        .collect()
}

fn github_client_request(
    client: &reqwest::Client,
    access: &GithubAccess,
    url: &str,
) -> reqwest::RequestBuilder {
    let builder = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "auren-ai")
        .timeout(std::time::Duration::from_secs(30));
    match &access.token {
        Some(token) => builder.bearer_auth(token),
        None => builder,
    }
}

async fn default_branch(
    client: &reqwest::Client,
    repo: &RepoRef,
    access: &GithubAccess,
) -> Result<String, ApiError> {
    let url = format!("https://api.github.com/repos/{}/{}", repo.owner, repo.repo);
    let response = github_client_request(client, access, &url)
        .send()
        .await
        .map_err(|_| ApiError::ServiceUnavailable)?;
    match response.status() {
        reqwest::StatusCode::NOT_FOUND => Err(ApiError::NotFound),
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => Err(
            ApiError::Conflict("GitHub rejected the request; connect an account or check the repository name".into()),
        ),
        s if s.is_success() => {
            let body: Value = response.json().await.unwrap_or(json!({}));
            Ok(body
                .get("default_branch")
                .and_then(|v| v.as_str())
                .unwrap_or("main")
                .to_string())
        }
        _ => Err(ApiError::ServiceUnavailable),
    }
}

async fn fetch_tree(
    client: &reqwest::Client,
    repo: &RepoRef,
    reference: &str,
    access: &GithubAccess,
) -> Result<Value, ApiError> {
    // Recursive tree listing: one request instead of walking directories.
    let url = format!(
        "https://api.github.com/repos/{}/{}/git/trees/{}?recursive=1",
        repo.owner, repo.repo, reference
    );
    let response = github_client_request(client, access, &url)
        .send()
        .await
        .map_err(|_| ApiError::ServiceUnavailable)?;
    match response.status() {
        reqwest::StatusCode::NOT_FOUND => Err(ApiError::NotFound),
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => Err(
            ApiError::Conflict("GitHub rejected the request; connect an account or check the reference".into()),
        ),
        s if s.is_success() => response.json().await.map_err(|_| ApiError::ServiceUnavailable),
        _ => Err(ApiError::ServiceUnavailable),
    }
}

async fn fetch_json(
    client: &reqwest::Client,
    repo: &RepoRef,
    reference: &str,
    path: &str,
    access: &GithubAccess,
) -> Result<Value, ApiError> {
    let url = format!(
        "https://raw.githubusercontent.com/{}/{}/{}/{}",
        repo.owner, repo.repo, reference, path
    );
    let response = github_client_request(client, access, &url)
        .send()
        .await
        .map_err(|_| ApiError::ServiceUnavailable)?;
    if !response.status().is_success() {
        return Err(ApiError::ServiceUnavailable);
    }
    response.json().await.map_err(|_| ApiError::Unprocessable("malformed JSON".into()))
}

/// Download a skill package directory as a ZIP suitable for the installer.
///
/// The archive mirrors the `skill.json` layout expected by the installer: a
/// single top-level directory containing the manifest and its files.
pub async fn download_skill_package(
    repo: &RepoRef,
    reference: &str,
    skill_path: &str,
    access: &GithubAccess,
) -> Result<Vec<u8>, ApiError> {
    use std::io::Write;

    let client = reqwest::Client::new();
    let tree = fetch_tree(&client, repo, reference, access).await?;
    let entries = tree
        .get("tree")
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();

    let prefix = skill_path.trim_matches('/').to_string();
    let folder_name = prefix
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(&repo.repo)
        .to_string();

    let buffer = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(buffer);
    let options: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
    let mut added = 0usize;

    for entry in &entries {
        let Some(path) = entry.get("path").and_then(|p| p.as_str()) else { continue };
        if entry.get("type").and_then(|t| t.as_str()) != Some("blob") {
            continue;
        }
        let matches_scope = if prefix.is_empty() {
            depth(path) <= MAX_DEPTH
        } else {
            path.starts_with(&format!("{prefix}/"))
        };
        if !matches_scope || depth(path) > MAX_DEPTH + 1 {
            continue;
        }

        let url = format!(
            "https://raw.githubusercontent.com/{}/{}/{}/{}",
            repo.owner, repo.repo, reference, path
        );
        let response = github_client_request(&client, access, &url)
            .send()
            .await
            .map_err(|_| ApiError::ServiceUnavailable)?;
        if !response.status().is_success() {
            continue;
        }
        let bytes = response.bytes().await.map_err(|_| ApiError::ServiceUnavailable)?;

        // Package entries are rooted at the skill folder, as the installer
        // expects a single top-level directory.
        let relative = if prefix.is_empty() {
            format!("{folder_name}/{path}")
        } else {
            format!("{folder_name}/{}", path.trim_start_matches(&format!("{prefix}/")))
        };
        zip.start_file(relative, options)
            .map_err(|e| ApiError::Internal(anyhow::anyhow!("failed to build package: {e}")))?;
        zip.write_all(&bytes)
            .map_err(|e| ApiError::Internal(anyhow::anyhow!("failed to build package: {e}")))?;
        added += 1;
    }

    if added == 0 {
        return Err(ApiError::NotFound);
    }
    let cursor = zip
        .finish()
        .map_err(|e| ApiError::Internal(anyhow::anyhow!("failed to finalize package: {e}")))?;
    Ok(cursor.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_repository_forms() {
        let r = RepoRef::parse("octocat/hello-world").unwrap();
        assert_eq!((r.owner.as_str(), r.repo.as_str()), ("octocat", "hello-world"));
        assert!(r.git_ref.is_none());

        let r = RepoRef::parse("https://github.com/octocat/hello-world.git").unwrap();
        assert_eq!(r.repo, "hello-world");

        let r = RepoRef::parse("https://github.com/owner/repo/tree/dev/skills/foo").unwrap();
        assert_eq!(r.git_ref.as_deref(), Some("dev"));
        assert_eq!(r.path.as_deref(), Some("skills/foo"));

        assert!(RepoRef::parse("").is_err());
        assert!(RepoRef::parse("justoneword").is_err());
        assert!(RepoRef::parse("owner/repo with space").is_err());
        // Traversal and scheme injection are rejected.
        assert!(RepoRef::parse("../../etc/passwd").is_err());
    }

    #[test]
    fn extracts_skill_manifest_fields() {
        let manifest = json!({
            "id": "pdf-tools",
            "name": "PDF Tools",
            "version": "1.2.0",
            "description": "Work with PDFs"
        });
        let skill = skill_from_manifest("skills/pdf-tools/skill.json", &manifest).unwrap();
        assert_eq!(skill.id, "pdf-tools");
        assert_eq!(skill.name, "PDF Tools");
        assert_eq!(skill.version, "1.2.0");
        assert_eq!(skill.path, "skills/pdf-tools");

        // A manifest without id/version is not a skill.
        assert!(skill_from_manifest("x/skill.json", &json!({ "name": "x" })).is_none());
    }

    #[test]
    fn reads_mcp_servers_and_flags_transport() {
        let value = json!({
            "mcpServers": {
                "remote": { "url": "https://example.com/mcp" },
                "local": { "command": "npx", "args": ["-y", "server"] }
            }
        });
        let servers = mcp_servers_from(".mcp.json", &value);
        assert_eq!(servers.len(), 2);
        let remote = servers.iter().find(|s| s.name == "remote").unwrap();
        assert_eq!(remote.transport, "http");
        assert_eq!(remote.url.as_deref(), Some("https://example.com/mcp"));
        // stdio servers cannot be launched here; the UI must say so.
        let local = servers.iter().find(|s| s.name == "local").unwrap();
        assert_eq!(local.transport, "stdio");
        assert!(local.url.is_none());
    }

    #[test]
    fn depth_counts_nested_levels() {
        assert_eq!(depth("skill.json"), 0);
        assert_eq!(depth("tools/read.json"), 1);
        assert_eq!(depth("a/b/c/d.json"), 3);
    }
}
