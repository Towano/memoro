use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};

use axum::{
    body::Body,
    http::{header, Request, StatusCode},
};
use memoro::{
    config::{save_spaces, SpaceSettings},
    server::{
        http_router, resolve_serve_config_with_environment, validate_http_binding, MemoroServer,
        MemoryCreateInput, MemoryDeleteInput, MemoryGetInput, MemoryListInput, MemoryPatchInput,
        MemoryReplaceInput, MemorySearchInput, PatchEditInput, ServeOverrides, Transport,
    },
    service::Service,
};
use serde_json::{json, Value};
use tower::ServiceExt;

fn server_for(home: &std::path::Path) -> MemoroServer {
    MemoroServer::new(Arc::new(
        Service::open(home.to_path_buf()).expect("open temporary service"),
    ))
}

fn json_text(text: String) -> Value {
    serde_json::from_str(&text).expect("MCP tool result is JSON text")
}

#[tokio::test]
async fn tool_handlers_drive_service_crud_and_search() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let server = server_for(temporary.path());

    let created = json_text(
        server
            .memory_create(rmcp::handler::server::wrapper::Parameters(
                MemoryCreateInput {
                    title: "Release policy".to_string(),
                    summary: "Deploy carefully.".to_string(),
                    body: "validate before release".to_string(),
                    kind: "project".to_string(),
                    project: Some("memoro".to_string()),
                    tags: vec!["release".to_string()],
                    space: None,
                },
            ))
            .await
            .expect("create through MCP adapter"),
    );
    let id = created["receipt"]["memory"]["id"]
        .as_str()
        .expect("created ID")
        .to_string();
    let revision = created["revision"]
        .as_str()
        .expect("created revision")
        .to_string();

    let fetched = json_text(
        server
            .memory_get(rmcp::handler::server::wrapper::Parameters(MemoryGetInput {
                id: id.clone(),
                space: None,
            }))
            .await
            .expect("get through MCP adapter"),
    );
    assert_eq!(fetched["memory"]["body"], "validate before release");

    server
        .memory_create(rmcp::handler::server::wrapper::Parameters(
            MemoryCreateInput {
                title: "Unrelated fact".to_string(),
                summary: "A different record.".to_string(),
                body: "routine material".to_string(),
                kind: "persona".to_string(),
                project: None,
                tags: Vec::new(),
                space: None,
            },
        ))
        .await
        .expect("create unrelated memory through MCP adapter");
    server
        .memory_create(rmcp::handler::server::wrapper::Parameters(
            MemoryCreateInput {
                title: "Second unrelated fact".to_string(),
                summary: "Another different record.".to_string(),
                body: "more routine material".to_string(),
                kind: "persona".to_string(),
                project: None,
                tags: Vec::new(),
                space: None,
            },
        ))
        .await
        .expect("create second unrelated memory through MCP adapter");

    let patched = json_text(
        server
            .memory_patch(rmcp::handler::server::wrapper::Parameters(
                MemoryPatchInput {
                    id: id.clone(),
                    base_revision: revision,
                    edits: vec![PatchEditInput {
                        old_text: "validate".to_string(),
                        new_text: "review".to_string(),
                    }],
                    summary: None,
                    space: None,
                },
            ))
            .await
            .expect("patch through MCP adapter"),
    );
    let patched_revision = patched["revision"]
        .as_str()
        .expect("patched revision")
        .to_string();
    assert_eq!(
        patched["receipt"]["memory"]["body"],
        "review before release"
    );

    let replaced = json_text(
        server
            .memory_replace(rmcp::handler::server::wrapper::Parameters(
                MemoryReplaceInput {
                    id: id.clone(),
                    base_revision: patched_revision,
                    summary: "Reviewed release policy.".to_string(),
                    body: "deploy after review".to_string(),
                    tags: vec!["stable".to_string()],
                    space: None,
                },
            ))
            .await
            .expect("replace through MCP adapter"),
    );
    let replaced_revision = replaced["revision"]
        .as_str()
        .expect("replaced revision")
        .to_string();
    assert_eq!(replaced["receipt"]["memory"]["body"], "deploy after review");

    let listed = json_text(
        server
            .memory_list(rmcp::handler::server::wrapper::Parameters(
                MemoryListInput {
                    space: None,
                    kind: Some("project".to_string()),
                    project: Some("memoro".to_string()),
                },
            ))
            .await
            .expect("list through MCP adapter"),
    );
    assert_eq!(listed["memories"].as_array().map(Vec::len), Some(1));

    let searched = json_text(
        server
            .memory_search(rmcp::handler::server::wrapper::Parameters(
                MemorySearchInput {
                    query: "deploy".to_string(),
                    space: None,
                    kind: None,
                    project: None,
                    limit: Some(5),
                },
            ))
            .await
            .expect("search through MCP adapter"),
    );
    assert_eq!(searched["total_matches"], 1);
    assert_eq!(searched["hits"][0]["memory"]["memory"]["id"], id);

    let deleted = json_text(
        server
            .memory_delete(rmcp::handler::server::wrapper::Parameters(
                MemoryDeleteInput {
                    id,
                    expected_title: "Release policy".to_string(),
                    base_revision: replaced_revision,
                    space: None,
                },
            ))
            .await
            .expect("delete through MCP adapter"),
    );
    assert_eq!(deleted["receipt"]["operation"], "delete");
}

