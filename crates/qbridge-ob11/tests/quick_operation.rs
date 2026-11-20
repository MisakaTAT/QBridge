use qbridge_ob11::ParseError;
use qbridge_ob11::quick_operation::expand;
use serde_json::{Map, Value, json};

fn object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(object) => object,
        _ => panic!("expected object"),
    }
}

fn run(context: Value, operation: Value) -> Result<Vec<(&'static str, Value)>, ParseError> {
    expand(42, &object(context), &object(operation)).map(|actions| {
        actions
            .into_iter()
            .map(|(name, params)| (name, Value::Object(params)))
            .collect()
    })
}

fn group_context() -> Value {
    json!({"self_id": 42, "post_type": "message", "message_type": "group", "group_id": 7, "user_id": 9, "message_id": 3, "anonymous": null})
}

#[test]
fn group_reply_mentions_sender_by_default() {
    let actions = run(group_context(), json!({"reply": "hi"})).unwrap();
    assert_eq!(
        actions,
        vec![(
            "send_group_msg",
            json!({"group_id": 7, "message": [
                {"type": "at", "data": {"qq": "9"}},
                {"type": "text", "data": {"text": "hi"}},
            ]})
        )]
    );
}

#[test]
fn at_sender_can_be_disabled_and_auto_escape_respected() {
    let actions = run(
        group_context(),
        json!({"reply": "[CQ:face,id=1]", "at_sender": false, "auto_escape": true}),
    )
    .unwrap();
    assert_eq!(
        actions[0].1["message"],
        json!([{"type": "text", "data": {"text": "[CQ:face,id=1]"}}])
    );
}

#[test]
fn private_reply_never_mentions() {
    let context = json!({"post_type": "message", "message_type": "private", "user_id": 9});
    let actions = run(
        context,
        json!({"reply": "hi", "delete": true, "kick": true}),
    )
    .unwrap();
    assert_eq!(
        actions,
        vec![(
            "send_private_msg",
            json!({"user_id": 9, "message": [{"type": "text", "data": {"text": "hi"}}]})
        )]
    );
}

#[test]
fn group_management_operations_expand_in_order() {
    let actions = run(
        group_context(),
        json!({"delete": true, "kick": true, "ban": true, "ban_duration": 60}),
    )
    .unwrap();
    let names: Vec<_> = actions.iter().map(|(name, _)| *name).collect();
    assert_eq!(names, ["delete_msg", "set_group_kick", "set_group_ban"]);
    assert_eq!(actions[0].1, json!({"message_id": 3}));
    assert_eq!(
        actions[2].1,
        json!({"group_id": 7, "user_id": 9, "duration": 60})
    );
}

#[test]
fn anonymous_senders_are_banned_by_flag_and_never_kicked() {
    let mut context = group_context();
    context["anonymous"] = json!({"id": 1, "name": "a", "flag": "f"});
    let actions = run(context, json!({"reply": "hi", "kick": true, "ban": true})).unwrap();
    let names: Vec<_> = actions.iter().map(|(name, _)| *name).collect();
    assert_eq!(names, ["send_group_msg", "set_group_anonymous_ban"]);
    assert_eq!(actions[0].1["message"].as_array().unwrap().len(), 1);
    assert_eq!(actions[1].1["duration"], 1800);
}

#[test]
fn requests_expand_to_approval_actions() {
    let friend =
        json!({"post_type": "request", "request_type": "friend", "flag": "f", "user_id": 9});
    assert_eq!(
        run(friend, json!({"approve": true, "remark": "r"})).unwrap(),
        vec![(
            "set_friend_add_request",
            json!({"flag": "f", "approve": true, "remark": "r"})
        )]
    );
    let group = json!({"post_type": "request", "request_type": "group", "sub_type": "add", "flag": "g", "user_id": 9, "group_id": 7});
    assert_eq!(
        run(group.clone(), json!({"approve": false, "reason": "no"})).unwrap(),
        vec![(
            "set_group_add_request",
            json!({"flag": "g", "sub_type": "add", "approve": false, "reason": "no"})
        )]
    );
    assert!(run(group, json!({})).unwrap().is_empty());
}

#[test]
fn invalid_contexts_and_operations_are_rejected() {
    assert!(run(json!({"post_type": "notice"}), json!({"reply": "x"})).is_err());
    assert!(
        run(json!({"post_type": "notice"}), json!({}))
            .unwrap()
            .is_empty()
    );
    assert!(run(group_context(), json!({"delete": "yes"})).is_err());
    assert!(
        run(
            json!({"post_type": "message", "message_type": "group", "user_id": 9}),
            json!({})
        )
        .is_err()
    );
    assert!(
        run(
            json!({"post_type": "request", "request_type": "friend"}),
            json!({"approve": true})
        )
        .is_err()
    );
}

#[test]
fn self_id_mismatch_is_rejected() {
    let mut context = group_context();
    context["self_id"] = json!(1);
    assert!(run(context, json!({"reply": "hi"})).is_err());
}

#[test]
fn boolean_fields_are_validated_in_every_context() {
    let private = json!({"post_type": "message", "message_type": "private", "user_id": 9});
    assert!(run(private.clone(), json!({"delete": "yes"})).is_err());
    assert!(run(private, json!({"at_sender": 1})).is_err());
    assert!(run(group_context(), json!({"auto_escape": "true"})).is_err());
}
