//! 发送流程：plan → resolve → deliver → record。

use crate::{
    bridge::{
        Bridge, forward,
        plan::{self, Body, MediaKind, MediaPlan, MediaSource, Plan, StreamState, TextPart},
    },
    error::{ActionError, NotFoundKind},
    store::Conversation,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use qbridge_ob11::{
    Event, EventKind, Message, MessageFormat, Segment, Target,
    action::SendMsg,
    event::{GroupSubType, MessageEvent, MessageScene, PrivateSubType, Sender},
};
use qbridge_qq::{GroupOpenId, Keyboard, Markdown, Peer, StreamRequest, UserOpenId};
use serde_json::{Map, Value, json};

const MAX_DIRECT_BYTES: usize = plan::MAX_DIRECT_BYTES;
const MAX_CHUNKED_BYTES: usize = 200 * 1024 * 1024;

pub async fn send(bridge: &Bridge, request: SendMsg, now: i64) -> Result<Value, ActionError> {
    let message = prepare_actions(bridge, request.target, request.message, now).await?;
    if message.0.is_empty() {
        return Ok(json!({"message_id": 0}));
    }
    let plan = plan::plan(request.target, &message, &request.extra)?;
    let resolved = resolve(bridge, request.target, plan, now).await?;
    let message_id = deliver(bridge, resolved, message, now).await?;
    Ok(json!({ "message_id": message_id }))
}

async fn prepare_actions(
    bridge: &Bridge,
    target: Target,
    message: Message,
    now: i64,
) -> Result<Message, ActionError> {
    let mut kept = Vec::new();
    for segment in message.0 {
        match &segment {
            Segment::Other { kind, data } if kind == "set_group" => {
                if let Err(error) = run_group_code(bridge, target, data, now).await {
                    tracing::warn!(reason = ?error, "set_group CQ action failed");
                }
            }
            Segment::Other { kind, .. } if kind == "member" => {}
            _ => kept.push(segment),
        }
    }
    Ok(Message(kept))
}

async fn run_group_code(
    bridge: &Bridge,
    target: Target,
    data: &Map<String, Value>,
    now: i64,
) -> Result<(), ActionError> {
    let action = data
        .get("action")
        .and_then(Value::as_str)
        .ok_or_else(|| ActionError::BadParam("set_group action is required".into()))?;
    let group_id = data
        .get("group_id")
        .and_then(Value::as_i64)
        .or_else(|| {
            data.get("group_id")
                .and_then(Value::as_str)
                .and_then(|id| id.parse().ok())
        })
        .or(match target {
            Target::Group(id) => Some(id),
            _ => None,
        })
        .ok_or_else(|| ActionError::BadParam("set_group requires group_id".into()))?;
    let mut params = Map::new();
    params.insert("group_id".into(), group_id.into());
    match action {
        "ban" => {
            params.insert("user_id".into(), integer_param(data, "user_id")?.into());
            params.insert(
                "duration".into(),
                data.get("duration").cloned().unwrap_or(0.into()),
            );
            let duration = data
                .get("duration")
                .and_then(|value| {
                    value
                        .as_u64()
                        .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
                })
                .unwrap_or(0);
            crate::bridge::group::ban(
                bridge,
                qbridge_ob11::action::SetGroupBan {
                    group_id,
                    user_id: integer_param(data, "user_id")?,
                    duration,
                },
                now,
            )
            .await
            .map(|_| ())
        }
        "kick" => {
            add_user_ids(&mut params, data)?;
            params.insert(
                "add_blacklist".into(),
                bool_param(data, "add_blacklist")?.into(),
            );
            crate::bridge::group::kick(
                bridge,
                qbridge_ob11::action::SetGroupKick {
                    group_id,
                    user_id: integer_param(data, "user_id")?,
                    reject_add_request: false,
                    add_blacklist: bool_param(data, "add_blacklist")?,
                },
            )
            .await
            .map(|_| ())
        }
        "blacklist_add" | "blacklist_del" => {
            params.insert(
                "op".into(),
                (if action == "blacklist_add" {
                    "add"
                } else {
                    "del"
                })
                .into(),
            );
            add_user_ids(&mut params, data)?;
            let group = crate::bridge::group::group_openid(bridge, group_id).await?;
            let ids = params
                .get("user_ids")
                .and_then(Value::as_array)
                .ok_or_else(|| ActionError::BadParam("user_ids is required".into()))?;
            let mut members = Vec::with_capacity(ids.len());
            for id in ids {
                let id = id
                    .as_i64()
                    .ok_or_else(|| ActionError::BadParam("user_ids must be integers".into()))?;
                members.push(crate::bridge::group::member_openid(bridge, id).await?);
            }
            Ok(bridge
                .qq
                .update_group_blacklist(
                    &group,
                    if action == "blacklist_add" {
                        "add"
                    } else {
                        "del"
                    },
                    &members,
                )
                .await
                .map(|_| ())?)
        }
        "add_request" => {
            let user_id = integer_param(data, "user_id")?;
            let request_id = data
                .get("flag")
                .and_then(Value::as_str)
                .filter(|flag| !flag.is_empty())
                .ok_or_else(|| {
                    ActionError::BadParam("set_group add_request requires flag".into())
                })?;
            params.insert("user_id".into(), user_id.into());
            params.insert("sub_type".into(), Value::String("add".into()));
            params.insert(
                "flag".into(),
                Value::String(crate::bridge::group::join_flag(
                    group_id, user_id, request_id,
                )),
            );
            params.insert("approve".into(), bool_param(data, "approve")?.into());
            params.insert(
                "reason".into(),
                data.get("reason").cloned().unwrap_or_else(|| "".into()),
            );
            crate::bridge::group::add_request(
                bridge,
                qbridge_ob11::action::SetGroupAddRequest {
                    flag: params
                        .get("flag")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    sub_type: "add".into(),
                    approve: bool_param(data, "approve")?,
                    reason: data
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                },
            )
            .await
            .map(|_| ())
        }
        "strategy_execute" | "strategy_delete" => {
            params.insert(
                "strategy_id".into(),
                data.get("strategy_id").cloned().unwrap_or(Value::Null),
            );
            let strategy_id = data
                .get("strategy_id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .ok_or_else(|| ActionError::BadParam("strategy_id is required".into()))?;
            if action == "strategy_execute" {
                Ok(bridge.qq.join_strategy_execute(strategy_id).await?)
            } else {
                Ok(bridge.qq.join_strategy_delete(strategy_id).await?)
            }
        }
        "whole_ban" => Err(ActionError::Unsupported(
            crate::error::UnsupportedKind::Action,
        )),
        _ => Err(ActionError::BadParam(format!(
            "unknown set_group action {action}"
        ))),
    }
}

fn integer_param(data: &Map<String, Value>, key: &str) -> Result<i64, ActionError> {
    data.get(key)
        .and_then(Value::as_i64)
        .or_else(|| {
            data.get(key)
                .and_then(Value::as_str)
                .and_then(|value| value.parse().ok())
        })
        .ok_or_else(|| ActionError::BadParam(format!("set_group requires integer {key}")))
}

fn bool_param(data: &Map<String, Value>, key: &str) -> Result<bool, ActionError> {
    match data.get(key) {
        None => Ok(false),
        Some(Value::Bool(value)) => Ok(*value),
        Some(Value::String(value)) if value == "true" => Ok(true),
        Some(Value::String(value)) if value == "false" => Ok(false),
        _ => Err(ActionError::BadParam(format!(
            "set_group {key} must be boolean"
        ))),
    }
}

fn add_user_ids(
    params: &mut Map<String, Value>,
    data: &Map<String, Value>,
) -> Result<(), ActionError> {
    let mut ids = Vec::new();
    if data.contains_key("user_id") {
        ids.push(integer_param(data, "user_id")?);
    }
    if let Some(text) = data.get("user_ids").and_then(Value::as_str) {
        for part in text.split(',') {
            if !part.is_empty() {
                ids.push(
                    part.parse()
                        .map_err(|_| ActionError::BadParam("user_ids must be integers".into()))?,
                );
            }
        }
    }
    if ids.is_empty() || ids.len() > 20 {
        return Err(ActionError::BadParam(
            "set_group requires 1 to 20 users".into(),
        ));
    }
    params.insert("user_ids".into(), ids.into());
    Ok(())
}

struct Resolved {
    conversation: Conversation,
    openid: String,
    body: ResolvedBody,
    reference: Option<String>,
    /// 指向入站消息时复用该条的被动上下文；否则使用最新入站消息。
    reply_inbound: Option<i32>,
    active: bool,
    wakeup: bool,
}

enum ResolvedBody {
    Text(String),
    Media {
        media: MediaPlan,
        text: String,
    },
    Markdown {
        markdown: Markdown,
        keyboard: Option<Keyboard>,
    },
    Card(qbridge_qq::Card),
    InputNotify {
        input_type: u8,
        input_second: u16,
    },
    Stream {
        state: StreamState,
        text: String,
    },
}

async fn resolve(
    bridge: &Bridge,
    target: Target,
    plan: Plan,
    now: i64,
) -> Result<Resolved, ActionError> {
    let conversation = match target {
        Target::Private(id) => Conversation::Private(id),
        Target::Group(id) => Conversation::Group(id),
    };
    let kind = match conversation {
        Conversation::Private(_) => crate::store::IdentityKind::User,
        Conversation::Group(_) => crate::store::IdentityKind::Group,
    };
    let openid = bridge
        .store
        .openid(kind, conversation.target())
        .await?
        .ok_or(ActionError::NotFound(NotFoundKind::UnknownTarget))?;

    let mut reference = None;
    let mut reply_inbound = None;
    if let Some(reply) = plan.reply {
        let quote = bridge.store.quote(conversation, reply, now).await?;
        if quote.inbound {
            reply_inbound = Some(reply);
            if quote.reference.is_none() {
                tracing::warn!(
                    message_id = reply,
                    "QQ inbound message has no msg_idx; replying without a native quote"
                );
            }
        }
        reference = quote.reference;
    }

    let body = match plan.body {
        Body::Text(parts) => ResolvedBody::Text(resolve_text(bridge, &parts).await?),
        Body::Media { media, text } => ResolvedBody::Media {
            media,
            text: resolve_text(bridge, &text).await?,
        },
        Body::Markdown { markdown, keyboard } => ResolvedBody::Markdown {
            markdown,
            keyboard: match keyboard {
                Some(keyboard) => Some(resolve_keyboard(bridge, keyboard).await?),
                None => None,
            },
        },
        Body::Card(card) => ResolvedBody::Card(qbridge_qq::Card {
            kind: "tuwen".into(),
            content: qbridge_qq::CardContent {
                title: card.title,
                description: card.description,
                pic_url: card.pic_url,
                url: card.url,
            },
        }),
        Body::InputNotify {
            input_type,
            input_second,
        } => ResolvedBody::InputNotify {
            input_type,
            input_second,
        },
        Body::Stream { state, text } => ResolvedBody::Stream {
            state,
            text: resolve_text(bridge, &text).await?,
        },
        Body::Forward { nodes, options } => {
            let nodes = forward::prepare_media(bridge, nodes).await?;
            let content = forward::render(&bridge.store, &nodes, &options, now).await?;
            ResolvedBody::Markdown {
                markdown: Markdown::Content { content },
                keyboard: None,
            }
        }
    };

    Ok(Resolved {
        conversation,
        openid,
        body,
        reference,
        reply_inbound,
        active: plan.active,
        wakeup: plan.wakeup,
    })
}

async fn resolve_keyboard(
    bridge: &Bridge,
    mut keyboard: Keyboard,
) -> Result<Keyboard, ActionError> {
    let Some(content) = keyboard.content.as_mut() else {
        return Ok(keyboard);
    };
    for row in &mut content.rows {
        for button in &mut row.buttons {
            let Some(Value::Object(permission)) = button.action.permission.as_mut() else {
                continue;
            };
            let Some(ids) = permission.get_mut("specify_user_ids") else {
                continue;
            };
            if ids.is_null() {
                continue;
            }
            let Value::Array(items) = ids else {
                return Err(ActionError::BadParam(
                    "keyboard permission specify_user_ids must be an array".into(),
                ));
            };
            for item in items.iter_mut() {
                match item {
                    Value::Number(number) => {
                        let id = number.as_i64().filter(|id| *id > 0).ok_or_else(|| {
                            ActionError::BadParam(
                                "keyboard permission specify_user_ids must contain positive integers or OpenIDs"
                                    .into(),
                            )
                        })?;
                        let openid = bridge
                            .store
                            .openid(crate::store::IdentityKind::Member, id)
                            .await?
                            .ok_or(ActionError::NotFound(NotFoundKind::UnknownTarget))?;
                        *item = Value::String(openid);
                    }
                    Value::String(text) => {
                        if text.is_empty() {
                            return Err(ActionError::BadParam(
                                "keyboard permission specify_user_ids cannot contain empty strings"
                                    .into(),
                            ));
                        }
                        if text.bytes().all(|byte| byte.is_ascii_digit()) {
                            let id = text.parse::<i64>().ok().filter(|id| *id > 0).ok_or_else(
                                || {
                                    ActionError::BadParam(
                                        "keyboard permission specify_user_ids must contain positive integers or OpenIDs"
                                            .into(),
                                    )
                                },
                            )?;
                            let openid = bridge
                                .store
                                .openid(crate::store::IdentityKind::Member, id)
                                .await?
                                .ok_or(ActionError::NotFound(NotFoundKind::UnknownTarget))?;
                            *text = openid;
                        }
                    }
                    _ => {
                        return Err(ActionError::BadParam(
                            "keyboard permission specify_user_ids must contain integers or OpenIDs"
                                .into(),
                        ));
                    }
                }
            }
        }
    }
    Ok(keyboard)
}

async fn resolve_text(bridge: &Bridge, parts: &[TextPart]) -> Result<String, ActionError> {
    let mut text = String::new();
    for part in parts {
        match part {
            TextPart::Text(value) => text.push_str(value),
            TextPart::Avatar(id) => {
                let openid = bridge
                    .store
                    .openid(crate::store::IdentityKind::User, *id)
                    .await?
                    .ok_or(ActionError::NotFound(NotFoundKind::UnknownTarget))?;
                text.push_str(&bridge.qq.avatar_url(&openid));
            }
            TextPart::GroupInfo { group_id, field } => {
                let openid = bridge
                    .store
                    .openid(crate::store::IdentityKind::Group, *group_id)
                    .await?
                    .ok_or(ActionError::NotFound(NotFoundKind::UnknownTarget))?;
                let group = qbridge_qq::GroupOpenId::new(openid)
                    .map_err(|_| ActionError::NotFound(NotFoundKind::UnknownTarget))?;
                let info = bridge.qq.group_info(&group).await?;
                text.push_str(&match field.as_str() {
                    "group_name" => info.group_name,
                    "group_brief" => info.group_finger_memo.unwrap_or_default(),
                    "member_count" => info.group_member_num.unwrap_or(0).to_string(),
                    "json" => serde_json::to_string(&info).unwrap_or_default(),
                    _ => {
                        return Err(ActionError::BadParam(
                            "group_info field must be group_name, group_brief, member_count or json"
                                .into(),
                        ));
                    }
                });
            }
            TextPart::At(id) => {
                let openid = bridge
                    .store
                    .openid(crate::store::IdentityKind::Member, *id)
                    .await?
                    .ok_or(ActionError::NotFound(NotFoundKind::UnknownTarget))?;
                text.push_str("<@!");
                text.push_str(&openid);
                text.push('>');
            }
        }
    }
    Ok(text)
}

async fn deliver(
    bridge: &Bridge,
    resolved: Resolved,
    cached_message: Message,
    now: i64,
) -> Result<i32, ActionError> {
    let file_info = match &resolved.body {
        ResolvedBody::Media { media, .. } => {
            Some(upload(bridge, resolved.conversation, &resolved.openid, media).await?)
        }
        _ => None,
    };
    if let ResolvedBody::Stream { state, text } = resolved.body {
        let Peer::User(user) = peer(resolved.conversation, &resolved.openid)? else {
            return Err(ActionError::Unsupported(
                crate::error::UnsupportedKind::MessageCombination,
            ));
        };
        let key = resolved.conversation.target();
        let user_lock = bridge.streams.lock_user(key).await;
        let _stream_guard = user_lock.lock().await;
        let (stream_msg_id, index, msg_id, msg_seq, content_raw, local_id) = match state {
            StreamState::Start => {
                if resolved.active {
                    return Err(ActionError::BadParam(
                        "stream start requires a passive msg_id".into(),
                    ));
                }
                let context = bridge
                    .store
                    .reserve_reply(resolved.conversation, resolved.reply_inbound, now)
                    .await?;
                (
                    None,
                    0,
                    Some(context.official_id.clone()),
                    Some(context.sequence),
                    text,
                    None,
                )
            }
            StreamState::Mid | StreamState::Finish => {
                let progress = bridge
                    .streams
                    .get_valid(key, now, bridge.cache.stream_ttl_secs)
                    .await
                    .ok_or_else(|| {
                        ActionError::BadParam("stream mid/finish without a started stream".into())
                    })?;
                let content = format!("{}{}", progress.text, text);
                (
                    Some(progress.id),
                    progress.index + 1,
                    progress.msg_id,
                    progress.msg_seq,
                    content,
                    Some(progress.local_id),
                )
            }
        };
        let base_stream_id = stream_msg_id.clone();
        let request = StreamRequest {
            input_mode: (state == StreamState::Start).then(|| "replace".into()),
            input_state: if state == StreamState::Finish { 10 } else { 1 },
            index,
            content_type: "text".into(),
            content_raw: content_raw.clone(),
            stream_msg_id,
            msg_id,
            msg_seq,
        };
        let sent = bridge.qq.send_stream(&user, &request).await?;
        let resolved_stream_id = if !sent.id.is_empty() {
            sent.id.clone()
        } else if let Some(existing) = base_stream_id.clone() {
            existing
        } else {
            return Err(ActionError::SendOutcomeUnknown);
        };
        let stream_message = stream_cache_message(&content_raw);
        let local_id = match local_id {
            Some(local_id) => {
                update_record(
                    bridge,
                    local_id,
                    resolved.conversation,
                    &resolved_stream_id,
                    &sent,
                    stream_message,
                    now,
                )
                .await;
                local_id
            }
            None => record(bridge, resolved.conversation, &sent, stream_message, now).await?,
        };
        if state == StreamState::Finish {
            bridge.streams.remove(key).await;
        } else {
            bridge
                .streams
                .set(
                    key,
                    crate::bridge::StreamProgress {
                        id: base_stream_id.unwrap_or(resolved_stream_id),
                        index,
                        msg_id: request.msg_id,
                        msg_seq: request.msg_seq,
                        text: content_raw,
                        local_id,
                        updated_at: now,
                    },
                )
                .await;
        }
        return Ok(local_id);
    }
    let context = if resolved.active {
        None
    } else {
        Some(
            bridge
                .store
                .reserve_reply(resolved.conversation, resolved.reply_inbound, now)
                .await?,
        )
    };
    let openid = context
        .as_ref()
        .map(|context| context.openid.as_str())
        .unwrap_or(&resolved.openid);
    let peer = peer(resolved.conversation, openid)?;
    let cache_message = !matches!(&resolved.body, ResolvedBody::InputNotify { .. });
    let mut outgoing = match resolved.body {
        ResolvedBody::Text(text) => qbridge_qq::OutgoingMessage::text(text),
        ResolvedBody::Media { text, .. } => qbridge_qq::OutgoingMessage::media(
            file_info.unwrap_or_default(),
            (!text.is_empty()).then_some(text),
        ),
        ResolvedBody::Markdown { markdown, keyboard } => {
            qbridge_qq::OutgoingMessage::markdown(markdown, keyboard)
        }
        ResolvedBody::Card(card) => qbridge_qq::OutgoingMessage::card(card),
        ResolvedBody::InputNotify {
            input_type,
            input_second,
        } => qbridge_qq::OutgoingMessage::input_notify(input_type, input_second),
        ResolvedBody::Stream { .. } => unreachable!("stream is delivered above"),
    };
    if resolved.wakeup {
        outgoing = outgoing.wakeup();
    }
    if let Some(context) = &context {
        outgoing = outgoing.passive(context.official_id.clone(), context.sequence);
    }
    if let Some(reference) = &resolved.reference {
        outgoing = outgoing.referencing(reference.clone());
    }
    let sent = bridge.qq.send_message(&peer, &outgoing).await?;
    if !cache_message {
        return Ok(0);
    }
    if sent.id.is_empty() {
        return Err(ActionError::SendOutcomeUnknown);
    }
    record(bridge, resolved.conversation, &sent, cached_message, now).await
}

async fn upload(
    bridge: &Bridge,
    conversation: Conversation,
    openid: &str,
    media: &MediaPlan,
) -> Result<String, ActionError> {
    let peer = peer(conversation, openid)?;
    if let MediaSource::File(path) = &media.source {
        let metadata = tokio::fs::metadata(path)
            .await
            .map_err(|_| ActionError::BadParam("cannot inspect local media file".into()))?;
        if !metadata.is_file() || metadata.len() == 0 {
            return Err(ActionError::BadParam(
                "local media must be a regular nonempty file".into(),
            ));
        }
        if metadata.len() > MAX_CHUNKED_BYTES as u64 {
            return Err(ActionError::BadParam(
                "local media exceeds the 200 MiB QQ limit".into(),
            ));
        }
        if metadata.len() > MAX_DIRECT_BYTES as u64 {
            let uploaded = bridge
                .qq
                .upload_file_chunked(&peer, media.kind.file_type(), &media.file_name, path)
                .await?;
            return file_info(uploaded);
        }
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|_| ActionError::BadParam("cannot read local media file".into()))?;
        if bytes.is_empty() || bytes.len() > MAX_DIRECT_BYTES {
            return Err(ActionError::BadParam(
                "local media must be 1 byte to 5 MiB for direct upload".into(),
            ));
        }
        let upload = qbridge_qq::MediaUpload {
            file_type: media.kind.file_type(),
            source: qbridge_qq::MediaSource::Data {
                file_data: STANDARD.encode(bytes),
                file_name: media.file_name.clone(),
            },
        };
        return file_info(bridge.qq.upload_media(&peer, &upload).await?);
    }
    let source = match &media.source {
        MediaSource::Url(url) => qbridge_qq::MediaSource::Url {
            url: url.clone(),
            file_name: (media.kind == MediaKind::File).then(|| media.file_name.clone()),
        },
        MediaSource::Base64(encoded) => qbridge_qq::MediaSource::Data {
            file_data: encoded.clone(),
            file_name: media.file_name.clone(),
        },
        MediaSource::File(_) => unreachable!("local files are handled above"),
    };
    let upload = qbridge_qq::MediaUpload {
        file_type: media.kind.file_type(),
        source,
    };
    file_info(bridge.qq.upload_media(&peer, &upload).await?)
}

fn file_info(uploaded: qbridge_qq::UploadedMedia) -> Result<String, ActionError> {
    if uploaded.file_info.is_empty() {
        return Err(ActionError::Platform(qbridge_qq::Error::decode(
            "QQ upload response omitted file_info",
        )));
    }
    Ok(uploaded.file_info)
}

fn peer(conversation: Conversation, openid: &str) -> Result<Peer, ActionError> {
    match conversation {
        Conversation::Private(_) => UserOpenId::new(openid)
            .map(Peer::User)
            .map_err(|_| ActionError::NotFound(NotFoundKind::UnknownTarget)),
        Conversation::Group(_) => GroupOpenId::new(openid)
            .map(Peer::Group)
            .map_err(|_| ActionError::NotFound(NotFoundKind::UnknownTarget)),
    }
}

fn stream_cache_message(text: &str) -> Message {
    if text.is_empty() {
        Message(Vec::new())
    } else {
        Message(vec![Segment::Text(text.to_owned())])
    }
}

async fn outbound_event_content(
    bridge: &Bridge,
    conversation: Conversation,
    official_id: &str,
    sent: &qbridge_qq::SentMessage,
    message: Message,
    now: i64,
) -> Value {
    let scene = match conversation {
        Conversation::Private(_) => MessageScene::Private {
            sub_type: PrivateSubType::Friend,
        },
        Conversation::Group(id) => MessageScene::Group {
            group_id: id,
            sub_type: GroupSubType::Normal,
            anonymous: None,
        },
    };
    let sender = Sender {
        user_id: bridge.self_id,
        nickname: Some(bridge.state.nickname().await),
        card: None,
        role: None,
    };
    let mut event = Event {
        time: now,
        self_id: bridge.self_id,
        kind: EventKind::Message(MessageEvent::new(scene, 0, bridge.self_id, message, sender)),
    };
    let mut qq = Map::new();
    qq.insert("local_cache".into(), Value::Bool(true));
    qq.insert("message_id".into(), Value::String(official_id.to_owned()));
    if let Some(ext_info) = &sent.ext_info {
        qq.insert(
            "ext_info".into(),
            serde_json::to_value(ext_info).unwrap_or(Value::Null),
        );
    }
    if let EventKind::Message(message) = &mut event.kind {
        message.extra.insert("qq".into(), Value::Object(qq));
    }
    event.to_value(MessageFormat::Array)
}

async fn record(
    bridge: &Bridge,
    conversation: Conversation,
    sent: &qbridge_qq::SentMessage,
    message: Message,
    now: i64,
) -> Result<i32, ActionError> {
    let content = outbound_event_content(bridge, conversation, &sent.id, sent, message, now).await;
    let reference = sent
        .ext_info
        .as_ref()
        .and_then(|ext| ext.ref_idx.as_deref());
    match bridge
        .store
        .save_outbound(
            conversation,
            &sent.id,
            now,
            now.saturating_add(bridge.cache.sent_ttl_secs),
            &content,
            reference,
        )
        .await
    {
        Ok(id) => Ok(id),
        Err(error) => {
            tracing::warn!(
                reason = %error,
                "QQ accepted a message but caching it locally failed"
            );
            Err(ActionError::SendOutcomeUnknown)
        }
    }
}

async fn update_record(
    bridge: &Bridge,
    local_id: i32,
    conversation: Conversation,
    official_id: &str,
    sent: &qbridge_qq::SentMessage,
    message: Message,
    now: i64,
) {
    let content =
        outbound_event_content(bridge, conversation, official_id, sent, message, now).await;
    let reference = sent
        .ext_info
        .as_ref()
        .and_then(|ext| ext.ref_idx.as_deref());
    if let Err(error) = bridge
        .store
        .update_outbound(
            local_id,
            now.saturating_add(bridge.cache.sent_ttl_secs),
            &content,
            reference,
        )
        .await
    {
        tracing::warn!(
            message_id = local_id,
            reason = %error,
            "QQ accepted a stream chunk but updating the local cache failed"
        );
    }
}
