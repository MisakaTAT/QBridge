//! OB11 消息：类型化的消息段，以及与 CQ 字符串、段数组两种格式的互转。

mod cq;

pub use cq::{escape_param, escape_text, unescape_param, unescape_text};

use crate::lenient::{parse_int, scalar_text};
use serde::{Serialize, Serializer};
use serde_json::{Map, Value};

/// 消息格式错误，例如段参数类型不对。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct MessageError(pub String);

fn invalid(message: impl Into<String>) -> MessageError {
    MessageError(message.into())
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Message(pub Vec<Segment>);

#[derive(Debug, Clone, PartialEq)]
pub enum Segment {
    Text(String),
    Face(String),
    Image(Media),
    Record(Media),
    Video(Media),
    File(Media),
    At(AtTarget),
    Reply(i32),
    Node(Node),
    /// 标准之外或本层不解释的段，例如 `markdown`、`keyboard`。
    Other {
        kind: String,
        data: Map<String, Value>,
    },
}

/// 图片、语音、视频共用的参数。`extra` 保存 `type`、`cache` 等其余参数。
#[derive(Debug, Clone, PartialEq)]
pub struct Media {
    pub file: String,
    pub url: Option<String>,
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtTarget {
    User(i64),
    All,
}

/// 合并转发节点。
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    /// 引用一条已有消息。
    Reference(i32),
    /// 自定义节点。作者字段兼容 `user_id`/`uin` 与 `nickname`/`name` 两种写法。
    Custom {
        user_id: Option<String>,
        nickname: Option<String>,
        content: Message,
        extra: Map<String, Value>,
    },
}

impl Message {
    pub fn text(text: impl Into<String>) -> Self {
        Self(vec![Segment::Text(text.into())])
    }

    /// 解析 OB11 消息参数：CQ 字符串、段数组或单个段对象。
    ///
    /// `auto_escape` 为真时，字符串整体作为纯文本，不解析 CQ 码。
    pub fn from_value(value: Value, auto_escape: bool) -> Result<Self, MessageError> {
        match value {
            Value::String(text) if auto_escape => Ok(Self::text(text)),
            Value::String(text) => Self::from_cq(&text),
            Value::Array(items) => items
                .into_iter()
                .map(Segment::from_json)
                .collect::<Result<_, _>>()
                .map(Self),
            Value::Object(_) => Segment::from_json(value).map(|segment| Self(vec![segment])),
            _ => Err(invalid(
                "message must be a string, a segment, or a segment array",
            )),
        }
    }

    pub fn from_cq(input: &str) -> Result<Self, MessageError> {
        cq::parse(input)
            .into_iter()
            .map(|(kind, data)| Segment::from_parts(&kind, data))
            .collect::<Result<_, _>>()
            .map(Self)
    }

    /// 段数组格式。
    pub fn to_value(&self) -> Value {
        Value::Array(self.0.iter().map(Segment::to_json).collect())
    }

    /// CQ 字符串格式。
    pub fn to_cq(&self) -> String {
        self.0
            .iter()
            .map(|segment| match segment {
                Segment::Text(text) => escape_text(text),
                other => {
                    let (kind, data) = other.to_parts(true);
                    cq::encode(&kind, &data)
                }
            })
            .collect()
    }
}

impl Serialize for Message {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(serializer)
    }
}

impl Segment {
    fn from_json(value: Value) -> Result<Self, MessageError> {
        let Value::Object(mut object) = value else {
            return Err(invalid("message segment must be an object"));
        };
        let kind = match object.remove("type") {
            Some(Value::String(kind)) => kind,
            _ => return Err(invalid("message segment requires a string type")),
        };
        let data = match object.remove("data") {
            None | Some(Value::Null) => Map::new(),
            Some(Value::Object(data)) => data,
            Some(_) => return Err(invalid("message segment data must be an object")),
        };
        if !cq::valid_kind(&kind) {
            return Err(invalid("invalid message segment type"));
        }
        if !data.keys().all(|key| cq::valid_key(key)) {
            return Err(invalid("invalid message segment parameter name"));
        }
        Self::from_parts(&kind, data)
    }

    fn from_parts(kind: &str, mut data: Map<String, Value>) -> Result<Self, MessageError> {
        let segment = match kind {
            "text" => match data.remove("text") {
                Some(Value::String(text)) => Self::Text(text),
                _ => return Err(invalid("text segment requires string text")),
            },
            "face" => Self::Face(
                data.get("id")
                    .and_then(scalar_text)
                    .ok_or_else(|| invalid("face segment requires id"))?,
            ),
            "image" => Self::Image(Media::from_data(data)?),
            "record" => Self::Record(Media::from_data(data)?),
            "video" => Self::Video(Media::from_data(data)?),
            "file" => Self::File(Media::from_data(data)?),
            "at" => Self::At(match data.remove("qq") {
                Some(Value::String(text)) if text == "all" => AtTarget::All,
                Some(value) => AtTarget::User(
                    parse_int(value)
                        .map_err(|_| invalid("at segment qq must be all or an integer"))?,
                ),
                None => return Err(invalid("at segment requires qq")),
            }),
            "reply" => Self::Reply(int32(data.remove("id"), "reply segment id")?),
            "node" => Self::Node(Node::from_data(data)?),
            _ => Self::Other {
                kind: kind.to_owned(),
                data,
            },
        };
        Ok(segment)
    }

