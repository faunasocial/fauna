//! Standardized API error response type.
//!
//! Provides consistent JSON error bodies (`{"error": "..."}`) across all
//! nest route handlers, replacing the ad-hoc mix of plain string bodies,
//! tuple responses, and raw status codes.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde_json::json;

/// A structured API error that always serializes as `{"error": "<message>"}`.
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

impl ApiError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: msg.into(),
        }
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: msg.into(),
        }
    }

    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: msg.into(),
        }
    }

    pub fn unauthorized(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: msg.into(),
        }
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: msg.into(),
        }
    }

    pub fn conflict(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: msg.into(),
        }
    }

    pub fn gone(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::GONE,
            message: msg.into(),
        }
    }

    pub fn too_many_requests(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: msg.into(),
        }
    }

    pub fn bad_gateway(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            message: msg.into(),
        }
    }

    pub fn not_implemented(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_IMPLEMENTED,
            message: msg.into(),
        }
    }

    pub fn service_unavailable(msg: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: msg.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({"error": self.message}))).into_response()
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.status.as_u16(), self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;

    #[test]
    fn api_error_produces_json_body() {
        let err = ApiError::bad_request("invalid input");
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn api_error_internal_uses_500() {
        let err = ApiError::internal("db failure");
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn api_error_not_found_uses_404() {
        let err = ApiError::not_found("no such item");
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn api_error_not_implemented_uses_501() {
        let err = ApiError::not_implemented("not yet");
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
    }

    #[test]
    fn service_unavailable_maps_to_503() {
        let resp =
            ApiError::service_unavailable("CalDAV not available on this nest").into_response();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
