//! QQ 事件类型。
//!
//! 按事件名 `t` 反序列化为具体类型；无法识别的 `t` 或解析失败的 `d`
//! 降级为 [`Dispatch::Unknown`]，事件流不中断。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// 一条 QQ 事件。`id` 是 Webhook 的事件 ID。
#[derive(Debug, Clone, PartialEq)]
pub struct Envelope {
    pub id: Option<String>,
    pub dispatch: Dispatch,
}

impl Envelope {
    /// 从 Webhook/`op=0` 的完整 JSON 构造。缺少 `t` 时返回 `None`。
    pub fn from_json(value: &Value) -> Option<Self> {
        let event_type = value.get("t")?.as_str()?;
        let id = value.get("id").and_then(Value::as_str).map(str::to_owned);
        let data = value.get("d").cloned().unwrap_or(Value::Null);
        Some(Self {
            id,
            dispatch: Dispatch::parse(event_type, data),
        })
    }

    pub fn parse(id: Option<String>, event_type: &str, data: Value) -> Self {
        Self {
            id,
            dispatch: Dispatch::parse(event_type, data),
        }
    }
}

/// QQ 机器人订阅的事件。
#[derive(Debug, Clone, PartialEq)]
pub enum Dispatch {
    C2cMessage(C2cMessage),
    GroupAtMessage(GroupMessage),
    GroupMessage(GroupMessage),
    FriendAdd(UserEvent),
    FriendDel(UserEvent),
    GroupAddRobot(GroupRobotEvent),
    GroupDelRobot(GroupRobotEvent),
    GroupMsgReceive(GroupRobotEvent),
    GroupMsgReject(GroupRobotEvent),
    GroupMemberAdd(GroupMemberEvent),
    GroupMemberRemove(GroupMemberEvent),
    GroupJoinRequest(GroupJoinRequest),
    C2cMsgReceive(UserEvent),
    C2cMsgReject(UserEvent),
    Interaction(Interaction),
    /// 未登记的事件名或解析失败的事件体。`READY` 也走这里，由上层读取机器人昵称。
    Unknown {
        t: String,
        d: Value,
    },
}

impl Dispatch {
    pub fn parse(event_type: &str, data: Value) -> Self {
        macro_rules! typed {
            ($variant:ident, $ty:ty) => {
                match serde_json::from_value::<$ty>(data.clone()) {
                    Ok(event) => return Self::$variant(event),
                    Err(error) => {
                        tracing::warn!(
                            event_type,
                            reason = %error,
                            "QQ event payload does not match its type; downgrading to Unknown"
                        );
                    }
                }
            };
        }
        match event_type {
            "C2C_MESSAGE_CREATE" => typed!(C2cMessage, C2cMessage),
            "GROUP_AT_MESSAGE_CREATE" => typed!(GroupAtMessage, GroupMessage),
            "GROUP_MESSAGE_CREATE" => typed!(GroupMessage, GroupMessage),
            "FRIEND_ADD" => typed!(FriendAdd, UserEvent),
            "FRIEND_DEL" => typed!(FriendDel, UserEvent),
            "GROUP_ADD_ROBOT" => typed!(GroupAddRobot, GroupRobotEvent),
            "GROUP_DEL_ROBOT" => typed!(GroupDelRobot, GroupRobotEvent),
            "GROUP_MSG_RECEIVE" => typed!(GroupMsgReceive, GroupRobotEvent),
            "GROUP_MSG_REJECT" => typed!(GroupMsgReject, GroupRobotEvent),
            "GROUP_MEMBER_ADD" => typed!(GroupMemberAdd, GroupMemberEvent),
            "GROUP_MEMBER_REMOVE" => typed!(GroupMemberRemove, GroupMemberEvent),
            "GROUP_JOIN_REQUEST" => typed!(GroupJoinRequest, GroupJoinRequest),
            "C2C_MSG_RECEIVE" => typed!(C2cMsgReceive, UserEvent),
            "C2C_MSG_REJECT" => typed!(C2cMsgReject, UserEvent),
            "INTERACTION_CREATE" => typed!(Interaction, Interaction),
            _ => {}
        }
        tracing::debug!(event_type, "QQ event has no typed mapping");
        Self::Unknown {
            t: event_type.to_owned(),
            d: data,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Author {
    #[serde(default)]
    pub user_openid: Option<String>,
    #[serde(default)]
    pub member_openid: Option<String>,
    #[serde(default)]
    pub union_openid: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub member_role: Option<String>,
    #[serde(default)]
    pub bot: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Author {
    /// 单聊发送者。
    pub fn user_openid(&self) -> Option<&str> {
        self.user_openid.as_deref()
    }

    /// 群聊发送者。
    pub fn member_openid(&self) -> Option<&str> {
        self.member_openid.as_deref()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub width: Option<u64>,
    #[serde(default)]
    pub height: Option<u64>,
    #[serde(default)]
    pub voice_wav_url: Option<String>,
    #[serde(default)]
    pub asr_refer_text: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct C2cMessage {
    pub id: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub timestamp: String,
    pub author: Author,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    #[serde(default)]
    pub message_scene: Option<Value>,
    #[serde(default)]
    pub message_type: Option<i64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupMessage {
    pub id: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub timestamp: String,
    pub author: Author,
    pub group_openid: String,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    #[serde(default)]
    pub message_scene: Option<Value>,
    #[serde(default)]
    pub message_type: Option<i64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserEvent {
    pub openid: String,
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupRobotEvent {
    pub group_openid: String,
    #[serde(default)]
    pub op_member_openid: Option<String>,
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Interaction {
    pub id: String,
    #[serde(rename = "type", default)]
    pub kind: Option<i64>,
    #[serde(default)]
    pub chat_type: Option<i64>,
    #[serde(default)]
    pub group_openid: Option<String>,
    #[serde(default)]
    pub group_member_openid: Option<String>,
    #[serde(default)]
    pub user_openid: Option<String>,
    #[serde(default)]
    pub button_data: Option<String>,
    #[serde(default)]
    pub button_id: Option<String>,
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default)]
    pub data: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupMemberEvent {
    pub group_openid: String,
    pub member_openid: String,
    #[serde(default)]
    pub op_member_openid: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub timestamp: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupJoinRequest {
    pub group_openid: String,
    pub join_request_id: String,
    pub member_openid: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub apply_at: Option<Value>,
    #[serde(default)]
    pub risk_tips: Option<String>,
    #[serde(default)]
    pub apply_source: Option<String>,
    #[serde(default)]
    pub invited_by: Option<String>,
    #[serde(default)]
    pub bot: Option<bool>,
    #[serde(default)]
    pub verify_info: Option<Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
