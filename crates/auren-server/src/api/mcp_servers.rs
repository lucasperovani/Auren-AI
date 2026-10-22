//! MCP servers API: connect external MCP servers, list their tools, and
//! expose them to agents through the tool registry.

use axum::{
    extract::{Path, State},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::Row;

use crate::{
    api::auth_user::AuthUser,
    error::{ApiError, ApiResult},
    services::mcp::{self, McpServerConfig},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_servers).post(connect_server))
        .route("/{server_id}", get(get_server).patch(update_server).delete(delete_server))
        .route("/{server_id}/tools", get(list_tools))
        .route("/{server_id}/tools/{tool_name}/invoke", post(invoke_tool))
}

/// MCP server configs are stored in `app_settings` under a namespaced key.
const KEY_PREFIX: &str = "mcp_server:";

async fn load_server(state: &AppState, server_id: &str) -> ApiResult<McpServerConfig> {
    let row = sqlx::query("SELECT value_json FROM app_settings WHERE key = ?")
        .bind(format!("{KEY_PREFIX}{server_id}"))
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    let raw: String = row.get("value_json");
    serde_json::from_str(&raw).map_err(|_| ApiError::Internal(anyhow::anyhow!("corrupt MCP config")))
}

async fn save_server(state: &AppState, server: &McpServerConfig) -> ApiResult<()> {
    sqlx::query(
        "INSERT INTO app_settings (key, value_json, updated_at) VALUES (?, ?, ?)
         ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json, updated_at = excluded.updated_at",
    )
    .bind(format!("{KEY_PREFIX}{}", server.id))
    .bind(serde_json::to_string(server).unwrap_or_default())
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(&state.db)
    .await?;
    Ok(())
}

pub async fn list_servers(State(state): State<AppState>, _user: AuthUser) -> ApiResult<Json<serde_json::Value>> {
    let rows = sqlx::query("SELECT key, value_json FROM app_settings WHERE key LIKE ?")
        .bind(format!("{KEY_PREFIX}%"))
        .fetch_all(&state.db)
        .await?;
    let items: Vec<serde_json::Value> = rows
        .iter()
        .filter_map(|r| serde_json::from_str::<McpServerConfig>(&r.get::<String, _>("value_json")).ok())
        .map(|s| {
            json!({
                "id": s.id,
                "name": s.name,
                "url": s.url,
                "enabled": s.enabled,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items, "next_cursor": null })))
}

#[derive(Deserialize)]
pub struct ConnectServerRequest {
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub auth_header: Option<String>,
}

pub async fn connect_server(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(req): Json<ConnectServerRequest>,
) -> ApiResult<(axum::http::StatusCode, Json<serde_json::Value>)> {
    if req.name.trim().is_empty() || req.url.trim().is_empty() {
        return Err(ApiError::Unprocessable("name and url are required".into()));
    }
    // Validate the URL is reachable and speaks MCP during connect.
    let server = McpServerConfig {
        id: uuid::Uuid::new_v4().to_string(),
        name: req.name.trim().to_string(),
        url: req.url.trim().to_string(),
        auth_header: req.auth_header.clone(),
        enabled: true,
    };
    mcp::initialize(&server).await?;
    save_server(&state, &server).await?;
    Ok((
        axum::http::StatusCode::CREATED,
        Json(json!({ "id": server.id, "name": server.name })),
    ))
}

pub async fn get_server(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(server_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let server = load_server(&state, &server_id).await?;
    Ok(Json(json!({
        "id": server.id,
        "name": server.name,
        "url": server.url,
        "enabled": server.enabled,
    })))
}

#[derive(Deserialize)]
pub struct UpdateServerRequest {
    pub name: Option<String>,
    pub url: Option<String>,
    pub auth_header: Option<String>,
    pub enabled: Option<bool>,
}

pub async fn update_server(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(server_id): Path<String>,
    Json(req): Json<UpdateServerRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let mut server = load_server(&state, &server_id).await?;
    if let Some(n) = req.name {
        server.name = n.trim().to_string();
    }
    if let Some(u) = req.url {
        server.url = u.trim().to_string();
    }
    if req.auth_header.is_some() {
        server.auth_header = req.auth_header;
    }
    if let Some(e) = req.enabled {
        server.enabled = e;
    }
    save_server(&state, &server).await?;
    Ok(Json(json!({ "id": server.id, "updated": true })))
}

pub async fn delete_server(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(server_id): Path<String>,
) -> ApiResult<axum::http::StatusCode> {
    load_server(&state, &server_id).await?;
    sqlx::query("DELETE FROM app_settings WHERE key = ?")
        .bind(format!("{KEY_PREFIX}{server_id}"))
        .execute(&state.db)
        .await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// List the tools the server exposes (live query).
pub async fn list_tools(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(server_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let server = load_server(&state, &server_id).await?;
    let tools = mcp::list_server_tools(&server).await?;
    let items: Vec<serde_json::Value> = tools
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.input_schema,
            })
        })
        .collect();
    Ok(Json(json!({ "items": items, "next_cursor": null })))
}

#[derive(Deserialize)]
pub struct InvokeToolRequest {
    pub tool_name: String,
    #[serde(default)]
    pub arguments: serde_json::Value,
}

/// Invoke an MCP tool. Authorization: MCP tools are treated as medium risk
/// by default; the permission engine governs agent-level access.
pub async fn invoke_tool(
    State(state): State<AppState>,
    _user: AuthUser,
    Path((server_id, tool_name)): Path<(String, String)>,
    Json(req): Json<InvokeToolRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let server = load_server(&state, &server_id).await?;
    if !server.enabled {
        return Err(ApiError::Conflict("MCP server is disabled".into()));
    }
    let result = mcp::invoke_server_tool(&server, &tool_name, &req.arguments).await?;
    Ok(Json(result))
}