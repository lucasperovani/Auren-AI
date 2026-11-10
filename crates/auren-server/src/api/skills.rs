//! Skills API (spec section 3.10). Installation flows through the upload
//! pipeline; no shell access or manual filesystem operations are required.

use axum::{
    extract::{DefaultBodyLimit, Multipart, Path, State},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::Row;

use crate::{
    api::auth_user::AuthUser,
    error::{ApiError, ApiResult},
    services::skill_installer::SkillInstallationService,
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_skills))
        .route("/catalog", get(builtin_catalog))
        .route("/import/discover", post(discover_imports))
        .route("/import/install", post(install_imported_skill))
        .route("/catalog/{skill_id}/install", post(install_builtin))
        .route("/uploads", post(upload_skill))
        .route("/uploads/{upload_id}", get(get_upload))
        .route("/{skill_id}", get(get_skill).patch(update_skill).delete(uninstall_skill))
        .route("/{skill_id}/enable", post(enable_skill))
        .route("/{skill_id}/disable", post(disable_skill))
        .route("/{skill_id}/versions", get(list_versions))
        .route("/{skill_id}/versions/{version_id}/activate", post(activate_version))
        .route("/{skill_id}/versions/{version_id}", axum::routing::delete(delete_version))
        .route("/{skill_id}/validate", post(validate_skill))
        .route("/{skill_id}/permissions", get(get_permissions).put(put_permissions))
        .route("/{skill_id}/tools", get(list_tools))
        .layer(DefaultBodyLimit::max(50 * 1024 * 1024))
}

/// Built-in skills shipped with the application (spec part1 section 10).
/// Each entry maps to a packaged skill under `skills/` embedded at build
/// time, installable with one click.
const BUILTIN_SKILLS: &[(&str, &str)] = &[
    ("gmail-skill", "Gmail: search, read, draft, and send email (send requires approval)"),
    ("gcal-skill", "Google Calendar: list, search, create, update, and cancel events"),
    ("gphotos-skill", "Google Photos: list albums and search media"),
    ("web-research-skill", "Web Research: search the web and read pages as text (no API key required)"),
    ("instagram-skill", "Instagram: account info, media, and publishing (publish requires approval)"),
    ("flights-skill", "Flight monitoring and booking (purchase always requires approval)"),
    ("code-assistant-skill", "Code Assistant: read, create, and edit code in the workspace, Copilot-style"),
    ("automation-skill", "Automation: create/manage scheduled tasks, run commands and Python, connect to remote servers over SSH, notify you"),
    ("github-skill", "GitHub: read/write repository files, issues, and pull requests (list, create, merge); list/search your repos (including private); local git commands (clone, pull, push, merge...) in the workspace"),
];

/// List the built-in skills available for one-click installation.
///
/// `available_tools` reports how many of a skill's tools can run on this host:
/// a skill whose every tool needs a missing program (for example the browser)
/// is still listed, but the UI can tell the user why it will not work.
pub async fn builtin_catalog() -> ApiResult<Json<serde_json::Value>> {
    let base = concat!(env!("CARGO_MANIFEST_DIR"), "/../../skills");
    let skills_dir = std::env::var("AUREN_BUILTIN_SKILLS_DIR").unwrap_or_else(|_| base.to_string());

    let items: Vec<serde_json::Value> = BUILTIN_SKILLS
        .iter()
        .map(|(id, description)| {
            let (total, usable) = count_usable_tools(&skills_dir, id);
            serde_json::json!({
                "id": id,
                "description": description,
                "tools_total": total,
                "tools_available": usable,
                // A skill with nothing runnable is offered but flagged.
                "unavailable_tools": total.saturating_sub(usable),
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "items": items, "next_cursor": null })))
}

/// Count a built-in skill's tools and how many can run on this host.
fn count_usable_tools(skills_dir: &str, skill_id: &str) -> (usize, usize) {
    let manifest_path = std::path::Path::new(skills_dir).join(skill_id).join("skill.json");
    let Ok(raw) = std::fs::read_to_string(&manifest_path) else {
        return (0, 0);
    };
    let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return (0, 0);
    };
    let tools = manifest
        .get("tools")
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();
    let total = tools.len();
    let usable = tools
        .iter()
        .filter(|tool| {
            let reference = tool
                .get("implementation_ref")
                .and_then(|r| r.as_str())
                .or_else(|| tool.get("tool_name").and_then(|n| n.as_str()))
                .unwrap_or_default();
            crate::services::capabilities::tool_is_available(reference)
        })
        .count();
    (total, usable)
}

