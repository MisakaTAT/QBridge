//! 配置读取、环境变量解析与校验。

use crate::server::{PostTargetConfig, session::Role};
use qbridge_ob11::MessageFormat;
use qbridge_qq::Intents;
use serde::Deserialize;
use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};
use url::Url;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("cannot read configuration: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid configuration: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("environment variable {0} is not set")]
    MissingEnv(String),
    #[error("{0}")]
    Invalid(String),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// OB11 通用设置，不属于任何一种传输。
    pub self_id: i64,
    #[serde(default)]
    pub access_token: Option<Secret>,
    #[serde(default)]
    pub message_format: MessageFormatConfig,
    #[serde(default = "default_heartbeat_ms")]
    pub heartbeat_ms: u64,
    /// 下游 HTTP API；省略该段表示不启用 HTTP API。
    #[serde(default)]
    pub http: Option<HttpConfig>,
    /// 正向 WebSocket；省略整段表示不启用。
    #[serde(default)]
    pub forward_ws: Option<ForwardWsConfig>,
    /// 反向 WebSocket，可以配置多条。
    #[serde(default)]
    pub reverse_ws: Vec<ReverseWsConfig>,
    /// HTTP POST 上报，可以配置多条。
    #[serde(default)]
    pub http_post: Vec<HttpPostConfig>,
    pub qq: QqConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub limits: Limits,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QqConfig {
    pub app_id: String,
    pub secret: Secret,
    #[serde(default)]
    pub source: QqSource,
    #[serde(default = "default_intents")]
    pub intents: Vec<IntentName>,
    #[serde(default)]
    pub webhook: Option<WebhookConfig>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum QqSource {
    #[default]
    Gateway,
    Webhook,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IntentName {
    GroupAndC2c,
    GroupMember,
    Interaction,
}

fn default_intents() -> Vec<IntentName> {
    vec![
        IntentName::GroupAndC2c,
        IntentName::GroupMember,
        IntentName::Interaction,
    ]
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebhookConfig {
    pub bind: SocketAddr,
    pub path: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    #[serde(default = "default_storage_path")]
    pub path: PathBuf,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            path: default_storage_path(),
        }
    }
}

fn default_storage_path() -> PathBuf {
    PathBuf::from("data/qbridge.sqlite")
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageFormatConfig {
    #[default]
    Array,
    String,
}

fn default_heartbeat_ms() -> u64 {
    15_000
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpConfig {
    #[serde(default = "default_http_bind")]
    pub bind: SocketAddr,
}

impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            bind: default_http_bind(),
        }
    }
}

fn default_http_bind() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 5700))
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForwardWsConfig {
    /// 可选独立监听地址；省略时与 HTTP API 共用地址，没有 HTTP API 时默认 127.0.0.1:5700。
    #[serde(default)]
    pub bind: Option<SocketAddr>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReverseWsConfig {
    pub url: String,
    #[serde(default)]
    pub role: ReverseRole,
    #[serde(default = "default_reconnect_ms")]
    pub reconnect_ms: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReverseRole {
    #[default]
    Universal,
    Api,
    Event,
}

fn default_reconnect_ms() -> u64 {
    3000
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HttpPostConfig {
    pub url: String,
    #[serde(default)]
    pub secret: Option<Secret>,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

fn default_timeout_ms() -> u64 {
    5000
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    #[serde(default = "default_body_limit")]
    pub max_body_bytes: usize,
    #[serde(default = "default_body_limit")]
    pub max_ws_bytes: usize,
    #[serde(default = "default_queue_capacity")]
    pub queue_capacity: usize,
    #[serde(default = "default_rate_limit_ms")]
    pub rate_limit_ms: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_body_bytes: default_body_limit(),
            max_ws_bytes: default_body_limit(),
            queue_capacity: default_queue_capacity(),
            rate_limit_ms: default_rate_limit_ms(),
        }
    }
}

fn default_body_limit() -> usize {
    1024 * 1024
}

fn default_queue_capacity() -> usize {
    256
}

fn default_rate_limit_ms() -> u64 {
    500
}

/// 字符串或 `{ env = "NAME" }`。
#[derive(Clone, Deserialize)]
#[serde(untagged)]
pub enum Secret {
    Literal(String),
    Env(EnvRef),
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Secret([REDACTED])")
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvRef {
    pub env: String,
}

impl Secret {
    pub fn resolve(&self, field: &str) -> Result<String, Error> {
        let value = match self {
            Self::Literal(value) => value.clone(),
            Self::Env(reference) => std::env::var(&reference.env)
                .map_err(|_| Error::MissingEnv(reference.env.clone()))?,
        };
        if value.trim().is_empty() {
            return Err(Error::Invalid(format!("{field} is empty")));
        }
        Ok(value)
    }
}

pub struct ResolvedConfig {
    pub qq: ResolvedQq,
    pub storage_path: PathBuf,
    pub self_id: i64,
    pub access_token: Option<String>,
    pub message_format: MessageFormat,
    pub heartbeat_ms: u64,
    pub http_bind: Option<SocketAddr>,
    pub forward_ws_bind: Option<SocketAddr>,
    pub reverse_ws: Vec<ResolvedReverseWs>,
    pub http_post: Vec<ResolvedHttpPost>,
    pub limits: ResolvedLimits,
}

pub struct ResolvedQq {
    pub app_id: String,
    pub secret: String,
    pub source: QqSource,
    pub intents: Intents,
    pub webhook: Option<WebhookConfig>,
}

pub struct ResolvedReverseWs {
    pub url: Url,
    pub role: Role,
    pub reconnect: Duration,
}

pub struct ResolvedHttpPost {
    pub url: Url,
    pub secret: Option<String>,
    pub timeout: Duration,
}

pub struct ResolvedLimits {
    pub max_body_bytes: usize,
    pub max_ws_bytes: usize,
    pub queue_capacity: usize,
    pub rate_limit: Duration,
}

impl ResolvedHttpPost {
    pub fn target(&self) -> PostTargetConfig {
        PostTargetConfig {
            url: self.url.clone(),
            secret: self.secret.clone(),
            timeout: self.timeout,
        }
    }
}

fn forward_ws_bind(
    forward: Option<&ForwardWsConfig>,
    http: Option<&HttpConfig>,
) -> Option<SocketAddr> {
    forward.map(|forward| {
        forward
            .bind
            .or_else(|| http.map(|http| http.bind))
            .unwrap_or_else(default_http_bind)
    })
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, Error> {
        let text = std::fs::read_to_string(path)?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self, Error> {
        Ok(toml::from_str(text)?)
    }

    pub fn resolve(&self) -> Result<ResolvedConfig, Error> {
        let app_id = self.qq.app_id.trim();
        if app_id.is_empty() {
            return Err(Error::Invalid("qq.app_id is empty".into()));
        }
        let secret = self.qq.secret.resolve("qq.secret")?;
        if self.qq.intents.is_empty() {
            return Err(Error::Invalid("qq.intents is empty".into()));
        }
        let mut intents = Intents::empty();
        for intent in &self.qq.intents {
            intents = intents.union(match intent {
                IntentName::GroupAndC2c => Intents::GROUP_AND_C2C,
                IntentName::GroupMember => Intents::GROUP_MEMBER,
                IntentName::Interaction => Intents::INTERACTION,
            });
        }
        if self.qq.source == QqSource::Webhook && self.qq.webhook.is_none() {
            return Err(Error::Invalid(
                "qq.webhook is required when source is webhook".into(),
            ));
        }
        if let Some(webhook) = &self.qq.webhook
            && (!webhook.path.starts_with('/') || webhook.path.len() > 128)
        {
            return Err(Error::Invalid(
                "qq.webhook.path must start with '/' and be at most 128 bytes".into(),
            ));
        }
        if self.self_id <= 0 {
            return Err(Error::Invalid("self_id must be positive".into()));
        }
        let access_token = self
            .access_token
            .as_ref()
            .map(|secret| secret.resolve("access_token"))
            .transpose()?;
        if self.limits.max_body_bytes == 0
            || self.limits.max_ws_bytes == 0
            || self.limits.queue_capacity == 0
        {
            return Err(Error::Invalid(
                "limits must have positive sizes and queue capacity".into(),
            ));
        }
        if self.http.is_none() && self.forward_ws.is_none() {
            return Err(Error::Invalid(
                "at least one of http or forward_ws must be configured".into(),
            ));
        }
        let mut reverse_ws = Vec::new();
        for target in &self.reverse_ws {
            let url = Url::parse(&target.url)
                .map_err(|_| Error::Invalid("reverse_ws.url is invalid".into()))?;
            if !matches!(url.scheme(), "ws" | "wss") || url.host_str().is_none() {
                return Err(Error::Invalid(
                    "reverse_ws.url must be a ws:// or wss:// URL".into(),
                ));
            }
            if target.reconnect_ms == 0 {
                return Err(Error::Invalid(
                    "reverse_ws.reconnect_ms must be positive".into(),
                ));
            }
            reverse_ws.push(ResolvedReverseWs {
                url,
                role: match target.role {
                    ReverseRole::Universal => Role::Universal,
                    ReverseRole::Api => Role::Api,
                    ReverseRole::Event => Role::Event,
                },
                reconnect: Duration::from_millis(target.reconnect_ms),
            });
        }
        let mut http_post = Vec::new();
        for target in &self.http_post {
            let url = Url::parse(&target.url)
                .map_err(|_| Error::Invalid("http_post.url is invalid".into()))?;
            if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
                return Err(Error::Invalid(
                    "http_post.url must be an http:// or https:// URL".into(),
                ));
            }
            if target.timeout_ms == 0 {
                return Err(Error::Invalid(
                    "http_post.timeout_ms must be positive".into(),
                ));
            }
            http_post.push(ResolvedHttpPost {
                url,
                secret: target
                    .secret
                    .as_ref()
                    .map(|secret| secret.resolve("http_post.secret"))
                    .transpose()?,
                timeout: Duration::from_millis(target.timeout_ms),
            });
        }
        Ok(ResolvedConfig {
            qq: ResolvedQq {
                app_id: app_id.to_owned(),
                secret,
                source: self.qq.source,
                intents,
                webhook: self.qq.webhook.clone(),
            },
            storage_path: self.storage.path.clone(),
            self_id: self.self_id,
            access_token,
            message_format: match self.message_format {
                MessageFormatConfig::Array => MessageFormat::Array,
                MessageFormatConfig::String => MessageFormat::String,
            },
            heartbeat_ms: self.heartbeat_ms,
            http_bind: self.http.as_ref().map(|http| http.bind),
            forward_ws_bind: forward_ws_bind(self.forward_ws.as_ref(), self.http.as_ref()),
            reverse_ws,
            http_post,
            limits: ResolvedLimits {
                max_body_bytes: self.limits.max_body_bytes,
                max_ws_bytes: self.limits.max_ws_bytes,
                queue_capacity: self.limits.queue_capacity,
                rate_limit: Duration::from_millis(self.limits.rate_limit_ms),
            },
        })
    }
}
