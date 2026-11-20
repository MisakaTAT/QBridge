//! QQ OpenID 的 newtype。构造时校验字符集，避免把任意字符串拼进请求路径。

use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// OpenID 不合法：为空或包含字母、数字、下划线、连字符之外的字符。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid openid")]
pub struct OpenIdError;

macro_rules! define_openid {
    ($name:ident, $label:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            /// 校验并构造 OpenID。
            pub fn new(value: impl Into<String>) -> Result<Self, OpenIdError> {
                let value = value.into();
                if value.is_empty()
                    || !value
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
                {
                    return Err(OpenIdError);
                }
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl FromStr for $name {
            type Err = OpenIdError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }

        impl TryFrom<&str> for $name {
            type Error = OpenIdError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl TryFrom<String> for $name {
            type Error = OpenIdError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }
    };
}

define_openid!(UserOpenId, "user");
define_openid!(GroupOpenId, "group");
define_openid!(MemberOpenId, "member");

/// QQ 消息的接收方：单聊用户或群聊。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Peer {
    User(UserOpenId),
    Group(GroupOpenId),
}

impl Peer {
    pub fn openid(&self) -> &str {
        match self {
            Self::User(openid) => openid.as_str(),
            Self::Group(openid) => openid.as_str(),
        }
    }
}
