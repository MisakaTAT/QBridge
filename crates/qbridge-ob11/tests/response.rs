use qbridge_ob11::Response;
use serde_json::{Value, json};

#[test]
fn success_has_empty_message_and_wording() {
    let value = serde_json::to_value(Response::ok(json!({"message_id": 1}))).unwrap();
    assert_eq!(
        value,
        json!({"status": "ok", "retcode": 0, "data": {"message_id": 1}, "message": "", "wording": ""})
    );
}

#[test]
fn accepted_is_async_with_retcode_one() {
    let value = serde_json::to_value(Response::accepted()).unwrap();
    assert_eq!(value["status"], "async");
    assert_eq!(value["retcode"], 1);
    assert_eq!(value["data"], Value::Null);
}

#[test]
fn failure_repeats_message_as_wording_and_keeps_reason() {
    let value =
        serde_json::to_value(Response::failed(1400, "BAD_PARAMETER", "missing user_id")).unwrap();
    assert_eq!(value["status"], "failed");
    assert_eq!(value["retcode"], 1400);
    assert_eq!(value["message"], "missing user_id");
    assert_eq!(value["wording"], "missing user_id");
    assert_eq!(value["reason"], "BAD_PARAMETER");
}

#[test]
fn echo_is_returned_verbatim_including_null() {
    let echo = json!({"seq": [1, "a"]});
    let value =
        serde_json::to_value(Response::ok(Value::Null).with_echo(Some(echo.clone()))).unwrap();
    assert_eq!(value["echo"], echo);
    let null =
        serde_json::to_value(Response::ok(Value::Null).with_echo(Some(Value::Null))).unwrap();
    assert!(null.as_object().unwrap().contains_key("echo"));
    let absent = serde_json::to_value(Response::ok(Value::Null)).unwrap();
    assert!(!absent.as_object().unwrap().contains_key("echo"));
}
