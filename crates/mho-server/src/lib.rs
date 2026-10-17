//! HTTP server: REST API under `/api/v1`, live WebSocket at `/api/v1/stream`,
//! MCP (streamable HTTP) at `/mcp`, OpenAPI at `/api/v1/openapi.json`, docs at `/docs`.

mod guard;
pub mod mcp;
mod rest;
mod ws;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use mho_service::{LocalBackend, ScopeApi};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use tokio_util::sync::CancellationToken;
use tower_http::trace::TraceLayer;
use tracing::info;

pub use mcp::McpScope;
pub use rest::ApiDoc;

#[derive(Clone)]
pub struct AppState {
    pub backend: LocalBackend,
}

#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub bind: SocketAddr,
    /// Directory for files written on behalf of MCP clients.
    pub capture_dir: PathBuf,
    /// Bearer token required on every request; mandatory when not bound to loopback.
    pub api_token: Option<String>,
}

impl ServerOptions {
    /// Refuses a network-reachable server without a token.
    pub fn validate(&self) -> anyhow::Result<()> {
        if !self.bind.ip().is_loopback() && self.api_token.as_deref().is_none_or(str::is_empty) {
            anyhow::bail!(
                "listening on {} requires `api_token` in the config (the API controls the instrument)",
                self.bind
            );
        }
        Ok(())
    }
}

/// Builds the complete router (REST, WebSocket, MCP, docs).
pub fn router(backend: LocalBackend, opts: &ServerOptions, shutdown: CancellationToken) -> Router {
    let api: Arc<dyn ScopeApi> = Arc::new(backend.clone());
    let capture_dir = opts.capture_dir.clone();
    let mut mcp_config = StreamableHttpServerConfig::default();
    mcp_config.cancellation_token = shutdown.child_token();
    if !opts.bind.ip().is_loopback() {
        // Host-header validation only admits loopback names by default; the token guards
        // network access instead.
        mcp_config = mcp_config.disable_allowed_hosts();
    }
    let guard_cfg = guard::GuardConfig {
        loopback_only: opts.bind.ip().is_loopback(),
        token: opts
            .api_token
            .as_deref()
            .filter(|t| !t.is_empty())
            .map(Arc::from),
    };
    let mcp_service: StreamableHttpService<McpScope, LocalSessionManager> =
        StreamableHttpService::new(
            move || Ok(McpScope::new(api.clone(), capture_dir.clone())),
            Default::default(),
            mcp_config,
        );

    Router::new()
        .nest("/api/v1", rest::routes().merge(ws::routes()))
        .merge(rest::doc_routes())
        .nest_service("/mcp", mcp_service)
        .layer(axum::middleware::from_fn_with_state(
            guard_cfg,
            guard::guard,
        ))
        .layer(TraceLayer::new_for_http())
        .with_state(AppState { backend })
}

/// Serves until `shutdown` is cancelled.
pub async fn serve(
    backend: LocalBackend,
    opts: ServerOptions,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    opts.validate()?;
    let listener = tokio::net::TcpListener::bind(opts.bind).await?;
    let addr = listener.local_addr()?;
    info!("listening on http://{addr}  (REST /api/v1, docs /docs, MCP /mcp)");
    let app = router(backend, &opts, shutdown.clone());
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown.cancelled_owned())
        .await?;
    Ok(())
}

/// Re-exported so embedders can serve [`router`] without depending on axum directly.
pub mod axum_reexport {
    pub use axum::{Router, serve};
}
