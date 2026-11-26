//! 反向 WebSocket 客户端。

use super::{
    Hub,
    session::{self, Role},
    ws::Ws,
};
use std::{sync::Arc, time::Duration};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest, http::HeaderValue, protocol::WebSocketConfig,
};

#[derive(Debug, Clone)]
pub struct ReverseConfig {
    pub url: url::Url,
    pub role: Role,
    pub reconnect: Duration,
}

pub fn spawn(hub: Arc<Hub>, config: ReverseConfig) -> JoinHandle<()> {
    tokio::spawn(async move {
        let cancel = hub.cancellation();
        loop {
            if cancel.is_cancelled() {
                break;
            }
            match connect(&hub, &config).await {
                Ok(socket) => {
                    tracing::info!(
                        role = config.role.header(),
                        "OB11 reverse WebSocket connected"
                    );
                    session::run(
                        hub.clone(),
                        config.role,
                        Ws::Reverse(Box::new(socket)),
                        cancel.clone(),
                    )
                    .await;
                    tracing::info!(
                        role = config.role.header(),
                        "OB11 reverse WebSocket disconnected"
                    );
                }
                Err(reason) => {
                    tracing::warn!(
                        role = config.role.header(),
                        reason,
                        "OB11 reverse WebSocket connection failed"
                    );
                }
            }
            if cancel.is_cancelled() {
                break;
            }
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tokio::time::sleep(config.reconnect) => {}
            }
        }
    })
}

async fn connect(
    hub: &Arc<Hub>,
    config: &ReverseConfig,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    &'static str,
> {
    let mut request = config
        .url
        .as_str()
        .into_client_request()
        .map_err(|_| "invalid reverse WebSocket URL")?;
    let self_id =
        HeaderValue::from_str(&hub.self_id().to_string()).map_err(|_| "invalid self_id header")?;
    request.headers_mut().insert("X-Self-ID", self_id);
    request.headers_mut().insert(
        "X-Client-Role",
        HeaderValue::from_static(config.role.header()),
    );
    request.headers_mut().insert(
        "User-Agent",
        HeaderValue::from_static(concat!("qbridge/", env!("CARGO_PKG_VERSION"))),
    );
    if let Some(token) = hub.access_token() {
        let mut authorization = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| "invalid access_token header")?;
        authorization.set_sensitive(true);
        request.headers_mut().insert("Authorization", authorization);
    }
    let mut socket_config = WebSocketConfig::default();
    socket_config.max_message_size = Some(hub.max_ws_bytes());
    socket_config.max_frame_size = Some(hub.max_ws_bytes());
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::connect_async_with_config(request, Some(socket_config), false),
    )
    .await
    .map_err(|_| "reverse WebSocket connect timed out")?;
    result
        .map(|(socket, _)| socket)
        .map_err(|_| "reverse WebSocket handshake failed")
}
