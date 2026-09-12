//! Shared harness for the wire-contract tests.
//!
//! Each test builds its own router over a private in-memory SQLite database and
//! drives it through `tower::ServiceExt::oneshot`, so the tests are parallel
//! safe and never touch the operator's real token or database.

#![allow(dead_code)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderName, Method, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt as _;
use serde_json::Value;
use tabby_alt_sync::api::{self, AppState};
use tabby_alt_sync::auth::TokenSecret;
use tabby_alt_sync::db::Db;
use tower::ServiceExt as _;

pub const TOKEN: &str = "test-token-0123456789abcdef";
pub const DEFAULT_BODY_LIMIT: usize = 8 * 1024 * 1024;
pub const TOKEN_QUERY_PARAM: &str = "auth_token";

pub struct TestApp {
    pub router: Router,
    pub token: &'static str,
}

pub fn app() -> TestApp {
    app_with_body_limit(DEFAULT_BODY_LIMIT)
}

pub fn app_with_body_limit(max_body_bytes: usize) -> TestApp {
    let state = AppState {
        db: Db::open_in_memory().expect("in-memory database"),
        secret: Arc::new(TokenSecret::new(TOKEN)),
        token: Arc::from(TOKEN),
        username: Arc::from("tabby"),
        max_body_bytes,
    };
    TestApp {
        router: api::router(state),
        token: TOKEN,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Auth {
    /// `Authorization: Bearer <token>` — what the Tabby client sends.
    Header,
    /// `?auth_token=<token>` — the form tabby-web's middleware also accepts.
    Query,
    None,
}

pub struct Call {
    method: Method,
    path: String,
    body: Option<String>,
    auth: Auth,
    authorization: Option<String>,
    content_type: Option<String>,
    content_length: Option<usize>,
    origin: Option<String>,
}

impl Call {
    pub fn new(method: Method, path: impl Into<String>) -> Self {
        Self {
            method,
            path: path.into(),
            body: None,
            auth: Auth::Header,
            authorization: None,
            content_type: None,
            content_length: None,
            origin: None,
        }
    }

    pub fn get(path: impl Into<String>) -> Self {
        Self::new(Method::GET, path)
    }

    pub fn post(path: impl Into<String>) -> Self {
        Self::new(Method::POST, path)
    }

    pub fn put(path: impl Into<String>) -> Self {
        Self::new(Method::PUT, path)
    }

    pub fn patch(path: impl Into<String>) -> Self {
        Self::new(Method::PATCH, path)
    }

    pub fn delete(path: impl Into<String>) -> Self {
        Self::new(Method::DELETE, path)
    }

    pub fn body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into());
        self
    }

    pub fn json(mut self, value: &Value) -> Self {
        self.body = Some(value.to_string());
        self.content_type = Some("application/json".to_owned());
        self
    }

    pub fn auth(mut self, auth: Auth) -> Self {
        self.auth = auth;
        self
    }

    pub fn no_auth(self) -> Self {
        self.auth(Auth::None)
    }

    /// Sets a raw `Authorization` value verbatim, bypassing `auth`.
    pub fn authorization(mut self, value: impl Into<String>) -> Self {
        self.authorization = Some(value.into());
        self.auth = Auth::None;
        self
    }

    /// Sets `Content-Length` explicitly, to exercise the pre-emptive limit.
    pub fn content_length(mut self, value: usize) -> Self {
        self.content_length = Some(value);
        self
    }

    pub fn content_type(mut self, value: impl Into<String>) -> Self {
        self.content_type = Some(value.into());
        self
    }

    pub fn origin(mut self, value: impl Into<String>) -> Self {
        self.origin = Some(value.into());
        self
    }

    fn uri(&self, token: &str) -> String {
        match self.auth {
            Auth::Query => {
                let separator = if self.path.contains('?') { '&' } else { '?' };
                format!("{}{separator}{TOKEN_QUERY_PARAM}={token}", self.path)
            }
            _ => self.path.clone(),
        }
    }
}

#[derive(Debug)]
pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub bytes: Vec<u8>,
}

impl Reply {
    pub fn body(&self) -> String {
        String::from_utf8(self.bytes.clone()).expect("response body is utf-8")
    }

    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body()).expect("response body is json")
    }

    pub fn header(&self, name: HeaderName) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    pub fn detail(&self) -> String {
        self.json()["detail"]
            .as_str()
            .expect("detail is a string")
            .to_owned()
    }
}

pub async fn send(app: &TestApp, call: Call) -> Reply {
    let mut builder = Request::builder()
        .method(call.method.clone())
        .uri(call.uri(app.token));

    if let Some(value) = call.authorization.or_else(|| match call.auth {
        Auth::Header => Some(format!("Bearer {}", app.token)),
        Auth::Query | Auth::None => None,
    }) {
        builder = builder.header(header::AUTHORIZATION, value);
    }
    if let Some(value) = call.content_type {
        builder = builder.header(header::CONTENT_TYPE, value);
    }
    if let Some(value) = call.content_length {
        builder = builder.header(header::CONTENT_LENGTH, value);
    }
    if let Some(value) = call.origin {
        builder = builder.header(header::ORIGIN, value);
    }

    let request = builder
        .body(match call.body {
            Some(body) => Body::from(body),
            None => Body::empty(),
        })
        .expect("request is valid");

    let response = app
        .router
        .clone()
        .oneshot(request)
        .await
        .expect("router never fails at the transport level");

    let (parts, body) = response.into_parts();
    let bytes = body
        .collect()
        .await
        .expect("body is readable")
        .to_bytes()
        .to_vec();

    Reply {
        status: parts.status,
        headers: parts.headers,
        bytes,
    }
}

/// Creates a config through the API and returns its id.
pub async fn create_config(app: &TestApp, name: &str) -> i64 {
    let reply = send(
        app,
        Call::post("/api/1/configs").json(&serde_json::json!({ "name": name })),
    )
    .await;
    assert_eq!(
        reply.status,
        StatusCode::CREATED,
        "create failed: {reply:?}"
    );
    reply.json()["id"].as_i64().expect("id is an integer")
}

pub async fn sleep_past_clock_tick() {
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
}