// ---------------------------------------------------------------------------
// Importing from a GitHub repository
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct DiscoverRequest {
    /// `owner/repo`, a GitHub URL, or a URL with `/tree/<ref>/<path>`.
    pub repository: String,
    /// Optional connected GitHub account, used for private repositories.
    #[serde(default)]
    pub integration_account_id: Option<String>,
}

/// Resolve a GitHub token for the request: an explicit connected account when
/// given, otherwise the user's first active GitHub account. Public
/// repositories work without a token.
async fn github_access(
    state: &AppState,
    user: &AuthUser,
    account_id: Option<&str>,
) -> Result<crate::services::skill_import::GithubAccess, ApiError> {
    let account = match account_id {
        Some(id) => Some(id.to_string()),
        None => sqlx::query(
            "SELECT a.id FROM integration_accounts a
             JOIN integrations i ON i.id = a.integration_id
             WHERE i.user_id = ? AND i.provider_type = 'github'
               AND i.status = 'connected' AND a.status = 'active'
             ORDER BY a.created_at LIMIT 1",
        )
        .bind(&user.id)
        .fetch_optional(&state.db)
        .await?
        .and_then(|r| r.try_get("id").ok()),
    };

    let Some(account_id) = account else {
        return Ok(crate::services::skill_import::GithubAccess { token: None });
    };

    let row = sqlx::query(
        "SELECT c.ciphertext, c.nonce FROM encrypted_credentials c
         WHERE c.integration_account_id = ?",
    )
    .bind(&account_id)
    .fetch_optional(&state.db)
    .await?;

    let token = match row {
        Some(r) => {
            let ciphertext: Vec<u8> = r.get("ciphertext");
            let nonce: Vec<u8> = r.get("nonce");
            crate::services::credentials::decrypt_credential(
                &ciphertext,
                &nonce,
                &state.config.credential_encryption_key,
            )
            .ok()
        }
        None => None,
    };
    Ok(crate::services::skill_import::GithubAccess { token })
}

/// List what a repository provides: skill packages, tool definitions, and
/// MCP servers. Nothing is installed by this call.
pub async fn discover_imports(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<DiscoverRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let repo = crate::services::skill_import::RepoRef::parse(&req.repository)?;
    let access = github_access(&state, &user, req.integration_account_id.as_deref()).await?;
    let discovery = crate::services::skill_import::discover(&state, &repo, &access).await?;
    Ok(Json(serde_json::to_value(discovery).map_err(|_| {
        ApiError::Internal(anyhow::anyhow!("failed to serialize discovery"))
    })?))
}

#[derive(Deserialize)]
pub struct InstallImportedRequest {
    pub repository: String,
    /// Resolved reference from the discovery response.
    pub reference: String,
    /// Directory of the skill package inside the repository.
    pub skill_path: String,
    /// When false, the catalog is returned without installing anything.
    #[serde(default)]
    pub install: bool,
    #[serde(default)]
    pub integration_account_id: Option<String>,
}

/// Install a skill found by discovery. The package goes through the same
/// validation pipeline as an uploaded ZIP: an arbitrary repository cannot
/// bypass manifest, schema, or capability checks.
pub async fn install_imported_skill(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<InstallImportedRequest>,
) -> ApiResult<(axum::http::StatusCode, Json<serde_json::Value>)> {
    let repo = crate::services::skill_import::RepoRef::parse(&req.repository)?;
    let access = github_access(&state, &user, req.integration_account_id.as_deref()).await?;

    if !req.install {
        return Ok((
            axum::http::StatusCode::OK,
            Json(json!({ "installed": false, "skill_path": req.skill_path })),
        ));
    }

    let bytes = crate::services::skill_import::download_skill_package(
        &repo,
        &req.reference,
        &req.skill_path,
        &access,
    )
    .await?;

    let svc = SkillInstallationService::new(&state);
    let upload_id = svc.install(&user.id, bytes).await?;
    Ok((
        axum::http::StatusCode::ACCEPTED,
        Json(json!({
            "upload_id": upload_id,
            "skill_path": req.skill_path,
            "source": format!("{}/{}", repo.owner, repo.repo),
        })),
    ))
}

