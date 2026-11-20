//! 发送计划：把 OB11 消息段分类并校验组合规则。纯函数，不做 IO。

use crate::error::{ActionError, UnsupportedKind};
use base64::{Engine, engine::general_purpose::STANDARD};
use qbridge_ob11::{AtTarget, Media, Message, Node, Segment, Target};
use qbridge_qq::{Keyboard, KeyboardContent, Markdown, MarkdownParam};
use serde_json::{Map, Value};
use std::path::PathBuf;
use url::Url;

pub const MAX_DIRECT_BYTES: usize = 5 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub body: Body,
    pub reply: Option<i32>,
    pub active: bool,
    pub wakeup: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Body {
    Text(Vec<TextPart>),
    Media {
        media: MediaPlan,
        text: Vec<TextPart>,
    },
    Markdown {
        markdown: Markdown,
        keyboard: Option<Keyboard>,
    },
    Forward {
        nodes: Vec<Node>,
        options: ForwardOptions,
    },
    Card(CardPlan),
    InputNotify {
        input_type: u8,
        input_second: u16,
    },
    Stream {
        state: StreamState,
        text: Vec<TextPart>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamState {
    Start,
    Mid,
    Finish,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardPlan {
    pub title: String,
    pub description: String,
    pub pic_url: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextPart {
    Text(String),
    At(i64),
    Avatar(i64),
    GroupInfo { group_id: i64, field: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaPlan {
    pub kind: MediaKind,
    pub source: MediaSource,
    pub file_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
    Record,
    File,
}

impl MediaKind {
    pub fn file_type(self) -> u8 {
        match self {
            Self::Image => 1,
            Self::Video => 2,
            Self::Record => 3,
            Self::File => 4,
        }
    }

    fn default_name(self) -> &'static str {
        match self {
            Self::Image => "image.png",
            Self::Video => "video.mp4",
            Self::Record => "record.silk",
            Self::File => "file.bin",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaSource {
    Url(String),
    Base64(String),
    File(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ForwardOptions {
    pub source: Option<String>,
    pub prompt: Option<String>,
    pub news: Vec<String>,
    pub summary: Option<String>,
}

/// 校验并分类一条消息。`target` 只用于判断 `at` 是否允许。
pub fn plan(
    target: Target,
    message: &Message,
    extra: &Map<String, Value>,
) -> Result<Plan, ActionError> {
    let mut active = flag(extra, "active")?.unwrap_or(false);
    let mut text = Vec::new();
    let mut media: Option<MediaPlan> = None;
    let mut markdown: Option<Markdown> = None;
    let mut keyboard: Option<Keyboard> = None;
    let mut reply: Option<i32> = None;
    let mut nodes = Vec::new();
    let mut card: Option<CardPlan> = None;
    let mut input_notify: Option<(u8, u16)> = None;
    let mut stream: Option<StreamState> = None;
    let mut wakeup = false;

    for segment in &message.0 {
        match segment {
            Segment::Text(value) => {
                if !value.is_empty() {
                    text.push(TextPart::Text(value.clone()));
                }
            }
            Segment::At(AtTarget::All) => {
                if !matches!(target, Target::Group(_)) {
                    return Err(ActionError::Unsupported(UnsupportedKind::Segment));
                }
                text.push(TextPart::Text("<qqbot-at-everyone />".into()));
            }
            Segment::At(AtTarget::User(id)) => {
                if !matches!(target, Target::Group(_)) {
                    return Err(ActionError::Unsupported(UnsupportedKind::Segment));
                }
                text.push(TextPart::At(*id));
            }
            Segment::Image(value) => {
                set_media(&mut media, direct_media(MediaKind::Image, value)?)?;
            }
            Segment::Video(value) => {
                set_media(&mut media, direct_media(MediaKind::Video, value)?)?;
            }
            Segment::Record(value) => {
                set_media(&mut media, direct_media(MediaKind::Record, value)?)?;
            }
            Segment::File(value) => {
                set_media(&mut media, direct_media(MediaKind::File, value)?)?;
            }
            Segment::Reply(id) => {
                if reply.is_some() {
                    return Err(ActionError::BadParam(
                        "only one reply segment is allowed".into(),
                    ));
                }
                reply = Some(*id);
            }
            Segment::Face(_) => {
                return Err(ActionError::Unsupported(UnsupportedKind::Segment));
            }
            Segment::Node(node) => nodes.push(node.clone()),
            Segment::Other { kind, data } if kind == "markdown" => {
                if markdown.is_some() {
                    return Err(ActionError::Unsupported(
                        UnsupportedKind::MessageCombination,
                    ));
                }
                markdown = Some(parse_markdown(data)?);
            }
            Segment::Other { kind, data } if kind == "card" => {
                if card.is_some() {
                    return Err(ActionError::Unsupported(
                        UnsupportedKind::MessageCombination,
                    ));
                }
                card = Some(parse_card(data)?);
            }
            Segment::Other { kind, data } if kind == "input_notify" => {
                if input_notify.is_some() {
                    return Err(ActionError::Unsupported(
                        UnsupportedKind::MessageCombination,
                    ));
                }
                input_notify = Some(parse_input_notify(data)?);
            }
            Segment::Other { kind, data } if kind == "stream" => {
                if stream.is_some() {
                    return Err(ActionError::Unsupported(
                        UnsupportedKind::MessageCombination,
                    ));
                }
                let state = match data.get("type").and_then(Value::as_str) {
                    Some("start") => StreamState::Start,
                    Some("mid") => StreamState::Mid,
                    Some("finish") => StreamState::Finish,
                    _ => {
                        return Err(ActionError::BadParam(
                            "stream type must be start, mid or finish".into(),
                        ));
                    }
                };
                if let Some(user) = data.get("qq") {
                    let user = match user {
                        Value::Number(number) => number.as_i64(),
                        Value::String(text) => text.parse().ok(),
                        _ => None,
                    }
                    .ok_or_else(|| ActionError::BadParam("stream qq must be an integer".into()))?;
                    if !matches!(target, Target::Private(id) if id == user) {
                        return Err(ActionError::BadParam(
                            "stream qq must match the private target".into(),
                        ));
                    }
                }
                stream = Some(state);
            }
            Segment::Other { kind, data } if kind == "avatar" => {
                text.push(TextPart::Avatar(integer_field(data, "user_id")?));
            }
            Segment::Other { kind, data } if kind == "group_info" => {
                if !matches!(target, Target::Group(_)) {
                    return Err(ActionError::Unsupported(UnsupportedKind::Segment));
                }
                text.push(TextPart::GroupInfo {
                    group_id: integer_field(data, "group_id")?,
                    field: data
                        .get("field")
                        .and_then(Value::as_str)
                        .unwrap_or("group_name")
                        .to_owned(),
                });
            }
            Segment::Other { kind, data } if kind == "wakeup" => {
                if stream.is_some()
                    && (matches!(target, Target::Group(_))
                        || media.is_some()
                        || markdown.is_some()
                        || card.is_some()
                        || input_notify.is_some()
                        || !nodes.is_empty()
                        || reply.is_some())
                {
                    return Err(ActionError::Unsupported(
                        UnsupportedKind::MessageCombination,
                    ));
                }
                if wakeup {
                    return Err(ActionError::Unsupported(
                        UnsupportedKind::MessageCombination,
                    ));
                }
                if let Some(user) = data.get("userid") {
                    let user = match user {
                        Value::Number(number) => number.as_i64(),
                        Value::String(text) => text.parse().ok(),
                        _ => None,
                    }
                    .ok_or_else(|| {
                        ActionError::BadParam("wakeup userid must be an integer".into())
                    })?;
                    if !matches!(target, Target::Private(id) if id == user) {
                        return Err(ActionError::BadParam(
                            "wakeup userid must match the private target".into(),
                        ));
                    }
                }
                wakeup = true;
            }
            Segment::Other { kind, data } if kind == "keyboard" => {
                if keyboard.is_some() {
                    return Err(ActionError::Unsupported(
                        UnsupportedKind::MessageCombination,
                    ));
                }
                keyboard = Some(parse_keyboard(data)?);
            }
            Segment::Other { .. } => {
                return Err(ActionError::Unsupported(UnsupportedKind::Segment));
            }
        }
    }

    if active && reply.is_some() {
        return Err(ActionError::Unsupported(
            UnsupportedKind::MessageCombination,
        ));
    }
    if card.is_some()
        && (!matches!(target, Target::Group(_))
            || !text.is_empty()
            || media.is_some()
            || markdown.is_some()
            || input_notify.is_some()
            || !nodes.is_empty())
    {
        return Err(ActionError::Unsupported(
            UnsupportedKind::MessageCombination,
        ));
    }
    if input_notify.is_some()
        && (matches!(target, Target::Group(_))
            || !text.is_empty()
            || media.is_some()
            || markdown.is_some()
            || card.is_some()
            || !nodes.is_empty())
    {
        return Err(ActionError::Unsupported(
            UnsupportedKind::MessageCombination,
        ));
    }
    if stream.is_some()
        && (matches!(target, Target::Group(_))
            || media.is_some()
            || markdown.is_some()
            || card.is_some()
            || input_notify.is_some()
            || !nodes.is_empty()
            || reply.is_some())
    {
        return Err(ActionError::Unsupported(
            UnsupportedKind::MessageCombination,
        ));
    }
    if wakeup {
        if !matches!(target, Target::Private(_)) || reply.is_some() {
            return Err(ActionError::Unsupported(
                UnsupportedKind::MessageCombination,
            ));
        }
        active = true;
    }
    if keyboard.is_some() && markdown.is_none() {
        return Err(ActionError::Unsupported(
            UnsupportedKind::MessageCombination,
        ));
    }
    if markdown.is_some() && (!text.is_empty() || media.is_some()) {
        return Err(ActionError::Unsupported(
            UnsupportedKind::MessageCombination,
        ));
    }
    if !nodes.is_empty() {
        if !text.is_empty() || media.is_some() || markdown.is_some() {
            return Err(ActionError::Unsupported(
                UnsupportedKind::MessageCombination,
            ));
        }
        return Ok(Plan {
            body: Body::Forward {
                nodes,
                options: forward_options(extra)?,
            },
            reply,
            active,
            wakeup,
        });
    }
    if let Some(state) = stream {
        if text.is_empty() {
            return Err(ActionError::BadParam("stream message requires text".into()));
        }
        return Ok(Plan {
            body: Body::Stream { state, text },
            reply,
            active,
            wakeup,
        });
    }
    if let Some(card) = card {
        return Ok(Plan {
            body: Body::Card(card),
            reply,
            active,
            wakeup,
        });
    }
    if let Some((input_type, input_second)) = input_notify {
        return Ok(Plan {
            body: Body::InputNotify {
                input_type,
                input_second,
            },
            reply,
            active,
            wakeup,
        });
    }
    if let Some(markdown) = markdown {
        return Ok(Plan {
            body: Body::Markdown { markdown, keyboard },
            reply,
            active,
            wakeup,
        });
    }
    if let Some(media) = media {
        return Ok(Plan {
            body: Body::Media { media, text },
            reply,
            active,
            wakeup,
        });
    }
    if text.is_empty() {
        return Err(ActionError::BadParam(
            "message must contain text, media, markdown or a forward node".into(),
        ));
    }
    Ok(Plan {
        body: Body::Text(text),
        reply,
        active,
        wakeup,
    })
}

fn set_media(slot: &mut Option<MediaPlan>, media: MediaPlan) -> Result<(), ActionError> {
    if slot.is_some() {
        return Err(ActionError::Unsupported(
            UnsupportedKind::MessageCombination,
        ));
    }
    *slot = Some(media);
    Ok(())
}

fn direct_media(kind: MediaKind, media: &Media) -> Result<MediaPlan, ActionError> {
    for key in ["cache", "proxy", "timeout"] {
        let _ = media.extra.get(key);
    }
    if let Some(value) = media.extra.get("type")
        && !value.is_null()
        && value.as_str().is_none_or(|value| !value.is_empty())
    {
        return Err(ActionError::Unsupported(UnsupportedKind::SegmentOption));
    }
    if flag(&media.extra, "magic")?.unwrap_or(false) {
        return Err(ActionError::Unsupported(UnsupportedKind::SegmentOption));
    }
    let (source, mut file_name) = media_source(kind, &media.file)?;
    if let Some(name) = media.extra.get("name").and_then(Value::as_str)
        && !name.trim().is_empty()
    {
        file_name = name.to_owned();
    }
    Ok(MediaPlan {
        kind,
        source,
        file_name,
    })
}

fn media_source(kind: MediaKind, file: &str) -> Result<(MediaSource, String), ActionError> {
    if let Some(encoded) = file.strip_prefix("base64://") {
        let bytes = STANDARD
            .decode(encoded)
            .map_err(|_| ActionError::BadParam("invalid media base64".into()))?;
        if bytes.is_empty() || bytes.len() > MAX_DIRECT_BYTES {
            return Err(ActionError::BadParam(
                "direct media must contain 1 byte to 5 MiB".into(),
            ));
        }
        return Ok((
            MediaSource::Base64(encoded.to_owned()),
            kind.default_name().into(),
        ));
    }
    let url = Url::parse(file).map_err(|_| {
        ActionError::BadParam("media file must be an HTTP(S) URL, file URI or base64 URI".into())
    })?;
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(ActionError::BadParam(
            "media URL must not contain credentials or a fragment".into(),
        ));
    }
    match url.scheme() {
        "http" | "https" if url.host_str().is_some() => Ok((
            MediaSource::Url(url.to_string()),
            kind.default_name().into(),
        )),
        "file" if url.host_str().is_none() && url.query().is_none() => {
            let path = url
                .to_file_path()
                .map_err(|_| ActionError::BadParam("invalid local file URI".into()))?;
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.is_empty())
                .unwrap_or(kind.default_name())
                .to_owned();
            Ok((MediaSource::File(path), name))
        }
        _ => Err(ActionError::BadParam(
            "media file must be an HTTP(S) URL, file URI or base64 URI".into(),
        )),
    }
}

fn integer_field(data: &Map<String, Value>, key: &str) -> Result<i64, ActionError> {
    match data.get(key) {
        Some(Value::Number(number)) => number
            .as_i64()
            .ok_or_else(|| ActionError::BadParam(format!("{key} must be an integer"))),
        Some(Value::String(text)) => text
            .parse()
            .map_err(|_| ActionError::BadParam(format!("{key} must be an integer"))),
        _ => Err(ActionError::BadParam(format!("{key} is required"))),
    }
}

fn parse_card(data: &Map<String, Value>) -> Result<CardPlan, ActionError> {
    let string = |key: &str| {
        data.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let pic_url = string("pic")
        .or_else(|| string("pic_url"))
        .ok_or_else(|| ActionError::BadParam("card requires pic".into()))?;
    let url = string("url").ok_or_else(|| ActionError::BadParam("card requires url".into()))?;
    Ok(CardPlan {
        title: string("title").unwrap_or_default(),
        description: string("desc")
            .or_else(|| string("description"))
            .unwrap_or_default(),
        pic_url,
        url,
    })
}

fn parse_input_notify(data: &Map<String, Value>) -> Result<(u8, u16), ActionError> {
    let parse = |key: &str, default: u64| -> Result<u64, ActionError> {
        match data.get(key) {
            None => Ok(default),
            Some(Value::Number(number)) => number.as_u64().ok_or_else(|| {
                ActionError::BadParam(format!("input_notify {key} must be a positive integer"))
            }),
            Some(Value::String(text)) => text.parse().map_err(|_| {
                ActionError::BadParam(format!("input_notify {key} must be a positive integer"))
            }),
            Some(_) => Err(ActionError::BadParam(format!(
                "input_notify {key} must be a positive integer"
            ))),
        }
    };
    let input_type = parse("type", 1)?;
    let input_second = parse("second", 60)?;
    Ok((
        u8::try_from(input_type)
            .map_err(|_| ActionError::BadParam("input_notify type is out of range".into()))?,
        u16::try_from(input_second)
            .map_err(|_| ActionError::BadParam("input_notify second is out of range".into()))?,
    ))
}

fn parse_markdown(data: &Map<String, Value>) -> Result<Markdown, ActionError> {
    if let Some(template) = data.get("custom_template_id") {
        if data.contains_key("content") {
            return Err(ActionError::BadParam(
                "markdown cannot combine content and custom_template_id".into(),
            ));
        }
        let template = template
            .as_str()
            .filter(|template| !template.trim().is_empty())
            .ok_or_else(|| {
                ActionError::BadParam(
                    "markdown.custom_template_id must be a nonempty string".into(),
                )
            })?;
        let params = data
            .get("params")
            .cloned()
            .ok_or_else(|| ActionError::BadParam("markdown template requires params".into()))?;
        let params: Vec<MarkdownParam> = serde_json::from_value(params)
            .map_err(|_| ActionError::BadParam("markdown.params must be an array".into()))?;
        if params.is_empty() {
            return Err(ActionError::BadParam(
                "markdown template requires at least one param".into(),
            ));
        }
        return Ok(Markdown::Template {
            custom_template_id: template.to_owned(),
            params,
        });
    }
    let content = data.get("content").ok_or_else(|| {
        ActionError::BadParam("markdown requires content or custom_template_id".into())
    })?;
    let content = match content {
        Value::String(content) => content.clone(),
        Value::Object(object) => object
            .get("content")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                ActionError::BadParam("markdown.content object requires a content string".into())
            })?
            .to_owned(),
        _ => {
            return Err(ActionError::BadParam(
                "markdown.content must be a string".into(),
            ));
        }
    };
    if content.trim().is_empty() {
        return Err(ActionError::BadParam("markdown content is empty".into()));
    }
    Ok(Markdown::Content { content })
}

fn parse_keyboard(data: &Map<String, Value>) -> Result<Keyboard, ActionError> {
    let keyboard: Keyboard = serde_json::from_value(Value::Object(data.clone()))
        .map_err(|_| ActionError::BadParam("invalid keyboard segment".into()))?;
    match (&keyboard.id, &keyboard.content) {
        (Some(id), None) => {
            if id.trim().is_empty() {
                return Err(ActionError::BadParam(
                    "keyboard.id must be a nonempty string".into(),
                ));
            }
            Ok(Keyboard {
                id: Some(id.clone()),
                content: None,
            })
        }
        (None, Some(content)) => {
            validate_keyboard(content)?;
            Ok(keyboard)
        }
        _ => Err(ActionError::BadParam(
            "keyboard requires exactly one of id or content".into(),
        )),
    }
}

fn validate_keyboard(content: &KeyboardContent) -> Result<(), ActionError> {
    if content.rows.is_empty() || content.rows.len() > 5 {
        return Err(ActionError::BadParam(
            "keyboard must contain 1 to 5 rows".into(),
        ));
    }
    for row in &content.rows {
        if row.buttons.is_empty() || row.buttons.len() > 5 {
            return Err(ActionError::BadParam(
                "keyboard rows must contain 1 to 5 buttons".into(),
            ));
        }
    }
    Ok(())
}

fn forward_options(extra: &Map<String, Value>) -> Result<ForwardOptions, ActionError> {
    let text = |key: &str| -> Result<Option<String>, ActionError> {
        match extra.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(value)) => Ok(Some(value.clone())),
            Some(_) => Err(ActionError::BadParam(format!(
                "forward {key} must be a string"
            ))),
        }
    };
    let news = match extra.get("news") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(entries)) => {
            let mut news = Vec::new();
            for entry in entries {
                let text = entry.get("text").and_then(Value::as_str).ok_or_else(|| {
                    ActionError::BadParam("forward news entries require text".into())
                })?;
                news.push(text.to_owned());
            }
            news
        }
        Some(_) => {
            return Err(ActionError::BadParam(
                "forward news must be an array".into(),
            ));
        }
    };
    Ok(ForwardOptions {
        source: text("source")?,
        prompt: text("prompt")?,
        news,
        summary: text("summary")?,
    })
}

/// 解析可选布尔扩展参数。数字 0/1 与字符串形式也接受。
pub fn flag(extra: &Map<String, Value>, key: &str) -> Result<Option<bool>, ActionError> {
    match extra.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(Value::Number(number)) => match number.as_u64() {
            Some(0) => Ok(Some(false)),
            Some(1) => Ok(Some(true)),
            _ => Err(ActionError::BadParam(format!("{key} must be a boolean"))),
        },
        Some(Value::String(value)) if value == "true" || value == "1" => Ok(Some(true)),
        Some(Value::String(value)) if value == "false" || value == "0" => Ok(Some(false)),
        Some(_) => Err(ActionError::BadParam(format!("{key} must be a boolean"))),
    }
}
