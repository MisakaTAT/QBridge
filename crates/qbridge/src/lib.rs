//! QBridge：把 QQ 官方机器人 API v2 桥接为 OneBot v11。
//!
//! 本 crate 包含存储、桥接逻辑、OB11 服务端和应用组装。

#![forbid(unsafe_code)]

pub mod app;
pub mod bridge;
pub mod config;
pub mod error;
pub mod server;
pub mod store;

pub use error::{ActionError, NotFoundKind, ReplyContextKind, UnsupportedKind};

/// 程序版本。发布构建由 CI 从 Git tag 注入 `QBRIDGE_VERSION`；本地构建带 `-dev` 后缀。
pub const VERSION: &str = match option_env!("QBRIDGE_VERSION") {
    Some(version) if !version.is_empty() => version,
    _ => concat!(env!("CARGO_PKG_VERSION"), "-dev"),
};
