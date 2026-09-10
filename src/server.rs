//! MCP and HTTP transport adapters for Memoro's application service.

use std::{
    collections::HashMap,
    future::IntoFuture,
    net::IpAddr,
    path::PathBuf,
    sync::{Arc, OnceLock},
    time::Duration,
};

use axum::{
    extract::{Request, State},
    http::{header::AUTHORIZATION, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Json, Response},
    routing::get,
    Router,
};
use regex::Regex;
use rmcp::schemars;
use rmcp::{handler::server::wrapper::Parameters, tool, tool_router};
use rmcp::{
    transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
    },
    ServiceExt,
};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::{net::TcpListener, task::spawn_blocking};

use crate::{
    config,
    errors::MemoroError,
    models::PatchEdit,
    service::{
        MemoryCreateRequest, MemoryDeleteRequest, MemoryGetRequest, MemoryListRequest,
        MemoryPatchRequest, MemoryReplaceRequest, MemorySearchRequest, Service, SyncRequest,
    },
};

const JOIN_FAILURE_MESSAGE: &str =
    "Memoro could not complete the requested operation. Retry the request.";
const SERIALIZATION_FAILURE_MESSAGE: &str =
    "Memoro could not format the requested result. Retry the request.";

/// Requested serving transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// MCP over standard input and output.
    Stdio,
    /// MCP streamable HTTP at `/mcp`.
    Http,
}

/// Optional CLI values before environment/default precedence is resolved.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServeOverrides {
    pub transport: Option<String>,
    pub home: Option<String>,
    pub host: Option<String>,
    pub port: Option<String>,
    pub token: Option<String>,
}

/// Fully resolved service startup configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeConfig {
    pub transport: Transport,
    pub home: PathBuf,
    pub host: String,
    pub port: u16,
    pub token: Option<String>,
}

/// Resolve CLI, process environment, and defaults for `memoro serve`.
pub fn resolve_serve_config(overrides: &ServeOverrides) -> Result<ServeConfig, String> {
    resolve_serve_config_with_environment(overrides, None)
}

/// Resolve serve settings with an injectable environment for tests.
pub fn resolve_serve_config_with_environment(
    overrides: &ServeOverrides,
    environment: Option<&HashMap<String, String>>,
) -> Result<ServeConfig, String> {
    let transport = parse_transport(
        overrides
            .transport
            .clone()
            .or_else(|| environment_value(environment, "MEMORO_TRANSPORT"))
            .unwrap_or_else(|| "stdio".to_string()),
    )?;
    let home = config::resolve_home(overrides.home.as_deref(), environment)
        .map_err(|error| sanitize_error_message(&error.to_string()))?;
    let host = overrides
        .host
        .clone()
        .or_else(|| environment_value(environment, "MEMORO_HOST"))
        .unwrap_or_else(|| "127.0.0.1".to_string());
    let port = parse_port(
        overrides
            .port
            .clone()
            .or_else(|| environment_value(environment, "MEMORO_PORT"))
            .unwrap_or_else(|| "8000".to_string()),
    )?;
    let token = overrides
        .token
        .clone()
        .or_else(|| environment_value(environment, "MEMORO_TOKEN"))
        .filter(|value| !value.trim().is_empty());

    Ok(ServeConfig {
        transport,
        home,
        host,
        port,
        token,
    })
}

fn environment_value(environment: Option<&HashMap<String, String>>, key: &str) -> Option<String> {
    match environment {
        Some(values) => values.get(key).cloned(),
        None => std::env::var(key).ok(),
    }
}

fn parse_transport(value: String) -> Result<Transport, String> {
    match value.as_str() {
        "stdio" => Ok(Transport::Stdio),
        "http" => Ok(Transport::Http),
        _ => Err(format!(
            "Invalid transport '{value}'. Use 'stdio' or 'http'."
        )),
    }
}

fn parse_port(value: String) -> Result<u16, String> {
    match value.parse::<u16>() {
        Ok(port) if port != 0 => Ok(port),
        _ => Err(format!(
            "Invalid port '{value}'. Use an integer from 1 to 65535."
        )),
    }
}

/// Input for `memory_create`.
#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
pub struct MemoryCreateInput {
    pub title: String,
    pub summary: String,
    pub body: String,
    pub kind: String,
    pub project: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub space: Option<String>,
}

/// One text replacement for `memory_patch`.
#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
pub struct PatchEditInput {
    pub old_text: String,
    pub new_text: String,
}

