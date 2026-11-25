//! 非标准扩展动作：合并转发与互动回应。

use crate::{
    bridge::{
        Bridge,
        group::{group_openid, join_flag, member_openid},
        send,
    },
    error::{ActionError, NotFoundKind},
    store::IdentityKind,
};
use futures_util::{StreamExt, TryStreamExt, stream};
use qbridge_ob11::{Message, Target, action::SendMsg};
use serde_json::{Map, Value};

pub async fn execute(
    bridge: &Bridge,
    name: &str,
    params: Map<String, Value>,
    now: i64,
) -> Result<Value, ActionError> {
    match name {
        "send_private_forward_msg" | "send_group_forward_msg" | "send_forward_msg" => {
            forward(bridge, name, params, now).await
        }
        "put_interaction" => put_interaction(bridge, params).await,
        "get_group_join_request_list" => join_request_list(bridge, params).await,
        "get_group_bot_state" => bot_state(bridge, params).await,
        "get_group_member_blacklist" => member_blacklist(bridge, params).await,
        "set_group_member_blacklist" => set_member_blacklist(bridge, params).await,
        "get_group_list" => group_list(bridge).await,
        "get_friend_list" => friend_list(bridge).await,
        "get_avatar" => avatar(bridge, params).await,
        "get_robot_share_link" => share_link(bridge, params).await,
        "join_approval_strategy_create" => strategy_create(bridge, params).await,
        "join_approval_strategy_list" => strategy_list(bridge, params).await,
        "join_approval_strategy_update" => strategy_update(bridge, params).await,
        "join_approval_strategy_execute" => strategy_execute(bridge, params).await,
        "join_approval_strategy_whitelist" => strategy_whitelist(bridge, params).await,
        "join_approval_strategy_delete" => strategy_delete(bridge, params).await,
        "get_custom_menu" => bridge.qq.custom_menu().await.map_err(Into::into),
        "set_custom_menu" => set_custom_menu(bridge, params).await,
        "get_panel_list" => panel_list(bridge, params).await,
        "create_panel" => bridge
            .qq
            .create_panel(&Value::Object(params))
            .await
            .map_err(Into::into),
        "get_panel" => panel(bridge, params).await,
        "set_panel" => set_panel(bridge, params).await,
        "delete_panel" => delete_panel(bridge, params).await,
        "set_panel_target" => set_panel_target(bridge, params).await,
        _ => Err(ActionError::UnknownAction),
    }
}

