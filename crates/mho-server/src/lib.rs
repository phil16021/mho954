//! HTTP server: REST API under `/api/v1`, live WebSocket at `/api/v1/stream`,
//! MCP (streamable HTTP) at `/mcp`, OpenAPI at `/api/v1/openapi.json`, docs at `/docs`.

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
use tower_http::cors::CorsLayer;
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
}

/// Builds the complete router (REST, WebSocket, MCP, docs).
pub fn router(backend: LocalBackend, opts: &ServerOptions, shutdown: CancellationToken) -> Router {
    let api: Arc<dyn ScopeApi> = Arc::new(backend.clone());
    let capture_dir = opts.capture_dir.clone();
    let mut mcp_config = StreamableHttpServerConfig::default();
    mcp_config.cancellation_token = shutdown.child_token();
    if !opts.bind.ip().is_loopback() {
        // Host-header validation only admits loopback names by default.
        mcp_config = mcp_config.disable_allowed_hosts();
    }
    let mcp_service: StreamableHttpService<McpScope, LocalSessionManager> = StreamableHttpService::new(
        move || Ok(McpScope::new(api.clone(), capture_dir.clone())),
        Default::default(),
        mcp_config,
    );

    Router::new()
        .nest("/api/v1", rest::routes().merge(ws::routes()))
        .merge(rest::doc_routes())
        .nest_service("/mcp", mcp_service)
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .with_state(AppState { backend })
}

/// Serves until `shutdown` is cancelled.
pub async fn serve(backend: LocalBackend, opts: ServerOptions, shutdown: CancellationToken) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(opts.bind).await?;
    let addr = listener.local_addr()?;
    info!("listening on http://{addr}  (REST /api/v1, docs /docs, MCP /mcp)");
    let app = router(backend, &opts, shutdown.clone());
    axum::serve(listener, app).with_graceful_shutdown(shutdown.cancelled_owned()).await?;
    Ok(())
}

/// Re-exported so embedders can serve [`router`] without depending on axum directly.
pub mod axum_reexport {
    pub use axum::{Router, serve};
}
