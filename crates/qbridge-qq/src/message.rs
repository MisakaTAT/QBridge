//! QQ API v2 的请求与响应模型。
//!
//! 这些类型只描述平台协议。OB11 消息段的解释与组合规则在 `qbridge::bridge`。

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// 发往 QQ 的消息。使用构造器保证 `msg_type` 与内容字段一致。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OutgoingMessage {
    msg_type: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    markdown: Option<Markdown>,
    #[serde(skip_serializing_if = "Option::is_none")]
    keyboard: Option<Keyboard>,
    #[serde(skip_serializing_if = "Option::is_none")]
    media: Option<MediaReference>,
    /// 被动回复的消息 ID。
    #[serde(skip_serializing_if = "Option::is_none")]
    msg_id: Option<String>,
    /// 被动回复序号。
    #[serde(skip_serializing_if = "Option::is_none")]
    msg_seq: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_reference: Option<MessageReference>,
    #[serde(skip_serializing_if = "Option::is_none")]
    card: Option<Card>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ark: Option<Ark>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_notify: Option<InputNotify>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    is_wakeup: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ark {
    pub template_id: i64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kv: Vec<ArkKv>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArkKv {
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub obj: Option<Vec<ArkObj>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArkObj {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub obj_kv: Vec<ArkObjKv>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArkObjKv {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Card {
    #[serde(rename = "type")]
    pub kind: String,
    pub content: CardContent,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CardContent {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub pic_url: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InputNotify {
    pub input_type: u8,
    pub input_second: u16,
}

impl OutgoingMessage {
    /// `msg_type=0`：纯文本。
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            msg_type: 0,
            content: Some(content.into()),
            markdown: None,
            keyboard: None,
            media: None,
            msg_id: None,
            msg_seq: None,
            message_reference: None,
            card: None,
            ark: None,
            input_notify: None,
            is_wakeup: false,
        }
    }

    /// `msg_type=2`：Markdown，可附带键盘。
    pub fn markdown(markdown: Markdown, keyboard: Option<Keyboard>) -> Self {
        Self {
            msg_type: 2,
            content: None,
            markdown: Some(markdown),
            keyboard,
            media: None,
            msg_id: None,
            msg_seq: None,
            message_reference: None,
            card: None,
            ark: None,
            input_notify: None,
            is_wakeup: false,
        }
    }

    /// `msg_type=3`：Ark 模板卡片。
    pub fn ark(ark: Ark) -> Self {
        Self {
            msg_type: 3,
            content: None,
            markdown: None,
            keyboard: None,
            media: None,
            msg_id: None,
            msg_seq: None,
            message_reference: None,
            card: None,
            ark: Some(ark),
            input_notify: None,
            is_wakeup: false,
        }
    }

    /// `msg_type=7`：已上传的富媒体，可附带文本。
    pub fn media(file_info: impl Into<String>, content: Option<String>) -> Self {
        Self {
            msg_type: 7,
            content,
            markdown: None,
            keyboard: None,
            media: Some(MediaReference {
                file_info: file_info.into(),
            }),
            msg_id: None,
            msg_seq: None,
            message_reference: None,
            card: None,
            ark: None,
            input_notify: None,
            is_wakeup: false,
        }
    }

    /// `msg_type=8`：图文卡片（群聊与单聊）。
    pub fn card(card: Card) -> Self {
        Self {
            msg_type: 8,
            content: None,
            markdown: None,
            keyboard: None,
            media: None,
            msg_id: None,
            msg_seq: None,
            message_reference: None,
            card: Some(card),
            ark: None,
            input_notify: None,
            is_wakeup: false,
        }
    }

    /// `msg_type=6`：单聊输入状态通知。
    pub fn input_notify(input_type: u8, input_second: u16) -> Self {
        Self {
            msg_type: 6,
            content: None,
            markdown: None,
            keyboard: None,
            media: None,
            msg_id: None,
            msg_seq: None,
            message_reference: None,
            card: None,
            ark: None,
            input_notify: Some(InputNotify {
                input_type,
                input_second,
            }),
            is_wakeup: false,
        }
    }

    /// C2C 互动召回标记。
    pub fn wakeup(mut self) -> Self {
        self.is_wakeup = true;
        self
    }

    /// 设置被动回复上下文。
    pub fn passive(mut self, msg_id: impl Into<String>, msg_seq: u32) -> Self {
        self.msg_id = Some(msg_id.into());
        self.msg_seq = Some(msg_seq);
        self
    }

    /// 设置原生引用。
    pub fn referencing(mut self, message_id: impl Into<String>) -> Self {
        self.message_reference = Some(MessageReference {
            message_id: message_id.into(),
        });
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageReference {
    pub message_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct MediaReference {
    file_info: String,
}

/// Markdown 正文或模板。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Markdown {
    Content {
        content: String,
    },
    Template {
        custom_template_id: String,
        params: Vec<MarkdownParam>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarkdownParam {
    pub key: String,
    pub values: Vec<String>,
}

/// 键盘：模板 ID 或自定义内容，二选一。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Keyboard {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<KeyboardContent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyboardContent {
    pub rows: Vec<KeyboardRow>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyboardRow {
    pub buttons: Vec<KeyboardButton>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyboardButton {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub render_data: RenderData,
    pub action: ButtonAction,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenderData {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visited_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<i64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ButtonAction {
    #[serde(rename = "type")]
    pub kind: i64,
    pub data: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub click_limit: Option<u64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// 富媒体上传请求。URL 或 Base64 数据二选一。
#[derive(Debug, Clone, PartialEq)]
pub struct MediaUpload {
    /// 1 图片、2 视频、3 语音、4 文件。
    pub file_type: u8,
    pub source: MediaSource,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum MediaSource {
    Url {
        url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        file_name: Option<String>,
    },
    Data {
        file_data: String,
        file_name: String,
    },
}

impl Serialize for MediaUpload {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Body<'a> {
            file_type: u8,
            srv_send_msg: bool,
            #[serde(flatten)]
            source: &'a MediaSource,
        }
        Body {
            file_type: self.file_type,
            srv_send_msg: false,
            source: &self.source,
        }
        .serialize(serializer)
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UploadedMedia {
    pub file_info: String,
    #[serde(default)]
    pub ttl: Option<u64>,
    #[serde(default)]
    pub id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UploadedMediaImage {
    #[serde(default)]
    pub image_id: String,
    pub url: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// 发送成功后的消息信息。
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SentMessage {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub timestamp: Option<String>,
    #[serde(default)]
    pub ext_info: Option<SentExtInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SentExtInfo {
    #[serde(default)]
    pub ref_idx: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BotProfile {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub bot: Option<bool>,
    #[serde(default)]
    pub share_url: Option<String>,
    #[serde(default)]
    pub welcome_msg: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserInfo {
    #[serde(default)]
    pub user_openid: Option<String>,
    #[serde(default)]
    pub nickname: Option<String>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl UserInfo {
    pub fn display_name(&self) -> Option<&str> {
        self.nickname
            .as_deref()
            .filter(|name| !name.is_empty())
            .or_else(|| self.username.as_deref().filter(|name| !name.is_empty()))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupInfo {
    #[serde(default)]
    pub group_openid: Option<String>,
    #[serde(default)]
    pub union_group_openid: Option<String>,
    #[serde(default)]
    pub group_name: String,
    #[serde(default)]
    pub group_finger_memo: Option<String>,
    #[serde(default)]
    pub group_member_num: Option<u64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Member {
    pub member_openid: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub member_role: Option<String>,
    #[serde(default)]
    pub bot: Option<bool>,
    #[serde(default)]
    pub joined_at: Option<String>,
    #[serde(default)]
    pub union_openid: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemberPage {
    #[serde(default)]
    pub members: Vec<Member>,
    #[serde(default)]
    pub next_cursor: String,
}

/// 成员禁言操作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MuteOp {
    Add {
        member: crate::openid::MemberOpenId,
        expire_at: String,
    },
    Update {
        member: crate::openid::MemberOpenId,
        expire_at: String,
    },
    Delete {
        member: crate::openid::MemberOpenId,
    },
}

impl MuteOp {
    pub fn add(member: crate::openid::MemberOpenId, expire_at: impl Into<String>) -> Self {
        Self::Add {
            member,
            expire_at: expire_at.into(),
        }
    }

    pub fn update(member: crate::openid::MemberOpenId, expire_at: impl Into<String>) -> Self {
        Self::Update {
            member,
            expire_at: expire_at.into(),
        }
    }

    pub fn delete(member: crate::openid::MemberOpenId) -> Self {
        Self::Delete { member }
    }
}

impl Serialize for MuteOp {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Body<'a> {
            op: &'static str,
            member_openid: &'a str,
            mute_expire_at: &'a str,
        }
        let (op, member, expire_at) = match self {
            Self::Add { member, expire_at } => ("add", member, expire_at.as_str()),
            Self::Update { member, expire_at } => ("update", member, expire_at.as_str()),
            Self::Delete { member } => ("del", member, ""),
        };
        Body {
            op,
            member_openid: member.as_str(),
            mute_expire_at: expire_at,
        }
        .serialize(serializer)
    }
}
