//! QQ 平台错误。桥接层把它映射为 `ActionError::Platform`。

use std::fmt;

/// QQ 官方 API、鉴权、网络和响应解析错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// 鉴权失败：凭证无效、token 被拒绝或权限不足。
    Auth(String),
    /// 平台返回了非 0 的业务错误码。
    Api {
        http_status: u16,
        code: Option<i64>,
        message: String,
    },
    /// 连接失败、超时或响应读取失败。
    Network(String),
    /// 响应无法解析或缺少必需字段。
    Decode(String),
}

impl Error {
    pub fn is_auth(&self) -> bool {
        matches!(self, Self::Auth(_))
    }

    pub fn auth(message: impl Into<String>) -> Self {
        Self::Auth(message.into())
    }

    pub fn network(message: impl Into<String>) -> Self {
        Self::Network(message.into())
    }

    pub fn decode(message: impl Into<String>) -> Self {
        Self::Decode(message.into())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auth(message) => write!(formatter, "QQ authentication failed: {message}"),
            Self::Api {
                http_status,
                code,
                message,
            } => match code {
                Some(code) => write!(
                    formatter,
                    "QQ API error (HTTP {http_status}, code {code}): {message}"
                ),
                None => write!(
                    formatter,
                    "QQ API error (HTTP {http_status}, code none): {message}"
                ),
            },
            Self::Network(message) => write!(formatter, "QQ network error: {message}"),
            Self::Decode(message) => write!(formatter, "QQ response decode error: {message}"),
        }
    }
}

impl std::error::Error for Error {}
