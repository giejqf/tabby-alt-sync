//! The wire contract from `AGENTS.md`, endpoint by endpoint.
//!
//! Upstream references (Eugeny/tabby-web `backend/tabby/…`, tabby
//! `tabby-settings/src/services/configSync.service.ts`) are cited per test.

mod common;

use axum::http::{header, Method, StatusCode};
use common::*;
use regex::Regex;
use serde_json::json;

/// DRF renders aware datetimes as `YYYY-MM-DDTHH:MM:SS.ffffffZ`
/// (`rest_framework/fields.py`, `DateTimeField.to_representation`).
fn assert_drf_datetime(value: &str) {
    let pattern =
        Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{6}Z$").expect("valid regex");
    assert!(pattern.is_match(value), "{value:?} is not a DRF datetime");
}

fn config_keys() -> Vec<String> {
    let mut keys: Vec<String> = [
        "id",
        "user",
        "name",
        "content",
        "last_used_with_version",
        "created_at",
        "modified_at",
    ]
    .map(str::to_owned)
    .to_vec();
    keys.sort();
    keys
}

fn user_keys() -> Vec<String> {
    let mut keys: Vec<String> = [
        "id",
        "username",
        "active_config",
        "custom_connection_gateway",
        "custom_connection_gateway_token",
        "config_sync_token",
        "is_pro",
        "is_sponsor",
        "github_username",
    ]
    .map(str::to_owned)
    .to_vec();
    keys.sort();
    keys
}

fn keys_of(value: &serde_json::Value) -> Vec<String> {
    let mut keys: Vec<String> = value.as_object().expect("object").keys().cloned().collect();
    keys.sort();
    keys
}

// ---------------------------------------------------------------- auth

fn endpoints() -> Vec<(Method, &'static str)> {
    vec![
        (Method::GET, "/api/1/user"),
        (Method::PUT, "/api/1/user"),
        (Method::GET, "/api/1/configs"),
        (Method::POST, "/api/1/configs"),
        (Method::GET, "/api/1/configs/1"),
        (Method::PUT, "/api/1/configs/1"),
        (Method::PATCH, "/api/1/configs/1"),
        (Method::DELETE, "/api/1/configs/1"),
        (Method::GET, "/api/1/versions"),
    ]
}

#[tokio::test]
async fn every_api_route_requires_a_token() {
    let app = app();
    for (method, path) in endpoints() {
        let reply = send(&app, Call::new(method.clone(), path).no_auth()).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{method} {path}");
        assert_eq!(reply.header(header::WWW_AUTHENTICATE), Some("Bearer"));
        assert_eq!(reply.json(), json!({ "detail": "Invalid token." }));
    }
}

#[tokio::test]
async fn wrong_token_is_rejected() {
    let app = app();
    let reply = send(
        &app,
        Call::get("/api/1/user").authorization("Bearer not-the-token-0000"),
    )
    .await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(reply.json(), json!({ "detail": "Invalid token." }));
}

#[tokio::test]
async fn wrong_scheme_is_rejected() {
    let app = app();
    for header in [
        format!("Basic {TOKEN}"),
        format!("Token {TOKEN}"),
        format!("Bearer{TOKEN}"),
        "Bearer".to_owned(),
        "Bearer ".to_owned(),
    ] {
        let reply = send(&app, Call::get("/api/1/user").authorization(&header)).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{header}");
    }
}

/// Upstream compares `token_type == "Bearer"` exactly; accepting any case can
/// only ever help a client.
#[tokio::test]
async fn scheme_comparison_is_case_insensitive() {
    let app = app();
    for header in [format!("bearer {TOKEN}"), format!("BEARER {TOKEN}")] {
        let reply = send(&app, Call::get("/api/1/user").authorization(&header)).await;
        assert_eq!(reply.status, StatusCode::OK);
    }
}

