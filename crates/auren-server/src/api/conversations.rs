//! Conversations API (spec section 3.5).

use axum::{
    extract::{Path, Query, State},
    routing::get,
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Row;

use crate::{
    api::{auth_user::AuthUser, pagination},
    error::{ApiError, ApiResult},
    state::AppState,
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/", get(list_conversations).post(create_conversation))
        .route("/{conversation_id}", get(get_conversation).patch(update_conversation).delete(delete_conversation))
        .route("/{conversation_id}/messages", get(list_messages).post(post_message))
}

#[derive(Deserialize)]
pub struct ListParams {
    pub limit: Option<i64>,
    pub cursor: Option<String>,
}

#[derive(Serialize)]
pub struct ConversationResponse {
    pub id: String,
    pub agent_id: String,
    pub title: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

fn row_to_conversation(r: &sqlx::sqlite::SqliteRow) -> ConversationResponse {
    ConversationResponse {
        id: r.get("id"),
        agent_id: r.get("agent_id"),
        title: r.get("title"),
        status: r.get("status"),
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
    }
}

pub async fn list_conversations(
    State(state): State<AppState>,
    user: AuthUser,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<serde_json::Value>> {
    let limit = params.limit.unwrap_or(50).clamp(1, 200);
    let cursor = params.cursor.as_deref().and_then(pagination::decode_cursor);
    let rows = match &cursor {
        Some(c) => {
            sqlx::query("SELECT * FROM conversations WHERE user_id = ? AND id > ? ORDER BY id LIMIT ?")
                .bind(&user.id).bind(c).bind(limit + 1)
                .fetch_all(&state.db).await?
        }
        None => {
            sqlx::query("SELECT * FROM conversations WHERE user_id = ? ORDER BY id LIMIT ?")
                .bind(&user.id).bind(limit + 1)
                .fetch_all(&state.db).await?
        }
    };
    let has_more = rows.len() as i64 > limit;
    let items: Vec<ConversationResponse> = rows.iter().take(limit as usize).map(row_to_conversation).collect();
    let next_cursor = if has_more {
        items.last().map(|c| pagination::encode_cursor(&c.id))
    } else {
        None
    };
    Ok(Json(json!({ "items": items, "next_cursor": next_cursor })))
}

#[derive(Deserialize)]
pub struct CreateConversationRequest {
    pub agent_id: String,
    #[serde(default)]
    pub title: String,
}

pub async fn create_conversation(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateConversationRequest>,
) -> ApiResult<(axum::http::StatusCode, Json<ConversationResponse>)> {
    // The agent must belong to the user.
    let owner: Option<String> = sqlx::query("SELECT user_id FROM agents WHERE id = ?")
        .bind(&req.agent_id)
        .fetch_optional(&state.db)
        .await?
        .map(|r| r.get("user_id"));
    if owner.as_deref() != Some(user.id.as_str()) {
        return Err(ApiError::NotFound);
    }

    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO conversations (id, user_id, agent_id, title, status, created_at, updated_at)
         VALUES (?, ?, ?, ?, 'active', ?, ?)",
    )
    .bind(&id)
    .bind(&user.id)
    .bind(&req.agent_id)
    .bind(&req.title)
    .bind(&now)
    .bind(&now)
    .execute(&state.db)
    .await?;
    let row = sqlx::query("SELECT * FROM conversations WHERE id = ?").bind(&id).fetch_one(&state.db).await?;
    Ok((axum::http::StatusCode::CREATED, Json(row_to_conversation(&row))))
}

async fn require_owned_conversation(state: &AppState, user: &AuthUser, conversation_id: &str) -> ApiResult<()> {
    let row = sqlx::query("SELECT user_id FROM conversations WHERE id = ?")
        .bind(conversation_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)?;
    let owner: String = row.get("user_id");
    if owner != user.id {
        return Err(ApiError::NotFound);
    }
    Ok(())
}

pub async fn get_conversation(
    State(state): State<AppState>,
    user: AuthUser,
    Path(conversation_id): Path<String>,
) -> ApiResult<Json<ConversationResponse>> {
    require_owned_conversation(&state, &user, &conversation_id).await?;
    let row = sqlx::query("SELECT * FROM conversations WHERE id = ?")
        .bind(&conversation_id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(row_to_conversation(&row)))
}

#[derive(Deserialize)]
pub struct UpdateConversationRequest {
    pub title: Option<String>,
}

pub async fn update_conversation(
    State(state): State<AppState>,
    user: AuthUser,
    Path(conversation_id): Path<String>,
    Json(req): Json<UpdateConversationRequest>,
) -> ApiResult<Json<ConversationResponse>> {
    require_owned_conversation(&state, &user, &conversation_id).await?;
    if let Some(t) = &req.title {
        sqlx::query("UPDATE conversations SET title = ?, updated_at = ? WHERE id = ?")
            .bind(t).bind(chrono::Utc::now().to_rfc3339()).bind(&conversation_id)
            .execute(&state.db).await?;
    }
    let row = sqlx::query("SELECT * FROM conversations WHERE id = ?")
        .bind(&conversation_id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(row_to_conversation(&row)))
}

pub async fn delete_conversation(
    State(state): State<AppState>,
    user: AuthUser,
    Path(conversation_id): Path<String>,
) -> ApiResult<axum::http::StatusCode> {
    require_owned_conversation(&state, &user, &conversation_id).await?;
    sqlx::query("DELETE FROM conversations WHERE id = ?")
        .bind(&conversation_id)
        .execute(&state.db)
        .await?;
    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
pub struct MessageResponse {
    pub id: String,
    pub conversation_id: String,
    pub role: String,
    pub content: String,
    pub created_at: String,
}

pub async fn list_messages(
    State(state): State<AppState>,
    user: AuthUser,
    Path(conversation_id): Path<String>,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<serde_json::Value>> {
    require_owned_conversation(&state, &user, &conversation_id).await?;
    let limit = params.limit.unwrap_or(50).clamp(1, 200);
    let cursor = params.cursor.as_deref().and_then(pagination::decode_cursor);
    let rows = match &cursor {
        Some(c) => {
            sqlx::query("SELECT * FROM messages WHERE conversation_id = ? AND id > ? ORDER BY id LIMIT ?")
                .bind(&conversation_id).bind(c).bind(limit + 1)
                .fetch_all(&state.db).await?
        }
        None => {
            sqlx::query("SELECT * FROM messages WHERE conversation_id = ? ORDER BY id LIMIT ?")
                .bind(&conversation_id).bind(limit + 1)
                .fetch_all(&state.db).await?
        }
    };
    let has_more = rows.len() as i64 > limit;
    let items: Vec<MessageResponse> = rows
        .iter()
        .take(limit as usize)
        .map(|r| MessageResponse {
            id: r.get("id"),
            conversation_id: r.get("conversation_id"),
            role: r.get("role"),
            content: r.get("content"),
            created_at: r.get("created_at"),
        })
        .collect();
    let next_cursor = if has_more {
        items.last().map(|m| pagination::encode_cursor(&m.id))
    } else {
        None
    };
    Ok(Json(json!({ "items": items, "next_cursor": next_cursor })))
}

#[derive(Deserialize)]
pub struct PostMessageRequest {
    pub content: String,
}

/// Submit a user message. Processing is asynchronous: the message is
/// persisted immediately and `202 Accepted` returns an operation identifier
/// the frontend can follow (spec 3.5).
pub async fn post_message(
    State(state): State<AppState>,
    user: AuthUser,
    Path(conversation_id): Path<String>,
    Json(req): Json<PostMessageRequest>,
) -> ApiResult<(axum::http::StatusCode, Json<serde_json::Value>)> {
    require_owned_conversation(&state, &user, &conversation_id).await?;
    if req.content.trim().is_empty() {
        return Err(ApiError::Unprocessable("content is required".into()));
    }
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query("INSERT INTO messages (id, conversation_id, role, content, created_at) VALUES (?, ?, 'user', ?, ?)")
        .bind(&id)
        .bind(&conversation_id)
        .bind(req.content.trim())
        .bind(&now)
        .execute(&state.db)
        .await?;
    sqlx::query("UPDATE conversations SET updated_at = ? WHERE id = ?")
        .bind(&now)
        .bind(&conversation_id)
        .execute(&state.db)
        .await?;

    // The agent runtime processes the message asynchronously; the operation
    // identifier lets the frontend follow progress via the WebSocket channel.
    Ok((
        axum::http::StatusCode::ACCEPTED,
        Json(json!({ "message_id": id, "operation_id": id, "status": "accepted" })),
    ))
}