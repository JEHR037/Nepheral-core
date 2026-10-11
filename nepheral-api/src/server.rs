// SPDX-License-Identifier: BUSL-1.1
use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use tower_http::cors::{Any, CorsLayer};

use crate::backend::MeshBackend;
use crate::{ollama, openai};

pub struct ServerConfig {
    pub bind: SocketAddr,
    pub security: ApiSecurity,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], 11434)),
            security: ApiSecurity::default(),
        }
    }
}

/// Access policy for the HTTP daemon. `auth_token`, when set, gates every
/// route but `/healthz` and `/api/version` behind `Authorization: Bearer` or
/// `X-Nepheral-Api-Token`; session tokens travel in `X-Nepheral-Session-Token`
/// instead. `allowed_origins` empty (the default) emits no CORS headers, so
/// no web page can call this API from a browser.
#[derive(Debug, Clone, Default)]
pub struct ApiSecurity {
    pub auth_token: Option<String>,
    pub allowed_origins: Vec<String>,
    pub allow_unauthenticated: bool,
    /// Decides per request, in place of `auth_token`, for a daemon that
    /// keeps no copy of its token (only a hash of it) or can rotate it.
    pub gate: Option<Arc<dyn ApiGate>>,
}

/// Checks a presented token without the daemon holding the token itself.
pub trait ApiGate: Send + Sync + std::fmt::Debug {
    /// Whether `presented` is the current token.
    fn accepts(&self, presented: &str) -> bool;
    /// Called for each request let in.
    fn let_in(&self) {}
}

pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

fn host_is_literal_or_localhost(host: &str) -> bool {
    let name = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or("")
    } else {
        host.rsplit_once(':').map(|(h, _)| h).unwrap_or(host)
    };
    name.eq_ignore_ascii_case("localhost") || name.parse::<std::net::IpAddr>().is_ok()
}

pub async fn serve<B: MeshBackend>(
    backend: Arc<B>,
    config: ServerConfig,
) -> Result<(), std::io::Error> {
    let app = router(backend, config.security);
    let listener = tokio::net::TcpListener::bind(config.bind).await?;
    tracing::info!(addr = %config.bind, "nepheral-api listening");
    axum::serve(listener, app).await
}

pub fn router<B: MeshBackend>(backend: Arc<B>, security: ApiSecurity) -> Router {
    let mut app = Router::new()
        .route("/v1/models", get(openai::list_models::<B>))
        .route("/v1/chat/completions", post(openai::chat_completions::<B>))
        .route(
            "/v1/routing",
            get(openai::get_routing::<B>).put(openai::put_routing::<B>),
        )
        .route("/v1/sessions", post(openai::open_session::<B>))
        .route("/api/chat", post(ollama::chat::<B>))
        .route("/api/generate", post(ollama::generate::<B>))
        .route("/api/tags", get(ollama::tags::<B>))
        .route("/api/version", get(ollama::version))
        .route("/healthz", get(healthz))
        .with_state(backend);

    let static_token = security.auth_token.filter(|t| !t.trim().is_empty());
    match (security.gate, static_token) {
        (Some(gate), _) => {
            app = app.layer(axum::middleware::from_fn(
                move |req: Request, next: Next| {
                    let gate = gate.clone();
                    async move { require_gate(gate, req, next).await }
                },
            ));
        }
        (None, Some(token)) => {
            app = app.layer(axum::middleware::from_fn(
                move |req: Request, next: Next| {
                    let token = token.clone();
                    async move { require_api_token(Some(token), req, next).await }
                },
            ));
        }
        (None, None) if !security.allow_unauthenticated => {
            app =
                app.layer(axum::middleware::from_fn(
                    move |req: Request, next: Next| async move {
                        require_api_token(None, req, next).await
                    },
                ));
        }
        (None, None) => {}
    }

    app = app.layer(axum::middleware::from_fn(reject_rebound_hosts));

    let origins: Vec<HeaderValue> = security
        .allowed_origins
        .iter()
        .map(|o| o.trim())
        .filter(|o| !o.is_empty() && *o != "*")
        .filter_map(|o| o.parse().ok())
        .collect();
    if !origins.is_empty() {
        let cors = CorsLayer::new()
            .allow_origin(origins)
            .allow_methods(Any)
            .allow_headers(Any);
        app = app.layer(cors);
    }

    app
}

async fn reject_rebound_hosts(req: Request, next: Next) -> axum::response::Response {
    let host_ok = req
        .headers()
        .get("host")
        .and_then(|v| v.to_str().ok())
        .map(host_is_literal_or_localhost)
        .unwrap_or(true);
    if host_ok {
        return next.run(req).await;
    }
    (
        StatusCode::MISDIRECTED_REQUEST,
        axum::Json(serde_json::json!({
            "error": {
                "message": "requests must address the daemon by IP or localhost",
                "type": "misdirected",
                "code": "bad_host"
            }
        })),
    )
        .into_response()
}

fn is_open_path(req: &Request) -> bool {
    let path = req.uri().path();
    path == "/healthz" || path == "/api/version"
}

/// The tokens a request presents: `Authorization: Bearer` and
/// `X-Nepheral-Api-Token`.
fn presented_tokens(req: &Request) -> impl Iterator<Item = &str> {
    let bearer = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let header = req
        .headers()
        .get("x-nepheral-api-token")
        .and_then(|v| v.to_str().ok());
    bearer.into_iter().chain(header)
}

fn carries_token(req: &Request, token: &str) -> bool {
    presented_tokens(req).any(|v| constant_time_eq(v.as_bytes(), token.as_bytes()))
}

async fn require_gate(
    gate: Arc<dyn ApiGate>,
    req: Request,
    next: Next,
) -> axum::response::Response {
    if is_open_path(&req) {
        return next.run(req).await;
    }
    if presented_tokens(&req).any(|t| gate.accepts(t)) {
        gate.let_in();
        return next.run(req).await;
    }
    unauthorized()
}

async fn require_api_token(
    token: Option<String>,
    req: Request,
    next: Next,
) -> axum::response::Response {
    if is_open_path(&req) {
        return next.run(req).await;
    }
    if token.is_some_and(|t| carries_token(&req, &t)) {
        return next.run(req).await;
    }
    unauthorized()
}

fn unauthorized() -> axum::response::Response {
    (
        StatusCode::UNAUTHORIZED,
        axum::Json(serde_json::json!({
            "error": {
                "message": "missing or invalid API token — send Authorization: Bearer <token>; the Nepheral app shows it in its API tab, `nepheral-node status` prints it on a headless node",
                "type": "unauthorized",
                "code": "bad_api_token"
            }
        })),
    )
        .into_response()
}

async fn healthz() -> &'static str {
    "ok"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_comparison_is_exact() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }

    #[test]
    fn rebinding_hosts_are_refused() {
        assert!(host_is_literal_or_localhost("127.0.0.1:11434"));
        assert!(host_is_literal_or_localhost("localhost:11434"));
        assert!(host_is_literal_or_localhost("[::1]:11434"));
        assert!(host_is_literal_or_localhost("192.168.1.5"));
        assert!(!host_is_literal_or_localhost("attacker.example:11434"));
        assert!(!host_is_literal_or_localhost("rebind.evil.com"));
    }
}
