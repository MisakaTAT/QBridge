//! QQ 事件 → OB11 事件。
//!
//! 消息事件会写入本地缓存；转换失败只跳过该事件并记录 WARN。

use super::notices::{
    translate_c2c_message_notice, translate_friend, translate_group_join_request,
    translate_group_member, translate_group_message_notice, translate_group_robot,
    translate_interaction,
};
use crate::{
    bridge::Bridge,
    store::{Conversation, IdentityKind},
};
use chrono::DateTime;
use qbridge_ob11::{
    AtTarget, Event, EventKind, Media, Message, Segment,
    event::{GroupSubType, MessageEvent, MessageScene, PrivateSubType, Role, Sender},
};
use qbridge_qq::{
    Dispatch, Envelope,
    dispatch::{Attachment, Author, C2cMessage, GroupMessage},
};
use serde_json::{Map, Value};

pub async fn translate(bridge: &Bridge, envelope: Envelope) -> Option<Event> {
    let event_id = envelope.id.clone();
    let result = match envelope.dispatch {
        Dispatch::C2cMessage(message) => translate_c2c(bridge, event_id.as_deref(), message).await,
        Dispatch::GroupAtMessage(message) => {
            translate_group(
                bridge,
                event_id.as_deref(),
                "GROUP_AT_MESSAGE_CREATE",
                message,
            )
            .await
        }
        Dispatch::GroupMessage(message) => {
            translate_group(bridge, event_id.as_deref(), "GROUP_MESSAGE_CREATE", message).await
        }
        Dispatch::FriendAdd(event) => {
            translate_friend(
                bridge,
                event_id.as_deref(),
                event,
                "FRIEND_ADD",
                "friend_add",
            )
            .await
        }
        Dispatch::FriendDel(event) => {
            translate_friend(
                bridge,
                event_id.as_deref(),
                event,
                "FRIEND_DEL",
                "friend_decrease",
            )
            .await
        }
        Dispatch::GroupAddRobot(event) => {
            translate_group_robot(
                bridge,
                event_id.as_deref(),
                event,
                "GROUP_ADD_ROBOT",
                "group_increase",
                "invite",
            )
            .await
        }
        Dispatch::GroupDelRobot(event) => {
            translate_group_robot(
                bridge,
                event_id.as_deref(),
                event,
                "GROUP_DEL_ROBOT",
                "group_decrease",
                "kick_me",
            )
            .await
        }
        Dispatch::GroupMsgReceive(event) => {
            translate_group_message_notice(
                bridge,
                event_id.as_deref(),
                event,
                "GROUP_MSG_RECEIVE",
                "group_receive",
            )
            .await
        }
        Dispatch::GroupMsgReject(event) => {
            translate_group_message_notice(
                bridge,
                event_id.as_deref(),
                event,
                "GROUP_MSG_REJECT",
                "group_reject",
            )
            .await
        }
        Dispatch::GroupMemberAdd(event) => {
            translate_group_member(bridge, event_id.as_deref(), event, "GROUP_MEMBER_ADD", true)
                .await
        }
        Dispatch::GroupMemberRemove(event) => {
            translate_group_member(
                bridge,
                event_id.as_deref(),
                event,
                "GROUP_MEMBER_REMOVE",
                false,
            )
            .await
        }
        Dispatch::GroupJoinRequest(event) => {
            translate_group_join_request(bridge, event_id.as_deref(), event).await
        }
        Dispatch::C2cMsgReceive(event) => {
            translate_c2c_message_notice(
                bridge,
                event_id.as_deref(),
                event,
                "C2C_MSG_RECEIVE",
                "c2c_msg_receive",
            )
            .await
        }
        Dispatch::C2cMsgReject(event) => {
            translate_c2c_message_notice(
                bridge,
                event_id.as_deref(),
                event,
                "C2C_MSG_REJECT",
                "c2c_msg_reject",
            )
            .await
        }
        Dispatch::Interaction(event) => {
            translate_interaction(bridge, event_id.as_deref(), event).await
        }
        Dispatch::Unknown { t, d } => {
            if t == "READY"
                && let Some(user) = d.get("user")
            {
                if let Some(nickname) = user
                    .get("username")
                    .and_then(Value::as_str)
                    .filter(|nickname| !nickname.is_empty())
                {
                    bridge.state.set_nickname(nickname).await;
                }
                if let Some(id) = user
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                {
                    bridge.state.set_bot_id(id).await;
                }
            }
            return None;
        }
    };
    match result {
        Ok(Some(event)) => Some(event),
        Ok(None) => None,
        Err(reason) => {
            tracing::warn!(reason, "QQ event was skipped");
            None
        }
    }
}

