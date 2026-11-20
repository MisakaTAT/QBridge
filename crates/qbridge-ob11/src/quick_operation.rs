//! 快速操作：把事件上下文和操作对象展开成普通动作。
//!
//! 规则见 OB11 文档 event/message.md、event/request.md 中的“快速操作”。

use crate::action::ParseError;
use crate::message::{AtTarget, Message, Segment};
use serde_json::{Map, Value, json};

/// 展开后的动作名和参数，调用方再用 [`crate::Call::parse`] 解析执行。
pub type ExpandedAction = (&'static str, Map<String, Value>);

pub fn expand(
    self_id: i64,
    context: &Map<String, Value>,
    operation: &Map<String, Value>,
) -> Result<Vec<ExpandedAction>, ParseError> {
    if context
        .get("self_id")
        .is_some_and(|id| id.as_i64() != Some(self_id))
    {
        return Err(bad("context self_id does not match"));
    }
    let mut actions = Vec::new();
    match context.get("post_type").and_then(Value::as_str) {
        Some("message") => message(context, operation, &mut actions)?,
        Some("request") => request(context, operation, &mut actions)?,
        Some("notice" | "meta_event") if operation.is_empty() => {}
        _ => return Err(bad("context does not support quick operations")),
    }
    Ok(actions)
}

fn message(
    context: &Map<String, Value>,
    operation: &Map<String, Value>,
    actions: &mut Vec<ExpandedAction>,
) -> Result<(), ParseError> {
    let group = match context.get("message_type").and_then(Value::as_str) {
        Some("private") => None,
        Some("group") => Some(integer(context, "group_id")?),
        _ => return Err(bad("context requires message_type private or group")),
    };
    let user = integer(context, "user_id")?;
    let anonymous = context.get("anonymous").filter(|value| !value.is_null());
    let auto_escape = flag(operation, "auto_escape", false)?;
    let at_sender = flag(operation, "at_sender", true)?;
    let delete = flag(operation, "delete", false)?;
    let kick = flag(operation, "kick", false)?;
    let ban = flag(operation, "ban", false)?;
    if let Some(reply) = operation.get("reply") {
        let mut message = Message::from_value(reply.clone(), auto_escape)?;
        if group.is_some() && anonymous.is_none() && at_sender {
            message.0.insert(0, Segment::At(AtTarget::User(user)));
        }
        let message = message.to_value();
        actions.push(match group {
            Some(group) => (
                "send_group_msg",
                object(json!({"group_id": group, "message": message})),
            ),
            None => (
                "send_private_msg",
                object(json!({"user_id": user, "message": message})),
            ),
        });
    }
    let Some(group) = group else {
        return Ok(());
    };
    if delete {
        let message_id = integer(context, "message_id")?;
        actions.push(("delete_msg", object(json!({"message_id": message_id}))));
    }
    if kick && anonymous.is_none() {
        actions.push((
            "set_group_kick",
            object(json!({"group_id": group, "user_id": user})),
        ));
    }
    if ban {
        let duration = operation
            .get("ban_duration")
            .cloned()
            .unwrap_or(json!(1800));
        actions.push(match anonymous {
            Some(anonymous) => (
                "set_group_anonymous_ban",
                object(json!({"group_id": group, "anonymous": anonymous, "duration": duration})),
            ),
            None => (
                "set_group_ban",
                object(json!({"group_id": group, "user_id": user, "duration": duration})),
            ),
        });
    }
    Ok(())
}

fn request(
    context: &Map<String, Value>,
    operation: &Map<String, Value>,
    actions: &mut Vec<ExpandedAction>,
) -> Result<(), ParseError> {
    if !operation.contains_key("approve") {
        return Ok(());
    }
    let approve = flag(operation, "approve", true)?;
    let request_flag = context
        .get("flag")
        .and_then(Value::as_str)
        .ok_or_else(|| bad("context requires string flag"))?;
    let text = |key: &str| operation.get(key).cloned().unwrap_or(json!(""));
    actions.push(match context.get("request_type").and_then(Value::as_str) {
        Some("friend") => (
            "set_friend_add_request",
            object(json!({"flag": request_flag, "approve": approve, "remark": text("remark")})),
        ),
        Some("group") => (
            "set_group_add_request",
            object(json!({
                "flag": request_flag,
                "sub_type": context.get("sub_type").cloned().unwrap_or(Value::Null),
                "approve": approve,
                "reason": text("reason"),
            })),
        ),
        _ => return Err(bad("context requires request_type friend or group")),
    });
    Ok(())
}

fn bad(message: &str) -> ParseError {
    ParseError::BadParam(message.to_owned())
}

fn integer(context: &Map<String, Value>, key: &str) -> Result<i64, ParseError> {
    context
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| ParseError::BadParam(format!("context requires integer {key}")))
}

fn flag(operation: &Map<String, Value>, key: &str, default: bool) -> Result<bool, ParseError> {
    match operation.get(key) {
        None => Ok(default),
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(ParseError::BadParam(format!(
            "operation {key} must be boolean"
        ))),
    }
}

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(object) => object,
        _ => unreachable!("json! object literal"),
    }
}
