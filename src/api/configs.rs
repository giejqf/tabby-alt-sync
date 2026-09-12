use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use serde_json::{Map, Value};

use super::error::ApiError;
use super::{
    nullable_string_field, parse_id, parse_json_object, read_body, string_field, AppState, USER_ID,
};
use crate::db::configs::{self, ConfigChanges, ConfigRow, NewConfig};
use crate::time;

/// The config object, exactly as tabby-web's `ConfigSerializer` emits it: every
/// key always present (nulls included), no camelCase, no extra keys
/// (`backend/tabby/app/api/config.py`).
#[derive(Serialize)]
pub struct ConfigJson {
    pub id: i64,
    pub user: i64,
    pub name: String,
    pub content: String,
    pub last_used_with_version: Option<String>,
    pub created_at: String,
    pub modified_at: String,
}

impl From<&ConfigRow> for ConfigJson {
    fn from(row: &ConfigRow) -> Self {
        Self {
            id: row.id,
            user: USER_ID,
            name: row.name.clone(),
            content: row.content.clone(),
            last_used_with_version: row.last_used_with_version.clone(),
            created_at: row.created_at.clone(),
            modified_at: row.modified_at.clone(),
        }
    }
}

pub async fn collection(State(state): State<AppState>, req: Request) -> Result<Response, ApiError> {
    match method_of(&req) {
        // DRF answers HEAD on retrieve/list routes; hyper omits the body.
        "GET" | "HEAD" => list(&state),
        "POST" => {
            let body = read_body(req, state.max_body_bytes).await?;
            create(&state, parse_json_object(&body)?)
        }
        _ => Err(ApiError::MethodNotAllowed(method_of(&req).to_string())),
    }
}

pub async fn item(
    State(state): State<AppState>,
    Path(raw_id): Path<String>,
    req: Request,
) -> Result<Response, ApiError> {
    let Some(id) = parse_id(&raw_id) else {
        return Err(ApiError::NotFound);
    };
    tracing::Span::current().record("config_id", id);

    match method_of(&req) {
        "GET" | "HEAD" => fetch(&state, id),
        "PUT" | "PATCH" => {
            let body = read_body(req, state.max_body_bytes).await?;
            update(&state, id, parse_json_object(&body)?)
        }
        "DELETE" => remove(&state, id),
        _ => Err(ApiError::MethodNotAllowed(method_of(&req).to_string())),
    }
}

fn list(state: &AppState) -> Result<Response, ApiError> {
    // Upstream configures no pagination class, so this is a bare JSON array.
    let conn = state.db.lock()?;
    let rows = configs::list(&conn)?;
    let body: Vec<ConfigJson> = rows.iter().map(ConfigJson::from).collect();
    Ok(Json(body).into_response())
}

fn fetch(state: &AppState, id: i64) -> Result<Response, ApiError> {
    let conn = state.db.lock()?;
    match configs::get(&conn, id)? {
        Some(row) => Ok(Json(ConfigJson::from(&row)).into_response()),
        None => Err(ApiError::NotFound),
    }
}

fn create(state: &AppState, body: Option<Map<String, Value>>) -> Result<Response, ApiError> {
    let name = match provided_name(body.as_ref())? {
        Some(name) => name,
        None => fallback_name()?,
    };
    let content = string_field(body.as_ref(), "content")?;
    let last_used_with_version =
        nullable_string_field(body.as_ref(), "last_used_with_version")?.flatten();
    let timestamp = time::now_wire()?;

    let conn = state.db.lock()?;
    let row = configs::create(
        &conn,
        NewConfig {
            name: &name,
            content: content.as_deref(),
            last_used_with_version: last_used_with_version.as_deref(),
            timestamp: &timestamp,
        },
    )?;

    Ok((StatusCode::CREATED, Json(ConfigJson::from(&row))).into_response())
}

/// PUT behaves exactly like PATCH: every writable field on the upstream
/// serializer is `required = False`, so a non-partial update only assigns what
/// the body carries.
fn update(
    state: &AppState,
    id: i64,
    body: Option<Map<String, Value>>,
) -> Result<Response, ApiError> {
    let changes = ConfigChanges {
        name: provided_name(body.as_ref())?,
        content: string_field(body.as_ref(), "content")?,
        last_used_with_version: nullable_string_field(body.as_ref(), "last_used_with_version")?,
    };
    let timestamp = time::now_wire()?;

    let mut conn = state.db.lock()?;
    match configs::update(&mut conn, id, &changes, &timestamp)? {
        Some(row) => Ok(Json(ConfigJson::from(&row)).into_response()),
        None => Err(ApiError::NotFound),
    }
}

fn remove(state: &AppState, id: i64) -> Result<Response, ApiError> {
    let mut conn = state.db.lock()?;
    if configs::delete(&mut conn, id)? {
        Ok(StatusCode::NO_CONTENT.into_response())
    } else {
        Err(ApiError::NotFound)
    }
}

/// A missing or blank `name` falls back to `Unnamed config (YYYY-MM-DD)`, the
/// default tabby-web applies in `Config.save()`
/// (`backend/tabby/app/models.py`).
fn provided_name(body: Option<&Map<String, Value>>) -> Result<Option<String>, ApiError> {
    let Some(name) = string_field(body, "name")? else {
        return Ok(None);
    };
    let name = name.trim();
    if name.is_empty() {
        return Ok(Some(fallback_name()?));
    }
    Ok(Some(name.to_owned()))
}

fn fallback_name() -> Result<String, ApiError> {
    Ok(format!("Unnamed config ({})", time::today_wire()?))
}

fn method_of(req: &Request) -> &str {
    req.method().as_str()
}
