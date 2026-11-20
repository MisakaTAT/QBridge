use qbridge_ob11::action::{
    GetGroupInfo, MessageRef, SetGroupAddRequest, SetGroupBan, SetGroupKick, SetRestart,
};
use qbridge_ob11::{Action, AtTarget, Call, Message, Mode, ParseError, Segment, Target};
use serde_json::{Map, Value, json};

fn params(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(object) => object,
        _ => panic!("params must be an object"),
    }
}

fn parse(name: &str, value: Value) -> Result<Call, ParseError> {
    Call::parse(name, params(value))
}

fn send(call: Call) -> qbridge_ob11::action::SendMsg {
    match call.action {
        Action::SendMsg(send) => send,
        other => panic!("expected SendMsg, got {other:?}"),
    }
}

#[test]
fn suffixes_select_execution_mode() {
    assert_eq!(parse("get_status", json!({})).unwrap().mode, Mode::Sync);
    assert_eq!(
        parse("get_status_async", json!({})).unwrap().mode,
        Mode::Async
    );
    let limited = parse("get_status_rate_limited", json!({})).unwrap();
    assert_eq!(limited.mode, Mode::RateLimited);
    assert_eq!(limited.action, Action::GetStatus);
}

#[test]
fn send_actions_resolve_their_target() {
    let private = send(parse("send_private_msg", json!({"user_id": "1", "message": "a"})).unwrap());
    assert_eq!(private.target, Target::Private(1));
    let group = send(parse("send_group_msg", json!({"group_id": 2, "message": "a"})).unwrap());
    assert_eq!(group.target, Target::Group(2));
    let inferred = send(
        parse(
            "send_msg",
            json!({"group_id": 2, "user_id": 1, "message": "a"}),
        )
        .unwrap(),
    );
    assert_eq!(inferred.target, Target::Group(2));
    let explicit = send(
        parse(
            "send_msg",
            json!({"message_type": "private", "group_id": 2, "user_id": 1, "message": "a"}),
        )
        .unwrap(),
    );
    assert_eq!(explicit.target, Target::Private(1));
}

#[test]
fn send_actions_reject_missing_or_invalid_targets() {
    for (name, value) in [
        ("send_private_msg", json!({"message": "a"})),
        ("send_group_msg", json!({"user_id": 1, "message": "a"})),
        ("send_msg", json!({"message": "a"})),
        (
            "send_msg",
            json!({"message_type": "group", "user_id": 1, "message": "a"}),
        ),
        (
            "send_msg",
            json!({"message_type": "guild", "user_id": 1, "message": "a"}),
        ),
        ("send_private_msg", json!({"user_id": 1})),
    ] {
        assert!(
            matches!(parse(name, value), Err(ParseError::BadParam(_))),
            "{name}"
        );
    }
}

#[test]
fn send_message_honours_auto_escape_and_keeps_extra_params() {
    let parsed = send(
        parse(
            "send_private_msg",
            json!({"user_id": 1, "message": "[CQ:at,qq=2]"}),
        )
        .unwrap(),
    );
    assert_eq!(parsed.message.0, vec![Segment::At(AtTarget::User(2))]);
    let escaped = send(
        parse(
            "send_private_msg",
            json!({"user_id": 1, "message": "[CQ:at,qq=2]", "auto_escape": "true", "active": true}),
        )
        .unwrap(),
    );
    assert_eq!(escaped.message, Message::text("[CQ:at,qq=2]"));
    assert_eq!(escaped.extra.get("active"), Some(&json!(true)));
    assert!(!escaped.extra.contains_key("auto_escape"));
}

#[test]
fn defaults_follow_the_specification() {
    assert_eq!(
        parse("set_group_ban", json!({"group_id": 1, "user_id": 2}))
            .unwrap()
            .action,
        Action::SetGroupBan(SetGroupBan {
            group_id: 1,
            user_id: 2,
            duration: 1800
        })
    );
    assert_eq!(
        parse("set_restart", json!({})).unwrap().action,
        Action::SetRestart(SetRestart { delay: 0 })
    );
    assert_eq!(
        parse("get_group_info", json!({"group_id": "3"}))
            .unwrap()
            .action,
        Action::GetGroupInfo(GetGroupInfo {
            group_id: 3,
            no_cache: false
        })
    );
}

#[test]
fn message_ids_must_fit_int32() {
    assert_eq!(
        parse("delete_msg", json!({"message_id": "7"}))
            .unwrap()
            .action,
        Action::DeleteMsg(MessageRef { message_id: 7 })
    );
    assert!(parse("get_msg", json!({"message_id": 2147483648_i64})).is_err());
}

#[test]
fn negative_durations_are_rejected() {
    assert!(
        parse(
            "set_group_ban",
            json!({"group_id": 1, "user_id": 2, "duration": -1})
        )
        .is_err()
    );
    assert!(parse("set_restart", json!({"delay": -5})).is_err());
}

#[test]
fn parses_group_management_actions() {
    let kick = parse("set_group_kick", json!({"group_id": "1", "user_id": "2"})).unwrap();
    assert_eq!(
        kick.action,
        Action::SetGroupKick(SetGroupKick {
            group_id: 1,
            user_id: 2,
            reject_add_request: false,
            add_blacklist: false,
        })
    );
    let approve = parse(
        "set_group_add_request",
        json!({"flag": "f", "sub_type": "add"}),
    )
    .unwrap();
    assert_eq!(
        approve.action,
        Action::SetGroupAddRequest(SetGroupAddRequest {
            flag: "f".into(),
            sub_type: "add".into(),
            approve: true,
            reason: String::new(),
        })
    );
    assert!(parse("set_group_add_request", json!({"flag": "f"})).is_err());
}

#[test]
fn unsupported_standard_actions_skip_parameter_checks() {
    assert_eq!(
        parse("get_cookies", json!({"domain": 5})),
        Err(ParseError::Unsupported("get_cookies".into()))
    );
    assert_eq!(
        parse("set_group_admin_async", json!({})),
        Err(ParseError::Unsupported("set_group_admin".into()))
    );
}

#[test]
fn non_standard_names_become_extensions() {
    let call = parse("send_group_forward_msg_async", json!({"group_id": 1})).unwrap();
    assert_eq!(call.mode, Mode::Async);
    assert_eq!(
        call.action,
        Action::Extension {
            name: "send_group_forward_msg".into(),
            params: params(json!({"group_id": 1}))
        }
    );
}

#[test]
fn quick_operation_accepts_json_strings() {
    let call = parse(
        ".handle_quick_operation",
        json!({"context": "{\"post_type\":\"message\"}", "operation": {}}),
    )
    .unwrap();
    let Action::HandleQuickOperation(operation) = call.action else {
        panic!("expected quick operation");
    };
    assert_eq!(operation.context["post_type"], "message");
    assert!(parse(".handle_quick_operation", json!({"context": {}})).is_err());
}