/// Input for `memory_patch`.
#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
pub struct MemoryPatchInput {
    pub id: String,
    pub base_revision: String,
    pub edits: Vec<PatchEditInput>,
    pub summary: Option<String>,
    pub space: Option<String>,
}

/// Input for `memory_replace`.
#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
pub struct MemoryReplaceInput {
    pub id: String,
    pub base_revision: String,
    pub summary: String,
    pub body: String,
    pub tags: Vec<String>,
    pub space: Option<String>,
}

/// Input for `memory_delete`.
#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
pub struct MemoryDeleteInput {
    pub id: String,
    pub expected_title: String,
    pub base_revision: String,
    pub space: Option<String>,
}

/// Input for `memory_get`.
#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
pub struct MemoryGetInput {
    pub id: String,
    pub space: Option<String>,
}

/// Input for `memory_list`.
#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
pub struct MemoryListInput {
    pub space: Option<String>,
    pub kind: Option<String>,
    pub project: Option<String>,
}

/// Input for `memory_search`.
#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
pub struct MemorySearchInput {
    pub query: String,
    pub space: Option<String>,
    pub kind: Option<String>,
    pub project: Option<String>,
    pub limit: Option<i64>,
}

/// Input shared by the sync tools.
#[derive(Debug, Deserialize, rmcp::schemars::JsonSchema)]
pub struct SyncInput {
    pub space: Option<String>,
    pub remote: Option<String>,
    pub branch: Option<String>,
    pub timeout_secs: Option<u64>,
}

/// MCP server that adapts every operation to [`Service`].
#[derive(Debug, Clone)]
pub struct MemoroServer {
    service: Arc<Service>,
}

impl MemoroServer {
    /// Create an MCP adapter for one shared Memoro service.
    pub fn new(service: Arc<Service>) -> Self {
        Self { service }
    }

    /// Return the tool definitions generated by the live rmcp tool router.
    pub fn tool_definitions(&self) -> Vec<rmcp::model::Tool> {
        Self::tool_router().list_all()
    }

    async fn invoke<T, F>(&self, operation: F) -> Result<String, String>
    where
        T: Serialize + Send + 'static,
        F: FnOnce(&Service) -> Result<T, MemoroError> + Send + 'static,
    {
        let service = Arc::clone(&self.service);
        let result = spawn_blocking(move || operation(service.as_ref()))
            .await
            .map_err(|_| JOIN_FAILURE_MESSAGE.to_string())?;
        let result = result.map_err(|error| sanitize_error_message(&error.to_string()))?;
        serde_json::to_string(&result).map_err(|_| SERIALIZATION_FAILURE_MESSAGE.to_string())
    }
}