/// `TokenMiddleware` splits the header on whitespace, so trailing whitespace
/// never reaches the comparison.
#[tokio::test]
async fn trailing_whitespace_in_the_header_is_tolerated() {
    let app = app();
    let reply = send(
        &app,
        Call::get("/api/1/user").authorization(format!("Bearer {TOKEN}  ")),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
}

/// …while a query parameter is compared verbatim, so the same trailing space
/// there is simply part of a wrong token.
#[tokio::test]
async fn trailing_whitespace_in_the_query_is_not_tolerated() {
    let app = app();
    let reply = send(&app, Call::get("/api/1/user?auth_token=%20").no_auth()).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn token_in_query_parameter_is_accepted() {
    let app = app();
    let reply = send(&app, Call::get("/api/1/user").auth(Auth::Query)).await;
    assert_eq!(reply.status, StatusCode::OK);

    let created = create_config(&app, "query auth").await;
    let reply = send(
        &app,
        Call::patch(format!("/api/1/configs/{created}"))
            .auth(Auth::Query)
            .json(&json!({ "content": "version: 4\n" })),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
}

/// The header wins when both are present, matching the order in
/// `TokenMiddleware`.
#[tokio::test]
async fn header_takes_precedence_over_query() {
    let app = app();
    let reply = send(
        &app,
        Call::get("/api/1/user?auth_token=wrong-token").authorization(format!("Bearer {TOKEN}")),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
}

// ---------------------------------------------------------------- user

#[tokio::test]
async fn user_object_has_the_exact_shape() {
    let app = app();
    let reply = send(&app, Call::get("/api/1/user")).await;

    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.header(header::CONTENT_TYPE), Some("application/json"));
    assert_eq!(keys_of(&reply.json()), user_keys());

    let user = reply.json();
    assert_eq!(user["id"], json!(1));
    assert_eq!(user["username"], json!("tabby"));
    assert_eq!(user["active_config"], json!(null));
    assert_eq!(user["custom_connection_gateway"], json!(null));
    assert_eq!(user["custom_connection_gateway_token"], json!(null));
    assert_eq!(user["config_sync_token"], json!(TOKEN));
    assert_eq!(user["is_pro"], json!(true));
    assert_eq!(user["is_sponsor"], json!(false));
    assert_eq!(user["github_username"], json!(null));
}

#[tokio::test]
async fn put_user_sets_active_config_and_ignores_read_only_fields() {
    let app = app();
    let id = create_config(&app, "active one").await;

    let reply = send(
        &app,
        Call::put("/api/1/user").json(&json!({
            "id": 999,
            "username": "attacker",
            "config_sync_token": "swapped",
            "is_pro": false,
            "is_sponsor": true,
            "github_username": "ghost",
            "totally_unknown": [1, 2, 3],
            "active_config": id,
        })),
    )
    .await;

    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(keys_of(&reply.json()), user_keys());
    let user = reply.json();
    assert_eq!(user["id"], json!(1));
    assert_eq!(user["username"], json!("tabby"));
    assert_eq!(user["config_sync_token"], json!(TOKEN));
    assert_eq!(user["is_pro"], json!(true));
    assert_eq!(user["is_sponsor"], json!(false));
    assert_eq!(user["github_username"], json!(null));
    assert_eq!(user["active_config"], json!(id));
}

#[tokio::test]
async fn put_user_active_config_accepts_null_and_numbers_as_strings() {
    let app = app();
    let id = create_config(&app, "active two").await;

    let reply = send(
        &app,
        Call::put("/api/1/user").json(&json!({ "active_config": id.to_string() })),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json()["active_config"], json!(id));

    let reply = send(
        &app,
        Call::put("/api/1/user").json(&json!({ "active_config": null })),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json()["active_config"], json!(null));
}

#[tokio::test]
async fn put_user_rejects_unknown_active_config_with_drf_message() {
    let app = app();
    let reply = send(
        &app,
        Call::put("/api/1/user").json(&json!({ "active_config": 4242 })),
    )
    .await;

    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        reply.json(),
        json!({ "active_config": ["Invalid pk \"4242\" - object does not exist."] })
    );
}

#[tokio::test]
async fn put_user_rejects_non_integer_active_config() {
    let app = app();
    for value in [
        json!("not-a-number"),
        json!(true),
        json!([1]),
        json!({"a": 1}),
        json!(1.5),
    ] {
        let reply = send(
            &app,
            Call::put("/api/1/user").json(&json!({ "active_config": value })),
        )
        .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{value}");
        assert_eq!(
            reply.json(),
            json!({ "active_config": ["A valid integer is required."] })
        );
    }
}

#[tokio::test]
async fn put_user_custom_gateway_fields_round_trip() {
    let app = app();

    let reply = send(
        &app,
        Call::put("/api/1/user").json(&json!({
            "custom_connection_gateway": "gateway.example.com",
            "custom_connection_gateway_token": "s3cret-gateway",
        })),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply.json()["custom_connection_gateway"],
        json!("gateway.example.com")
    );
    assert_eq!(
        reply.json()["custom_connection_gateway_token"],
        json!("s3cret-gateway")
    );

    let reply = send(&app, Call::get("/api/1/user")).await;
    assert_eq!(
        reply.json()["custom_connection_gateway"],
        json!("gateway.example.com")
    );

    let reply = send(
        &app,
        Call::put("/api/1/user").json(&json!({ "custom_connection_gateway": null })),
    )
    .await;
    assert_eq!(reply.json()["custom_connection_gateway"], json!(null));
    assert_eq!(
        reply.json()["custom_connection_gateway_token"],
        json!("s3cret-gateway")
    );
}

#[tokio::test]
async fn deleting_the_active_config_clears_it() {
    let app = app();
    let id = create_config(&app, "doomed").await;
    send(
        &app,
        Call::put("/api/1/user").json(&json!({ "active_config": id })),
    )
    .await;

    let reply = send(&app, Call::delete(format!("/api/1/configs/{id}"))).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);

    let reply = send(&app, Call::get("/api/1/user")).await;
    assert_eq!(reply.json()["active_config"], json!(null));
}

#[tokio::test]
async fn user_route_rejects_other_methods() {
    let app = app();
    for method in [Method::POST, Method::DELETE, Method::OPTIONS] {
        let reply = send(&app, Call::new(method.clone(), "/api/1/user").no_auth()).await;
        // OPTIONS is answered by the CORS preflight handler before auth.
        if method == Method::OPTIONS {
            assert_eq!(reply.status, StatusCode::NO_CONTENT, "{method}");
            continue;
        }
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{method}");
    }

    for method in [Method::POST, Method::DELETE] {
        let reply = send(&app, Call::new(method.clone(), "/api/1/user")).await;
        assert_eq!(reply.status, StatusCode::METHOD_NOT_ALLOWED, "{method}");
        assert_eq!(reply.detail(), format!("Method \"{method}\" not allowed."));
    }
}

// ---------------------------------------------------------------- configs

#[tokio::test]
async fn list_is_a_bare_json_array() {
    let app = app();

    let reply = send(&app, Call::get("/api/1/configs")).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body(), "[]");

    create_config(&app, "one").await;
    create_config(&app, "two").await;

    let reply = send(&app, Call::get("/api/1/configs")).await;
    let list = reply.json();
    assert!(list.is_array(), "expected a bare array, got {list}");
    assert_eq!(list.as_array().expect("array").len(), 2);
    assert_eq!(list[0]["name"], json!("one"));
    assert_eq!(list[1]["name"], json!("two"));
    assert!(list.get("count").is_none());
    assert!(list.get("results").is_none());
}