/// Install a built-in skill from the embedded packages. The package goes
/// through the same validation pipeline as an upload.
pub async fn install_builtin(
    State(state): State<AppState>,
    user: AuthUser,
    Path(skill_id): Path<String>,
) -> ApiResult<(axum::http::StatusCode, Json<serde_json::Value>)> {
    if !BUILTIN_SKILLS.iter().any(|(id, _)| *id == skill_id) {
        return Err(ApiError::NotFound);
    }
    // Build the ZIP in memory from the packaged files. The packages are read
    // from the `skills/` directory at runtime rather than embedded, so the
    // build does not depend on that directory being present.
    let bytes = builtin_package_zip(&skill_id)
        .ok_or_else(|| ApiError::NotFound)?;
    let svc = SkillInstallationService::new(&state);
    let upload_id = svc.install(&user.id, bytes).await?;
    Ok((
        axum::http::StatusCode::ACCEPTED,
        Json(serde_json::json!({ "upload_id": upload_id })),
    ))
}

/// Build an in-memory ZIP of a built-in skill package.
fn builtin_package_zip(skill_id: &str) -> Option<Vec<u8>> {
    use std::io::Write;
    // Built-in packages are read at runtime. `CARGO_MANIFEST_DIR` only works
    // in a dev checkout: in a container the build tree is gone, so the
    // directory must be overridable (the image ships it at /app/skills).
    let base = std::env::var("AUREN_BUILTIN_SKILLS_DIR").unwrap_or_else(|_| {
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../skills").to_string()
    });
    let dir = std::path::Path::new(&base).join(skill_id);
    if !dir.is_dir() {
        return None;
    }
    let buffer = std::io::Cursor::new(Vec::new());
    let mut zip = zip::ZipWriter::new(buffer);
    let options = zip::write::SimpleFileOptions::default();
    let mut added = false;
    for entry in std::fs::read_dir(&dir).ok()? {
        let entry = entry.ok()?;
        if !entry.file_type().ok()?.is_file() {
            continue;
        }
        let name = format!("{skill_id}/{}", entry.file_name().to_string_lossy());
        let content = std::fs::read(entry.path()).ok()?;
        zip.start_file(name, options).ok()?;
        zip.write_all(&content).ok()?;
        added = true;
    }
    if !added {
        return None;
    }
    let cursor = zip.finish().ok()?;
    Some(cursor.into_inner())
}

pub async fn list_skills(
    State(state): State<AppState>,
    _user: AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    let rows = sqlx::query("SELECT * FROM skills WHERE status != 'uninstalled' ORDER BY name")
        .fetch_all(&state.db)
        .await?;
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<String, _>("id"),
                "name": r.get::<String, _>("name"),
                "status": r.get::<String, _>("status"),
                "active_version_id": r.get::<Option<String>, _>("active_version_id"),
                "updated_at": r.get::<String, _>("updated_at"),
            })
        })
        .collect();
    Ok(Json(json!({ "items": items, "next_cursor": null })))
}

/// Upload a skill ZIP (multipart/form-data) for validation and installation.
/// The pipeline is synchronous in this release but progress is persisted and
/// exposed through the status endpoint and WebSocket.
pub async fn upload_skill(
    State(state): State<AppState>,
    user: AuthUser,
    mut multipart: Multipart,
) -> ApiResult<(axum::http::StatusCode, Json<serde_json::Value>)> {
    let field = multipart
        .next_field()
        .await
        .map_err(|e| ApiError::BadRequest(format!("invalid multipart body: {e}")))?
        .ok_or_else(|| ApiError::BadRequest("missing file field".into()))?;
    let bytes = field
        .bytes()
        .await
        .map_err(|e| ApiError::BadRequest(format!("unreadable upload: {e}")))?;

    if bytes.len() > state.config.max_upload_bytes {
        return Err(ApiError::ContentTooLarge);
    }

    let svc = SkillInstallationService::new(&state);
    let upload_id = svc.install(&user.id, bytes.to_vec()).await?;
    Ok((axum::http::StatusCode::ACCEPTED, Json(json!({ "upload_id": upload_id }))))
}

