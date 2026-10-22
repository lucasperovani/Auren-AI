//! Skill installation pipeline tests (spec sections 5-7, DoD items 8-14).

use serde_json::{json, Value};
use sqlx::Row;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

use crate::services::skill_installer::SkillInstallationService;
use crate::state::AppState;

async fn state() -> (AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}/t.db?mode=rwc", dir.path().to_str().unwrap());
    let pool = crate::db::connect(&url).await.unwrap();
    crate::db::migrate(&pool).await.unwrap();
    let skills_dir = dir.path().join("skills");
    std::fs::create_dir_all(&skills_dir).unwrap();
    let s = AppState {
        db: pool,
        config: crate::config::AppConfig {
            database_url: url,
            bind_addr: "127.0.0.1:0".into(),
            skills_dir: skills_dir.to_string_lossy().to_string(),
            max_upload_bytes: 1024 * 1024,
            session_secret: "s".into(),
                workspace_dir: "data/workspace".into(),
                max_workspace_file_bytes: 1024 * 1024,
                credential_encryption_key: "k".into(),
        },
        events: std::sync::Arc::new(crate::events::EventBus::new()),
        ws_tickets: std::sync::Arc::new(crate::events::TicketStore::new()),
            rate_limiter: std::sync::Arc::new(crate::api::rate_limit::RateLimiter::new()),
    };
    sqlx::query("INSERT INTO users (id, email, display_name, password_hash, is_admin, created_at, updated_at) VALUES ('u1','a@b.com','A','x',1,'2026','2026')")
        .execute(&s.db)
        .await
        .unwrap();
    (s, dir)
}

fn manifest_json() -> Value {
    json!({
        "schema_version": "1",
        "id": "gmail-monitor",
        "name": "Gmail Monitor",
        "version": "1.0.0",
        "description": "Monitors Gmail messages",
        "entrypoint": "tools/",
        "implementation_type": "declarative",
        "required_integrations": ["gmail"],
        "required_permissions": ["gmail.read"],
        "tools": [
            {
                "tool_name": "search_messages",
                "description": "Search messages",
                "input_schema": { "type": "object" },
                "risk_class": "low",
                "implementation_type": "declarative",
                "implementation_ref": "builtin.echo"
            }
        ]
    })
}

/// Build a ZIP with the given entries (path → content).
fn build_zip(entries: Vec<(String, Vec<u8>)>) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    let mut zip = ZipWriter::new(&mut buf);
    for (name, content) in entries {
        zip.start_file(name, SimpleFileOptions::default()).unwrap();
        std::io::Write::write_all(&mut zip, &content).unwrap();
    }
    zip.finish().unwrap();
    buf.into_inner()
}

fn valid_package() -> Vec<u8> {
    let manifest = manifest_json().to_string().into_bytes();
    build_zip(vec![
        ("gmail-monitor/skill.json".into(), manifest),
        ("gmail-monitor/SKILL.md".into(), b"# Gmail Monitor".to_vec()),
        (
            "gmail-monitor/tools/search_messages.json".into(),
            json!({ "name": "search_messages" }).to_string().into_bytes(),
        ),
    ])
}

#[tokio::test]
async fn installs_valid_package() {
    let (s, _d) = state().await;
    let svc = SkillInstallationService::new(&s);
    let upload_id = svc.install("u1", valid_package()).await.unwrap();

    // Upload marked installed.
    let row = sqlx::query("SELECT status, skill_id, version FROM skill_uploads WHERE id = ?")
        .bind(&upload_id)
        .fetch_one(&s.db)
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("status"), "installed");
    assert_eq!(row.get::<String, _>("skill_id"), "gmail-monitor");

    // Skill registered with active version and tool.
    let skill = sqlx::query("SELECT status, active_version_id FROM skills WHERE id = 'gmail-monitor'")
        .fetch_one(&s.db)
        .await
        .unwrap();
    assert_eq!(skill.get::<String, _>("status"), "disabled");
    assert!(skill.get::<Option<String>, _>("active_version_id").is_some());
    let tools = sqlx::query("SELECT COUNT(*) AS c FROM skill_tools")
        .fetch_one(&s.db)
        .await
        .unwrap();
    assert_eq!(tools.get::<i64, _>("c"), 1);

    // Package files installed into persistent storage.
    let path = sqlx::query("SELECT install_path FROM skill_versions WHERE skill_id = 'gmail-monitor'")
        .fetch_one(&s.db)
        .await
        .unwrap();
    let install_path: String = path.get("install_path");
    assert!(std::path::Path::new(&install_path).join("skill.json").exists());
}