#[tokio::test]
async fn config_object_has_the_exact_shape() {
    let app = app();
    let id = create_config(&app, "New config on darwin").await;

    let reply = send(&app, Call::get(format!("/api/1/configs/{id}"))).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.header(header::CONTENT_TYPE), Some("application/json"));

    let keys = keys_of(&reply.json());
    assert_eq!(keys, config_keys());
    assert_drf_datetime(reply.json()["created_at"].as_str().expect("created_at"));
    assert_drf_datetime(reply.json()["modified_at"].as_str().expect("modified_at"));
}

#[tokio::test]
async fn post_applies_upstream_defaults() {
    let app = app();
    let reply = send(
        &app,
        Call::post("/api/1/configs").json(&json!({ "name": "from the client" })),
    )
    .await;

    assert_eq!(reply.status, StatusCode::CREATED);
    let config = reply.json();
    assert_eq!(keys_of(&config), config_keys());
    assert_eq!(config["id"].as_i64().expect("id is an integer"), 1);
    assert_eq!(config["user"], json!(1));
    assert_eq!(config["name"], json!("from the client"));
    assert_eq!(config["content"], json!("{}"));
    assert_eq!(config["last_used_with_version"], json!(null));
    assert_eq!(config["created_at"], config["modified_at"]);
}

