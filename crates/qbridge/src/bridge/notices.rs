//! QQ 通知事件 → OB11 notice 事件。

use crate::{
    bridge::{
        Bridge,
        events::{parse_time, parse_time_value},
    },
    store::IdentityKind,
};
use qbridge_ob11::{
    Event, EventKind,
    event::{GroupRequestType, NoticeEvent, RequestEvent, RequestKind},
};
use qbridge_qq::dispatch::{GroupJoinRequest, GroupMemberEvent, Interaction, UserEvent};
use serde_json::{Map, Value};

pub(super) async fn translate_friend(
    bridge: &Bridge,
    event_id: Option<&str>,
    event: UserEvent,
    event_type: &'static str,
    notice_type: &'static str,
) -> Result<Option<Event>, &'static str> {
    let time = parse_time(event.timestamp.as_deref())?;
    let user_id = bridge
        .store
        .identity(IdentityKind::User, &event.openid)
        .await
        .map_err(|_| "cannot map the friend")?;
    Ok(Some(notice_event(
        bridge,
        time,
        notice(
            time,
            notice_type,
            None,
            None,
            None,
            user_id,
            event_type,
            event_id,
            serde_json::to_value(&event).unwrap_or(Value::Null),
        ),
    )))
}

pub(super) async fn translate_group_robot(
    bridge: &Bridge,
    event_id: Option<&str>,
    event: qbridge_qq::dispatch::GroupRobotEvent,
    event_type: &'static str,
    notice_type: &'static str,
    sub_type: &'static str,
) -> Result<Option<Event>, &'static str> {
    let time = parse_time(event.timestamp.as_deref())?;
    let group_id = bridge
        .store
        .identity(IdentityKind::Group, &event.group_openid)
        .await
        .map_err(|_| "cannot map the group")?;
    let operator_openid = event
        .op_member_openid
        .as_deref()
        .ok_or("group robot event omitted op_member_openid")?;
    let operator = bridge
        .store
        .identity(IdentityKind::Member, operator_openid)
        .await
        .map_err(|_| "cannot map the group operator")?;
    Ok(Some(notice_event(
        bridge,
        time,
        notice(
            time,
            notice_type,
            Some(sub_type),
            Some(group_id),
            Some(operator),
            operator,
            event_type,
            event_id,
            serde_json::to_value(&event).unwrap_or(Value::Null),
        ),
    )))
}

pub(super) async fn translate_group_message_notice(
    bridge: &Bridge,
    event_id: Option<&str>,
    event: qbridge_qq::dispatch::GroupRobotEvent,
    event_type: &'static str,
    notice_type: &'static str,
) -> Result<Option<Event>, &'static str> {
    let time = parse_time(event.timestamp.as_deref())?;
    let group_id = bridge
        .store
        .identity(IdentityKind::Group, &event.group_openid)
        .await
        .map_err(|_| "cannot map the group")?;
    let operator_openid = event
        .op_member_openid
        .as_deref()
        .ok_or("group message event omitted op_member_openid")?;
    let operator = bridge
        .store
        .identity(IdentityKind::Member, operator_openid)
        .await
        .map_err(|_| "cannot map the group operator")?;
    Ok(Some(notice_event(
        bridge,
        time,
        notice(
            time,
            notice_type,
            Some("create"),
            Some(group_id),
            Some(operator),
            operator,
            event_type,
            event_id,
            serde_json::to_value(&event).unwrap_or(Value::Null),
        ),
    )))
}

pub(super) async fn translate_c2c_message_notice(
    bridge: &Bridge,
    event_id: Option<&str>,
    event: UserEvent,
    event_type: &'static str,
    notice_type: &'static str,
) -> Result<Option<Event>, &'static str> {
    let time = parse_time(event.timestamp.as_deref())?;
    let user_id = bridge
        .store
        .identity(IdentityKind::User, &event.openid)
        .await
        .map_err(|_| "cannot map the C2C user")?;
    Ok(Some(notice_event(
        bridge,
        time,
        notice(
            time,
            notice_type,
            Some("create"),
            None,
            None,
            user_id,
            event_type,
            event_id,
            serde_json::to_value(&event).unwrap_or(Value::Null),
        ),
    )))
}