async fn translate_c2c(
    bridge: &Bridge,
    event_id: Option<&str>,
    message: C2cMessage,
) -> Result<Option<Event>, &'static str> {
    let time = parse_time(Some(&message.timestamp))?;
    let openid = message
        .author
        .user_openid()
        .ok_or("C2C message author has no user_openid")?;
    let user_id = bridge
        .store
        .identity(IdentityKind::User, openid)
        .await
        .map_err(|_| "cannot map the C2C sender")?;
    let conversation = Conversation::Private(user_id);
    let content = build_message(&message.content, &message.attachments)?;
    let reference = scene_reference(&message.message_scene);
    let qq = message_extra(
        "C2C_MESSAGE_CREATE",
        event_id,
        &message.id,
        &message.author,
        &message.message_scene,
        serde_json::to_value(&message).unwrap_or(Value::Null),
    );
    save_message_event(
        bridge,
        conversation,
        time,
        user_id,
        content,
        Sender {
            user_id,
            nickname: message.author.username.clone(),
            card: None,
            role: None,
        },
        reference.as_deref(),
        qq,
        true,
        MessageScene::Private {
            sub_type: PrivateSubType::Friend,
        },
    )
    .await
}

async fn translate_group(
    bridge: &Bridge,
    event_id: Option<&str>,
    event_type: &'static str,
    message: GroupMessage,
) -> Result<Option<Event>, &'static str> {
    let time = parse_time(Some(&message.timestamp))?;
    let group_openid = message.group_openid.as_str();
    let member_openid = message
        .author
        .member_openid()
        .ok_or("group message author has no member_openid")?;
    let group_id = bridge
        .store
        .identity(IdentityKind::Group, group_openid)
        .await
        .map_err(|_| "cannot map the group")?;
    let user_id = bridge
        .store
        .identity(IdentityKind::Member, member_openid)
        .await
        .map_err(|_| "cannot map the group member")?;
    let conversation = Conversation::Group(group_id);
    let is_at_event = event_type == "GROUP_AT_MESSAGE_CREATE";
    let (content, to_me) =
        build_group_message(bridge, &message.content, &message.attachments, is_at_event).await?;
    let reference = scene_reference(&message.message_scene);
    let qq = message_extra(
        event_type,
        event_id,
        &message.id,
        &message.author,
        &message.message_scene,
        serde_json::to_value(&message).unwrap_or(Value::Null),
    );
    save_message_event(
        bridge,
        conversation,
        time,
        user_id,
        content,
        Sender {
            user_id,
            nickname: message.author.username.clone(),
            card: None,
            role: role(&message.author),
        },
        reference.as_deref(),
        qq,
        to_me,
        MessageScene::Group {
            group_id,
            sub_type: GroupSubType::Normal,
            anonymous: None,
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn save_message_event(
    bridge: &Bridge,
    conversation: Conversation,
    time: i64,
    user_id: i64,
    message: Message,
    sender: Sender,
    reference: Option<&str>,
    qq: Map<String, Value>,
    to_me: bool,
    scene: MessageScene,
) -> Result<Option<Event>, &'static str> {
    let mut event = Event {
        time,
        self_id: bridge.self_id,
        kind: EventKind::Message(MessageEvent::new(scene, 0, user_id, message, sender)),
    };
    if let EventKind::Message(message) = &mut event.kind {
        message.extra.insert("to_me".into(), Value::Bool(to_me));
        message.extra.insert("qq".into(), Value::Object(qq));
    }
    let cache_ttl = bridge
        .cache
        .inbound_ttl_secs
        .max(conversation.passive_window());
    let expires = time.saturating_add(cache_ttl);
    let content = event.to_value(qbridge_ob11::MessageFormat::Array);
    let id = match bridge
        .store
        .save_inbound(
            conversation,
            &official_id(&content)?,
            time,
            expires,
            &content,
            reference,
        )
        .await
    {
        Ok(Some(id)) => id,
        Ok(None) => {
            tracing::debug!("duplicate QQ message was ignored");
            return Ok(None);
        }
        Err(_) => return Err("cannot cache the QQ message"),
    };
    if let EventKind::Message(message) = &mut event.kind {
        message.message_id = id;
    }
    Ok(Some(event))
}

fn official_id(content: &Value) -> Result<String, &'static str> {
    content
        .get("qq")
        .and_then(|qq| qq.get("message_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .ok_or("QQ message event omitted its official message id")
}

async fn build_group_message(
    bridge: &Bridge,
    content: &str,
    attachments: &[Attachment],
    is_at_event: bool,
) -> Result<(Message, bool), &'static str> {
    let bot_id = bridge.state.bot_id().await;
    let mut segments = Vec::new();
    let mut to_me = is_at_event;
    let mut trim_leading_space = is_at_event;
    let mut rest = content;

    while !rest.is_empty() {
        let Some((index, tag)) = next_group_mention(rest) else {
            push_text_segment(&mut segments, rest, &mut trim_leading_space);
            break;
        };
        if index > 0 {
            push_text_segment(&mut segments, &rest[..index], &mut trim_leading_space);
        }
        match tag {
            GroupMention::All(len) => {
                trim_leading_space = false;
                segments.push(Segment::At(AtTarget::All));
                rest = &rest[index + len..];
            }
            GroupMention::User { id, len } => {
                if bot_id.as_deref() == Some(id) {
                    to_me = true;
                    trim_leading_space = true;
                } else if qbridge_qq::MemberOpenId::new(id).is_ok() {
                    let user_id = bridge
                        .store
                        .identity(IdentityKind::Member, id)
                        .await
                        .map_err(|_| "cannot map a mentioned group member")?;
                    trim_leading_space = false;
                    segments.push(Segment::At(AtTarget::User(user_id)));
                } else {
                    push_text_segment(
                        &mut segments,
                        &rest[index..index + len],
                        &mut trim_leading_space,
                    );
                }
                rest = &rest[index + len..];
            }
        }
    }

    append_attachments(&mut segments, attachments)?;
    Ok((Message(segments), to_me))
}

enum GroupMention<'a> {
    All(usize),
    User { id: &'a str, len: usize },
}

fn next_group_mention(text: &str) -> Option<(usize, GroupMention<'_>)> {
    let mut offset = 0;
    while offset < text.len() {
        let slice = &text[offset..];
        let rel = slice.find('<')?;
        let start = offset + rel;
        let candidate = &text[start..];
        if let Some(rest) = candidate.strip_prefix("<qqbot-at-everyone")
            && let Some(end) = rest.find('>')
            && rest[..end].trim() == "/"
        {
            return Some((
                start,
                GroupMention::All("<qqbot-at-everyone".len() + end + 1),
            ));
        }
        if let Some(rest) = candidate.strip_prefix("<@!")
            && let Some(end) = rest.find('>')
        {
            let id = &rest[..end];
            if !id.is_empty() && !id.contains('<') {
                return Some((
                    start,
                    GroupMention::User {
                        id,
                        len: "<@!".len() + end + 1,
                    },
                ));
            }
        }
        offset = start + 1;
    }
    None
}

fn push_text_segment(segments: &mut Vec<Segment>, text: &str, trim_leading_space: &mut bool) {
    let text = if *trim_leading_space {
        let trimmed = text.trim_start();
        if !trimmed.is_empty() {
            *trim_leading_space = false;
        }
        trimmed
    } else {
        text
    };
    if text.is_empty() {
        return;
    }
    if let Some(Segment::Text(existing)) = segments.last_mut() {
        existing.push_str(text);
    } else {
        segments.push(Segment::Text(text.to_owned()));
    }
}

fn build_message(content: &str, attachments: &[Attachment]) -> Result<Message, &'static str> {
    let mut segments = Vec::new();
    if !content.is_empty() {
        segments.push(Segment::Text(content.to_owned()));
    }
    append_attachments(&mut segments, attachments)?;
    Ok(Message(segments))
}