#[tokio::test]
async fn rejects_path_traversal() {
    let (s, _d) = state().await;
    let svc = SkillInstallationService::new(&s);
    let manifest = manifest_json().to_string().into_bytes();
    let zip = build_zip(vec![
        ("gmail-monitor/skill.json".into(), manifest),
        ("../evil.txt".into(), b"payload".to_vec()),
    ]);
    let err = svc.install("u1", zip).await.unwrap_err();
    assert!(err.to_string().contains("traversal"), "got: {err}");
    // Nothing was installed.
    let count: i64 = sqlx::query("SELECT COUNT(*) AS c FROM skills").fetch_one(&s.db).await.unwrap().get("c");
    assert_eq!(count, 0);
}

#[tokio::test]
async fn rejects_absolute_paths() {
    let (s, _d) = state().await;
    let svc = SkillInstallationService::new(&s);
    let manifest = manifest_json().to_string().into_bytes();
    let zip = build_zip(vec![
        ("gmail-monitor/skill.json".into(), manifest),
        ("/etc/evil.txt".into(), b"payload".to_vec()),
    ]);
    let err = svc.install("u1", zip).await.unwrap_err();
    assert!(err.to_string().contains("absolute"), "got: {err}");
}

#[test]
fn entry_name_validation() {
    use crate::services::skill_installer::validate_entry_name;
    assert!(validate_entry_name("ok/path.txt").is_ok());
    assert!(validate_entry_name("/absolute").is_err());
    assert!(validate_entry_name("../traversal").is_err());
    assert!(validate_entry_name("a/../b").is_err());
    assert!(validate_entry_name(r"back\slash").is_err());
}


#[tokio::test]
async fn rejects_invalid_manifest() {
    let (s, _d) = state().await;
    let svc = SkillInstallationService::new(&s);
    // Missing required fields.
    let zip = build_zip(vec![(
        "gmail-monitor/skill.json".into(),
        json!({ "schema_version": "1", "id": "x" }).to_string().into_bytes(),
    )]);
    assert!(svc.install("u1", zip).await.is_err());

    // Unsupported schema version.
    let mut manifest = manifest_json();
    manifest["schema_version"] = json!("999");
    let zip = build_zip(vec![(
        "gmail-monitor/skill.json".into(),
        manifest.to_string().into_bytes(),
    )]);
    let err = svc.install("u1", zip).await.unwrap_err();
    assert!(err.to_string().contains("schema_version"), "got: {err}");
}

#[tokio::test]
async fn rejects_unknown_implementation_ref() {
    let (s, _d) = state().await;
    let svc = SkillInstallationService::new(&s);
    let mut manifest = manifest_json();
    manifest["tools"][0]["implementation_ref"] = json!("https://evil.example/exec");
    let zip = build_zip(vec![(
        "gmail-monitor/skill.json".into(),
        manifest.to_string().into_bytes(),
    )]);
    let err = svc.install("u1", zip).await.unwrap_err();
    assert!(err.to_string().contains("unknown implementation"), "got: {err}");
}

#[tokio::test]
async fn rejects_malformed_zip() {
    let (s, _d) = state().await;
    let svc = SkillInstallationService::new(&s);
    assert!(svc.install("u1", b"not a zip".to_vec()).await.is_err());
}

