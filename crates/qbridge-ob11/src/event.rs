//! OB11 事件。
//!
//! 事件只需要序列化。字段由类型保证完整；平台扩展字段放在各结构的 `extra` 中，
//! 序列化时展开到顶层。

use crate::message::Message;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// 事件中 `message` 字段的格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageFormat {
    #[default]
    Array,
    String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Event {
    pub time: i64,
    pub self_id: i64,
    #[serde(flatten)]
    pub kind: EventKind,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "post_type", rename_all = "snake_case")]
pub enum EventKind {
    Message(MessageEvent),
    Notice(NoticeEvent),
    Request(RequestEvent),
    MetaEvent(MetaEvent),
}

impl Event {
    /// 序列化为 OB11 JSON。`format` 为 `String` 时，`message` 输出 CQ 字符串。
    pub fn to_value(&self, format: MessageFormat) -> Value {
        let mut value = serde_json::to_value(self).expect("events serialize to JSON");
        if let (MessageFormat::String, EventKind::Message(event)) = (format, &self.kind) {
            value["message"] = Value::String(event.message.to_cq());
        }
        value
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MessageEvent {
    #[serde(flatten)]
    pub scene: MessageScene,
    pub message_id: i32,
    pub user_id: i64,
    pub message: Message,
    pub raw_message: String,
    pub font: i32,
    pub sender: Sender,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl MessageEvent {
    /// `raw_message` 由 `message` 生成，保证两者一致。
    pub fn new(
        scene: MessageScene,
        message_id: i32,
        user_id: i64,
        message: Message,
        sender: Sender,
    ) -> Self {
        Self {
            scene,
            message_id,
            user_id,
            raw_message: message.to_cq(),
            message,
            font: 0,
            sender,
            extra: Map::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "message_type", rename_all = "snake_case")]
pub enum MessageScene {
    Private {
        sub_type: PrivateSubType,
    },
    Group {
        group_id: i64,
        sub_type: GroupSubType,
        /// 非匿名消息为 `null`，字段本身必须存在。
        anonymous: Option<Anonymous>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivateSubType {
    Friend,
    Group,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupSubType {
    Normal,
    Anonymous,
    Notice,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Anonymous {
    pub id: i64,
    pub name: String,
    pub flag: String,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct Sender {
    pub user_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<Role>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Owner,
    Admin,
    Member,
}

/// 通知事件。标准类型用对应的构造函数创建；平台自定义类型用 [`NoticeEvent::custom`]。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NoticeEvent {
    pub notice_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sub_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operator_id: Option<i64>,
    pub user_id: i64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupIncreaseType {
    Approve,
    Invite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupDecreaseType {
    Leave,
    Kick,
    KickMe,
}

impl NoticeEvent {
    pub fn custom(notice_type: impl Into<String>, user_id: i64) -> Self {
        Self {
            notice_type: notice_type.into(),
            sub_type: None,
            group_id: None,
            operator_id: None,
            user_id,
            extra: Map::new(),
        }
    }

    pub fn group_increase(
        sub_type: GroupIncreaseType,
        group_id: i64,
        user_id: i64,
        operator_id: i64,
    ) -> Self {
        let sub_type = match sub_type {
            GroupIncreaseType::Approve => "approve",
            GroupIncreaseType::Invite => "invite",
        };
        Self::group("group_increase", sub_type, group_id, user_id, operator_id)
    }

    pub fn group_decrease(
        sub_type: GroupDecreaseType,
        group_id: i64,
        user_id: i64,
        operator_id: i64,
    ) -> Self {
        let sub_type = match sub_type {
            GroupDecreaseType::Leave => "leave",
            GroupDecreaseType::Kick => "kick",
            GroupDecreaseType::KickMe => "kick_me",
        };
        Self::group("group_decrease", sub_type, group_id, user_id, operator_id)
    }

    pub fn friend_add(user_id: i64) -> Self {
        Self::custom("friend_add", user_id)
    }

    fn group(
        notice_type: &str,
        sub_type: &str,
        group_id: i64,
        user_id: i64,
        operator_id: i64,
    ) -> Self {
        Self {
            sub_type: Some(sub_type.into()),
            group_id: Some(group_id),
            operator_id: Some(operator_id),
            ..Self::custom(notice_type, user_id)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RequestEvent {
    #[serde(flatten)]
    pub kind: RequestKind,
    pub user_id: i64,
    pub comment: String,
    pub flag: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "request_type", rename_all = "snake_case")]
pub enum RequestKind {
    Friend,
    Group {
        sub_type: GroupRequestType,
        group_id: i64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupRequestType {
    Add,
    Invite,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "meta_event_type", rename_all = "snake_case")]
pub enum MetaEvent {
    Lifecycle { sub_type: LifecycleType },
    Heartbeat { status: BotStatus, interval: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleType {
    Enable,
    Disable,
    Connect,
}

/// `get_status` 和心跳事件共用的状态。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BotStatus {
    /// `None` 表示未知，序列化为 `null`。
    pub online: Option<bool>,
    pub good: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