fn append_attachments(
    segments: &mut Vec<Segment>,
    attachments: &[Attachment],
) -> Result<(), &'static str> {
    for attachment in attachments {
        let url = attachment
            .url
            .as_deref()
            .filter(|url| !url.is_empty())
            .ok_or("QQ attachment omitted its URL")?;
        let url = if let Some(rest) = url.strip_prefix("//") {
            format!("https://{rest}")
        } else {
            url.to_owned()
        };
        let mime = attachment
            .content_type
            .as_deref()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let raw = serde_json::to_value(attachment).unwrap_or(Value::Null);
        let mut extra = Map::new();
        extra.insert("qq_attachment".into(), raw);
        let media = Media {
            file: url.clone(),
            url: Some(url.clone()),
            extra,
        };
        if mime.starts_with("image/") {
            segments.push(Segment::Image(media));
        } else if mime.starts_with("audio/") {
            segments.push(Segment::Record(media));
        } else if mime.starts_with("video/") {
            segments.push(Segment::Video(media));
        } else {
            let mut data = Map::new();
            data.insert("file".into(), Value::String(url.clone()));
            data.insert("url".into(), Value::String(url));
            data.insert(
                "qq_attachment".into(),
                serde_json::to_value(attachment).unwrap_or(Value::Null),
            );
            segments.push(Segment::Other {
                kind: "qq_attachment".into(),
                data,
            });
        }
    }
    Ok(())
}