#[tokio::test]
async fn post_without_a_usable_name_falls_back_to_the_generated_one() {
    let app = app();
    let pattern = Regex::new(r"^Unnamed config \(\d{4}-\d{2}-\d{2}\)$").expect("valid regex");

    for body in [json!({}), json!({ "name": "" }), json!({ "name": "   " })] {
        let reply = send(&app, Call::post("/api/1/configs").json(&body)).await;
        assert_eq!(reply.status, StatusCode::CREATED, "{body}");
        let name = reply.json()["name"].as_str().expect("name").to_owned();
        assert!(pattern.is_match(&name), "{body} produced {name:?}");
    }

    // An empty body carries no fields at all, like DRF with no parsed data.
    let reply = send(&app, Call::post("/api/1/configs")).await;
    assert_eq!(reply.status, StatusCode::CREATED);
    assert!(pattern.is_match(reply.json()["name"].as_str().expect("name")));
}

#[tokio::test]
async fn post_coerces_and_validates_name_like_a_drf_char_field() {
    let app = app();

    let reply = send(
        &app,
        Call::post("/api/1/configs").json(&json!({ "name": 42 })),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    assert_eq!(reply.json()["name"], json!("42"));

    let reply = send(
        &app,
        Call::post("/api/1/configs").json(&json!({ "name": "  spaced  " })),
    )
    .await;
    assert_eq!(reply.json()["name"], json!("spaced"));

    for value in [json!(true), json!(["a"]), json!({"a": 1})] {
        let reply = send(
            &app,
            Call::post("/api/1/configs").json(&json!({ "name": value })),
        )
        .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{value}");
        assert_eq!(reply.json(), json!({ "name": ["Not a valid string."] }));
    }

    let reply = send(
        &app,
        Call::post("/api/1/configs").json(&json!({ "name": null })),
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        reply.json(),
        json!({ "name": ["This field may not be null."] })
    );
}

#[tokio::test]
async fn post_accepts_content_and_version_too() {
    let app = app();
    let reply = send(
        &app,
        Call::post("/api/1/configs").json(&json!({
            "name": "seeded",
            "content": "version: 4\n",
            "last_used_with_version": "1.0.235",
        })),
    )
    .await;

    assert_eq!(reply.status, StatusCode::CREATED);
    assert_eq!(reply.json()["content"], json!("version: 4\n"));
    assert_eq!(reply.json()["last_used_with_version"], json!("1.0.235"));
}

#[tokio::test]
async fn patch_is_partial_and_touches_only_modified_at() {
    let app = app();
    let id = create_config(&app, "keep me").await;
    let created = send(&app, Call::get(format!("/api/1/configs/{id}")))
        .await
        .json();

    sleep_past_clock_tick().await;
    let reply = send(
        &app,
        Call::patch(format!("/api/1/configs/{id}")).json(&json!({
            "content": "version: 4\nprofiles: []\n",
            "last_used_with_version": "1.0.235",
        })),
    )
    .await;

    assert_eq!(reply.status, StatusCode::OK);
    let patched = reply.json();
    assert_eq!(patched["name"], json!("keep me"));
    assert_eq!(patched["content"], json!("version: 4\nprofiles: []\n"));
    assert_eq!(patched["last_used_with_version"], json!("1.0.235"));
    assert_eq!(patched["created_at"], created["created_at"]);
    assert_ne!(patched["modified_at"], created["modified_at"]);
    assert!(
        patched["modified_at"].as_str().expect("modified_at")
            > created["created_at"].as_str().expect("created_at")
    );
}

#[tokio::test]
async fn patch_ignores_read_only_and_unknown_fields() {
    let app = app();
    let id = create_config(&app, "untouchable").await;
    let before = send(&app, Call::get(format!("/api/1/configs/{id}")))
        .await
        .json();

    let reply = send(
        &app,
        Call::patch(format!("/api/1/configs/{id}")).json(&json!({
            "id": 4242,
            "user": 4242,
            "created_at": "1999-01-01T00:00:00.000000Z",
            "modified_at": "1999-01-01T00:00:00.000000Z",
            "something_unknown": "ignored",
            "content": "changed",
        })),
    )
    .await;

    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(keys_of(&reply.json()), config_keys());
    let after = reply.json();
    assert_eq!(after["id"], json!(id));
    assert_eq!(after["user"], json!(1));
    assert_eq!(after["created_at"], before["created_at"]);
    assert_eq!(after["content"], json!("changed"));
}

#[tokio::test]
async fn patch_sets_explicit_nulls() {
    let app = app();
    let id = create_config(&app, "versioned").await;
    send(
        &app,
        Call::patch(format!("/api/1/configs/{id}"))
            .json(&json!({ "last_used_with_version": "1.0.235" })),
    )
    .await;

    let reply = send(
        &app,
        Call::patch(format!("/api/1/configs/{id}"))
            .json(&json!({ "last_used_with_version": null })),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json()["last_used_with_version"], json!(null));
}

#[tokio::test]
async fn put_behaves_exactly_like_patch() {
    let app = app();
    let id = create_config(&app, "via put").await;
    let before = send(&app, Call::get(format!("/api/1/configs/{id}")))
        .await
        .json();

    sleep_past_clock_tick().await;
    let reply = send(
        &app,
        Call::put(format!("/api/1/configs/{id}")).json(&json!({ "content": "put content" })),
    )
    .await;

    assert_eq!(reply.status, StatusCode::OK);
    let after = reply.json();
    assert_eq!(after["name"], before["name"]);
    assert_eq!(after["content"], json!("put content"));
    assert_eq!(after["last_used_with_version"], json!(null));
    assert_eq!(after["created_at"], before["created_at"]);
    assert_ne!(after["modified_at"], before["modified_at"]);
}

#[tokio::test]
async fn patch_without_content_type_is_still_understood() {
    let app = app();
    let id = create_config(&app, "no content type").await;

    let reply = send(
        &app,
        Call::patch(format!("/api/1/configs/{id}")).body(json!({ "content": "raw" }).to_string()),
    )
    .await;

    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json()["content"], json!("raw"));
}