    fn to_json(&self) -> Value {
        let (kind, data) = self.to_parts(false);
        let mut object = Map::new();
        object.insert("type".into(), Value::String(kind));
        object.insert("data".into(), Value::Object(data));
        Value::Object(object)
    }

    /// 转成通用的 `(类型, 参数)`。`for_cq` 为真时，嵌套消息编码为 CQ 字符串。
    fn to_parts(&self, for_cq: bool) -> (String, Map<String, Value>) {
        let mut data = Map::new();
        let kind = match self {
            Self::Text(text) => {
                data.insert("text".into(), Value::String(text.clone()));
                "text"
            }
            Self::Face(id) => {
                data.insert("id".into(), Value::String(id.clone()));
                "face"
            }
            Self::Image(media) => {
                media.write(&mut data);
                "image"
            }
            Self::Record(media) => {
                media.write(&mut data);
                "record"
            }
            Self::Video(media) => {
                media.write(&mut data);
                "video"
            }
            Self::File(media) => {
                media.write(&mut data);
                "file"
            }
            Self::At(target) => {
                let qq = match target {
                    AtTarget::User(id) => id.to_string(),
                    AtTarget::All => "all".into(),
                };
                data.insert("qq".into(), Value::String(qq));
                "at"
            }
            Self::Reply(id) => {
                data.insert("id".into(), Value::String(id.to_string()));
                "reply"
            }
            Self::Node(node) => {
                node.write(&mut data, for_cq);
                "node"
            }
            Self::Other { kind, data: other } => return (kind.clone(), other.clone()),
        };
        (kind.to_owned(), data)
    }
}

impl Media {
    fn from_data(mut data: Map<String, Value>) -> Result<Self, MessageError> {
        let file = match data.remove("file") {
            Some(Value::String(file)) if !file.is_empty() => file,
            _ => return Err(invalid("media segment requires a nonempty file")),
        };
        let url = match data.remove("url") {
            None | Some(Value::Null) => None,
            Some(Value::String(url)) => Some(url),
            Some(_) => return Err(invalid("media segment url must be a string")),
        };
        Ok(Self {
            file,
            url,
            extra: data,
        })
    }

    fn write(&self, data: &mut Map<String, Value>) {
        data.insert("file".into(), Value::String(self.file.clone()));
        if let Some(url) = &self.url {
            data.insert("url".into(), Value::String(url.clone()));
        }
        data.extend(self.extra.clone());
    }
}

impl Node {
    fn from_data(mut data: Map<String, Value>) -> Result<Self, MessageError> {
        let id = data.remove("id");
        let content = data.remove("content");
        match (id, content) {
            (Some(id), None) => Ok(Self::Reference(int32(Some(id), "node id")?)),
            (None, Some(content)) => {
                let content = Message::from_value(content, false)?;
                let user_id = author(&mut data, &["user_id", "uin"])?;
                let nickname = author(&mut data, &["nickname", "name"])?;
                Ok(Self::Custom {
                    user_id,
                    nickname,
                    content,
                    extra: data,
                })
            }
            _ => Err(invalid(
                "node segment requires exactly one of id or content",
            )),
        }
    }

    fn write(&self, data: &mut Map<String, Value>, for_cq: bool) {
        match self {
            Self::Reference(id) => {
                data.insert("id".into(), Value::String(id.to_string()));
            }
            Self::Custom {
                user_id,
                nickname,
                content,
                extra,
            } => {
                if let Some(user_id) = user_id {
                    data.insert("user_id".into(), Value::String(user_id.clone()));
                }
                if let Some(nickname) = nickname {
                    data.insert("nickname".into(), Value::String(nickname.clone()));
                }
                let content = if for_cq {
                    Value::String(content.to_cq())
                } else {
                    content.to_value()
                };
                data.insert("content".into(), content);
                data.extend(extra.clone());
            }
        }
    }
}

fn int32(value: Option<Value>, field: &str) -> Result<i32, MessageError> {
    value
        .and_then(|value| parse_int(value).ok())
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| invalid(format!("{field} must be an int32 integer")))
}

/// 取第一个存在的作者字段，并移除所有别名。
fn author(data: &mut Map<String, Value>, keys: &[&str]) -> Result<Option<String>, MessageError> {
    let mut found = None;
    for key in keys {
        if let Some(value) = data.remove(*key)
            && found.is_none()
        {
            found = Some(
                scalar_text(&value)
                    .ok_or_else(|| invalid(format!("node {key} must be a string or number")))?,
            );
        }
    }
    Ok(found)
}