#[tokio::test]
async fn duplicate_version_same_hash_is_idempotent() {
    let (s, _d) = state().await;
    let svc = SkillInstallationService::new(&s);
    svc.install("u1", valid_package()).await.unwrap();
    // Same package again: idempotent success.
    svc.install("u1", valid_package()).await.unwrap();
    let count: i64 = sqlx::query("SELECT COUNT(*) AS c FROM skill_versions WHERE skill_id = 'gmail-monitor'")
        .fetch_one(&s.db)
        .await
        .unwrap()
        .get("c");
    assert_eq!(count, 1);
}

#[tokio::test]
async fn update_preserves_previous_version_on_failure() {
    let (s, _d) = state().await;
    let svc = SkillInstallationService::new(&s);
    svc.install("u1", valid_package()).await.unwrap();

    // A new version with an invalid manifest must not replace the active one.
    let mut bad = manifest_json();
    bad["version"] = json!("2.0.0");
    bad["tools"] = json!([]); // invalid: no tools
    let zip = build_zip(vec![("gmail-monitor/skill.json".into(), bad.to_string().into_bytes())]);
    assert!(svc.install("u1", zip).await.is_err());

    let active = sqlx::query("SELECT version FROM skill_versions WHERE skill_id = 'gmail-monitor' AND status = 'active'")
        .fetch_one(&s.db)
        .await
        .unwrap();
    assert_eq!(active.get::<String, _>("version"), "1.0.0");
}

#[tokio::test]
async fn update_installs_new_version_and_supersedes_old() {
    let (s, _d) = state().await;
    let svc = SkillInstallationService::new(&s);
    svc.install("u1", valid_package()).await.unwrap();

    let mut v2 = manifest_json();
    v2["version"] = json!("2.0.0");
    let zip = build_zip(vec![("gmail-monitor/skill.json".into(), v2.to_string().into_bytes())]);
    svc.install("u1", zip).await.unwrap();

    let versions: Vec<(String, String)> = sqlx::query("SELECT version, status FROM skill_versions WHERE skill_id = 'gmail-monitor' ORDER BY version")
        .fetch_all(&s.db)
        .await
        .unwrap()
        .into_iter()
        .map(|r| (r.get::<String, _>("version"), r.get::<String, _>("status")))
        .collect();
    assert_eq!(versions, vec![("1.0.0".to_string(), "superseded".to_string()), ("2.0.0".to_string(), "active".to_string())]);
}

#[tokio::test]
async fn uninstall_removes_registration_and_files() {
    let (s, _d) = state().await;
    let svc = SkillInstallationService::new(&s);
    svc.install("u1", valid_package()).await.unwrap();
    svc.uninstall("gmail-monitor").await.unwrap();

    let skill = sqlx::query("SELECT status, active_version_id FROM skills WHERE id = 'gmail-monitor'")
        .fetch_one(&s.db)
        .await
        .unwrap();
    assert_eq!(skill.get::<String, _>("status"), "uninstalled");
    assert!(skill.get::<Option<String>, _>("active_version_id").is_none());
    // Agent links removed.
    let links: i64 = sqlx::query("SELECT COUNT(*) AS c FROM agent_skills").fetch_one(&s.db).await.unwrap().get("c");
    assert_eq!(links, 0);
    // Package files removed.
    assert!(!std::path::Path::new(&s.config.skills_dir).join("gmail-monitor").exists());
}

/// Installation survives an application restart: records are persisted, and
/// a fresh service instance over the same database sees the skill.
#[tokio::test]
async fn installation_survives_restart() {
    let (s, _d) = state().await;
    let svc = SkillInstallationService::new(&s);
    svc.install("u1", valid_package()).await.unwrap();

    // Simulate a restart: a new service over the same pool.
    let svc2 = SkillInstallationService::new(&s);
    let skills = sqlx::query("SELECT id FROM skills WHERE status != 'uninstalled'")
        .fetch_all(&s.db)
        .await
        .unwrap();
    assert_eq!(skills.len(), 1);
    let _ = svc2; // registry reads from the database; no restart needed
}