#[tokio::test]
async fn readonly_service_failures_are_tool_errors() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    save_spaces(
        temporary.path(),
        &BTreeMap::from([("archive".to_string(), SpaceSettings { readonly: true })]),
    )
    .expect("save readonly registry");
    let server = server_for(temporary.path());

    let error = server
        .memory_create(rmcp::handler::server::wrapper::Parameters(
            MemoryCreateInput {
                title: "Read only".to_string(),
                summary: "This must be rejected.".to_string(),
                body: "do not persist".to_string(),
                kind: "persona".to_string(),
                project: None,
                tags: Vec::new(),
                space: Some("archive".to_string()),
            },
        ))
        .await
        .expect_err("readonly mutation must use the tool error path");

    assert!(error.contains("readonly"));
    assert!(error.contains("archive"));
}

#[test]
fn generated_rmcp_router_registers_exactly_the_eleven_contract_tools() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let server = server_for(temporary.path());
    let names = server
        .tool_definitions()
        .into_iter()
        .map(|tool| tool.name.to_string())
        .collect::<HashSet<_>>();
    let expected = HashSet::from([
        "memory_create".to_string(),
        "memory_patch".to_string(),
        "memory_replace".to_string(),
        "memory_delete".to_string(),
        "memory_get".to_string(),
        "memory_list".to_string(),
        "memory_search".to_string(),
        "spaces_list".to_string(),
        "sync_status".to_string(),
        "sync_pull".to_string(),
        "sync_push".to_string(),
    ]);

    assert_eq!(names, expected);
}

#[test]
fn serve_configuration_uses_cli_then_environment_then_defaults() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let environment = HashMap::from([
        ("MEMORO_TRANSPORT".to_string(), "http".to_string()),
        (
            "MEMORO_HOME".to_string(),
            temporary.path().join("env-home").display().to_string(),
        ),
        ("MEMORO_HOST".to_string(), "env.example".to_string()),
        ("MEMORO_PORT".to_string(), "8123".to_string()),
        ("MEMORO_TOKEN".to_string(), "environment-token".to_string()),
    ]);
    let cli_home = temporary.path().join("cli-home");
    let overrides = ServeOverrides {
        transport: Some("stdio".to_string()),
        home: Some(cli_home.display().to_string()),
        host: Some("127.0.0.9".to_string()),
        port: Some("9000".to_string()),
        token: Some("cli-token".to_string()),
    };

    let resolved = resolve_serve_config_with_environment(&overrides, Some(&environment))
        .expect("resolve CLI overrides");
    assert_eq!(resolved.transport, Transport::Stdio);
    assert_eq!(resolved.home, cli_home);
    assert_eq!(resolved.host, "127.0.0.9");
    assert_eq!(resolved.port, 9000);
    assert_eq!(resolved.token.as_deref(), Some("cli-token"));

    let from_environment =
        resolve_serve_config_with_environment(&ServeOverrides::default(), Some(&environment))
            .expect("resolve environment");
    assert_eq!(from_environment.transport, Transport::Http);
    assert_eq!(from_environment.port, 8123);
    assert_eq!(from_environment.token.as_deref(), Some("environment-token"));

    let defaults =
        resolve_serve_config_with_environment(&ServeOverrides::default(), Some(&HashMap::new()))
            .expect("resolve defaults");
    assert_eq!(defaults.transport, Transport::Stdio);
    assert_eq!(defaults.host, "127.0.0.1");
    assert_eq!(defaults.port, 8000);
    assert_eq!(defaults.token, None);

    for (field, value) in [
        ("transport", "websocket"),
        ("port", "0"),
        ("port", "abc"),
        ("port", "65536"),
    ] {
        let invalid = if field == "transport" {
            ServeOverrides {
                transport: Some(value.to_string()),
                ..Default::default()
            }
        } else {
            ServeOverrides {
                port: Some(value.to_string()),
                ..Default::default()
            }
        };
        assert!(resolve_serve_config_with_environment(&invalid, Some(&HashMap::new())).is_err());
    }
}

