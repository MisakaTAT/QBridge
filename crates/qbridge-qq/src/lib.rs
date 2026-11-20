//! QQ 官方机器人 API v2 客户端与事件源。
//!
//! 本 crate 只封装 QQ 平台：HTTP 接口、Gateway WebSocket 和 Webhook 事件源。
//! 它不依赖 OneBot v11，也不做 OB11 与 QQ 之间的消息转换。

#![forbid(unsafe_code)]

pub mod client;
pub mod dispatch;
pub mod error;
pub mod gateway;
pub mod message;
pub mod openid;
pub mod redact;
pub mod webhook;

pub use client::{Client, Intents, StreamRequest};
pub use dispatch::{Dispatch, Envelope};
pub use error::Error;
pub use gateway::{GatewayConfig, GatewayStream, Session, SessionStore};
pub use message::{
    Card, CardContent, GroupInfo, Keyboard, KeyboardContent, Markdown, MarkdownParam, MediaSource,
    MediaUpload, Member, MemberPage, MessageReference, MuteOp, OutgoingMessage, SentMessage,
    UploadedMedia,
};
pub use openid::{GroupOpenId, MemberOpenId, OpenIdError, Peer, UserOpenId};
pub use webhook::{Webhook, WebhookConfig, WebhookStream};