#[tokio::test]
async fn patch_with_charset_content_type_is_understood() {
    let app = app();
    let id = create_config(&app, "charset").await;

    let reply = send(
        &app,
        Call::patch(format!("/api/1/configs/{id}"))
            .content_type("application/json; charset=utf-8")
            .body(json!({ "content": "with charset" }).to_string()),
    )
    .await;

    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json()["content"], json!("with charset"));
}

#[tokio::test]
async fn unknown_id_is_not_found_on_every_method() {
    let app = app();
    for method in [Method::GET, Method::PUT, Method::PATCH, Method::DELETE] {
        let reply = send(
            &app,
            Call::new(method.clone(), "/api/1/configs/4242").body("{}"),
        )
        .await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{method}");
        assert_eq!(reply.json(), json!({ "detail": "Not found." }));
    }
}

#[tokio::test]
async fn non_numeric_id_is_not_found() {
    let app = app();
    for path in [
        "/api/1/configs/abc",
        "/api/1/configs/1.5",
        "/api/1/configs/-1",
        "/api/1/configs/99999999999999999999999",
    ] {
        let reply = send(&app, Call::get(path)).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(reply.json(), json!({ "detail": "Not found." }));
    }
}

#[tokio::test]
async fn delete_returns_204_with_a_completely_empty_body() {
    let app = app();
    let id = create_config(&app, "to delete").await;

    let reply = send(&app, Call::delete(format!("/api/1/configs/{id}"))).await;

    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert!(
        reply.bytes.is_empty(),
        "204 must carry no body, got {:?}",
        reply.body()
    );
    assert_eq!(reply.header(header::CONTENT_TYPE), None);

    let reply = send(&app, Call::get(format!("/api/1/configs/{id}"))).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn ids_are_never_reused() {
    let app = app();
    let first = create_config(&app, "first").await;
    send(&app, Call::delete(format!("/api/1/configs/{first}"))).await;

    let second = create_config(&app, "second").await;
    assert_ne!(second, first);
    assert!(second > first);
}

#[tokio::test]
async fn wrong_method_on_an_existing_route_is_405() {
    let app = app();
    let id = create_config(&app, "method check").await;

    let cases = [
        (Method::DELETE, "/api/1/configs".to_owned()),
        (Method::PUT, "/api/1/configs".to_owned()),
        (Method::PATCH, "/api/1/configs".to_owned()),
        (Method::POST, format!("/api/1/configs/{id}")),
        (Method::DELETE, "/api/1/user".to_owned()),
        (Method::POST, "/api/1/user".to_owned()),
    ];
    for (method, path) in cases {
        let reply = send(&app, Call::new(method.clone(), &path).body("{}")).await;
        assert_eq!(
            reply.status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} {path}"
        );
        assert_eq!(
            reply.json(),
            json!({ "detail": format!("Method \"{method}\" not allowed.") })
        );
    }
}

// ---------------------------------------------------------------- bodies

