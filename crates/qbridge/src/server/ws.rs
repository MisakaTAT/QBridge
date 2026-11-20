//! 统一正向与反向 WebSocket 的文本帧读写。

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};

pub enum Ws {
    Forward(Box<axum::extract::ws::WebSocket>),
    Reverse(Box<WebSocketStream<MaybeTlsStream<TcpStream>>>),
}

impl Ws {
    pub async fn send_text(&mut self, text: &str) -> bool {
        match self {
            Self::Forward(socket) => socket
                .send(axum::extract::ws::Message::Text(text.to_owned().into()))
                .await
                .is_ok(),
            Self::Reverse(socket) => socket
                .send(Message::Text(text.to_owned().into()))
                .await
                .is_ok(),
        }
    }

    pub async fn next_text(&mut self) -> Option<String> {
        loop {
            match self {
                Self::Forward(socket) => match socket.recv().await {
                    Some(Ok(axum::extract::ws::Message::Text(text))) => {
                        return Some(text.to_string());
                    }
                    Some(Ok(axum::extract::ws::Message::Close(_))) | None => return None,
                    Some(Ok(axum::extract::ws::Message::Ping(payload))) => {
                        let _ = socket.send(axum::extract::ws::Message::Pong(payload)).await;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) => return None,
                },
                Self::Reverse(socket) => match socket.next().await {
                    Some(Ok(Message::Text(text))) => return Some(text.to_string()),
                    Some(Ok(Message::Close(_))) | None => return None,
                    Some(Ok(Message::Ping(payload))) => {
                        let _ = socket.send(Message::Pong(payload)).await;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(_)) => return None,
                },
            }
        }
    }

    pub async fn close(&mut self) {
        match self {
            Self::Forward(socket) => {
                let _ = socket.close().await;
            }
            Self::Reverse(socket) => {
                let _ = SinkExt::close(&mut **socket).await;
            }
        }
    }
}
