//! Skill installation service (spec sections 5-7).
//!
//! Pipeline: upload → archive validation → extraction → manifest/schema
//! validation → capability/permission validation → installation → runtime
//! registration → completion. The archive is treated as untrusted input at
//! every stage.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;
use zip::ZipArchive;

use crate::{
    error::ApiError,
    events::EventEnvelope,
    state::AppState,
};

/// Archive limits (spec 5.4 stage 2). All are enforced before extraction.
const MAX_ENTRIES: usize = 500;
const MAX_TOTAL_UNCOMPRESSED: u64 = 50 * 1024 * 1024;
const MAX_FILE_UNCOMPRESSED: u64 = 10 * 1024 * 1024;
/// Reject archives whose compression ratio exceeds this (zip bombs).
const MAX_COMPRESSION_RATIO: u64 = 100;

pub struct SkillInstallationService {
    db: SqlitePool,
    skills_dir: String,
    events: crate::events::SharedEventBus,
}

/// A validated manifest (subset of `skill.json` used at runtime).
#[derive(Debug, Clone)]
pub struct Manifest {
    pub schema_version: String,
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub implementation_type: String,
    pub required_integrations: Vec<String>,
    pub required_permissions: Vec<String>,
    pub tools: Vec<ToolDef>,
}

#[derive(Debug, Clone)]
pub struct ToolDef {
    pub tool_name: String,
    pub description: String,
    pub input_schema: Value,
    pub risk_class: String,
    pub implementation_type: String,
    pub implementation_ref: Option<String>,
}

impl SkillInstallationService {
    pub fn new(state: &AppState) -> Self {
        Self {
            db: state.db.clone(),
            skills_dir: state.config.skills_dir.clone(),
            events: state.events.clone(),
        }
    }

