use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};
use axum::Json;
use rusqlite::Connection;
use serde::Serialize;
use serde_json::{Map, Value};

use super::error::{validation_error, ApiError};
use super::{field_value, nullable_string_field, parse_json_object, read_body, AppState, USER_ID};
use crate::db::configs;
use crate::db::{
    meta_get, meta_set, KEY_ACTIVE_CONFIG, KEY_CUSTOM_GATEWAY, KEY_CUSTOM_GATEWAY_TOKEN,
};

/// The user object as tabby-web's `UserSerializer` renders it
/// (`backend/tabby/app/api/user.py`). `is_pro` is always true — upstream gates
/// features on it and we have no sponsorship backend. `config_sync_token`
/// echoes the presented token back, also as upstream does.
#[derive(Serialize)]
pub struct UserJson {
    pub id: i64,
    pub username: String,
    pub active_config: Option<i64>,
    pub custom_connection_gateway: Option<String>,
    pub custom_connection_gateway_token: Option<String>,
    pub config_sync_token: String,
    pub is_pro: bool,
    pub is_sponsor: bool,
    pub github_username: Option<String>,
}

impl UserJson {
    fn load(state: &AppState, conn: &Connection) -> Result<Self, ApiError> {
        Ok(Self {
            id: USER_ID,
            username: state.username.to_string(),
            active_config: active_config(conn)?,
            custom_connection_gateway: meta_get(conn, KEY_CUSTOM_GATEWAY)?,
            custom_connection_gateway_token: meta_get(conn, KEY_CUSTOM_GATEWAY_TOKEN)?,
            config_sync_token: state.token.to_string(),
            is_pro: true,
            is_sponsor: false,
            github_username: None,
        })
    }
}

pub async fn route(State(state): State<AppState>, req: Request) -> Result<Response, ApiError> {
    match req.method().as_str() {
        "GET" | "HEAD" => {
            let conn = state.db.lock()?;
            Ok(Json(UserJson::load(&state, &conn)?).into_response())
        }
        "PUT" => {
            let body = read_body(req, state.max_body_bytes).await?;
            update(&state, parse_json_object(&body)?)
        }
        _ => Err(ApiError::MethodNotAllowed(req.method().to_string())),
    }
}

/// `id` and `username` are read-only upstream and the `is_*` /
/// `github_username` fields are derived, so only the three persisted writable
/// fields are honoured; anything else is ignored rather than rejected.
fn update(state: &AppState, body: Option<Map<String, Value>>) -> Result<Response, ApiError> {
    let conn = state.db.lock()?;

    if let Some(active_config) = parse_active_config(body.as_ref(), &conn)? {
        match active_config {
            Some(id) => meta_set(&conn, KEY_ACTIVE_CONFIG, Some(&id.to_string()))?,
            None => meta_set(&conn, KEY_ACTIVE_CONFIG, None)?,
        }
    }
    if let Some(gateway) = nullable_string_field(body.as_ref(), "custom_connection_gateway")? {
        meta_set(&conn, KEY_CUSTOM_GATEWAY, gateway.as_deref())?;
    }
    if let Some(token) = nullable_string_field(body.as_ref(), "custom_connection_gateway_token")? {
        meta_set(&conn, KEY_CUSTOM_GATEWAY_TOKEN, token.as_deref())?;
    }

    Ok(Json(UserJson::load(state, &conn)?).into_response())
}

fn active_config(conn: &Connection) -> Result<Option<i64>, ApiError> {
    match meta_get(conn, KEY_ACTIVE_CONFIG)? {
        Some(raw) => Ok(raw.parse().ok()),
        None => Ok(None),
    }
}

/// `None` when the key is absent; otherwise the new value, where the inner
/// `None` clears it.
fn parse_active_config(
    body: Option<&Map<String, Value>>,
    conn: &Connection,
) -> Result<Option<Option<i64>>, ApiError> {
    let field = "active_config";
    let invalid = || validation_error(field, "A valid integer is required.");

    let Some(value) = field_value(body, field) else {
        return Ok(None);
    };

    let (id, rendered) = match value {
        Value::Null => return Ok(Some(None)),
        Value::Number(number) => match number.as_i64() {
            Some(id) => (id, id.to_string()),
            None => return Err(invalid()),
        },
        Value::String(text) => match text.trim().parse::<i64>() {
            Ok(id) => (id, text.trim().to_owned()),
            Err(_) => return Err(invalid()),
        },
        _ => return Err(invalid()),
    };

    if !configs::exists(conn, id)? {
        // DRF's PrimaryKeyRelatedField, see `rest_framework/relations.py`.
        return Err(validation_error(
            field,
            &format!("Invalid pk \"{rendered}\" - object does not exist."),
        ));
    }

    Ok(Some(Some(id)))
}
