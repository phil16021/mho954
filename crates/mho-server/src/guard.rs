//! Request guard: protects the unauthenticated loopback API against browsers.
//!
//! * `Host` must be a loopback name (DNS rebinding) unless the server listens on the network,
//! * a browser `Origin` that is not loopback is refused for state-changing requests and the
//!   WebSocket upgrade (cross-site requests from any web page),
//! * with an API token configured, every request needs `Authorization: Bearer <token>`.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use mho_service::{ApiError, ErrorKind};

#[derive(Debug, Clone)]
pub struct GuardConfig {
    /// Enforce loopback `Host` headers (only when bound to loopback).
    pub loopback_only: bool,
    pub token: Option<Arc<str>>,
}

fn host_name(authority: &str) -> &str {
    let a = authority.trim();
    if let Some(rest) = a.strip_prefix('[') {
        return rest.split(']').next().unwrap_or("");
    }
    a.rsplit_once(':').map(|(h, _)| h).unwrap_or(a)
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host_name(host).to_ascii_lowercase().as_str(), "localhost" | "127.0.0.1" | "::1")
}

fn origin_is_loopback(origin: &str) -> bool {
    // "http://127.0.0.1:8954" → authority part.
    let rest = origin.split_once("://").map(|(_, r)| r).unwrap_or(origin);
    is_loopback_host(rest.split('/').next().unwrap_or(""))
}

fn deny(status: StatusCode, msg: &str) -> Response {
    (status, axum::Json(ApiError::new(ErrorKind::Forbidden, msg))).into_response()
}

fn check(cfg: &GuardConfig, method: &Method, headers: &HeaderMap) -> Result<(), Response> {
    if cfg.loopback_only {
        let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
        if !is_loopback_host(host) {
            return Err(deny(StatusCode::FORBIDDEN, "host not allowed"));
        }
    }
    let upgrade = headers.get(header::UPGRADE).is_some();
    let changes_state = !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) || upgrade;
    if changes_state && let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        let host = headers.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
        let same_origin = origin.split_once("://").map(|(_, r)| r) == Some(host);
        if !same_origin && !origin_is_loopback(origin) {
            return Err(deny(StatusCode::FORBIDDEN, "cross-origin request refused"));
        }
    }
    if let Some(token) = &cfg.token {
        let given = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "));
        let ok = given.is_some_and(|g| g.len() == token.len() && g.bytes().zip(token.bytes()).all(|(a, b)| a == b));
        if !ok {
            return Err(deny(StatusCode::UNAUTHORIZED, "missing or wrong API token"));
        }
    }
    Ok(())
}

pub async fn guard(State(cfg): State<GuardConfig>, req: Request, next: Next) -> Response {
    match check(&cfg, req.method(), req.headers()) {
        Ok(()) => next.run(req).await,
        Err(resp) => resp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn host_rules() {
        let cfg = GuardConfig { loopback_only: true, token: None };
        assert!(check(&cfg, &Method::GET, &headers(&[("host", "127.0.0.1:8954")])).is_ok());
        assert!(check(&cfg, &Method::GET, &headers(&[("host", "localhost")])).is_ok());
        assert!(check(&cfg, &Method::GET, &headers(&[("host", "[::1]:8954")])).is_ok());
        assert!(check(&cfg, &Method::GET, &headers(&[("host", "evil.example:8954")])).is_err());
    }

    #[test]
    fn origin_rules() {
        let cfg = GuardConfig { loopback_only: true, token: None };
        let h = |o: &str| headers(&[("host", "127.0.0.1:8954"), ("origin", o)]);
        assert!(check(&cfg, &Method::PUT, &h("https://evil.example")).is_err());
        assert!(check(&cfg, &Method::PUT, &h("http://127.0.0.1:8954")).is_ok());
        assert!(check(&cfg, &Method::PUT, &h("http://localhost:3000")).is_ok());
        // Reads are allowed; the browser's same-origin policy keeps the response private.
        assert!(check(&cfg, &Method::GET, &h("https://evil.example")).is_ok());
        let ws = headers(&[("host", "127.0.0.1:8954"), ("origin", "https://evil.example"), ("upgrade", "websocket")]);
        assert!(check(&cfg, &Method::GET, &ws).is_err());
    }

    #[test]
    fn token_rules() {
        let cfg = GuardConfig { loopback_only: false, token: Some("s3cret".into()) };
        assert!(check(&cfg, &Method::GET, &headers(&[("host", "10.0.0.5:8954")])).is_err());
        let ok = headers(&[("host", "10.0.0.5:8954"), ("authorization", "Bearer s3cret")]);
        assert!(check(&cfg, &Method::GET, &ok).is_ok());
        let bad = headers(&[("host", "10.0.0.5:8954"), ("authorization", "Bearer nope")]);
        assert!(check(&cfg, &Method::GET, &bad).is_err());
    }
}
