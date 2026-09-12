use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use serde_json::{Map, Value};

use super::error::{ApiError, ValidationErrors};
use super::{
    nullable_string_field, parse_id, parse_json_object, read_body, string_field, AppState,
    CharFieldRules, USER_ID,
};
use crate::db::configs::{self, ConfigChanges, ConfigRow, NewConfig};
use crate::time;

const NAME_MAX_LENGTH: usize = 255;
const VERSION_MAX_LENGTH: usize = 32;
const COLLECTION_ALLOWED_METHODS: &str = "GET, POST, HEAD, OPTIONS";
const ITEM_ALLOWED_METHODS: &str = "GET, PUT, PATCH, DELETE, HEAD, OPTIONS";

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
        _ => Err(ApiError::MethodNotAllowed {
            method: method_of(&req).to_owned(),
            allowed: COLLECTION_ALLOWED_METHODS,
        }),
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
            update(&state, id, &body)
        }
        "DELETE" => remove(&state, id),
        _ => Err(ApiError::MethodNotAllowed {
            method: method_of(&req).to_owned(),
            allowed: ITEM_ALLOWED_METHODS,
        }),
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
    let mut errors = ValidationErrors::default();
    let name = provided_name(body.as_ref(), &mut errors, true);
    let content = string_field(
        body.as_ref(),
        "content",
        CharFieldRules::new(None).raw().allow_blank(),
        &mut errors,
    );
    let last_used_with_version = nullable_string_field(
        body.as_ref(),
        "last_used_with_version",
        CharFieldRules::new(Some(VERSION_MAX_LENGTH)),
        &mut errors,
    )
    .flatten();
    errors.finish()?;

    let conn = state.db.lock()?;
    let (timestamp, date) = time::now_wire_and_date()?;
    let name = match name {
        Some(name) if !name.is_empty() => name,
        _ => format!("Unnamed config ({date})"),
    };
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
fn update(state: &AppState, id: i64, body: &[u8]) -> Result<Response, ApiError> {
    let mut conn = state.db.lock()?;
    if configs::get(&conn, id)?.is_none() {
        return Err(ApiError::NotFound);
    }

    let body = parse_json_object(body)?;
    let mut errors = ValidationErrors::default();
    let changes = ConfigChanges {
        name: provided_name(body.as_ref(), &mut errors, false),
        content: string_field(
            body.as_ref(),
            "content",
            CharFieldRules::new(None).raw().allow_blank(),
            &mut errors,
        ),
        last_used_with_version: nullable_string_field(
            body.as_ref(),
            "last_used_with_version",
            CharFieldRules::new(Some(VERSION_MAX_LENGTH)),
            &mut errors,
        ),
    };
    errors.finish()?;

    let timestamp = time::now_wire()?;
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

fn provided_name(
    body: Option<&Map<String, Value>>,
    errors: &mut ValidationErrors,
    allow_blank: bool,
) -> Option<String> {
    let mut rules = CharFieldRules::new(Some(NAME_MAX_LENGTH));
    if allow_blank {
        rules = rules.allow_blank();
    }
    string_field(body, "name", rules, errors)
}

fn method_of(req: &Request) -> &str {
    req.method().as_str()
}
