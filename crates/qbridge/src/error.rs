//! 动作错误。`retcode` 与 `reason` 只在本模块定义。

use crate::store;
use qbridge_ob11::ParseError;
use qbridge_qq::Error as QqError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedKind {
    Action,
    Segment,
    MessageCombination,
    SegmentOption,
    ForwardMedia,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotFoundKind {
    MessageNotCached,
    MediaNotFound,
    UnknownTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyContextKind {
    Unavailable,
    LimitExceeded,
    QuoteReferenceUnavailable,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ActionError {
    BadParam(String),
    UnknownAction,
    Unsupported(UnsupportedKind),
    NotFound(NotFoundKind),
    QueueFull,
    ShuttingDown,
    IdExhausted,
    Storage,
    ReplyContext(ReplyContextKind),
    SendOutcomeUnknown,
    NotOwned,
    Platform(QqError),
}

impl ActionError {
    pub fn retcode(&self) -> i64 {
        match self {
            Self::BadParam(_) => 1400,
            Self::UnknownAction => 1404,
            Self::Unsupported(_) => 100,
            Self::NotFound(_) => 101,
            Self::QueueFull => 102,
            Self::ShuttingDown => 103,
            Self::IdExhausted => 106,
            Self::Storage => 110,
            Self::ReplyContext(_) => 111,
            Self::SendOutcomeUnknown => 112,
            Self::NotOwned => 113,
            Self::Platform(error) => match error {
                QqError::Api { .. } => 120,
                QqError::Auth(_) => 121,
                QqError::Network(_) => 122,
                QqError::Decode(_) => 123,
            },
        }
    }

    pub fn reason(&self) -> &'static str {
        match self {
            Self::BadParam(_) => "BAD_PARAMETER",
            Self::UnknownAction => "UNKNOWN_ACTION",
            Self::Unsupported(kind) => match kind {
                UnsupportedKind::Action => "UNSUPPORTED_ACTION",
                UnsupportedKind::Segment => "UNSUPPORTED_SEGMENT",
                UnsupportedKind::MessageCombination => "UNSUPPORTED_MESSAGE_COMBINATION",
                UnsupportedKind::SegmentOption => "UNSUPPORTED_SEGMENT_OPTION",
                UnsupportedKind::ForwardMedia => "UNSUPPORTED_FORWARD_MEDIA",
            },
            Self::NotFound(kind) => match kind {
                NotFoundKind::MessageNotCached => "MESSAGE_NOT_CACHED",
                NotFoundKind::MediaNotFound => "MEDIA_NOT_FOUND",
                NotFoundKind::UnknownTarget => "UNKNOWN_TARGET",
            },
            Self::QueueFull => "QUEUE_FULL",
            Self::ShuttingDown => "SHUTTING_DOWN",
            Self::IdExhausted => "MESSAGE_ID_EXHAUSTED",
            Self::Storage => "STORAGE_ERROR",
            Self::ReplyContext(kind) => match kind {
                ReplyContextKind::Unavailable => "REPLY_CONTEXT_UNAVAILABLE",
                ReplyContextKind::LimitExceeded => "REPLY_LIMIT_EXCEEDED",
                ReplyContextKind::QuoteReferenceUnavailable => "QUOTE_REFERENCE_UNAVAILABLE",
            },
            Self::SendOutcomeUnknown => "SEND_OUTCOME_UNKNOWN",
            Self::NotOwned => "MESSAGE_NOT_OWNED",
            Self::Platform(error) => match error {
                QqError::Api { .. } => "QQ_API_ERROR",
                QqError::Auth(_) => "QQ_AUTH_ERROR",
                QqError::Network(_) => "QQ_NETWORK_ERROR",
                QqError::Decode(_) => "QQ_DECODE_ERROR",
            },
        }
    }

    /// 面向调用方的说明，不用于机器判断。
    pub fn message(&self) -> String {
        match self {
            Self::BadParam(message) => message.clone(),
            Self::UnknownAction => "unknown action".into(),
            Self::Unsupported(kind) => match kind {
                UnsupportedKind::Action => "this action is not supported".into(),
                UnsupportedKind::Segment => "this message segment is not supported".into(),
                UnsupportedKind::MessageCombination => {
                    "this combination of message segments is not supported".into()
                }
                UnsupportedKind::SegmentOption => {
                    "this message segment option is not supported".into()
                }
                UnsupportedKind::ForwardMedia => {
                    "forward messages only support public image URLs".into()
                }
            },
            Self::NotFound(kind) => match kind {
                NotFoundKind::MessageNotCached => "message is not cached".into(),
                NotFoundKind::MediaNotFound => "media is not cached".into(),
                NotFoundKind::UnknownTarget => "unknown target".into(),
            },
            Self::QueueFull => "action queue is full".into(),
            Self::ShuttingDown => "bridge is shutting down".into(),
            Self::IdExhausted => "message id space is exhausted".into(),
            Self::Storage => "storage operation failed".into(),
            Self::ReplyContext(kind) => match kind {
                ReplyContextKind::Unavailable => "reply context is unavailable".into(),
                ReplyContextKind::LimitExceeded => "reply limit exceeded".into(),
                ReplyContextKind::QuoteReferenceUnavailable => {
                    "quote reference is unavailable".into()
                }
            },
            Self::SendOutcomeUnknown => {
                "QQ accepted the message but local caching failed; do not retry".into()
            }
            Self::NotOwned => "only bot messages and group member messages can be recalled".into(),
            Self::Platform(error) => error.to_string(),
        }
    }
}

impl From<ParseError> for ActionError {
    fn from(error: ParseError) -> Self {
        match error {
            ParseError::Unsupported(_) => Self::Unsupported(UnsupportedKind::Action),
            ParseError::BadParam(message) => Self::BadParam(message),
        }
    }
}

impl From<store::Error> for ActionError {
    fn from(error: store::Error) -> Self {
        match error {
            store::Error::UnknownTarget => Self::NotFound(NotFoundKind::UnknownTarget),
            store::Error::MessageNotCached => Self::NotFound(NotFoundKind::MessageNotCached),
            store::Error::ReplyContextUnavailable => {
                Self::ReplyContext(ReplyContextKind::Unavailable)
            }
            store::Error::ReplyLimitExceeded => Self::ReplyContext(ReplyContextKind::LimitExceeded),
            store::Error::QuoteReferenceUnavailable => {
                Self::ReplyContext(ReplyContextKind::QuoteReferenceUnavailable)
            }
            store::Error::NotOwned => Self::NotOwned,
            store::Error::IdExhausted => Self::IdExhausted,
            store::Error::InvalidConfig
            | store::Error::Database(_)
            | store::Error::AppMismatch
            | store::Error::SchemaVersion(_)
            | store::Error::IdentityCollision
            | store::Error::InvalidContent => Self::Storage,
        }
    }
}

impl From<QqError> for ActionError {
    fn from(error: QqError) -> Self {
        Self::Platform(error)
    }
}