#[tokio::test]
async fn malformed_json_is_a_drf_parse_error() {
    let app = app();
    let id = create_config(&app, "parse").await;

    for body in ["{", "{\"name\": ", "not json at all", "[1, 2"] {
        let reply = send(
            &app,
            Call::post("/api/1/configs")
                .content_type("application/json")
                .body(body),
        )
        .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{body:?}");
        let detail = reply.json()["detail"].as_str().expect("detail").to_owned();
        assert!(
            detail.starts_with("JSON parse error - "),
            "unexpected detail {detail:?}"
        );

        let reply = send(
            &app,
            Call::patch(format!("/api/1/configs/{id}"))
                .content_type("application/json")
                .body(body),
        )
        .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{body:?}");
    }
}

#[tokio::test]
async fn valid_json_that_is_not_an_object_is_a_validation_error() {
    let app = app();

    for (body, datatype) in [
        ("[1, 2, 3]", "list"),
        ("null", "NoneType"),
        ("\"just a string\"", "str"),
        ("42", "int"),
        ("true", "bool"),
    ] {
        let reply = send(
            &app,
            Call::post("/api/1/configs")
                .content_type("application/json")
                .body(body),
        )
        .await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(
            reply.json(),
            json!({ "non_field_errors": [format!("Invalid data. Expected a dictionary, but got {datatype}.")] })
        );
    }
}

#[tokio::test]
async fn content_round_trips_byte_for_byte() {
    let app = app();
    let big = "version: 4\n".repeat(120_000); // > 1 MiB
    assert!(big.len() > 1024 * 1024);

    let cases: Vec<(&str, String)> = vec![
        ("empty", String::new()),
        ("plain", "version: 4\nprofiles: []\n".to_owned()),
        (
            "crlf",
            "version: 4\r\nprofiles:\r\n  - name: a\r\n".to_owned(),
        ),
        ("non-ascii", "name: Ünïcödé 中文 ☃\n".to_owned()),
        ("emoji", "name: 🦊🔐🖥️\n".to_owned()),
        (
            "control chars",
            "a\tb\u{1}\u{2}\u{1b}[0m\u{7f}\n".to_owned(),
        ),
        (
            "quotes and backslashes",
            "path: \"C:\\tmp\"\nquote: 'x'\n".to_owned(),
        ),
        ("huge", big.clone()),
    ];

    for (label, content) in cases {
        let id = create_config(&app, label).await;

        let reply = send(
            &app,
            Call::patch(format!("/api/1/configs/{id}")).json(&json!({ "content": content })),
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK, "{label}");
        assert_eq!(
            reply.json()["content"].as_str().expect("content"),
            content,
            "{label} changed through PATCH"
        );

        let reply = send(&app, Call::get(format!("/api/1/configs/{id}"))).await;
        assert_eq!(
            reply.json()["content"].as_str().expect("content"),
            content,
            "{label} changed through GET"
        );
    }
}