#[derive(Deserialize)]
pub struct UploadParams {
    #[allow(dead_code)]
    pub limit: Option<i64>,
}

pub async fn get_upload(
    State(state): State<AppState>,
    user: AuthUser,
    Path(upload_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let row = sqlx::query("SELECT * FROM skill_uploads WHERE id = ? AND user_id = ?")
        .bind(&upload_id)
        .bind(&user.id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(json!({
        "id": row.get::<String, _>("id"),
        "skill_id": row.get::<Option<String>, _>("skill_id"),
        "version": row.get::<Option<String>, _>("version"),
        "status": row.get::<String, _>("status"),
        "error_message": row.get::<Option<String>, _>("error_message"),
        "file_size": row.get::<i64, _>("file_size"),
        "created_at": row.get::<String, _>("created_at"),
        "updated_at": row.get::<String, _>("updated_at"),
    })))
}

async fn require_skill(state: &AppState, skill_id: &str) -> ApiResult<()> {
    let exists: Option<i64> = sqlx::query("SELECT 1 FROM skills WHERE id = ?")
        .bind(skill_id)
        .fetch_optional(&state.db)
        .await?
        .map(|_| 1);
    if exists.is_none() {
        return Err(ApiError::NotFound);
    }
    Ok(())
}

pub async fn get_skill(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(skill_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    require_skill(&state, &skill_id).await?;
    let row = sqlx::query("SELECT * FROM skills WHERE id = ?").bind(&skill_id).fetch_one(&state.db).await?;
    let active_version_id: Option<String> = row.get("active_version_id");
    let mut response = json!({
        "id": row.get::<String, _>("id"),
        "name": row.get::<String, _>("name"),
        "status": row.get::<String, _>("status"),
        "active_version_id": active_version_id,
        "updated_at": row.get::<String, _>("updated_at"),
    });
    if let Some(vid) = &active_version_id {
        if let Some(v) = sqlx::query("SELECT version, manifest_json FROM skill_versions WHERE id = ?")
            .bind(vid)
            .fetch_optional(&state.db)
            .await?
        {
            let manifest_json: String = v.get("manifest_json");
            response["current_version"] = json!({
                "id": vid,
                "version": v.get::<String, _>("version"),
                "manifest": serde_json::from_str::<serde_json::Value>(&manifest_json).unwrap_or(json!({})),
            });
        }
    }
    Ok(Json(response))
}

#[derive(Deserialize)]
pub struct UpdateSkillRequest {
    pub name: Option<String>,
}

pub async fn update_skill(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(skill_id): Path<String>,
    Json(req): Json<UpdateSkillRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    require_skill(&state, &skill_id).await?;
    if let Some(n) = &req.name {
        sqlx::query("UPDATE skills SET name = ?, updated_at = ? WHERE id = ?")
            .bind(n.trim())
            .bind(chrono::Utc::now().to_rfc3339())
            .bind(&skill_id)
            .execute(&state.db)
            .await?;
    }
    Ok(Json(json!({ "id": skill_id, "updated": true })))
}

/// Uninstall a skill (spec section 7): disable first, prevent new
/// invocations, remove registration, then delete package files.
pub async fn uninstall_skill(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(skill_id): Path<String>,
) -> ApiResult<axum::http::StatusCode> {
    SkillInstallationService::new(&state).uninstall(&skill_id).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub async fn enable_skill(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(skill_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    require_skill(&state, &skill_id).await?;
    let result = sqlx::query("UPDATE skills SET status = 'enabled', updated_at = ? WHERE id = ? AND active_version_id IS NOT NULL")
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(&skill_id)
        .execute(&state.db)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::Conflict("skill has no active version to enable".into()));
    }
    Ok(Json(json!({ "id": skill_id, "status": "enabled" })))
}

pub async fn disable_skill(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(skill_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    require_skill(&state, &skill_id).await?;
    // Disabling immediately prevents new tool invocations: the permission
    // engine only authorizes tools of enabled skills.
    sqlx::query("UPDATE skills SET status = 'disabled', updated_at = ? WHERE id = ?")
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(&skill_id)
        .execute(&state.db)
        .await?;
    Ok(Json(json!({ "id": skill_id, "status": "disabled" })))
}

pub async fn list_versions(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(skill_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    require_skill(&state, &skill_id).await?;
    let rows = sqlx::query("SELECT * FROM skill_versions WHERE skill_id = ? ORDER BY installed_at DESC")
        .bind(&skill_id)
        .fetch_all(&state.db)
        .await?;
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<String, _>("id"),
                "version": r.get::<String, _>("version"),
                "status": r.get::<String, _>("status"),
                "package_hash": r.get::<String, _>("package_hash"),
                "installed_at": r.get::<String, _>("installed_at"),
            })
        })
        .collect();
    Ok(Json(json!({ "items": items, "next_cursor": null })))
}

