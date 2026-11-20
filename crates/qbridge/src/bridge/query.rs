//! 本地查询与账号信息类动作。

use crate::{
    bridge::Bridge,
    error::{ActionError, NotFoundKind},
};
use qbridge_ob11::action::MessageRef;
use serde_json::{Value, json};

pub async fn get_msg(bridge: &Bridge, request: MessageRef, now: i64) -> Result<Value, ActionError> {
    let mut event = bridge
        .store
        .message(request.message_id, now)
        .await?
        .ok_or(ActionError::NotFound(NotFoundKind::MessageNotCached))?;
    if let Value::Object(object) = &mut event {
        object.insert("real_id".into(), Value::from(request.message_id));
        object.insert(
            "group".into(),
            Value::Bool(object.get("message_type").and_then(Value::as_str) == Some("group")),
        );
        if let Some(qq) = object.get_mut("qq").and_then(Value::as_object_mut) {
            qq.insert("local_cache".into(), Value::Bool(true));
        }
    }
    Ok(event)
}

pub async fn delete_msg(bridge: &Bridge, request: MessageRef) -> Result<Value, ActionError> {
    let target = bridge.store.recall_target(request.message_id).await?;
    if target.inbound {
        return Err(ActionError::NotOwned);
    }
    let peer = peer(target.conversation, &target.openid)?;
    bridge.qq.recall(&peer, &target.official_id).await?;
    if let Err(error) = bridge.store.remove_outbound(request.message_id).await {
        tracing::warn!(
            message_id = request.message_id,
            reason = %error,
            "QQ recalled a message but local cache cleanup failed"
        );
    }
    Ok(Value::Null)
}

fn peer(
    conversation: crate::store::Conversation,
    openid: &str,
) -> Result<qbridge_qq::Peer, ActionError> {
    match conversation {
        crate::store::Conversation::Private(_) => qbridge_qq::UserOpenId::new(openid)
            .map(qbridge_qq::Peer::User)
            .map_err(|_| ActionError::NotFound(NotFoundKind::UnknownTarget)),
        crate::store::Conversation::Group(_) => qbridge_qq::GroupOpenId::new(openid)
            .map(qbridge_qq::Peer::Group)
            .map_err(|_| ActionError::NotFound(NotFoundKind::UnknownTarget)),
    }
}

pub async fn get_image(bridge: &Bridge, file: &str, now: i64) -> Result<Value, ActionError> {
    let url = bridge
        .store
        .image_url(file, now)
        .await?
        .ok_or(ActionError::NotFound(NotFoundKind::MediaNotFound))?;
    Ok(json!({"file": url, "url": url, "local_cache": true}))
}

pub async fn login_info(bridge: &Bridge) -> Result<Value, ActionError> {
    Ok(json!({
        "user_id": bridge.self_id,
        "nickname": bridge.state.nickname().await
    }))
}

pub fn status(bridge: &Bridge) -> Result<Value, ActionError> {
    let online = bridge.state.online();
    Ok(json!({
        "online": online,
        "good": online,
        "identity": bridge.store.identity_stats(),
    }))
}

pub fn version() -> Result<Value, ActionError> {
    Ok(json!({
        "app_name": "qbridge",
        "app_version": env!("CARGO_PKG_VERSION"),
        "protocol_version": "v11"
    }))
}

pub async fn clean_cache(bridge: &Bridge, now: i64) -> Result<Value, ActionError> {
    bridge.store.clean_expired(now).await?;
    Ok(Value::Null)
}
