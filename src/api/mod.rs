pub mod configs;
pub mod error;
pub mod user;

use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::Request;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum::Router;
use http_body_util::BodyExt as _;
use serde_json::{Map, Value};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::trace::{DefaultOnFailure, DefaultOnResponse, TraceLayer};
use tracing::Level;

use crate::auth::{self, TokenSecret};
use crate::db::Db;
use error::ApiError;

/// Single implicit user. Upstream serialises relations as integer primary keys
/// (`fields = "__all__"` on a `ModelSerializer`), so this shows up verbatim in
/// `config.user`.
pub const USER_ID: i64 = 1;

const ALLOW_METHODS: &str = "GET, POST, PUT, PATCH, DELETE, OPTIONS";
const ALLOW_HEADERS: &str = "authorization, content-type";
const MAX_AGE: &str = "86400";

#[derive(Debug, Clone)]
pub struct AppState {
    pub db: Db,
    pub secret: Arc<TokenSecret>,
    /// Echoed back as `config_sync_token`, exactly as tabby-web's
    /// `UserSerializer` does. The caller had to present it to get here.
    pub token: Arc<str>,
    pub username: Arc<str>,
    pub max_body_bytes: usize,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/1/user", any(user::route))
        .route("/api/1/user/", any(user::route))
        .route("/api/1/configs", any(configs::collection))
        .route("/api/1/configs/", any(configs::collection))
        .route("/api/1/configs/{id}", any(configs::item))
        .route("/api/1/configs/{id}/", any(configs::item))
        .fallback(not_found)
        .layer(RequestBodyLimitLayer::new(state.max_body_bytes))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_token,
        ))
        .layer(middleware::from_fn(cors))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(|request: &axum::http::Request<Body>| {
                    tracing::info_span!(
                        "request",
                        method = %request.method(),
                        path = %redacted_path(request.uri()),
                        config_id = tracing::field::Empty,
                    )
                })
                .on_response(DefaultOnResponse::new().level(Level::INFO))
                .on_failure(DefaultOnFailure::new().level(Level::ERROR)),
        )
        .with_state(state)
}

async fn not_found() -> ApiError {
    ApiError::NotFound
}

/// Some client builds call the API from an Electron renderer, where CORS
/// applies. The preflight is answered before authentication, because it never
/// carries the `Authorization` header.
async fn cors(req: Request, next: Next) -> Response {
    let api = auth::is_api_path(req.uri().path());
    let origin = allowed_origin(req.headers());

    if api && req.method() == Method::OPTIONS {
        let mut response = StatusCode::NO_CONTENT.into_response();
        set_cors_headers(response.headers_mut(), origin);
        return response;
    }

    let mut response = next.run(req).await;
    if api {
        set_cors_headers(response.headers_mut(), origin);
    }
    response
}

fn allowed_origin(headers: &HeaderMap) -> HeaderValue {
    headers
        .get(header::ORIGIN)
        .and_then(|value| HeaderValue::from_bytes(value.as_bytes()).ok())
        .unwrap_or_else(|| HeaderValue::from_static("*"))
}

fn set_cors_headers(headers: &mut HeaderMap, origin: HeaderValue) {
    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static(ALLOW_METHODS),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static(ALLOW_HEADERS),
    );
    headers.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static(MAX_AGE),
    );
}

/// Never let `?auth_token=` reach the log: the tracing layer is the single
/// place that redacts it, so call sites don't have to remember.
fn redacted_path(uri: &Uri) -> String {
    match uri.query() {
        Some(query) => format!("{}?{}", uri.path(), redact_query(query)),
        None => uri.path().to_owned(),
    }
}

fn redact_query(query: &str) -> String {
    query
        .split('&')
        .map(|pair| {
            let key = pair.split('=').next().unwrap_or(pair);
            if key.eq_ignore_ascii_case(auth::TOKEN_QUERY_PARAM) {
                "auth_token=[redacted]"
            } else {
                pair
            }
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// Buffers a request body, refusing anything past the configured limit.
///
/// `RequestBodyLimitLayer` already rejects oversized requests that announce
/// `Content-Length`; this covers the chunked case, so an oversized body always
/// ends up with the same answer.
pub async fn read_body(req: Request, max_body_bytes: usize) -> Result<Bytes, ApiError> {
    let mut body = req.into_body();
    let mut collected: Vec<u8> = Vec::new();

    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|error| {
            let text = error.to_string();
            if text.contains("length limit exceeded") {
                ApiError::PayloadTooLarge
            } else {
                tracing::debug!(error = %text, "failed to read request body");
                ApiError::JsonParse(text)
            }
        })?;
        if let Some(data) = frame.data_ref() {
            if collected.len() + data.len() > max_body_bytes {
                return Err(ApiError::PayloadTooLarge);
            }
            collected.extend_from_slice(data);
        }
    }

    Ok(Bytes::from(collected))
}

/// Parses a request body the way DRF's JSON parser sees it: an empty body
/// carries no fields, malformed JSON is a parse error, and valid JSON that is
/// not an object is a `non_field_errors` validation failure.
pub fn parse_json_object(body: &[u8]) -> Result<Option<Map<String, Value>>, ApiError> {
    if body.is_empty() {
        return Ok(None);
    }
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(fields)) => Ok(Some(fields)),
        Ok(other) => Err(error::not_an_object_error(&other)),
        Err(parse_error) => Err(ApiError::JsonParse(parse_error.to_string())),
    }
}

/// Reads a field the way DRF's `CharField` does: strings are taken verbatim,
/// numbers are coerced, booleans and composites are rejected, and an explicit
/// `null` is reported separately so nullable columns can distinguish it.
pub fn string_field(
    body: Option<&Map<String, Value>>,
    field: &str,
) -> Result<Option<String>, ApiError> {
    match field_value(body, field) {
        None => Ok(None),
        Some(value) => Ok(Some(coerce_string(field, value)?)),
    }
}

/// `None` — key absent, leave the stored value alone.
/// `Some(None)` — explicit JSON null.
/// `Some(Some(text))` — new value.
pub fn nullable_string_field(
    body: Option<&Map<String, Value>>,
    field: &str,
) -> Result<Option<Option<String>>, ApiError> {
    match field_value(body, field) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(value) => Ok(Some(Some(coerce_string(field, value)?))),
    }
}

pub(crate) fn field_value<'a>(
    body: Option<&'a Map<String, Value>>,
    field: &str,
) -> Option<&'a Value> {
    body.and_then(|fields| fields.get(field))
}

fn coerce_string(field: &str, value: &Value) -> Result<String, ApiError> {
    match value {
        Value::String(text) => Ok(text.clone()),
        Value::Number(number) => Ok(number.to_string()),
        Value::Null => Err(error::validation_error(
            field,
            "This field may not be null.",
        )),
        _ => Err(error::validation_error(field, "Not a valid string.")),
    }
}

/// Route ids are numeric, as Django's URL resolver expects. Anything else —
/// `abc`, `1.5`, an overflowing number — simply does not match a route.
pub fn parse_id(raw: &str) -> Option<i64> {
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    raw.parse().ok()
}