#[tokio::test]
async fn health_is_stable_and_not_protected_by_mcp_token() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let service = Arc::new(Service::open(temporary.path().to_path_buf()).expect("open service"));

    let response = http_router(service, Some("test-token".to_string()))
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .expect("health response");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    let body = axum::body::to_bytes(response.into_body(), 1024)
        .await
        .expect("health body");
    assert_eq!(
        serde_json::from_slice::<Value>(&body).expect("health JSON"),
        json!({"status": "ok", "version": env!("CARGO_PKG_VERSION")})
    );
}

#[tokio::test]
async fn http_router_enforces_bearer_authentication_only_for_mcp() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let service = Arc::new(Service::open(temporary.path().to_path_buf()).expect("open service"));

    let open_response = http_router(Arc::clone(&service), None)
        .oneshot(initialize_request(None))
        .await
        .expect("open router response");
    assert_ne!(open_response.status(), StatusCode::UNAUTHORIZED);
    assert_mcp_initialize_response(open_response);

    let missing = http_router(Arc::clone(&service), Some("test-token".to_string()))
        .oneshot(initialize_request(None))
        .await
        .expect("missing-token response");
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

    let wrong_scheme = http_router(Arc::clone(&service), Some("test-token".to_string()))
        .oneshot(initialize_request(Some("Basic test-token")))
        .await
        .expect("wrong-scheme response");
    assert_eq!(wrong_scheme.status(), StatusCode::UNAUTHORIZED);

    let wrong_token = http_router(Arc::clone(&service), Some("test-token".to_string()))
        .oneshot(initialize_request(Some("Bearer other-token")))
        .await
        .expect("wrong-token response");
    assert_eq!(wrong_token.status(), StatusCode::UNAUTHORIZED);

    let accepted = http_router(service, Some("test-token".to_string()))
        .oneshot(initialize_request(Some("Bearer test-token")))
        .await
        .expect("correct-token response");
    assert_ne!(accepted.status(), StatusCode::UNAUTHORIZED);
    assert_mcp_initialize_response(accepted);
}

#[test]
fn allowed_mcp_hosts_exclude_unsuitable_wildcard_address() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let service = Arc::new(Service::open(temporary.path().to_path_buf()).expect("open service"));

    let response = tokio::runtime::Runtime::new()
        .expect("test runtime")
        .block_on(async {
            http_router(service, None)
                .oneshot(initialize_request_with_host(None, "0.0.0.0"))
                .await
                .expect("host validation response")
        });
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[test]
fn http_binding_requires_a_token_off_loopback() {
    for host in ["0.0.0.0", "192.0.2.10", "example.test"] {
        let error = validate_http_binding(host, None).expect_err("token must be required");
        assert!(error.contains(host));
        assert!(error.contains("Bearer token"));
    }

    for host in ["localhost", "127.0.0.1", "::1"] {
        assert!(
            validate_http_binding(host, None).is_ok(),
            "{host} is loopback"
        );
    }
    assert!(validate_http_binding("192.0.2.10", Some("test-token")).is_ok());
    assert!(validate_http_binding("192.0.2.10", Some("  test-token  ")).is_ok());
    assert!(validate_http_binding("192.0.2.10", Some("   ")).is_err());
}

fn initialize_request(authorization: Option<&str>) -> Request<Body> {
    initialize_request_with_host(authorization, "127.0.0.1")
}

fn initialize_request_with_host(authorization: Option<&str>, host: &str) -> Request<Body> {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/mcp")
        .header(header::HOST, host)
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(authorization) = authorization {
        builder = builder.header(header::AUTHORIZATION, authorization);
    }
    builder
        .body(Body::from(
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": { "name": "memoro-test", "version": "1.0" }
                }
            })
            .to_string(),
        ))
        .expect("valid initialize request")
}

fn assert_mcp_initialize_response(response: axum::response::Response) {
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .is_some(),
        "a session ID proves the request reached rmcp's initialize handler"
    );
    assert!(
        response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream")),
        "legacy MCP initialize responses are delivered through the session event stream"
    );
}
