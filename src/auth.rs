use axum::extract::{Request, State};
use axum::http::{HeaderMap, Uri};
use axum::middleware::Next;
use axum::response::Response;
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq;

use crate::api::error::ApiError;
use crate::api::AppState;

/// Query parameter tabby-web's `TokenMiddleware` accepts in addition to the
/// `Authorization` header (see `backend/tabby/middleware.py`).
pub const TOKEN_QUERY_PARAM: &str = "auth_token";

fn digest(value: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hasher.finalize().into()
}

/// The configured token, kept only as a SHA-256 digest so that comparisons leak
/// neither the value nor the length of the token through timing.
#[derive(Clone)]
pub struct TokenSecret {
    digest: [u8; 32],
}

impl std::fmt::Debug for TokenSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSecret").finish_non_exhaustive()
    }
}

impl TokenSecret {
    pub fn new(token: &str) -> Self {
        Self {
            digest: digest(token),
        }
    }

    pub fn verify(&self, candidate: &str) -> bool {
        self.digest.ct_eq(&digest(candidate)).into()
    }
}

/// Extracts a candidate token exactly the way upstream's `TokenMiddleware`
/// does: `?auth_token=` first, then an `Authorization: Bearer <token>` header
/// (which wins when both are present and well formed).
///
/// The scheme is matched case-insensitively; upstream splits on whitespace and
/// compares `token_type == "Bearer"`, so a trailing space in the header is
/// already gone by the time the credential is taken. Query values are used
/// verbatim (percent-decoded), as Django's `request.GET` does.
pub fn extract_token(headers: &HeaderMap, uri: &Uri) -> Option<String> {
    let mut candidate = query_token(uri);

    if let Some(value) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        let mut parts = value.split_whitespace();
        if let (Some(scheme), Some(credential), None) = (parts.next(), parts.next(), parts.next()) {
            if scheme.eq_ignore_ascii_case("bearer") {
                candidate = Some(credential.to_owned());
            }
        }
    }

    candidate
}

fn query_token(uri: &Uri) -> Option<String> {
    let query = uri.query()?;
    form_urlencoded::parse(query.as_bytes())
        .filter(|(key, _)| key == TOKEN_QUERY_PARAM)
        .last()
        .map(|(_, value)| value.into_owned())
}

pub async fn require_token(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    if !is_api_path(req.uri().path()) {
        return Ok(next.run(req).await);
    }

    let presented = extract_token(req.headers(), req.uri());
    match presented {
        Some(token) if state.secret.verify(&token) => Ok(next.run(req).await),
        _ => Err(ApiError::Unauthorized),
    }
}

pub fn is_api_path(path: &str) -> bool {
    path == "/api/1" || path.starts_with("/api/1/")
}