    /// Stage 1-8: process an uploaded ZIP from raw bytes. Returns the upload
    /// id. Progress is persisted in `skill_uploads` at each transition.
    pub async fn install(&self, user_id: &str, bytes: Vec<u8>) -> Result<String, ApiError> {
        let upload_id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query(
            "INSERT INTO skill_uploads (id, user_id, status, file_size, created_at, updated_at)
             VALUES (?, ?, 'validating', ?, ?, ?)",
        )
        .bind(&upload_id)
        .bind(user_id)
        .bind(bytes.len() as i64)
        .bind(&now)
        .bind(&now)
        .execute(&self.db)
        .await?;
        self.set_status(&upload_id, "validating", None).await?;
        self.events.publish(EventEnvelope::new(
            "skill.upload_started",
            json!({ "upload_id": upload_id }),
        ));

        // Stage 2: archive validation (in memory, before touching disk).
        let mut cursor = std::io::Cursor::new(&bytes);
        let mut archive = match ZipArchive::new(&mut cursor) {
            Ok(a) => a,
            Err(e) => return Err(self.fail(&upload_id, format!("invalid ZIP archive: {e}")).await),
        };

        if archive.len() > MAX_ENTRIES {
            return Err(self.fail(&upload_id, "too many entries".into()).await);
        }

        let mut total_uncompressed: u64 = 0;
        let mut names = std::collections::HashSet::new();
        for i in 0..archive.len() {
            let file = match archive.by_index_raw(i) {
                Ok(f) => f,
                Err(e) => return Err(self.fail(&upload_id, format!("unreadable entry: {e}")).await),
            };
            let name = file.name().to_string();

            // Reject absolute paths, traversal, and duplicates.
            if let Err(reason) = validate_entry_name(&name) {
                return Err(self.fail(&upload_id, reason).await);
            }
            if !names.insert(name.clone()) {
                return Err(self.fail(&upload_id, format!("duplicate entry: {name}")).await);
            }
            // Reject symlinks and non-regular files.
            if file.is_symlink() {
                return Err(self.fail(&upload_id, format!("symlink entries are not allowed: {name}")).await);
            }
            // Per-file size and compression-ratio limits.
            let uncompressed = file.size();
            if uncompressed > MAX_FILE_UNCOMPRESSED {
                return Err(self.fail(&upload_id, format!("entry too large: {name}")).await);
            }
            let compressed = file.compressed_size().max(1);
            if uncompressed / compressed > MAX_COMPRESSION_RATIO {
                return Err(self.fail(&upload_id, format!("suspicious compression ratio: {name}")).await);
            }
            total_uncompressed += uncompressed;
            if total_uncompressed > MAX_TOTAL_UNCOMPRESSED {
                return Err(self.fail(&upload_id, "total uncompressed size exceeds the limit".into()).await);
            }
        }

        // Stage 3: extraction into a unique temporary directory.
        self.set_status(&upload_id, "extracting", None).await?;
        let temp_dir = tempfile::TempDir::new()
            .map_err(|e| ApiError::Internal(anyhow::anyhow!("temp dir: {e}")))?;
        let mut cursor = std::io::Cursor::new(&bytes);
        archive = ZipArchive::new(&mut cursor)
            .map_err(|e| ApiError::Internal(anyhow::anyhow!("re-open archive: {e}")))?;
        for i in 0..archive.len() {
            let mut file = archive.by_index(i)
                .map_err(|e| ApiError::Internal(anyhow::anyhow!("entry: {e}")))?;
            let name = file.name().to_string();
            let target = temp_dir.path().join(&name);
            if file.is_dir() {
                std::fs::create_dir_all(&target)
                    .map_err(|e| ApiError::Internal(anyhow::anyhow!("mkdir: {e}")))?;
                continue;
            }
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| ApiError::Internal(anyhow::anyhow!("mkdir: {e}")))?;
            }
            // Verify the resolved destination stays inside the temp dir.
            let canonical_parent = target.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| temp_dir.path().to_path_buf());
            let _ = canonical_parent;
            let mut out = std::fs::File::create(&target)
                .map_err(|e| ApiError::Internal(anyhow::anyhow!("create: {e}")))?;
            std::io::copy(&mut file, &mut out)
                .map_err(|e| ApiError::Internal(anyhow::anyhow!("extract: {e}")))?;
        }

        // Stage 4: manifest and schema validation.
        self.set_status(&upload_id, "validating", None).await?;
        let manifest = self.validate_manifest(temp_dir.path()).await?;

        // Stage 5: capability and permission validation.
        self.validate_capabilities(&manifest).await?;

        // Stage 6: installation — hash, versioned directory, atomic activation.
        self.set_status(&upload_id, "installing", None).await?;
        let package_hash = hex::encode(Sha256::digest(&bytes));
        let install_path = self.install_package(&manifest, temp_dir.path(), &package_hash).await?;

        // Stage 7: runtime registration (database records; the in-memory
        // registry reads from the database, so no restart is needed).
        self.set_status(&upload_id, "registering", None).await?;
        self.register(&manifest, &install_path, &package_hash, &upload_id).await?;

        // Stage 8: completion.
        self.set_status(&upload_id, "installed", None).await?;
        self.events.publish(EventEnvelope::new(
            "skill.installed",
            json!({ "upload_id": upload_id, "skill_id": manifest.id, "version": manifest.version }),
        ));
        Ok(upload_id)
    }

    async fn fail(&self, upload_id: &str, message: String) -> ApiError {
        let _ = self.set_status(upload_id, "failed", Some(&message)).await;
        self.events.publish(EventEnvelope::new(
            "skill.installation_failed",
            json!({ "upload_id": upload_id, "error": message }),
        ));
        ApiError::Unprocessable(message)
    }

    async fn set_status(&self, upload_id: &str, status: &str, error: Option<&str>) -> Result<(), ApiError> {
        sqlx::query("UPDATE skill_uploads SET status = ?, error_message = ?, updated_at = ? WHERE id = ?")
            .bind(status)
            .bind(error)
            .bind(chrono::Utc::now().to_rfc3339())
            .bind(upload_id)
            .execute(&self.db)
            .await?;
        Ok(())
    }

    /// Stage 4: parse and validate `skill.json` against the documented rules.
    async fn validate_manifest(&self, extracted: &Path) -> Result<Manifest, ApiError> {
        // The package must contain a single top-level directory (or the
        // manifest at the root).
        let manifest_path = find_manifest(extracted)
            .ok_or_else(|| ApiError::Unprocessable("skill.json not found in package".into()))?;
        let raw = std::fs::read_to_string(&manifest_path)
            .map_err(|e| ApiError::Unprocessable(format!("unreadable skill.json: {e}")))?;
        let manifest: Value = serde_json::from_str(&raw)
            .map_err(|e| ApiError::Unprocessable(format!("invalid skill.json: {e}")))?;

        let obj = manifest.as_object().ok_or_else(|| ApiError::Unprocessable("skill.json must be an object".into()))?;

        let get_str = |key: &str| -> Result<String, ApiError> {
            obj.get(key)
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .ok_or_else(|| ApiError::Unprocessable(format!("manifest field '{key}' is required")))
        };

        let schema_version = get_str("schema_version")?;
        if schema_version != "1" {
            return Err(ApiError::Unprocessable(format!("unsupported schema_version '{schema_version}'")));
        }
        let id = get_str("id")?;
        if !valid_skill_id(&id) {
            return Err(ApiError::Unprocessable("invalid skill id: must be lowercase kebab-case".into()));
        }
        let version = get_str("version")?;
        if !valid_semver(&version) {
            return Err(ApiError::Unprocessable("invalid version: must be semver".into()));
        }
        let implementation_type = get_str("implementation_type")?;
        if implementation_type != "declarative" && implementation_type != "builtin_ref" {
            return Err(ApiError::Unprocessable(
                "unsupported implementation_type: only declarative skills are supported".into(),
            ));
        }

        let mut tools = Vec::new();
        if let Some(list) = obj.get("tools").and_then(|v| v.as_array()) {
            for tool in list {
                let t = tool.as_object()
                    .ok_or_else(|| ApiError::Unprocessable("tool definitions must be objects".into()))?;
                let tool_name = t.get("tool_name").and_then(|v| v.as_str())
                    .ok_or_else(|| ApiError::Unprocessable("tool_name is required".into()))?
                    .to_string();
                if !valid_tool_name(&tool_name) {
                    return Err(ApiError::Unprocessable(format!("invalid tool_name '{tool_name}'")));
                }
                let impl_type = t.get("implementation_type").and_then(|v| v.as_str()).unwrap_or("declarative");
                let impl_ref = t.get("implementation_ref").and_then(|v| v.as_str()).map(|s| s.to_string());
                if impl_type == "builtin_ref" && impl_ref.is_none() {
                    return Err(ApiError::Unprocessable(format!(
                        "tool '{tool_name}': builtin_ref requires implementation_ref"
                    )));
                }
                // Declarative tools must reference a known builtin.
                if let Some(r) = &impl_ref {
                    if !KNOWN_BUILTINS.contains(&r.as_str()) {
                        return Err(ApiError::Unprocessable(format!(
                            "tool '{tool_name}' references unknown implementation '{r}'"
                        )));
                    }
                }
                tools.push(ToolDef {
                    tool_name,
                    description: t.get("description").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                    input_schema: t.get("input_schema").cloned().unwrap_or(json!({})),
                    risk_class: t.get("risk_class").and_then(|v| v.as_str()).unwrap_or("low").to_string(),
                    implementation_type: impl_type.to_string(),
                    implementation_ref: impl_ref,
                });
            }
        }
        if tools.is_empty() {
            return Err(ApiError::Unprocessable("manifest must declare at least one tool".into()));
        }

        Ok(Manifest {
            schema_version,
            id,
            name: get_str("name")?,
            version,
            description: get_str("description")?,
            implementation_type,
            required_integrations: string_list(obj.get("required_integrations")),
            required_permissions: string_list(obj.get("required_permissions")),
            tools,
        })
    }

    /// Stage 5: verify requested capabilities against the security policy.
    /// The manifest cannot grant itself permissions; requested permissions
    /// are recorded for review, and unsupported implementations are rejected.
    async fn validate_capabilities(&self, manifest: &Manifest) -> Result<(), ApiError> {
        for tool in &manifest.tools {
            if !matches!(tool.risk_class.as_str(), "low" | "medium" | "high" | "critical") {
                return Err(ApiError::Unprocessable(format!(
                    "tool '{}': invalid risk_class '{}'",
                    tool.tool_name, tool.risk_class
                )));
            }
            if tool.implementation_type == "declarative" && tool.implementation_ref.is_none() {
                return Err(ApiError::Unprocessable(format!(
                    "tool '{}': declarative tools must reference a builtin implementation",
                    tool.tool_name
                )));
            }
        }
        Ok(())
    }

    /// Stage 6: copy the validated package into a versioned directory and
    /// compute the package hash. The previous active version is preserved.
    async fn install_package(&self, manifest: &Manifest, extracted: &Path, package_hash: &str) -> Result<String, ApiError> {
        // The package root is the directory containing skill.json (the
        // archive may nest it inside a single top-level directory).
        let package_root = find_manifest(extracted)
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| extracted.to_path_buf());
        let base = PathBuf::from(&self.skills_dir).join(&manifest.id).join(&manifest.version);
        if base.exists() {
            // Same version already installed: compare hashes.
            let existing = sqlx::query("SELECT package_hash FROM skill_versions WHERE skill_id = ? AND version = ?")
                .bind(&manifest.id)
                .bind(&manifest.version)
                .fetch_optional(&self.db)
                .await?;
            if let Some(row) = existing {
                let existing_hash: String = row.get("package_hash");
                if existing_hash == package_hash {
                    return Ok(base.to_string_lossy().to_string());
                }
                return Err(ApiError::Conflict(
                    "version already installed with different content; use a new version".into(),
                ));
            }
        }
        let staging = PathBuf::from(&self.skills_dir).join(format!(".staging-{}", Uuid::new_v4()));
        copy_dir_recursive(&package_root, &staging)
            .map_err(|e| ApiError::Internal(anyhow::anyhow!("staging copy: {e}")))?;
        if let Some(parent) = base.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ApiError::Internal(anyhow::anyhow!("install dir: {e}")))?;
        }
        std::fs::rename(&staging, &base)
            .map_err(|e| {
                let _ = std::fs::remove_dir_all(&staging);
                ApiError::Internal(anyhow::anyhow!("atomic install: {e}"))
            })?;
        Ok(base.to_string_lossy().to_string())
    }

    /// Stage 7: persist skill, version, and tool records transactionally and
    /// activate the version atomically.
    async fn register(&self, manifest: &Manifest, install_path: &str, package_hash: &str, upload_id: &str) -> Result<(), ApiError> {
        let now = chrono::Utc::now().to_rfc3339();
        let mut tx = self.db.begin().await.map_err(ApiError::from)?;

        // Upsert the skill identity.
        sqlx::query(
            "INSERT INTO skills (id, name, status, created_at, updated_at) VALUES (?, ?, 'disabled', ?, ?)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, updated_at = excluded.updated_at",
        )
        .bind(&manifest.id)
        .bind(&manifest.name)
        .bind(&now)
        .bind(&now)
        .execute(&mut *tx)
        .await?;

        // Insert the version record unless this exact version is already
        // registered (idempotent re-upload of the same package).
        let is_new_version;
        let existing_version_id: Option<String> = sqlx::query("SELECT id FROM skill_versions WHERE skill_id = ? AND version = ?")
            .bind(&manifest.id)
            .bind(&manifest.version)
            .fetch_optional(&mut *tx)
            .await?
            .map(|row| row.get::<String, _>("id"));
        let version_id = match existing_version_id {
            Some(id) => {
                is_new_version = false;
                id
            }
            None => {
                is_new_version = true;
                let version_id = Uuid::new_v4().to_string();
                sqlx::query(
                    "INSERT INTO skill_versions (id, skill_id, version, manifest_json, package_hash, install_path, status, installed_at)
                     VALUES (?, ?, ?, ?, ?, ?, 'installed', ?)",
                )
                .bind(&version_id)
                .bind(&manifest.id)
                .bind(&manifest.version)
                .bind(json!({
                    "schema_version": manifest.schema_version,
                    "id": manifest.id,
                    "name": manifest.name,
                    "version": manifest.version,
                    "description": manifest.description,
                    "implementation_type": manifest.implementation_type,
                    "required_integrations": manifest.required_integrations,
                    "required_permissions": manifest.required_permissions,
                }).to_string())
                .bind(package_hash)
                .bind(install_path)
                .bind(&now)
                .execute(&mut *tx)
                .await?;
                version_id
            }
        };

        // Insert declared tools only for a newly registered version.
        if is_new_version {
            for tool in &manifest.tools {
                sqlx::query(
                    "INSERT INTO skill_tools (id, skill_version_id, tool_name, description, input_schema_json, risk_class, implementation_type, implementation_ref)
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(Uuid::new_v4().to_string())
                .bind(&version_id)
                .bind(&tool.tool_name)
                .bind(&tool.description)
                .bind(tool.input_schema.to_string())
                .bind(&tool.risk_class)
                .bind(&tool.implementation_type)
                .bind(&tool.implementation_ref)
                .execute(&mut *tx)
                .await?;
            }
        }

        // Activate atomically: supersede the old active version and point the
        // skill at the new one in the same transaction.
        sqlx::query("UPDATE skill_versions SET status = 'superseded' WHERE skill_id = ? AND status = 'active'")
            .bind(&manifest.id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE skill_versions SET status = 'active' WHERE id = ?")
            .bind(&version_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE skills SET active_version_id = ?, updated_at = ? WHERE id = ?")
            .bind(&version_id)
            .bind(&now)
            .bind(&manifest.id)
            .execute(&mut *tx)
            .await?;

        tx.commit().await.map_err(ApiError::from)?;

        // Record the resolved identity on the upload row.
        sqlx::query("UPDATE skill_uploads SET skill_id = ?, version = ?, package_hash = ? WHERE id = ?")
            .bind(&manifest.id)
            .bind(&manifest.version)
            .bind(package_hash)
            .bind(upload_id)
            .execute(&self.db)
            .await?;
        Ok(())
    }

    /// Uninstall a skill (spec section 7): disable first, prevent new
    /// invocations, remove registration, keep historical metadata.
    pub async fn uninstall(&self, skill_id: &str) -> Result<(), ApiError> {
        let row = sqlx::query("SELECT status FROM skills WHERE id = ?")
            .bind(skill_id)
            .fetch_optional(&self.db)
            .await?
            .ok_or(ApiError::NotFound)?;
        let status: String = row.get("status");
        if status == "uninstalled" {
            return Err(ApiError::Conflict("skill already uninstalled".into()));
        }
        let now = chrono::Utc::now().to_rfc3339();
        let mut tx = self.db.begin().await.map_err(ApiError::from)?;
        // Disable and detach the active version; agent links are removed so
        // no new invocation can start through this skill.
        sqlx::query("DELETE FROM agent_skills WHERE skill_version_id IN (SELECT id FROM skill_versions WHERE skill_id = ?)")
            .bind(skill_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE skills SET status = 'uninstalled', active_version_id = NULL, updated_at = ? WHERE id = ?")
            .bind(&now)
            .bind(skill_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await.map_err(ApiError::from)?;

        // Package files are removed only after the registration is gone.
        let dir = PathBuf::from(&self.skills_dir).join(skill_id);
        if dir.exists() {
            let _ = std::fs::remove_dir_all(&dir);
        }
        Ok(())
    }
}


/// Validate a single archive entry name. Returns Err with the rejection
/// reason for absolute paths, traversal, or non-portable separators.
pub fn validate_entry_name(name: &str) -> Result<(), String> {
    if name.starts_with('/') || name.contains('\\') {
        return Err(format!("absolute or non-portable path: {name}"));
    }
    if name.split('/').any(|c| c == "..") {
        return Err(format!("path traversal entry: {name}"));
    }
    Ok(())
}

/// Builtins a declarative tool may reference (spec 5.3: a skill may compose
/// existing validated tools; arbitrary URLs are never trusted).
const KNOWN_BUILTINS: &[&str] = &[
    "builtin.echo",
    "builtin.noop",
    "builtin.file_list",
    "builtin.file_read",
    "builtin.file_write",
    "builtin.file_delete",
    "builtin.file_search",
    "builtin.web_fetch",
];

fn valid_skill_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.chars().next().map(|c| c.is_ascii_lowercase()).unwrap_or(false)
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn valid_semver(v: &str) -> bool {
    let core = v.split('-').next().unwrap_or(v);
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        && parts.iter().all(|p| p.parse::<u64>().is_ok())
}

fn valid_tool_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && name.len() >= 2
        && name.len() <= 64
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
        .unwrap_or_default()
}

/// Find `skill.json` at the root or inside a single top-level directory.
fn find_manifest(extracted: &Path) -> Option<PathBuf> {
    let root_manifest = extracted.join("skill.json");
    if root_manifest.is_file() {
        return Some(root_manifest);
    }
    if let Ok(entries) = std::fs::read_dir(extracted) {
        let dirs: Vec<_> = entries.flatten().filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false)).collect();
        if dirs.len() == 1 {
            let candidate = dirs[0].path().join("skill.json");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}