#[tool_router(server_handler)]
impl MemoroServer {
    /// Create a new committed memory.
    #[tool(
        name = "memory_create",
        description = "Create a new memory in a Memoro space."
    )]
    pub async fn memory_create(
        &self,
        Parameters(input): Parameters<MemoryCreateInput>,
    ) -> Result<String, String> {
        self.invoke(move |service| {
            service.memory_create(MemoryCreateRequest {
                space: input.space,
                title: input.title,
                summary: input.summary,
                body: input.body,
                kind: input.kind,
                project: input.project,
                tags: input.tags,
            })
        })
        .await
    }

    /// Patch unique body text in a memory.
    #[tool(
        name = "memory_patch",
        description = "Patch text in an existing memory."
    )]
    pub async fn memory_patch(
        &self,
        Parameters(input): Parameters<MemoryPatchInput>,
    ) -> Result<String, String> {
        self.invoke(move |service| {
            service.memory_patch(MemoryPatchRequest {
                space: input.space,
                id: input.id,
                base_revision: input.base_revision,
                edits: input
                    .edits
                    .into_iter()
                    .map(|edit| PatchEdit {
                        old_text: edit.old_text,
                        new_text: edit.new_text,
                    })
                    .collect(),
                summary: input.summary,
            })
        })
        .await
    }

    /// Replace an existing memory body and metadata.
    #[tool(
        name = "memory_replace",
        description = "Replace the summary, body, and tags of an existing memory."
    )]
    pub async fn memory_replace(
        &self,
        Parameters(input): Parameters<MemoryReplaceInput>,
    ) -> Result<String, String> {
        self.invoke(move |service| {
            service.memory_replace(MemoryReplaceRequest {
                space: input.space,
                id: input.id,
                base_revision: input.base_revision,
                summary: input.summary,
                body: input.body,
                tags: input.tags,
            })
        })
        .await
    }

    /// Delete an existing memory.
    #[tool(
        name = "memory_delete",
        description = "Delete a memory by ID and revision."
    )]
    pub async fn memory_delete(
        &self,
        Parameters(input): Parameters<MemoryDeleteInput>,
    ) -> Result<String, String> {
        self.invoke(move |service| {
            service.memory_delete(MemoryDeleteRequest {
                space: input.space,
                id: input.id,
                expected_title: input.expected_title,
                base_revision: input.base_revision,
            })
        })
        .await
    }

    /// Fetch one memory by ID.
    #[tool(name = "memory_get", description = "Get a memory by ID.")]
    pub async fn memory_get(
        &self,
        Parameters(input): Parameters<MemoryGetInput>,
    ) -> Result<String, String> {
        self.invoke(move |service| {
            service.memory_get(MemoryGetRequest {
                space: input.space,
                id: input.id,
            })
        })
        .await
    }

    /// List memories in one space.
    #[tool(name = "memory_list", description = "List memories in a Memoro space.")]
    pub async fn memory_list(
        &self,
        Parameters(input): Parameters<MemoryListInput>,
    ) -> Result<String, String> {
        self.invoke(move |service| {
            service.memory_list(MemoryListRequest {
                space: input.space,
                kind: input.kind,
                project: input.project,
            })
        })
        .await
    }

    /// Search committed memories.
    #[tool(
        name = "memory_search",
        description = "Search committed Memoro memories."
    )]
    pub async fn memory_search(
        &self,
        Parameters(input): Parameters<MemorySearchInput>,
    ) -> Result<String, String> {
        self.invoke(move |service| {
            service.memory_search(MemorySearchRequest {
                space: input.space,
                query: input.query,
                kind: input.kind,
                project: input.project,
                limit: input.limit,
            })
        })
        .await
    }

    /// List registered spaces.
    #[tool(
        name = "spaces_list",
        description = "List every registered Memoro space."
    )]
    pub async fn spaces_list(&self) -> Result<String, String> {
        self.invoke(|service| service.spaces_list()).await
    }

    /// Inspect remote synchronization state.
    #[tool(
        name = "sync_status",
        description = "Show synchronization status for one space."
    )]
    pub async fn sync_status(
        &self,
        Parameters(input): Parameters<SyncInput>,
    ) -> Result<String, String> {
        self.invoke(move |service| service.sync_status(sync_request(input)))
            .await
    }

    /// Pull committed history from a configured remote.
    #[tool(name = "sync_pull", description = "Pull memory history from a remote.")]
    pub async fn sync_pull(
        &self,
        Parameters(input): Parameters<SyncInput>,
    ) -> Result<String, String> {
        self.invoke(move |service| service.sync_pull(sync_request(input)))
            .await
    }

    /// Push committed history to a configured remote.
    #[tool(name = "sync_push", description = "Push memory history to a remote.")]
    pub async fn sync_push(
        &self,
        Parameters(input): Parameters<SyncInput>,
    ) -> Result<String, String> {
        self.invoke(move |service| service.sync_push(sync_request(input)))
            .await
    }
}

fn sync_request(input: SyncInput) -> SyncRequest {
    SyncRequest {
        space: input.space,
        remote: input.remote,
        branch: input.branch,
        timeout_secs: input.timeout_secs,
    }
}

/// Run a Memoro MCP server over stdio without writing any banner to stdout.
pub async fn run_stdio(server: MemoroServer) -> Result<(), String> {
    server
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|_| "Memoro could not start the stdio server. Retry the command.".to_string())?
        .waiting()
        .await
        .map(|_| ())
        .map_err(|_| "Memoro stdio server stopped unexpectedly.".to_string())
}

const HTTP_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_ALLOWED_HOSTS: [&str; 3] = ["localhost", "127.0.0.1", "::1"];

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// Validate the safety requirements for an HTTP binding.
pub fn validate_http_binding(host: &str, token: Option<&str>) -> Result<(), String> {
    let is_loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .map(|address| address.is_loopback())
            .unwrap_or(false);
    let has_token = token.is_some_and(|value| !value.trim().is_empty());

    if !is_loopback && !has_token {
        return Err(format!(
            "Memoro cannot bind HTTP to non-loopback host '{host}' without a Bearer token. Set --token or MEMORO_TOKEN, then retry."
        ));
    }

    Ok(())
}