/// An oversized body is refused whether or not the client announced it.
#[tokio::test]
async fn oversized_body_is_rejected() {
    let app = app_with_body_limit(4096);
    let id = create_config(&app, "small").await;

    let big = json!({ "content": "x".repeat(64 * 1024) }).to_string();

    let reply = send(
        &app,
        Call::patch(format!("/api/1/configs/{id}"))
            .content_type("application/json")
            .content_length(big.len())
            .body(big.clone()),
    )
    .await;
    assert_eq!(reply.status, StatusCode::PAYLOAD_TOO_LARGE);

    let reply = send(
        &app,
        Call::patch(format!("/api/1/configs/{id}"))
            .content_type("application/json")
            .body(big),
    )
    .await;
    assert_eq!(reply.status, StatusCode::PAYLOAD_TOO_LARGE);

    // Bodies at or below the limit still work.
    let small = json!({ "content": "x".repeat(2048) }).to_string();
    let reply = send(
        &app,
        Call::patch(format!("/api/1/configs/{id}"))
            .content_type("application/json")
            .body(small),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
}

// ---------------------------------------------------------------- routing

#[tokio::test]
async fn trailing_slashes_are_accepted_without_redirecting() {
    let app = app();

    let reply = send(
        &app,
        Call::post("/api/1/configs/").json(&json!({ "name": "slashed" })),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    assert_eq!(reply.json()["name"], json!("slashed"));

    let id = reply.json()["id"].as_i64().expect("id");

    for (method, path) in [
        (Method::GET, "/api/1/configs/".to_owned()),
        (Method::GET, format!("/api/1/configs/{id}/")),
        (Method::PUT, format!("/api/1/configs/{id}/")),
        (Method::PATCH, format!("/api/1/configs/{id}/")),
        (Method::GET, "/api/1/user/".to_owned()),
        (Method::PUT, "/api/1/user/".to_owned()),
    ] {
        let reply = send(&app, Call::new(method.clone(), &path).body("{}")).await;
        assert!(
            reply.status == StatusCode::OK,
            "{method} {path} returned {}",
            reply.status
        );
    }

    let reply = send(&app, Call::delete(format!("/api/1/configs/{id}/"))).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
}

/// DRF serves HEAD wherever it serves GET. The body itself is omitted by hyper,
/// so only the status line and headers are asserted here.
#[tokio::test]
async fn head_is_served_like_get() {
    let app = app();
    let id = create_config(&app, "headable").await;

    for path in [
        "/api/1/user".to_owned(),
        "/api/1/configs".to_owned(),
        format!("/api/1/configs/{id}"),
    ] {
        let reply = send(&app, Call::new(Method::HEAD, &path)).await;
        assert_eq!(reply.status, StatusCode::OK, "HEAD {path}");
        assert_eq!(reply.header(header::CONTENT_TYPE), Some("application/json"));
    }

    let reply = send(&app, Call::new(Method::HEAD, "/api/1/configs/4242")).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);

    let reply = send(&app, Call::new(Method::HEAD, "/api/1/user").no_auth()).await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn everything_else_under_api_is_not_found() {
    let app = app();
    for path in [
        "/api/1",
        "/api/1/versions",
        "/api/1/versions/",
        "/api/1/gateways/choose",
        "/api/1/auth/logout",
        "/api/1/auth/providers",
        "/api/1/configs/1/extra",
        "/api/1/users",
        "/api/2/configs",
        "/",
        "/favicon.ico",
    ] {
        let reply = send(&app, Call::get(path)).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(reply.json(), json!({ "detail": "Not found." }), "{path}");
    }
}

// ---------------------------------------------------------------- CORS

#[tokio::test]
async fn preflight_answers_with_the_documented_headers() {
    let app = app();
    let reply = send(
        &app,
        Call::new(Method::OPTIONS, "/api/1/configs/1").origin("https://app.tabby.sh"),
    )
    .await;

    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert_eq!(
        reply.header(header::ACCESS_CONTROL_ALLOW_ORIGIN),
        Some("https://app.tabby.sh")
    );
    assert_eq!(
        reply.header(header::ACCESS_CONTROL_ALLOW_METHODS),
        Some("GET, POST, PUT, PATCH, DELETE, OPTIONS")
    );
    assert_eq!(
        reply.header(header::ACCESS_CONTROL_ALLOW_HEADERS),
        Some("authorization, content-type")
    );
    assert_eq!(reply.header(header::ACCESS_CONTROL_MAX_AGE), Some("86400"));
    assert_eq!(
        reply.header(
            "access-control-allow-credentials"
                .parse()
                .expect("valid name")
        ),
        None
    );
}

#[tokio::test]
async fn preflight_without_origin_uses_a_wildcard() {
    let app = app();
    let reply = send(&app, Call::new(Method::OPTIONS, "/api/1/user").no_auth()).await;

    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert_eq!(reply.header(header::ACCESS_CONTROL_ALLOW_ORIGIN), Some("*"));
}

#[tokio::test]
async fn cors_headers_are_added_to_actual_responses_including_errors() {
    let app = app();
    let id = create_config(&app, "cors").await;

    for (call, expected) in [
        (
            Call::get("/api/1/user").origin("https://app.tabby.sh"),
            StatusCode::OK,
        ),
        (
            Call::get(format!("/api/1/configs/{id}")).origin("https://app.tabby.sh"),
            StatusCode::OK,
        ),
        (
            Call::delete("/api/1/configs/4242").origin("https://app.tabby.sh"),
            StatusCode::NOT_FOUND,
        ),
        (
            Call::get("/api/1/user")
                .no_auth()
                .origin("https://app.tabby.sh"),
            StatusCode::UNAUTHORIZED,
        ),
    ] {
        let reply = send(&app, call).await;
        assert_eq!(reply.status, expected);
        assert_eq!(
            reply.header(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            Some("https://app.tabby.sh")
        );
    }
}

#[tokio::test]
async fn options_on_unknown_paths_is_still_answered_or_404() {
    let app = app();
    let reply = send(&app, Call::new(Method::OPTIONS, "/nope").no_auth()).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
}
