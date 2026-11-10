//! Projects API, plus the per-task collaboration endpoints (notes, links, and
//! the activity timeline).

use axum::{
    extract::{Path, State},
    routing::{delete, get},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;

use crate::{
    api::auth_user::AuthUser,
    error::{ApiError, ApiResult},
    services::{projects::ProjectService, task_collab::TaskCollabService},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_projects).post(create_project))
        .route("/{project_id}", get(get_project).patch(update_project).delete(delete_project))
}

#[derive(Deserialize)]
pub struct CreateProjectRequest {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub color: Option<String>,
}

pub async fn create_project(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateProjectRequest>,
) -> ApiResult<(axum::http::StatusCode, Json<serde_json::Value>)> {
    let svc = ProjectService::new(&state);
    let id = svc
        .create(&user.id, &req.name, &req.description, req.color.as_deref())
        .await?;
    let projects = svc.list(&user.id).await?;
    let created = projects
        .into_iter()
        .find(|p| p["id"] == id.as_str())
        .unwrap_or_else(|| json!({ "id": id }));
    Ok((axum::http::StatusCode::CREATED, Json(created)))
}

pub async fn list_projects(
    State(state): State<AppState>,
    user: AuthUser,
) -> ApiResult<Json<serde_json::Value>> {
    let items = ProjectService::new(&state).list(&user.id).await?;
    Ok(Json(json!({ "items": items, "next_cursor": null })))
}

pub async fn get_project(
    State(state): State<AppState>,
    user: AuthUser,
    Path(project_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let svc = ProjectService::new(&state);
    svc.require_owned(&user.id, &project_id).await?;
    let items = svc.list(&user.id).await?;
    items
        .into_iter()
        .find(|p| p["id"] == project_id.as_str())
        .map(Json)
        .ok_or(ApiError::NotFound)
}

#[derive(Deserialize)]
pub struct UpdateProjectRequest {
    pub name: Option<String>,
    pub description: Option<String>,
    pub color: Option<String>,
    pub archived: Option<bool>,
}

pub async fn update_project(
    State(state): State<AppState>,
    user: AuthUser,
    Path(project_id): Path<String>,
    Json(req): Json<UpdateProjectRequest>,
) -> ApiResult<Json<serde_json::Value>> {
    let svc = ProjectService::new(&state);
    svc.update(
        &user.id,
        &project_id,
        req.name.as_deref(),
        req.description.as_deref(),
        req.color.as_deref(),
        req.archived,
    )
    .await?;
    let items = svc.list(&user.id).await?;
    items
        .into_iter()
        .find(|p| p["id"] == project_id.as_str())
        .map(Json)
        .ok_or(ApiError::NotFound)
}

/// Deleting a project detaches its tasks instead of deleting them.
pub async fn delete_project(
    State(state): State<AppState>,
    user: AuthUser,
    Path(project_id): Path<String>,
) -> ApiResult<axum::http::StatusCode> {
    ProjectService::new(&state).delete(&user.id, &project_id).await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Per-task: notes, links, activity
// ---------------------------------------------------------------------------

pub fn task_collab_router() -> Router<AppState> {
    Router::new()
        .route("/{task_id}/notes", get(list_notes).post(create_note))
        .route("/{task_id}/notes/{note_id}", delete(delete_note))
        .route("/{task_id}/links", get(list_links).post(create_link))
        .route("/{task_id}/links/{link_id}", delete(delete_link))
        .route("/{task_id}/activity", get(get_activity))
}

pub async fn list_notes(
    State(state): State<AppState>,
    user: AuthUser,
    Path(task_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let items = TaskCollabService::new(&state)
        .list_notes(&user.id, &task_id)
        .await?;
    Ok(Json(json!({ "items": items, "next_cursor": null })))
}

#[derive(Deserialize)]
pub struct CreateNoteRequest {
    pub body: String,
}

pub async fn create_note(
    State(state): State<AppState>,
    user: AuthUser,
    Path(task_id): Path<String>,
    Json(req): Json<CreateNoteRequest>,
) -> ApiResult<(axum::http::StatusCode, Json<serde_json::Value>)> {
    let id = TaskCollabService::new(&state)
        .add_note(&user.id, &task_id, "user", &req.body)
        .await?;
    Ok((
        axum::http::StatusCode::CREATED,
        Json(json!({ "id": id, "task_id": task_id })),
    ))
}

pub async fn delete_note(
    State(state): State<AppState>,
    user: AuthUser,
    Path((task_id, note_id)): Path<(String, String)>,
) -> ApiResult<axum::http::StatusCode> {
    TaskCollabService::new(&state)
        .delete_note(&user.id, &task_id, &note_id)
        .await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub async fn list_links(
    State(state): State<AppState>,
    user: AuthUser,
    Path(task_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let links = TaskCollabService::new(&state)
        .list_links(&user.id, &task_id)
        .await?;
    Ok(Json(links))
}

#[derive(Deserialize)]
pub struct CreateLinkRequest {
    /// The card being referenced.
    pub to_task_id: String,
    #[serde(default = "default_link_kind")]
    pub kind: String,
}

fn default_link_kind() -> String {
    "relates".into()
}

pub async fn create_link(
    State(state): State<AppState>,
    user: AuthUser,
    Path(task_id): Path<String>,
    Json(req): Json<CreateLinkRequest>,
) -> ApiResult<(axum::http::StatusCode, Json<serde_json::Value>)> {
    let id = TaskCollabService::new(&state)
        .link(&user.id, &task_id, &req.to_task_id, &req.kind)
        .await?;
    Ok((
        axum::http::StatusCode::CREATED,
        Json(json!({ "id": id, "kind": req.kind })),
    ))
}

pub async fn delete_link(
    State(state): State<AppState>,
    user: AuthUser,
    Path((task_id, link_id)): Path<(String, String)>,
) -> ApiResult<axum::http::StatusCode> {
    TaskCollabService::new(&state)
        .unlink(&user.id, &task_id, &link_id)
        .await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

/// Chronological timeline: notes plus every run and tool call on the card.
pub async fn get_activity(
    State(state): State<AppState>,
    user: AuthUser,
    Path(task_id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let items = TaskCollabService::new(&state)
        .activity(&user.id, &task_id)
        .await?;
    Ok(Json(json!({ "items": items, "next_cursor": null })))
}