/// Build the streamable HTTP router mounted at `/mcp`.
pub fn http_router(service: Arc<Service>, token: Option<String>) -> Router {
    let server = MemoroServer::new(service);
    let mcp = StreamableHttpService::new(
        move || Ok(server.clone()),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default()
            .with_json_response(true)
            .with_allowed_hosts(DEFAULT_ALLOWED_HOSTS),
    );
    let mcp_router = Router::new().fallback_service(mcp);
    let mcp_router = match token.filter(|value| !value.trim().is_empty()) {
        Some(token) => mcp_router.layer(middleware::from_fn_with_state(
            BearerToken(Arc::from(token)),
            require_bearer_token,
        )),
        None => mcp_router,
    };

    Router::new()
        .route("/health", get(health))
        .nest("/mcp", mcp_router)
}

/// Serve streamable HTTP at the fixed `/mcp` mount point.
pub async fn run_http(
    service: Arc<Service>,
    host: &str,
    port: u16,
    token: Option<String>,
) -> Result<(), String> {
    let listener = TcpListener::bind((host, port)).await.map_err(|_| {
        "Memoro could not bind the requested HTTP host and port. Check they are available, then retry."
            .to_string()
    })?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let server = axum::serve(listener, http_router(service, token))
        .with_graceful_shutdown(async {
            let _ = shutdown_rx.await;
        })
        .into_future();
    tokio::pin!(server);

    tokio::select! {
        result = &mut server => {
            result.map_err(|_| "Memoro HTTP server stopped unexpectedly.".to_string())
        }
        signal_result = shutdown_signal() => {
            signal_result?;
            let _ = shutdown_tx.send(());
            match tokio::time::timeout(HTTP_SHUTDOWN_TIMEOUT, &mut server).await {
                Ok(result) => result.map_err(|_| "Memoro HTTP server stopped unexpectedly.".to_string()),
                Err(_) => Err(
                    "Memoro HTTP server did not shut down within the grace period. Retry the command."
                        .to_string(),
                ),
            }
        }
    }
}

async fn shutdown_signal() -> Result<(), String> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let mut terminate = signal(SignalKind::terminate()).map_err(|_| {
            "Memoro could not install the HTTP shutdown signal handler. Retry the command."
                .to_string()
        })?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result.map_err(|_| {
                "Memoro could not receive the HTTP shutdown signal. Retry the command.".to_string()
            }),
            _ = terminate.recv() => Ok(()),
        }
    }

    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.map_err(|_| {
            "Memoro could not receive the HTTP shutdown signal. Retry the command.".to_string()
        })
    }
}

#[derive(Clone)]
struct BearerToken(Arc<str>);

async fn require_bearer_token(
    State(expected): State<BearerToken>,
    request: Request,
    next: Next,
) -> Response {
    let authorized = request
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|provided| bool::from(provided.as_bytes().ct_eq(expected.0.as_bytes())));

    if authorized {
        next.run(request).await
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

fn sanitize_error_message(message: &str) -> String {
    let message = redact_url_userinfo().replace_all(message, "$1<redacted>@");
    let message = redact_authorization_values().replace_all(&message, "Authorization: <redacted>");
    redact_bearer_tokens()
        .replace_all(&message, "Bearer <redacted>")
        .into_owned()
}

fn redact_url_userinfo() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"([A-Za-z][A-Za-z0-9+.-]*://)[^\s/@]+@").expect("valid URL redaction regex")
    })
}

fn redact_authorization_values() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"(?i)Authorization:\s*(?:Bearer\s+)?[^\s,;]+")
            .expect("valid authorization redaction regex")
    })
}

fn redact_bearer_tokens() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN
        .get_or_init(|| Regex::new(r"(?i)Bearer\s+[^\s,;]+").expect("valid bearer redaction regex"))
}

#[cfg(test)]
mod tests {
    use super::{parse_port, parse_transport, sanitize_error_message, Transport};

    #[test]
    fn sensitive_error_fragments_are_redacted() {
        let message = sanitize_error_message(
            "fetch https://alice:secret@example.test/repo with Authorization: Bearer token-value",
        );
        assert!(!message.contains("alice:secret"));
        assert!(!message.contains("token-value"));
        assert!(message.contains("https://<redacted>@example.test/repo"));
    }

    #[test]
    fn transport_and_port_parsers_are_strict() {
        assert_eq!(parse_transport("stdio".to_string()), Ok(Transport::Stdio));
        assert!(parse_transport("STDIO".to_string()).is_err());
        assert_eq!(parse_port("65535".to_string()), Ok(65535));
        assert!(parse_port("0".to_string()).is_err());
        assert!(parse_port("65536".to_string()).is_err());
    }
}