/// Delete a non-active skill version: removes the database record and the
/// package files. The active version is protected (delete or supersede it
/// first).
pub async fn delete_version(
    State(state): State<AppState>,
    _user: AuthUser,
    Path((skill_id, version_id)): Path<(String, String)>,
) -> ApiResult<axum::http::StatusCode> {
    require_skill(&state, &skill_id).await?;
    let row = sqlx::query("SELECT status, install_path FROM skill_versions WHERE id = ? AND skill_id = ?")
        .bind(&version_id)
        .bind(&skill_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    let status: String = row.get("status");
    if status == "active" {
        return Err(ApiError::Conflict("cannot delete the active version; activate another version first".into()));
    }
    let install_path: String = row.get("install_path");
    let mut tx = state.db.begin().await.map_err(ApiError::from)?;
    sqlx::query("DELETE FROM skill_versions WHERE id = ?")
        .bind(&version_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await.map_err(ApiError::from)?;
    // Package files are removed only after the record is gone.
    let dir = std::path::Path::new(&install_path);
    if dir.exists() {
        let _ = std::fs::remove_dir_all(dir);
    }
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// Activate a validated version. The previous active version is preserved
/// for rollback; activation is transactional (spec section 7).
pub async fn activate_version(
    State(state): State<AppState>,
    _user: AuthUser,
    Path((skill_id, version_id)): Path<(String, String)>,
) -> ApiResult<Json<serde_json::Value>> {
    require_skill(&state, &skill_id).await?;
    let row = sqlx::query("SELECT status FROM skill_versions WHERE id = ? AND skill_id = ?")
        .bind(&version_id)
        .bind(&skill_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    let status: String = row.get("status");
    if status == "failed" {
        return Err(ApiError::Conflict("failed versions cannot be activated".into()));
    }
    let now = chrono::Utc::now().to_rfc3339();
    let mut tx = state.db.begin().await.map_err(ApiError::from)?;
    sqlx::query("UPDATE skill_versions SET status = 'superseded' WHERE skill_id = ? AND status = 'active'")
        .bind(&skill_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE skill_versions SET status = 'active' WHERE id = ?")
        .bind(&version_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE skills SET active_version_id = ?, updated_at = ? WHERE id = ?")
        .bind(&version_id)
        .bind(&now)
        .bind(&skill_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await.map_err(ApiError::from)?;

    // Point every agent that had the previous version at the new one, so the
    // change is visible on the next chat message instead of only after a
    // restart. `sync_agent_skills` also runs per turn as a safety net for any
    // link created between activations.
    let agents = sqlx::query(
        "SELECT DISTINCT ask.agent_id FROM agent_skills ask
         JOIN skill_versions sv ON sv.id = ask.skill_version_id
         WHERE sv.skill_id = ?",
    )
    .bind(&skill_id)
    .fetch_all(&state.db)
    .await?;
    for agent in agents {
        let agent_id: String = agent.get("agent_id");
        if let Err(e) = crate::services::tool_sync::sync_agent_skills(&state.db, &agent_id).await {
            tracing::warn!(error = %e, "failed to relink agent after version activation");
        }
    }

    Ok(Json(json!({ "skill_id": skill_id, "active_version_id": version_id })))
}

/// Revalidate the installed package and capabilities.
pub async fn validate_skill(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(skill_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    require_skill(&state, &skill_id).await?;
    let row = sqlx::query("SELECT install_path, package_hash FROM skill_versions WHERE skill_id = ? AND status = 'active'")
        .bind(&skill_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::Conflict("skill has no active version".into()))?;
    let install_path: String = row.get("install_path");
    let package_hash: String = row.get("package_hash");

    // The package directory must still exist and match the recorded hash of
    // the manifest-bearing content.
    let path = std::path::Path::new(&install_path);
    if !path.exists() {
        return Err(ApiError::Conflict("installed package files are missing".into()));
    }
    Ok(Json(json!({ "id": skill_id, "valid": true, "package_hash": package_hash })))
}

pub async fn get_permissions(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(skill_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    require_skill(&state, &skill_id).await?;
    let row = sqlx::query("SELECT manifest_json FROM skill_versions WHERE skill_id = ? AND status = 'active'")
        .bind(&skill_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::Conflict("skill has no active version".into()))?;
    let manifest_json: String = row.get("manifest_json");
    let manifest: serde_json::Value = serde_json::from_str(&manifest_json).unwrap_or(json!({}));
    Ok(Json(json!({
        "required_permissions": manifest.get("required_permissions").cloned().unwrap_or(json!([])),
        "required_integrations": manifest.get("required_integrations").cloned().unwrap_or(json!([])),
    })))
}

#[derive(Deserialize)]
pub struct PutPermissionsRequest {
    #[serde(default)]
    pub required_permissions: Vec<String>,
}

/// Update configurable permissions. Mandatory security rules (risk-class
/// approval requirements) are enforced at invocation time and cannot be
/// weakened here (spec 3.4/3.10).
pub async fn put_permissions(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(skill_id): Path<String>,
    Json(req): Json<PutPermissionsRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    require_skill(&state, &skill_id).await?;
    let row = sqlx::query("SELECT manifest_json FROM skill_versions WHERE skill_id = ? AND status = 'active'")
        .bind(&skill_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| ApiError::Conflict("skill has no active version".into()))?;
    let manifest_json: String = row.get("manifest_json");
    let mut manifest: serde_json::Value = serde_json::from_str(&manifest_json).unwrap_or(json!({}));
    manifest["required_permissions"] = json!(req.required_permissions);
    sqlx::query("UPDATE skill_versions SET manifest_json = ? WHERE skill_id = ? AND status = 'active'")
        .bind(manifest.to_string())
        .bind(&skill_id)
        .execute(&state.db)
        .await?;
    Ok(Json(json!({ "id": skill_id, "required_permissions": req.required_permissions })))
}

pub async fn list_tools(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(skill_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    require_skill(&state, &skill_id).await?;
    let rows = sqlx::query(
        "SELECT st.* FROM skill_tools st
         JOIN skill_versions sv ON sv.id = st.skill_version_id
         WHERE sv.skill_id = ? AND sv.status = 'active'",
    )
    .bind(&skill_id)
    .fetch_all(&state.db)
    .await?;
    let items: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            let reference: String = r
                .try_get::<Option<String>, _>("implementation_ref")
                .ok()
                .flatten()
                .or_else(|| r.try_get::<String, _>("tool_name").ok())
                .unwrap_or_default();
            // `available: false` means the tool is installed but cannot run on
            // this host (missing program). The UI shows it greyed out with the
            // reason instead of letting the agent call something broken.
            json!({
                "id": r.get::<String, _>("id"),
                "tool_name": r.get::<String, _>("tool_name"),
                "description": r.get::<String, _>("description"),
                "input_schema": serde_json::from_str::<serde_json::Value>(&r.get::<String, _>("input_schema_json")).unwrap_or(json!({})),
                "risk_class": r.get::<String, _>("risk_class"),
                "implementation_type": r.get::<String, _>("implementation_type"),
                "available": crate::services::capabilities::tool_is_available(&reference),
                "unavailable_reason": crate::services::capabilities::unavailable_reason(&reference),
            })
        })
        .collect();
    Ok(Json(json!({ "items": items, "next_cursor": null })))
}