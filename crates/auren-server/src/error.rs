use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};

/// Canonical API error representation (spec section 3.1).
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{0}")]
    BadRequest(String),
    #[error("authentication required")]
    Unauthorized,
    #[error("{0}")]
    Forbidden(String),
    #[error("resource not found")]
    NotFound,
    #[error("{0}")]
    Conflict(String),
    #[error("upload exceeds the configured limit")]
    ContentTooLarge,
    #[error("{0}")]
    Unprocessable(String),
    #[error("rate limit exceeded")]
    TooManyRequests,
    #[error("this operation requires explicit approval")]
    ApprovalRequired { approval_id: String },
    #[error("required service unavailable")]
    ServiceUnavailable,
    #[error("internal server error")]
    Internal(#[from] anyhow::Error),
}

impl ApiError {
    pub fn code(&self) -> &'static str {
        match self {
            ApiError::BadRequest(_) => "bad_request",
            ApiError::Unauthorized => "unauthorized",
            ApiError::Forbidden(_) => "forbidden",
            ApiError::NotFound => "not_found",
            ApiError::Conflict(_) => "conflict",
            ApiError::ContentTooLarge => "content_too_large",
            ApiError::Unprocessable(_) => "unprocessable",
            ApiError::TooManyRequests => "too_many_requests",
            ApiError::ApprovalRequired { .. } => "approval_required",
            ApiError::ServiceUnavailable => "service_unavailable",
            ApiError::Internal(_) => "internal_error",
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::Unauthorized => StatusCode::UNAUTHORIZED,
            ApiError::Forbidden(_) => StatusCode::FORBIDDEN,
            ApiError::NotFound => StatusCode::NOT_FOUND,
            ApiError::Conflict(_) => StatusCode::CONFLICT,
            ApiError::ContentTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            ApiError::Unprocessable(_) => StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::TooManyRequests => StatusCode::TOO_MANY_REQUESTS,
            ApiError::ApprovalRequired { .. } => StatusCode::FORBIDDEN,
            ApiError::ServiceUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Optional machine-readable details carried in the error envelope.
    fn details(&self) -> Option<serde_json::Value> {
        match self {
            ApiError::ApprovalRequired { approval_id } => {
                Some(serde_json::json!({ "approval_id": approval_id }))
            }
            _ => None,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        let message = match &self {
            // Never leak internal error details to clients.
            ApiError::Internal(_) => "An unexpected error occurred.".to_string(),
            other => other.to_string(),
        };
        let mut error = serde_json::json!({
            "code": self.code(),
            "message": message,
        });
        if let Some(details) = self.details() {
            error["details"] = details;
        }
        let body = serde_json::json!({ "error": error });
        (status, Json(body)).into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        match err {
            sqlx::Error::RowNotFound => ApiError::NotFound,
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                ApiError::Conflict("resource already exists".to_string())
            }
            _ => ApiError::Internal(anyhow::Error::from(err)),
        }
    }
}