//! A replay of the exact request sequence `configSync.service.ts` performs,
//! against a fresh database, using nothing but the public API.

mod common;

use axum::http::{header, StatusCode};
use common::*;
use serde_json::json;

fn remote_content(reply: &Reply) -> String {
    reply.json()["content"]
        .as_str()
        .expect("content is a string")
        .to_owned()
}

#[tokio::test]
async fn full_client_lifecycle() {
    let app = app();

    // 1. Settings → Config sync → "Test connection": GET /api/1/user.
    let reply = send(&app, Call::get("/api/1/user")).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.header(header::CONTENT_TYPE), Some("application/json"));
    assert_eq!(reply.json()["is_pro"], json!(true));

    // 2. Opening the config list: GET /api/1/configs, a bare array.
    let reply = send(&app, Call::get("/api/1/configs")).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json(), json!([]));

    // 3. "Upload as new config": POST /api/1/configs with only a name. The
    //    response id is what the client stores as `configSync.configID` and
    //    later compares with `===`, so it must be an integer.
    let reply = send(
        &app,
        Call::post("/api/1/configs").json(&json!({ "name": "New config on linux" })),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let created = reply.json();
    let config_id: i64 = created["id"].as_i64().expect("configID is an integer");
    assert_eq!(created["content"], json!("{}"));

    // 4. The upload itself: PATCH content plus the client's app version.
    let local_yaml = "version: 4\nprofiles:\n  - name: local\n    command: bash\n";
    sleep_past_clock_tick().await;
    let reply = send(
        &app,
        Call::patch(format!("/api/1/configs/{config_id}")).json(&json!({
            "content": local_yaml,
            "last_used_with_version": "1.0.235",
        })),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    let uploaded = reply.json();
    assert_eq!(remote_content(&reply), local_yaml);
    assert_eq!(uploaded["last_used_with_version"], json!("1.0.235"));
    let created_at: String = uploaded["created_at"]
        .as_str()
        .expect("created_at")
        .to_owned();
    let mut last_remote_change: String = uploaded["modified_at"]
        .as_str()
        .expect("modified_at")
        .to_owned();

    // 5. The 60s autosync poll sees no change yet: `new Date(modified_at) >
    //    lastRemoteChange` must be false.
    sleep_past_clock_tick().await;
    let reply = send(&app, Call::get(format!("/api/1/configs/{config_id}"))).await;
    assert_eq!(reply.status, StatusCode::OK);
    let polled = reply.json();
    assert_eq!(
        polled["modified_at"].as_str().expect("modified_at"),
        last_remote_change.as_str()
    );
    assert_eq!(polled["created_at"], json!(created_at));

    // 6. Another machine uploads.
    let other_yaml = "version: 4\nprofiles:\n  - name: laptop\n    command: zsh\n";
    sleep_past_clock_tick().await;
    let reply = send(
        &app,
        Call::patch(format!("/api/1/configs/{config_id}")).json(&json!({
            "content": other_yaml,
            "last_used_with_version": "1.0.240",
        })),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    let remote_write = reply.json();
    assert!(
        remote_write["modified_at"].as_str().expect("modified_at") > last_remote_change.as_str(),
        "the poll would not have noticed the change"
    );
    assert_eq!(
        remote_write["created_at"].as_str().expect("created_at"),
        created_at,
        "created_at must never move"
    );

    // 7. This client's poll now downloads the newer config.
    let reply = send(&app, Call::get(format!("/api/1/configs/{config_id}"))).await;
    assert_eq!(remote_content(&reply), other_yaml);
    assert_eq!(
        reply.json()["modified_at"].as_str().expect("modified_at"),
        remote_write["modified_at"].as_str().expect("modified_at")
    );
    last_remote_change = reply.json()["modified_at"]
        .as_str()
        .expect("modified_at")
        .to_owned();

    // 8. The list view still shows one config, with its name and version.
    let reply = send(&app, Call::get("/api/1/configs")).await;
    let body = reply.json();
    let list = body.as_array().expect("bare array");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["id"], json!(config_id));
    assert_eq!(list[0]["name"], json!("New config on linux"));
    assert_eq!(list[0]["last_used_with_version"], json!("1.0.240"));
    assert!(list[0]["modified_at"].as_str().expect("modified_at") <= last_remote_change.as_str());

    // 9. "Delete remote config": 204, and the client does
    //    `text ? JSON.parse(text) : undefined`, so the body must be absent.
    let reply = send(&app, Call::delete(format!("/api/1/configs/{config_id}"))).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert!(reply.bytes.is_empty());

    // 10. Everything referencing that id is now a DRF 404.
    for path in [
        format!("/api/1/configs/{config_id}"),
        format!("/api/1/configs/{config_id}/"),
    ] {
        let reply = send(&app, Call::get(&path)).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(reply.json(), json!({ "detail": "Not found." }));
    }

    let reply = send(&app, Call::get("/api/1/configs")).await;
    assert_eq!(reply.json(), json!([]));
}

#[tokio::test]
async fn re_upload_after_a_download_keeps_history_consistent() {
    let app = app();

    send(&app, Call::get("/api/1/user")).await;
    let config_id = create_config(&app, "round trip").await;

    let mut previous_modified: Option<String> = None;
    for round in 0..5 {
        let content = format!("version: 4\nround: {round}\n");
        sleep_past_clock_tick().await;
        let reply = send(
            &app,
            Call::patch(format!("/api/1/configs/{config_id}")).json(&json!({
                "content": content,
                "last_used_with_version": "1.0.235",
            })),
        )
        .await;
        assert_eq!(reply.status, StatusCode::OK);

        let body = reply.json();
        assert_eq!(body["content"].as_str().expect("content"), content);
        let modified = body["modified_at"]
            .as_str()
            .expect("modified_at")
            .to_owned();
        if let Some(previous) = &previous_modified {
            assert!(&modified > previous, "modified_at must strictly advance");
        }
        previous_modified = Some(modified);
    }
}