pub(super) async fn translate_interaction(
    bridge: &Bridge,
    event_id: Option<&str>,
    event: Interaction,
) -> Result<Option<Event>, &'static str> {
    let time = parse_time(event.timestamp.as_deref())?;
    let (user_id, group_id) = match event.chat_type {
        Some(1) => {
            let group_openid = event
                .group_openid
                .as_deref()
                .ok_or("group interaction omitted group_openid")?;
            let member_openid = event
                .group_member_openid
                .as_deref()
                .ok_or("group interaction omitted group_member_openid")?;
            (
                bridge
                    .store
                    .identity(IdentityKind::Member, member_openid)
                    .await
                    .map_err(|_| "cannot map the interaction member")?,
                Some(
                    bridge
                        .store
                        .identity(IdentityKind::Group, group_openid)
                        .await
                        .map_err(|_| "cannot map the interaction group")?,
                ),
            )
        }
        Some(2) => {
            let openid = event
                .user_openid
                .as_deref()
                .ok_or("C2C interaction omitted user_openid")?;
            (
                bridge
                    .store
                    .identity(IdentityKind::User, openid)
                    .await
                    .map_err(|_| "cannot map the interaction user")?,
                None,
            )
        }
        _ => return Ok(None),
    };
    let mut notice = notice(
        time,
        "interaction",
        Some("button"),
        group_id,
        Some(user_id),
        user_id,
        "INTERACTION_CREATE",
        event_id,
        serde_json::to_value(&event).unwrap_or(Value::Null),
    );
    notice
        .extra
        .insert("interaction_id".into(), Value::String(event.id.clone()));
    Ok(Some(notice_event(bridge, time, notice)))
}

pub(super) fn notice_event(bridge: &Bridge, time: i64, notice: NoticeEvent) -> Event {
    Event {
        time,
        self_id: bridge.self_id,
        kind: EventKind::Notice(notice),
    }
}

#[allow(clippy::too_many_arguments)]
fn notice(
    _time: i64,
    notice_type: &'static str,
    sub_type: Option<&'static str>,
    group_id: Option<i64>,
    operator_id: Option<i64>,
    user_id: i64,
    event_type: &str,
    event_id: Option<&str>,
    data: Value,
) -> NoticeEvent {
    let mut event = NoticeEvent::custom(notice_type, user_id);
    event.sub_type = sub_type.map(str::to_owned);
    event.group_id = group_id;
    event.operator_id = operator_id;
    let mut qq = Map::new();
    qq.insert("event_type".into(), Value::String(event_type.into()));
    if let Some(event_id) = event_id {
        qq.insert("event_id".into(), Value::String(event_id.into()));
    }
    qq.insert("data".into(), data);
    event.extra.insert("qq".into(), Value::Object(qq));
    event
}

pub(super) async fn translate_group_member(
    bridge: &Bridge,
    event_id: Option<&str>,
    event: GroupMemberEvent,
    event_type: &'static str,
    added: bool,
) -> Result<Option<Event>, &'static str> {
    let time =
        parse_time_value(event.timestamp.as_ref()).unwrap_or_else(|_| crate::bridge::unix_now());
    let group_id = bridge
        .store
        .identity(IdentityKind::Group, &event.group_openid)
        .await
        .map_err(|_| "cannot map the group")?;
    let user_id = bridge
        .store
        .identity(IdentityKind::Member, &event.member_openid)
        .await
        .map_err(|_| "cannot map the group member")?;
    let operator = match event.op_member_openid.as_deref() {
        Some(openid) if openid != event.member_openid => Some(
            bridge
                .store
                .identity(IdentityKind::Member, openid)
                .await
                .map_err(|_| "cannot map the group operator")?,
        ),
        _ => None,
    };
    let (notice_type, sub_type) = if added {
        ("group_increase", "approve")
    } else if operator.is_some() {
        ("group_decrease", "kick")
    } else {
        ("group_decrease", "leave")
    };
    Ok(Some(notice_event(
        bridge,
        time,
        notice(
            time,
            notice_type,
            Some(sub_type),
            Some(group_id),
            operator,
            user_id,
            event_type,
            event_id,
            serde_json::to_value(&event).unwrap_or(Value::Null),
        ),
    )))
}

pub(super) async fn translate_group_join_request(
    bridge: &Bridge,
    event_id: Option<&str>,
    event: GroupJoinRequest,
) -> Result<Option<Event>, &'static str> {
    let time =
        parse_time_value(event.apply_at.as_ref()).unwrap_or_else(|_| crate::bridge::unix_now());
    let group_id = bridge
        .store
        .identity(IdentityKind::Group, &event.group_openid)
        .await
        .map_err(|_| "cannot map the group")?;
    let user_id = bridge
        .store
        .identity(IdentityKind::Member, &event.member_openid)
        .await
        .map_err(|_| "cannot map the group member")?;
    let flag = crate::bridge::group::join_flag(group_id, user_id, &event.join_request_id);
    let mut qq = Map::new();
    qq.insert(
        "event_type".into(),
        Value::String("GROUP_JOIN_REQUEST".into()),
    );
    if let Some(event_id) = event_id {
        qq.insert("event_id".into(), Value::String(event_id.into()));
    }
    qq.insert(
        "data".into(),
        serde_json::to_value(&event).unwrap_or(Value::Null),
    );
    let mut request = RequestEvent {
        kind: RequestKind::Group {
            sub_type: GroupRequestType::Add,
            group_id,
        },
        user_id,
        comment: event.risk_tips.unwrap_or_default(),
        flag,
        extra: Map::new(),
    };
    request.extra.insert("qq".into(), Value::Object(qq));
    Ok(Some(Event {
        time,
        self_id: bridge.self_id,
        kind: EventKind::Request(request),
    }))
}
