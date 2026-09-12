use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Map, Value};
use thiserror::Error;

use crate::db::DbError;
use crate::time::TimeError;

/// Every error the API can produce, rendered the way DRF renders it: a single
/// `detail` string, or a field-name-to-array-of-messages object for validation
/// failures.
#[derive(Debug, Error)]
pub enum ApiError {
    #[error("invalid token")]
    Unauthorized,

    #[error("not found")]
    NotFound,

    #[error("method {0} not allowed")]
    MethodNotAllowed(String),

    #[error("malformed JSON body: {0}")]
    JsonParse(String),

    #[error("validation failed")]
    Validation(Map<String, Value>),

    #[error("request body too large")]
    PayloadTooLarge,

    #[error("database failure")]
    Db(#[from] DbError),

    #[error("clock failure")]
    Time(#[from] TimeError),
}

fn not_found_body() -> Value {
    json!({ "detail": "Not found." })
}

pub fn validation_error(field: &str, message: &str) -> ApiError {
    let mut errors = Map::new();
    errors.insert(field.to_owned(), json!([message]));
    ApiError::Validation(errors)
}

/// DRF rejects a request body that is valid JSON but not an object with
/// `non_field_errors` (see `rest_framework/serializers.py`,
/// `Serializer.to_internal_value`, which formats `type(data).__name__`).
pub fn datatype(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(number) if number.is_i64() || number.is_u64() => "int",
        Value::Number(_) => "float",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

pub fn not_an_object_error(value: &Value) -> ApiError {
    validation_error(
        "non_field_errors",
        &format!(
            "Invalid data. Expected a dictionary, but got {}.",
            datatype(value)
        ),
    )
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            ApiError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                [(header::WWW_AUTHENTICATE, "Bearer")],
                Json(json!({ "detail": "Invalid token." })),
            )
                .into_response(),
            ApiError::NotFound => (StatusCode::NOT_FOUND, Json(not_found_body())).into_response(),
            ApiError::MethodNotAllowed(method) => (
                StatusCode::METHOD_NOT_ALLOWED,
                Json(json!({ "detail": format!("Method \"{method}\" not allowed.") })),
            )
                .into_response(),
            ApiError::JsonParse(message) => (
                StatusCode::BAD_REQUEST,
                Json(json!({ "detail": format!("JSON parse error - {message}") })),
            )
                .into_response(),
            ApiError::Validation(errors) => {
                (StatusCode::BAD_REQUEST, Json(Value::Object(errors))).into_response()
            }
            ApiError::PayloadTooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(json!({ "detail": "Request body too large." })),
            )
                .into_response(),
            ApiError::Db(error) => {
                tracing::error!(error = %error, "database error while serving request");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "detail": "Internal server error." })),
                )
                    .into_response()
            }
            ApiError::Time(error) => {
                tracing::error!(error = %error, "could not timestamp request");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "detail": "Internal server error." })),
                )
                    .into_response()
            }
        }
    }
}