async fn forward(
    bridge: &Bridge,
    name: &str,
    mut params: Map<String, Value>,
    now: i64,
) -> Result<Value, ActionError> {
    let target = match name {
        "send_private_forward_msg" => Target::Private(integer(&params, "user_id")?),
        "send_group_forward_msg" => Target::Group(integer(&params, "group_id")?),
        _ => match params.get("message_type").and_then(Value::as_str) {
            Some("group") => Target::Group(integer(&params, "group_id")?),
            Some("private") => Target::Private(integer(&params, "user_id")?),
            Some(_) => {
                return Err(ActionError::BadParam(
                    "message_type must be private or group".into(),
                ));
            }
            None if params.contains_key("group_id") => Target::Group(integer(&params, "group_id")?),
            None => Target::Private(integer(&params, "user_id")?),
        },
    };
    let messages = params
        .remove("messages")
        .ok_or_else(|| ActionError::BadParam("messages is required".into()))?;
    let Value::Array(items) = messages else {
        return Err(ActionError::BadParam(
            "messages must be a node array".into(),
        ));
    };
    if items.is_empty() {
        return Err(ActionError::BadParam(
            "messages must contain at least one node".into(),
        ));
    }
    let items = items
        .into_iter()
        .map(|item| match item {
            Value::Object(object) if object.contains_key("type") => Ok(Value::Object(object)),
            Value::Object(object) => Ok(serde_json::json!({"type": "node", "data": object})),
            _ => Err(ActionError::BadParam(
                "messages entries must be node objects".into(),
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let message = Message::from_value(Value::Array(items), false)
        .map_err(|error| ActionError::BadParam(error.to_string()))?;
    params.remove("message_type");
    params.remove("user_id");
    params.remove("group_id");
    send::send(
        bridge,
        SendMsg {
            target,
            message,
            extra: params,
        },
        now,
    )
    .await
}

async fn put_interaction(
    bridge: &Bridge,
    params: Map<String, Value>,
) -> Result<Value, ActionError> {
    let id = params
        .get("interaction_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| ActionError::BadParam("interaction_id is required".into()))?;
    let code = params
        .get("code")
        .and_then(Value::as_i64)
        .filter(|code| (0..=255).contains(code))
        .ok_or_else(|| ActionError::BadParam("code must be an integer from 0 to 255".into()))?;
    bridge.qq.ack_interaction(id, code as u8).await?;
    Ok(Value::Null)
}

fn optional_u32(params: &Map<String, Value>, key: &str) -> Result<Option<u32>, ActionError> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .map(Some)
            .ok_or_else(|| ActionError::BadParam(format!("{key} must be a positive integer"))),
        Some(Value::String(text)) => text
            .parse()
            .map(Some)
            .map_err(|_| ActionError::BadParam(format!("{key} must be a positive integer"))),
        Some(_) => Err(ActionError::BadParam(format!(
            "{key} must be a positive integer"
        ))),
    }
}

fn user_ids(params: &Map<String, Value>) -> Result<Vec<i64>, ActionError> {
    let mut ids = Vec::new();
    if let Some(id) = params.get("user_id") {
        ids.push(integer_value(id, "user_id")?);
    }
    if let Some(values) = params.get("user_ids") {
        let values = values
            .as_array()
            .ok_or_else(|| ActionError::BadParam("user_ids must be an array".into()))?;
        for value in values {
            ids.push(integer_value(value, "user_ids")?);
        }
    }
    if ids.is_empty() || ids.len() > 20 {
        return Err(ActionError::BadParam(
            "user_id/user_ids must contain 1 to 20 users".into(),
        ));
    }
    Ok(ids)
}

fn integer_value(value: &Value, key: &str) -> Result<i64, ActionError> {
    match value {
        Value::Number(number) => number
            .as_i64()
            .ok_or_else(|| ActionError::BadParam(format!("{key} must contain integers"))),
        Value::String(text) => text
            .parse()
            .map_err(|_| ActionError::BadParam(format!("{key} must contain integers"))),
        _ => Err(ActionError::BadParam(format!(
            "{key} must contain integers"
        ))),
    }
}

fn integer(params: &Map<String, Value>, key: &str) -> Result<i64, ActionError> {
    match params.get(key) {
        Some(Value::Number(number)) => number
            .as_i64()
            .ok_or_else(|| ActionError::BadParam(format!("{key} must be an integer"))),
        Some(Value::String(text)) => text
            .parse()
            .map_err(|_| ActionError::BadParam(format!("{key} must be an integer"))),
        _ => Err(ActionError::BadParam(format!("{key} is required"))),
    }
}

async fn join_request_list(
    bridge: &Bridge,
    params: Map<String, Value>,
) -> Result<Value, ActionError> {
    let group_id = integer(&params, "group_id")?;
    let group = group_openid(bridge, group_id).await?;
    let mut page = bridge
        .qq
        .join_requests(
            &group,
            params.get("cursor").and_then(Value::as_str),
            optional_u32(&params, "limit")?,
        )
        .await?;
    let items = page
        .get("list")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut output = Vec::with_capacity(items.len());
    for mut item in items {
        let Some(openid) = item
            .get("member_openid")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            continue;
        };
        let user_id = bridge.store.identity(IdentityKind::Member, &openid).await?;
        let request_id = item
            .get("join_request_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if let Value::Object(object) = &mut item {
            object.insert("group_id".into(), group_id.into());
            object.insert("user_id".into(), user_id.into());
            object.insert(
                "flag".into(),
                Value::String(join_flag(group_id, user_id, &request_id)),
            );
        }
        output.push(item);
    }
    page["list"] = Value::Array(output.clone());
    page["join_requests"] = Value::Array(output);
    Ok(page)
}

async fn bot_state(bridge: &Bridge, params: Map<String, Value>) -> Result<Value, ActionError> {
    let group_id = integer(&params, "group_id")?;
    let group = group_openid(bridge, group_id).await?;
    let mut state = bridge.qq.bot_group_state(&group).await?;
    state["group_id"] = group_id.into();
    Ok(state)
}

async fn member_blacklist(
    bridge: &Bridge,
    params: Map<String, Value>,
) -> Result<Value, ActionError> {
    let group_id = integer(&params, "group_id")?;
    let group = group_openid(bridge, group_id).await?;
    let mut page = bridge
        .qq
        .group_blacklist(
            &group,
            params.get("cursor").and_then(Value::as_str),
            optional_u32(&params, "limit")?,
        )
        .await?;
    let users = page
        .get("users")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut output = Vec::with_capacity(users.len());
    for mut user in users {
        if let Some(openid) = user
            .get("member_openid")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            let id = bridge.store.identity(IdentityKind::Member, &openid).await?;
            if let Value::Object(object) = &mut user {
                object.insert("user_id".into(), id.into());
            }
        }
        output.push(user);
    }
    page["users"] = Value::Array(output);
    Ok(page)
}

async fn set_member_blacklist(
    bridge: &Bridge,
    params: Map<String, Value>,
) -> Result<Value, ActionError> {
    let group_id = integer(&params, "group_id")?;
    let op = params
        .get("op")
        .and_then(Value::as_str)
        .filter(|op| matches!(*op, "add" | "del"))
        .ok_or_else(|| ActionError::BadParam("op must be add or del".into()))?;
    let ids = user_ids(&params)?;
    let group = group_openid(bridge, group_id).await?;
    let members = ids
        .into_iter()
        .map(|id| async move { member_openid(bridge, id).await })
        .collect::<Vec<_>>();
    let mut resolved = Vec::with_capacity(members.len());
    for member in members {
        resolved.push(member.await?);
    }
    Ok(bridge
        .qq
        .update_group_blacklist(&group, op, &resolved)
        .await?)
}

async fn group_list(bridge: &Bridge) -> Result<Value, ActionError> {
    const CONCURRENT_QUERIES: usize = 4;
    let identities = bridge.store.identities(IdentityKind::Group).await?;
    let groups = stream::iter(identities)
        .map(|(id, openid)| async move {
            let group = qbridge_qq::GroupOpenId::new(openid)
                .map_err(|_| ActionError::NotFound(NotFoundKind::UnknownTarget))?;
            let info = match bridge.qq.group_info(&group).await {
                Ok(info) => info,
                Err(qbridge_qq::Error::Api {
                    http_status: 404,
                    code,
                    ..
                }) if code != Some(11253) => return Ok(None),
                Err(error) => return Err(ActionError::from(error)),
            };
            Ok(Some(serde_json::json!({
                "group_id": id,
                "group_name": info.group_name,
                "member_count": info.group_member_num.unwrap_or(0),
                "max_member_count": 0,
            })))
        })
        .buffered(CONCURRENT_QUERIES)
        .try_filter_map(|group| async { Ok(group) })
        .try_collect()
        .await?;
    Ok(Value::Array(groups))
}

async fn friend_list(bridge: &Bridge) -> Result<Value, ActionError> {
    let identities = bridge.store.identities(IdentityKind::User).await?;
    let ids = identities.iter().map(|(id, _)| *id).collect::<Vec<_>>();
    let profiles = bridge
        .store
        .profiles_for_entities(&ids, crate::bridge::unix_now())
        .await?;
    Ok(Value::Array(
        identities
            .into_iter()
            .map(|(id, _)| {
                let profile = profiles.get(&id);
                serde_json::json!({
                    "user_id": id,
                    "nickname": profile.map(|profile| profile.display_name.as_str()).unwrap_or(""),
                    "remark": profile.map(|profile| profile.card.as_str()).unwrap_or(""),
                })
            })
            .collect(),
    ))
}

async fn avatar(bridge: &Bridge, params: Map<String, Value>) -> Result<Value, ActionError> {
    let user_id = integer(&params, "user_id")?;
    let openid = bridge
        .store
        .openid(IdentityKind::User, user_id)
        .await?
        .ok_or(ActionError::NotFound(
            crate::error::NotFoundKind::UnknownTarget,
        ))?;
    let url = bridge.qq.avatar_url(&openid);
    Ok(serde_json::json!({"user_id": user_id, "url": url, "message": url}))
}

async fn share_link(bridge: &Bridge, params: Map<String, Value>) -> Result<Value, ActionError> {
    let callback = params.get("callback_data").and_then(Value::as_str);
    let url = bridge.qq.generate_share_link(callback).await?;
    Ok(serde_json::json!({"url": url, "callback_data": callback.unwrap_or_default()}))
}

fn strategy_id(params: &Map<String, Value>) -> Result<&str, ActionError> {
    params
        .get("strategy_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| ActionError::BadParam("strategy_id is required".into()))
}

async fn strategy_body(bridge: &Bridge, params: &Map<String, Value>) -> Result<Value, ActionError> {
    let mut body = params.clone();
    body.remove("strategy_id");
    let mut groups = Vec::new();
    if let Some(ids) = body.remove("group_ids") {
        let ids = ids
            .as_array()
            .ok_or_else(|| ActionError::BadParam("group_ids must be an array".into()))?;
        for id in ids {
            let id = id
                .as_i64()
                .ok_or_else(|| ActionError::BadParam("group_ids must contain integers".into()))?;
            groups.push(group_openid(bridge, id).await?);
        }
    }
    if let Some(action) = body.get_mut("group_action").and_then(Value::as_object_mut)
        && let Some(ids) = action.remove("group_ids")
    {
        let ids = ids.as_array().ok_or_else(|| {
            ActionError::BadParam("group_action.group_ids must be an array".into())
        })?;
        let mut openids = Vec::new();
        for id in ids {
            let id = id.as_i64().ok_or_else(|| {
                ActionError::BadParam("group_action.group_ids must contain integers".into())
            })?;
            openids.push(group_openid(bridge, id).await?.as_str().to_owned());
        }
        action.insert(
            "group_openids".into(),
            Value::Array(openids.into_iter().map(Value::String).collect()),
        );
    }
    if !groups.is_empty() {
        body.insert(
            "group_openids".into(),
            Value::Array(
                groups
                    .iter()
                    .map(|group| Value::String(group.as_str().to_owned()))
                    .collect(),
            ),
        );
    }
    Ok(Value::Object(body))
}

async fn strategy_create(
    bridge: &Bridge,
    params: Map<String, Value>,
) -> Result<Value, ActionError> {
    let body = strategy_body(bridge, &params).await?;
    Ok(bridge.qq.join_strategy_create(&body).await?)
}

async fn strategy_list(bridge: &Bridge, params: Map<String, Value>) -> Result<Value, ActionError> {
    Ok(bridge
        .qq
        .join_strategy_list(
            params.get("cursor").and_then(Value::as_str),
            optional_u32(&params, "limit")?,
        )
        .await?)
}

async fn strategy_update(
    bridge: &Bridge,
    params: Map<String, Value>,
) -> Result<Value, ActionError> {
    let id = strategy_id(&params)?.to_owned();
    let body = strategy_body(bridge, &params).await?;
    Ok(bridge.qq.join_strategy_update(&id, &body).await?)
}

async fn strategy_execute(
    bridge: &Bridge,
    params: Map<String, Value>,
) -> Result<Value, ActionError> {
    let id = strategy_id(&params)?;
    bridge.qq.join_strategy_execute(id).await?;
    Ok(Value::Null)
}

async fn strategy_whitelist(
    bridge: &Bridge,
    params: Map<String, Value>,
) -> Result<Value, ActionError> {
    let id = strategy_id(&params)?.to_owned();
    let mut body = params;
    body.remove("strategy_id");
    Ok(bridge
        .qq
        .join_strategy_whitelist(&id, &Value::Object(body))
        .await?)
}

async fn strategy_delete(
    bridge: &Bridge,
    params: Map<String, Value>,
) -> Result<Value, ActionError> {
    let id = strategy_id(&params)?;
    bridge.qq.join_strategy_delete(id).await?;
    Ok(Value::Null)
}

async fn set_custom_menu(
    bridge: &Bridge,
    mut params: Map<String, Value>,
) -> Result<Value, ActionError> {
    let body = params
        .remove("menu")
        .map(|menu| serde_json::json!({"menu": menu}))
        .ok_or_else(|| ActionError::BadParam("menu is required".into()))?;
    Ok(bridge.qq.put_custom_menu(&body).await?)
}

async fn panel_list(bridge: &Bridge, params: Map<String, Value>) -> Result<Value, ActionError> {
    let scope = params
        .get("scope")
        .and_then(Value::as_str)
        .filter(|scope| !scope.is_empty())
        .ok_or_else(|| ActionError::BadParam("scope is required".into()))?;
    Ok(bridge
        .qq
        .panels(
            scope,
            params.get("cursor").and_then(Value::as_str),
            optional_u32(&params, "limit")?,
        )
        .await?)
}

async fn panel(bridge: &Bridge, params: Map<String, Value>) -> Result<Value, ActionError> {
    let id = params
        .get("panel_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| ActionError::BadParam("panel_id is required".into()))?;
    Ok(bridge.qq.panel(id).await?)
}

async fn set_panel(bridge: &Bridge, mut params: Map<String, Value>) -> Result<Value, ActionError> {
    let id = params
        .remove("panel_id")
        .and_then(|id| id.as_str().map(str::to_owned))
        .filter(|id| !id.is_empty())
        .ok_or_else(|| ActionError::BadParam("panel_id is required".into()))?;
    let body = params
        .remove("panel")
        .map(|panel| serde_json::json!({"panel": panel}))
        .ok_or_else(|| ActionError::BadParam("panel is required".into()))?;
    Ok(bridge.qq.update_panel(&id, &body).await?)
}

async fn delete_panel(bridge: &Bridge, params: Map<String, Value>) -> Result<Value, ActionError> {
    let id = params
        .get("panel_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| ActionError::BadParam("panel_id is required".into()))?;
    bridge.qq.delete_panel(id).await?;
    Ok(Value::Null)
}

async fn set_panel_target(
    bridge: &Bridge,
    mut params: Map<String, Value>,
) -> Result<Value, ActionError> {
    let id = params
        .remove("panel_id")
        .and_then(|id| id.as_str().map(str::to_owned))
        .filter(|id| !id.is_empty())
        .ok_or_else(|| ActionError::BadParam("panel_id is required".into()))?;
    bridge
        .qq
        .set_panel_target(&id, &Value::Object(params))
        .await?;
    Ok(Value::Null)
}
