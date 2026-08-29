use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};

use alx::{App, router, router_with_auth, serve};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt;

async fn raw_http(address: SocketAddr, request: String) -> String {
    tokio::task::spawn_blocking(move || {
        let mut stream = TcpStream::connect(address).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    })
    .await
    .unwrap()
}

fn response_body(response: &str) -> &str {
    response.split_once("\r\n\r\n").unwrap().1
}

#[tokio::test]
async fn non_loopback_server_refuses_to_start_without_password() {
    let directory = tempfile::tempdir().unwrap();
    let app = App::new(directory.path().join("alx.db")).unwrap();
    let error = serve(app, "0.0.0.0:0".parse().unwrap()).await.unwrap_err();
    assert!(error.to_string().contains("alx serve password set"));
}

#[tokio::test]
async fn protected_router_requires_password_and_accepts_session_cookie() {
    let directory = tempfile::tempdir().unwrap();
    let app = App::new(directory.path().join("alx.db")).unwrap();
    app.set_password("correct horse battery staple").unwrap();
    let service = router_with_auth(app.clone()).unwrap();

    let unauthorized = service
        .clone()
        .oneshot(
            Request::get("/")
                .header("host", "192.168.1.10:3000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::SEE_OTHER);
    assert_eq!(unauthorized.headers().get("location").unwrap(), "/login");
    let status = service
        .clone()
        .oneshot(
            Request::get("/api/status")
                .header("host", "192.168.1.10:3000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(status.status(), StatusCode::OK);

    let unauthorized_api = service
        .clone()
        .oneshot(
            Request::get("/api/tasks")
                .header("host", "192.168.1.10:3000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unauthorized_api.status(), StatusCode::UNAUTHORIZED);

    let login_page = service
        .clone()
        .oneshot(
            Request::get("/login")
                .header("host", "192.168.1.10:3000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login_page.status(), StatusCode::OK);
    let login_html = to_bytes(login_page.into_body(), usize::MAX).await.unwrap();
    assert!(String::from_utf8_lossy(&login_html).contains("Sign in to alx"));

    let bad_login = service
        .clone()
        .oneshot(
            Request::post("/api/login")
                .header("host", "192.168.1.10:3000")
                .header("content-type", "application/json")
                .body(Body::from(json!({"password": "wrong"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bad_login.status(), StatusCode::UNAUTHORIZED);

    let login = service
        .clone()
        .oneshot(
            Request::post("/api/login")
                .header("host", "192.168.1.10:3000")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"password": "correct horse battery staple"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::NO_CONTENT);
    let set_cookie = login
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert!(set_cookie.contains("HttpOnly"));
    assert!(set_cookie.contains("SameSite=Strict"));
    let cookie = set_cookie.split(';').next().unwrap();

    let authorized = service
        .clone()
        .oneshot(
            Request::get("/")
                .header("host", "192.168.1.10:3000")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(authorized.status(), StatusCode::OK);
    let hash = std::fs::read_to_string(app.password_path()).unwrap();
    assert!(hash.starts_with("$argon2id$"));
    assert!(!hash.contains("correct horse battery staple"));

    app.set_password("rotated password").unwrap();
    let rotated_cookie = service
        .clone()
        .oneshot(
            Request::get("/api/tasks")
                .header("host", "192.168.1.10:3000")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rotated_cookie.status(), StatusCode::UNAUTHORIZED);

    app.clear_password().unwrap();
    let cleared_cookie = service
        .oneshot(
            Request::get("/api/tasks")
                .header("host", "192.168.1.10:3000")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cleared_cookie.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn cli_review_token_authorizes_review_gate_requests() {
    let directory = tempfile::tempdir().unwrap();
    let app = App::new(directory.path().join("alx.db")).unwrap();
    app.set_password("password").unwrap();
    let task = app.create_task("CLI-GATE-1", "Body").unwrap();
    let artifact = app
        .create_artifact(&task.uuid, "notes", "Artifact")
        .unwrap();
    std::fs::write(directory.path().join("cli.token"), "test-cli-token\n").unwrap();
    let service = router_with_auth(app).unwrap();

    let response = service
        .oneshot(
            Request::post("/api/review-gates")
                .header("host", "100.64.0.8:3000")
                .header("x-alx-cli-token", "test-cli-token")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"artifact_uuid": &artifact.uuid}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn network_user_journey_loads_ui_and_manages_feedback() {
    let directory = tempfile::tempdir().unwrap();
    let app = App::new(directory.path().join("alx.db")).unwrap();
    let task = app.create_task("WEB-E2E", "Body").unwrap();
    let artifact = app
        .create_artifact_with_name(&task.uuid, "notes", Some("emoji-notes.md"), "A😀B")
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router(app.clone())).await.unwrap();
    });

    let root = raw_http(
        address,
        format!("GET / HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n"),
    )
    .await;
    assert!(root.starts_with("HTTP/1.1 200"), "{root:?}");
    assert!(response_body(&root).contains("function selectionOffsets"));
    assert!(response_body(&root).contains("Artifact Explorer"));
    assert!(response_body(&root).contains(".tree-row.depth-1 { padding-left: 31px; }"));
    assert!(response_body(&root).contains(".tree-row.depth-2 { padding-left: 52px; }"));
    assert!(response_body(&root).contains(".tree-row.depth-3 { padding-left: 73px; }"));
    assert!(response_body(&root).contains("row.className='tree-row depth-1'"));
    assert!(response_body(&root).contains("root.className='tree-row'"));
    assert!(response_body(&root).contains("appendTaskRoot('Completed','#/completed'"));
    assert!(response_body(&root).contains("appendTaskRoot('Archived','#/archived'"));
    assert!(response_body(&root).contains("taskFile.className='tree-row depth-2'"));
    assert!(response_body(&root).contains("child.className='tree-row depth-3'"));
    assert!(response_body(&root).contains("expandedTasks:new Set()"));
    assert!(response_body(&root).contains("row.onclick=()=>{if(selected)toggleExpanded"));
    assert!(response_body(&root).contains("folder.onclick=()=>{toggleExpanded"));
    assert!(response_body(&root).contains("root.onclick=()=>{toggleExpanded"));
    assert!(response_body(&root).contains("setAttribute('aria-expanded'"));
    assert!(response_body(&root).contains("function bindDirectoryRows"));
    assert!(response_body(&root).contains("row.setAttribute('role','link')"));
    assert!(response_body(&root).contains("event.key==='Enter'||event.key===' '"));
    assert!(response_body(&root).contains("task.md"));
    assert!(response_body(&root).contains("a.name||fallbackName(a)"));
    assert!(response_body(&root).contains("artifactGroups"));
    assert!(response_body(&root).contains("'/types/'"));
    assert!(response_body(&root).contains("annotation-dialog"));
    assert!(response_body(&root).contains("annotation-highlight"));
    assert!(response_body(&root).contains("function paintAnnotationHighlights"));
    assert!(response_body(&root).contains("function bindAnnotationInteractions"));
    assert!(response_body(&root).contains("data-annotation-card"));
    assert!(response_body(&root).contains("annotation-anchored"));
    assert!(response_body(&root).contains("annotation-popover-head"));
    assert!(response_body(&root).contains("annotationDialogEl.show()"));
    assert!(response_body(&root).contains("focus({preventScroll:true})"));
    assert!(response_body(&root).contains("limits.comparePoint"));
    assert!(response_body(&root).contains("document.addEventListener('pointerup'"));
    assert!(response_body(&root).contains("document.addEventListener('selectionchange'"));
    assert!(response_body(&root).contains("document.addEventListener('touchend'"));
    assert!(response_body(&root).contains("selectionPointerActive"));
    assert!(response_body(&root).contains("-webkit-user-select: text"));
    assert!(!response_body(&root).contains("kind-fieldset"));
    assert!(response_body(&root).contains("task-dialog"));
    assert!(response_body(&root).contains("task-edit-dialog"));
    assert!(response_body(&root).contains("data-edit-task"));
    assert!(response_body(&root).contains("function submitTaskEdit"));
    assert!(response_body(&root).contains("artifact-dialog"));
    assert!(response_body(&root).contains("data-new-task"));
    assert!(response_body(&root).contains("data-new-artifact"));
    assert!(response_body(&root).contains("function submitTask"));
    assert!(response_body(&root).contains("function submitArtifact"));
    assert!(response_body(&root).contains("data-copy-menu"));
    assert!(response_body(&root).contains("data-copy=\"uuid\""));
    assert!(response_body(&root).contains("function copyText"));
    assert!(response_body(&root).contains("function taskContext"));
    assert!(response_body(&root).contains("function markdownInline"));
    assert!(response_body(&root).contains("data-copy=\"context\""));
    assert!(response_body(&root).contains("copyMenu(true)"));
    assert!(response_body(&root).contains("function taskMetadata"));
    assert!(response_body(&root).contains("function artifactMetadata"));
    assert!(response_body(&root).contains("alx task read "));
    assert!(response_body(&root).contains("alx artifact read "));
    assert!(response_body(&root).contains("data-complete-task"));
    assert!(response_body(&root).contains("data-reopen-task"));
    assert!(response_body(&root).contains("data-archive-task"));
    assert!(response_body(&root).contains("data-delete-task"));
    assert!(response_body(&root).contains("window.confirm"));
    assert!(response_body(&root).contains("mermaid@11.12.3"));
    assert!(response_body(&root).contains("diagram-dialog"));
    assert!(response_body(&root).contains("function applyDiagramView"));
    assert!(response_body(&root).contains("function updateDiagramZoom"));
    assert!(response_body(&root).contains("function openExpandedDiagram"));
    assert!(response_body(&root).contains("diagramControlGroup('View controls')"));
    assert!(response_body(&root).contains("diagramControlGroup('Zoom controls')"));
    assert!(
        response_body(&root)
            .contains(".diagram-dialog-stage .mermaid.diagram-viewer { position: relative;")
    );
    assert!(!response_body(&root).contains("diagram-dialog-close"));
    assert!(response_body(&root).contains("DIAGRAM_EXPAND_ICON"));
    assert!(response_body(&root).contains("DIAGRAM_COLLAPSE_ICON"));
    assert!(response_body(&root).contains("DIAGRAM_FIT_ICON"));
    assert!(response_body(&root).contains("DIAGRAM_COLLAPSE_ICON,'Collapse diagram'"));
    assert!(!response_body(&root).contains("diagram.addEventListener('wheel'"));
    assert!(response_body(&root).contains(r"split(/[\s,]+/)"));
    assert!(!response_body(&root).contains(r"split(/[\\s,]+/)"));
    assert!(response_body(&root).contains("data-dragging"));
    assert!(root.contains("content-security-policy:"));
    assert!(root
        .contains("script-src 'self' 'unsafe-inline' https://cdn.jsdelivr.net; style-src 'unsafe-inline' https://cdn.jsdelivr.net"));

    let task_response = raw_http(
        address,
        format!(
            "GET /api/tasks/{} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n",
            task.uuid
        ),
    )
    .await;
    assert!(task_response.starts_with("HTTP/1.1 200"));
    let task_view: Value = serde_json::from_str(response_body(&task_response)).unwrap();
    assert_eq!(task_view["artifacts"][0]["uuid"], artifact.uuid);
    assert_eq!(task_view["artifacts"][0]["name"], "emoji-notes.md");
    assert_eq!(task_view["artifacts"][0]["type"], "notes");
    assert_eq!(task_view["rendered_task_html"], "<p>Body</p>\n");

    // Browser strings use UTF-16 units. Selecting the emoji in A😀B spans offsets 1..3.
    let body = json!({
        "kind": "good",
        "start_offset": 1,
        "end_offset": 3,
        "selected_text": "😀",
        "body": "Keep"
    })
    .to_string();
    let create_response = raw_http(
        address,
        format!(
            "POST /api/artifacts/{}/annotations HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            artifact.uuid,
            body.len()
        ),
    )
    .await;
    assert!(create_response.starts_with("HTTP/1.1 201"));
    let annotation: Value = serde_json::from_str(response_body(&create_response)).unwrap();
    let annotation_uuid = annotation["uuid"].as_str().unwrap();

    let resolve_response = raw_http(
        address,
        format!(
            "POST /api/annotations/{annotation_uuid}/resolve HTTP/1.1\r\nHost: {address}\r\nOrigin: http://{address}\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
        ),
    )
    .await;
    assert!(resolve_response.starts_with("HTTP/1.1 204"));
    server.abort();
}

#[tokio::test]
async fn review_gates_validate_artifact_tokens_and_finish_once() {
    let directory = tempfile::tempdir().unwrap();
    let app = App::new(directory.path().join("alx.db")).unwrap();
    let task = app.create_task("GATE-1", "Body").unwrap();
    let artifact = app
        .create_artifact(&task.uuid, "notes", "Artifact")
        .unwrap();
    let service = router(app);

    let response = service
        .clone()
        .oneshot(
            Request::post("/api/review-gates")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"artifact_uuid": &artifact.uuid}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let token = serde_json::from_slice::<Value>(&body).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned();

    let pending_status = service
        .clone()
        .oneshot(
            Request::get(format!("/api/review-gates/{token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(pending_status.status(), StatusCode::OK);
    let pending_body = to_bytes(pending_status.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(serde_json::from_slice::<Value>(&pending_body).unwrap()["outcome"].is_null());

    let page = service
        .clone()
        .oneshot(
            Request::get(format!("/review/{}?gate={token}", artifact.uuid))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    let page_body = to_bytes(page.into_body(), usize::MAX).await.unwrap();
    assert!(String::from_utf8_lossy(&page_body).contains("Artifact Explorer"));

    let mismatch = service
        .clone()
        .oneshot(
            Request::get(format!("/review/not-the-artifact?gate={token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(mismatch.status(), StatusCode::BAD_REQUEST);

    let finish = service
        .clone()
        .oneshot(
            Request::post(format!("/api/review-gates/{token}/finish"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(finish.status(), StatusCode::NO_CONTENT);

    let status = service
        .clone()
        .oneshot(
            Request::get(format!("/api/review-gates/{token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(status.status(), StatusCode::OK);
    let status_body = to_bytes(status.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&status_body).unwrap()["outcome"],
        "finished"
    );

    let repeated = service
        .oneshot(
            Request::post(format!("/api/review-gates/{token}/finish"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(repeated.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn http_routes_share_storage_and_sanitize_markdown() {
    let directory = tempfile::tempdir().unwrap();
    let app = App::new(directory.path().join("alx.db")).unwrap();
    let task = app.create_task("WEB-1", "Body").unwrap();
    let artifact = app
        .create_artifact(
            &task.uuid,
            "design",
            "# Safe\n<script>alert(1)</script>\n[bad](javascript:alert(1))\n![tracker](https://example.invalid/pixel)\n\n```mermaid\ngraph TD\n  A --> B\n```\n\n```rust\nfn main() {}\n```",
        )
        .unwrap();
    let service = router(app.clone());

    let response = service
        .clone()
        .oneshot(Request::get("/api/tasks").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let tasks: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(tasks[0]["id"], "WEB-1");

    let response = service
        .clone()
        .oneshot(
            Request::get(format!("/api/artifacts/{}", artifact.uuid))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let view: Value = serde_json::from_slice(&body).unwrap();
    let html = view["rendered_html"].as_str().unwrap();
    assert!(html.contains("<h1>Safe</h1>"));
    assert!(!html.contains("<script"));
    assert!(!html.contains("javascript:"));
    assert!(!html.contains("<img"));
    assert!(!html.contains("example.invalid"));
    assert!(html.contains("class=\"language-mermaid\""));
    assert!(html.contains("class=\"language-rust\""));
    assert!(html.contains("fn main()"));
    assert!(html.contains("graph TD"));

    let response = service
        .clone()
        .oneshot(
            Request::post(format!("/api/artifacts/{}/annotations", artifact.uuid))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "kind": "comment",
                        "start_offset": 0,
                        "end_offset": 4,
                        "selected_text": "Safe",
                        "body": "Useful"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let annotation: Value = serde_json::from_slice(&body).unwrap();
    let annotation_uuid = annotation["uuid"].as_str().unwrap();

    let response = service
        .clone()
        .oneshot(
            Request::post(format!("/api/annotations/{annotation_uuid}/resolve"))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(
        app.list_annotations(&artifact.uuid, false)
            .unwrap()
            .is_empty()
    );

    let response = service
        .oneshot(
            Request::get("/api/artifacts/not-a-uuid")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn http_creates_tasks_and_artifacts() {
    let directory = tempfile::tempdir().unwrap();
    let app = App::new(directory.path().join("alx.db")).unwrap();
    let service = router(app.clone());

    let response = service
        .clone()
        .oneshot(
            Request::post("/api/tasks")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"id": "WEB-CREATE", "body": "# New task"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let task: Value = serde_json::from_slice(&body).unwrap();
    let task_uuid = task["uuid"].as_str().unwrap();
    assert_eq!(app.read_task(task_uuid).unwrap().body, "# New task");

    let response = service
        .clone()
        .oneshot(
            Request::post(format!("/api/tasks/{task_uuid}/artifacts"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "type": "research",
                        "name": "web-findings.md",
                        "body": "Created in the browser"
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let artifact: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(artifact["task_uuid"], task_uuid);
    assert_eq!(artifact["type"], "research");
    assert_eq!(artifact["name"], "web-findings.md");
    assert_eq!(
        app.read_artifact(artifact["uuid"].as_str().unwrap())
            .unwrap()
            .body,
        "Created in the browser"
    );

    let duplicate = service
        .clone()
        .oneshot(
            Request::post("/api/tasks")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"id": "WEB-CREATE", "body": "duplicate"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(duplicate.status(), StatusCode::BAD_REQUEST);

    let invalid_artifact = service
        .oneshot(
            Request::post(format!("/api/tasks/{task_uuid}/artifacts"))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"type": " ", "name": null, "body": ""}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(invalid_artifact.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn http_updates_task_bodies_and_reports_missing_tasks() {
    let directory = tempfile::tempdir().unwrap();
    let app = App::new(directory.path().join("alx.db")).unwrap();
    let task = app.create_task("WEB-EDIT", "Old body").unwrap();
    let service = router(app.clone());

    let response = service
        .clone()
        .oneshot(
            Request::put(format!("/api/tasks/{}", task.uuid))
                .header("content-type", "application/json")
                .body(Body::from(json!({"body": "# Revised body"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let updated = app.read_task(&task.uuid).unwrap();
    assert_eq!(updated.body, "# Revised body");
    assert_eq!(updated.id, "WEB-EDIT");

    let response = service
        .clone()
        .oneshot(
            Request::put("/api/tasks/missing")
                .header("content-type", "application/json")
                .body(Body::from(json!({"body": "ignored"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn http_completes_and_reopens_read_only_tasks() {
    let directory = tempfile::tempdir().unwrap();
    let app = App::new(directory.path().join("alx.db")).unwrap();
    let task = app.create_task("WEB-COMPLETE", "Body").unwrap();
    let service = router(app.clone());

    let completed = service
        .clone()
        .oneshot(
            Request::post(format!("/api/tasks/{}/complete", task.uuid))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(completed.status(), StatusCode::NO_CONTENT);

    let list = service
        .clone()
        .oneshot(
            Request::get("/api/completed-tasks")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = to_bytes(list.into_body(), usize::MAX).await.unwrap();
    let tasks: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(tasks[0]["uuid"], task.uuid);
    assert!(tasks[0]["completed_at"].is_string());

    let edit = service
        .clone()
        .oneshot(
            Request::put(format!("/api/tasks/{}", task.uuid))
                .header("content-type", "application/json")
                .body(Body::from(json!({"body": "Changed"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(edit.status(), StatusCode::BAD_REQUEST);

    let create_artifact = service
        .clone()
        .oneshot(
            Request::post(format!("/api/tasks/{}/artifacts", task.uuid))
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"type": "notes", "name": null, "body": "Blocked"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(create_artifact.status(), StatusCode::BAD_REQUEST);

    let reopened = service
        .oneshot(
            Request::post(format!("/api/tasks/{}/reopen", task.uuid))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reopened.status(), StatusCode::NO_CONTENT);
    assert!(app.read_task(&task.uuid).unwrap().completed_at.is_none());
    assert_eq!(app.list_tasks().unwrap().len(), 1);
}

#[tokio::test]
async fn http_archives_and_confirmation_gates_permanent_task_deletion() {
    let directory = tempfile::tempdir().unwrap();
    let app = App::new(directory.path().join("alx.db")).unwrap();
    let task = app.create_task("WEB-ARCHIVE", "Body").unwrap();
    let artifact = app.create_artifact(&task.uuid, "notes", "Text").unwrap();
    let service = router(app.clone());

    let response = service
        .clone()
        .oneshot(
            Request::post(format!("/api/tasks/{}/archive", task.uuid))
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    let active = service
        .clone()
        .oneshot(Request::get("/api/tasks").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let body = to_bytes(active.into_body(), usize::MAX).await.unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), json!([]));

    let archived = service
        .clone()
        .oneshot(
            Request::get("/api/archived-tasks")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = to_bytes(archived.into_body(), usize::MAX).await.unwrap();
    let tasks: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(tasks[0]["uuid"], task.uuid);

    let unconfirmed = service
        .clone()
        .oneshot(
            Request::delete(format!("/api/tasks/{}", task.uuid))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"confirm":false}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unconfirmed.status(), StatusCode::BAD_REQUEST);
    assert!(app.read_task(&task.uuid).is_ok());

    let confirmed = service
        .oneshot(
            Request::delete(format!("/api/tasks/{}", task.uuid))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"confirm":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(confirmed.status(), StatusCode::NO_CONTENT);
    assert!(app.read_task(&task.uuid).is_err());
    assert!(app.read_artifact(&artifact.uuid).is_err());
}

#[tokio::test]
async fn archived_is_a_valid_task_id_in_http_routes() {
    let directory = tempfile::tempdir().unwrap();
    let app = App::new(directory.path().join("alx.db")).unwrap();
    app.create_task("archived", "Body").unwrap();
    let service = router(app.clone());

    let response = service
        .clone()
        .oneshot(
            Request::get("/api/tasks/archived")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let view: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(view["task"]["id"], "archived");

    let response = service
        .oneshot(
            Request::delete("/api/tasks/archived")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"confirm":true}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(app.read_task("archived").is_err());
}

#[tokio::test]
async fn http_distinguishes_domain_errors_from_storage_failures() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("alx.db");
    let app = App::new(&database_path).unwrap();
    let service = router(app.clone());

    let missing = service
        .clone()
        .oneshot(
            Request::get("/api/tasks/missing")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    let connection = rusqlite::Connection::open(&database_path).unwrap();
    connection.execute("DROP TABLE tasks", []).unwrap();
    let failure = service
        .oneshot(Request::get("/api/tasks").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(failure.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = to_bytes(failure.into_body(), usize::MAX).await.unwrap();
    assert_eq!(&body[..], b"internal server error");
}

#[tokio::test]
async fn http_rejects_rebinding_hosts_and_cross_origin_mutations() {
    let directory = tempfile::tempdir().unwrap();
    let app = App::new(directory.path().join("alx.db")).unwrap();
    let task = app.create_task("WEB-2", "Body").unwrap();
    let artifact = app.create_artifact(&task.uuid, "notes", "Text").unwrap();
    let annotation = app
        .create_annotation(
            &artifact.uuid,
            alx::NewAnnotation {
                kind: alx::AnnotationKind::Comment,
                start_offset: None,
                end_offset: None,
                selected_text: None,
                body: None,
            },
        )
        .unwrap();
    let service = router(app);
    let request_body = json!({
        "kind": "comment",
        "start_offset": 0,
        "end_offset": 4,
        "selected_text": "Text",
        "body": null
    })
    .to_string();

    let rebinding = service
        .clone()
        .oneshot(
            Request::post(format!("/api/artifacts/{}/annotations", artifact.uuid))
                .header("host", "attacker.example:3000")
                .header("content-type", "application/json")
                .body(Body::from(request_body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(rebinding.status(), StatusCode::FORBIDDEN);

    let cross_origin = service
        .clone()
        .oneshot(
            Request::post(format!("/api/artifacts/{}/annotations", artifact.uuid))
                .header("host", "127.0.0.1:3000")
                .header("origin", "http://attacker.example")
                .header("content-type", "application/json")
                .body(Body::from(request_body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cross_origin.status(), StatusCode::FORBIDDEN);

    let form_post = service
        .oneshot(
            Request::post(format!("/api/annotations/{}/resolve", annotation.uuid))
                .header("host", "127.0.0.1:3000")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(form_post.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
}
