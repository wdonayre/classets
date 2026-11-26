//! End to end against a mocked server: pair → pull one remote file → push one local file.

use classets_core::crypto::{sha256_hex, string_to_sign, verify_b64};
use classets_core::{Engine, EngineConfig, PairStatus};
use serde_json::json;
use wiremock::matchers::{
    body_json, body_partial_json, header, header_exists, method, path, query_param,
};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const SIGNED_HEADERS: [&str; 4] = [
    "authorization",
    "x-classets-device",
    "x-classets-timestamp",
    "x-classets-signature",
];

fn signed(m: wiremock::MockBuilder) -> wiremock::MockBuilder {
    SIGNED_HEADERS
        .iter()
        .fold(m, |m, h| m.and(header_exists(*h)))
        .and(header("authorization", "Bearer tok-123"))
        .and(header("x-classets-device", "dev-1"))
}

#[tokio::test]
async fn pair_pull_push() {
    let server = MockServer::start().await;
    let base = server.uri();
    let remote_body = b"hello from the cloud".to_vec();
    let remote_sha = sha256_hex(&remote_body);
    let local_body = b"made on this device".to_vec();
    let local_sha = sha256_hex(&local_body);

    // ---- pairing ----
    Mock::given(method("POST"))
        .and(path("/api/v1/pair/start"))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({
            "pairing_id": "p-1", "user_code": "KQ7M-4TZP",
            "verification_url": format!("{base}/approve?code=KQ7M-4TZP"), "expires_in": 600, "interval": 3
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/pair/poll"))
        .respond_with(ResponseTemplate::new(202).set_body_json(json!({"status": "pending"})))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/pair/poll"))
        .and(body_json(json!({"pairing_id": "p-1"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "approved", "device_id": "dev-1", "token": "tok-123",
            "user": {"id": 1, "name": "William", "email": "w@example.test"}, "workspace": {"id": 1, "name": "Team"}
        })))
        .mount(&server)
        .await;

    // ---- signed sync endpoints ----
    signed(Mock::given(method("GET")).and(path("/api/v1/folders")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"folders": [
            {"id": 1, "path": "Team", "access": "edit", "travel": "cloud_direct", "mode": "sync"},
            {"id": 2, "path": "Shared", "access": "view", "travel": "cloud_direct", "mode": "sync"}
        ]})))
        .mount(&server)
        .await;
    signed(Mock::given(method("GET")).and(path("/api/v1/changes")).and(query_param("since", "0")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "changes": [{"id": 1, "path": "Team/remote.txt", "deleted": false, "version_id": 5,
                         "sha256": remote_sha, "size": remote_body.len(), "modified_at": "2026-11-25T01:14:00Z"}],
            "cursor": 1, "has_more": false
        })))
        .expect(1)
        .mount(&server)
        .await;
    signed(
        Mock::given(method("GET"))
            .and(path("/api/v1/changes"))
            .and(query_param("since", "1")),
    )
    .respond_with(
        ResponseTemplate::new(200)
            .set_body_json(json!({"changes": [], "cursor": 1, "has_more": false})),
    )
    .mount(&server)
    .await;
    signed(Mock::given(method("GET")).and(path("/api/v1/files/versions/5/download")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"url": format!("{base}/blob/5"), "expires_in": 600})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/blob/5"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(remote_body.clone()))
        .expect(1)
        .mount(&server)
        .await;
    signed(Mock::given(method("POST")).and(path("/api/v1/files/intent")))
        .and(body_partial_json(json!({"path": "Team/local.txt", "sha256": local_sha, "size": local_body.len(),
                                      "base_version_id": null})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "action": "upload", "upload_id": "u-1", "url": format!("{base}/upload/u-1"), "method": "PUT",
            "headers": {"x-amz-meta-test": "1"}, "expires_in": 600
        })))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/upload/u-1"))
        .and(header("x-amz-meta-test", "1"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    signed(
        Mock::given(method("POST"))
            .and(path("/api/v1/files/commit"))
            .and(body_json(json!({"upload_id": "u-1"}))),
    )
    .respond_with(
        ResponseTemplate::new(200)
            .set_body_json(json!({"version_id": 6, "path": "Team/local.txt"})),
    )
    .expect(1)
    .mount(&server)
    .await;

    // ---- run ----
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("Team")).unwrap();
    std::fs::write(root.path().join("Team/local.txt"), &local_body).unwrap();
    // A local file in a view-only folder is never pushed.
    std::fs::create_dir_all(root.path().join("Shared")).unwrap();
    std::fs::write(root.path().join("Shared/mine.txt"), b"x").unwrap();

    let engine = Engine::new(EngineConfig {
        server_url: base.clone(),
        sync_root: root.path().to_path_buf(),
        data_dir: data.path().to_path_buf(),
        device_name: "TEST-PC".into(),
    })
    .unwrap();

    let start = engine.pair_start().await.unwrap();
    assert_eq!(start.user_code, "KQ7M-4TZP");
    assert_eq!(engine.pair_poll().await.unwrap(), PairStatus::Pending);
    assert_eq!(
        engine.pair_poll().await.unwrap(),
        PairStatus::Approved {
            device_id: "dev-1".into()
        }
    );
    assert!(engine.status().paired);

    let report = engine.sync_once().await.unwrap();
    assert_eq!(
        (
            report.downloaded,
            report.uploaded,
            report.deleted,
            report.conflicts
        ),
        (1, 1, 0, 0),
        "{report:?}"
    );
    assert!(report.errors.is_empty(), "{:?}", report.errors);
    assert_eq!(
        std::fs::read(root.path().join("Team/remote.txt")).unwrap(),
        remote_body
    );

    // A second pass finds nothing to do (intent/commit/download `expect(1)` checked on drop).
    let again = engine.sync_once().await.unwrap();
    assert_eq!((again.downloaded, again.uploaded), (0, 0), "{again:?}");
    let status = engine.status();
    assert!(status.last_sync.is_some());
    assert_eq!(status.last_report, Some(again));

    // ---- verify signatures and body hashes on every signed request ----
    let requests: Vec<Request> = server.received_requests().await.unwrap();
    let start_req = requests
        .iter()
        .find(|r| r.url.path() == "/api/v1/pair/start")
        .unwrap();
    let start_body: serde_json::Value = serde_json::from_slice(&start_req.body).unwrap();
    let public_key = start_body["public_key"].as_str().unwrap();
    assert_eq!(start_body["name"], "TEST-PC");

    let mut checked = 0;
    for r in &requests {
        let p = r.url.path();
        if !p.starts_with("/api/v1/") || p.starts_with("/api/v1/pair/") {
            assert!(
                r.headers.get("x-classets-signature").is_none(),
                "unsigned request {p} carried a signature"
            );
            continue;
        }
        let h = |name: &str| r.headers.get(name).unwrap().to_str().unwrap().to_string();
        let ts: i64 = h("x-classets-timestamp").parse().unwrap();
        let mut path_q = p.to_string();
        if let Some(q) = r.url.query() {
            path_q = format!("{path_q}?{q}");
        }
        let sts = string_to_sign(r.method.as_str(), &path_q, ts, &r.body);
        assert!(sts.ends_with(&sha256_hex(&r.body)));
        verify_b64(public_key, sts.as_bytes(), &h("x-classets-signature"))
            .unwrap_or_else(|e| panic!("bad signature on {} {path_q}: {e}", r.method));
        checked += 1;
    }
    // Pass 1: folders, changes, download link, intent, commit. Pass 2: folders, changes.
    assert_eq!(checked, 7, "signed requests checked");

    let put = requests
        .iter()
        .find(|r| r.url.path() == "/upload/u-1")
        .unwrap();
    assert_eq!(put.body, local_body);
    assert!(put.headers.get("authorization").is_none());
}
