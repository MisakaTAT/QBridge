//! OB11 HTTP API 与正向 WebSocket。

use super::{
    Hub,
    session::{self, Role},
    ws::Ws,
};
use crate::error::ActionError;
use axum::{
    Json, Router,
    body::Bytes,
    extract::{
        DefaultBodyLimit, Path, Query, State,
        ws::{WebSocketUpgrade, rejection::WebSocketUpgradeRejection},
    },
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response as HttpResponse},
    routing::{any, get},
};
use qbridge_ob11::{Call, Response};
use serde_json::{Map, Value};
use std::{collections::HashMap, net::SocketAddr, sync::Arc};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Routes {
    pub http: bool,
    pub forward_ws: bool,
}

impl Routes {
    pub const HTTP: Self = Self {
        http: true,
        forward_ws: false,
    };
    pub const FORWARD_WS: Self = Self {
        http: false,
        forward_ws: true,
    };
    pub const BOTH: Self = Self {
        http: true,
        forward_ws: true,
    };
}

pub async fn serve(
    hub: Arc<Hub>,
    bind: SocketAddr,
    cancel: CancellationToken,
    routes: Routes,
) -> Result<(), std::io::Error> {
    let listener = tokio::net::TcpListener::bind(bind).await?;
    serve_listener(hub, listener, cancel, routes).await
}

/// 使用已绑定的监听器启动服务，便于调用方提前获知端口。
pub async fn serve_listener(
    hub: Arc<Hub>,
    listener: tokio::net::TcpListener,
    cancel: CancellationToken,
    routes: Routes,
) -> Result<(), std::io::Error> {
    let bind = listener.local_addr()?;
    tracing::info!(
        bind = %bind,
        http = routes.http,
        forward_ws = routes.forward_ws,
        "OB11 HTTP server listening"
    );
    axum::serve(listener, router(hub, routes))
        .with_graceful_shutdown(async move { cancel.cancelled().await })
        .await
}

pub fn router(hub: Arc<Hub>, routes: Routes) -> Router {
    let body_limit = hub.max_body_bytes();
    let mut router = Router::new();
    if routes.forward_ws {
        router = router
            .route("/", get(ws_universal))
            .route("/api", get(ws_api))
            .route("/api/", get(ws_api))
            .route("/event", get(ws_event))
            .route("/event/", get(ws_event));
    }
    if routes.http {
        router = router
            .route("/{action}", any(http_action))
            .route("/{action}/", any(http_action));
    }
    router
        .layer(DefaultBodyLimit::max(body_limit))
        .with_state(hub)
}

async fn http_action(
    State(hub): State<Arc<Hub>>,
    Path(action): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> HttpResponse {
    if let Err(status) = authorize(&hub, &headers, &query) {
        return status.into_response();
    }
    let mut params = Map::new();
    for (key, value) in &query {
        if key != "access_token" {
            params.insert(key.clone(), Value::String(value.clone()));
        }
    }
    match method {
        Method::GET => {}
        Method::POST => {
            let content_type = headers
                .get(axum::http::header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            if content_type == "application/json" {
                match serde_json::from_slice::<Value>(&body) {
                    Ok(Value::Object(body)) => params.extend(body),
                    _ => return StatusCode::BAD_REQUEST.into_response(),
                }
            } else if content_type == "application/x-www-form-urlencoded" {
                let mut form = Map::new();
                for (key, value) in url::form_urlencoded::parse(&body) {
                    form.insert(key.into_owned(), Value::String(value.into_owned()));
                }
                params.extend(form);
            } else {
                return StatusCode::NOT_ACCEPTABLE.into_response();
            }
        }
        _ => return StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
    let echo = params.remove("echo");
    let call = match Call::parse(&action, params) {
        Ok(call) => call,
        Err(error) => {
            let response = failure(ActionError::from(error)).with_echo(echo);
            return Json(response).into_response();
        }
    };
    let response = hub.dispatch(call).await.with_echo(echo);
    Json(response).into_response()
}

fn failure(error: ActionError) -> Response {
    Response::failed(error.retcode(), error.reason(), error.message())
}

async fn ws_universal(
    State(hub): State<Arc<Hub>>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> HttpResponse {
    upgrade(hub, Role::Universal, query, headers, ws).await
}

async fn ws_api(
    State(hub): State<Arc<Hub>>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> HttpResponse {
    upgrade(hub, Role::Api, query, headers, ws).await
}

async fn ws_event(
    State(hub): State<Arc<Hub>>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> HttpResponse {
    upgrade(hub, Role::Event, query, headers, ws).await
}

async fn upgrade(
    hub: Arc<Hub>,
    role: Role,
    query: HashMap<String, String>,
    headers: HeaderMap,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> HttpResponse {
    if let Err(status) = authorize(&hub, &headers, &query) {
        return status.into_response();
    }
    let Ok(ws) = ws else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let cancel = hub.cancellation();
    let max = hub.max_ws_bytes();
    ws.max_message_size(max)
        .max_frame_size(max)
        .on_upgrade(move |socket| async move {
            tracing::info!(role = role.header(), "OB11 forward WebSocket connected");
            session::run(hub, role, Ws::Forward(Box::new(socket)), cancel).await;
            tracing::info!(role = role.header(), "OB11 forward WebSocket disconnected");
        })
}

fn authorize(
    hub: &Hub,
    headers: &HeaderMap,
    query: &HashMap<String, String>,
) -> Result<(), StatusCode> {
    let Some(expected) = hub.access_token() else {
        return Ok(());
    };
    let header = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let provided = header.or_else(|| query.get("access_token").map(String::as_str));
    match provided {
        None => Err(StatusCode::UNAUTHORIZED),
        Some(provided) if provided == expected => Ok(()),
        Some(_) => Err(StatusCode::FORBIDDEN),
    }
}