fn role(author: &Author) -> Option<Role> {
    match author.member_role.as_deref() {
        Some("owner") => Some(Role::Owner),
        Some("admin") => Some(Role::Admin),
        Some("member") => Some(Role::Member),
        _ => None,
    }
}

fn message_extra(
    event_type: &str,
    event_id: Option<&str>,
    official_id: &str,
    author: &Author,
    scene: &Option<Value>,
    data: Value,
) -> Map<String, Value> {
    let mut qq = Map::new();
    qq.insert("event_type".into(), Value::String(event_type.into()));
    if let Some(event_id) = event_id {
        qq.insert("event_id".into(), Value::String(event_id.into()));
    }
    qq.insert("message_id".into(), Value::String(official_id.into()));
    qq.insert(
        "author".into(),
        serde_json::to_value(author).unwrap_or(Value::Null),
    );
    if let Some(scene) = scene {
        qq.insert("message_scene".into(), scene.clone());
    }
    qq.insert("data".into(), data);
    qq
}

fn scene_reference(scene: &Option<Value>) -> Option<String> {
    scene
        .as_ref()?
        .get("ext")?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .find_map(|entry| entry.strip_prefix("msg_idx="))
        .filter(|reference| !reference.is_empty())
        .map(str::to_owned)
}

pub(super) fn parse_time(timestamp: Option<&str>) -> Result<i64, &'static str> {
    let timestamp = timestamp.ok_or("QQ event omitted its timestamp")?;
    DateTime::parse_from_rfc3339(timestamp)
        .map(|time| time.timestamp())
        .map_err(|_| "QQ event timestamp is invalid")
}

pub(super) fn parse_time_value(timestamp: Option<&Value>) -> Result<i64, &'static str> {
    match timestamp {
        Some(Value::String(text)) => parse_time(Some(text)),
        Some(Value::Number(number)) => number.as_i64().ok_or("QQ event timestamp is invalid"),
        Some(_) => Err("QQ event timestamp is invalid"),
        None => Err("QQ event omitted its timestamp"),
    }
}
