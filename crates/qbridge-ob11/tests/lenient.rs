use qbridge_ob11::lenient;
use serde::Deserialize;
use serde_json::{Map, Value, json};

#[derive(Debug, Deserialize)]
struct Params {
    #[serde(deserialize_with = "lenient::int")]
    int: i64,
    #[serde(default, deserialize_with = "lenient::opt_int")]
    opt_int: Option<i64>,
    #[serde(default, deserialize_with = "lenient::int32")]
    int32: i32,
    #[serde(default, deserialize_with = "lenient::uint")]
    uint: u64,
    #[serde(default, deserialize_with = "lenient::boolean")]
    boolean: bool,
    #[serde(default, deserialize_with = "lenient::object")]
    object: Map<String, Value>,
}

fn parse(value: Value) -> Result<Params, String> {
    serde_json::from_value(value).map_err(|error| error.to_string())
}

#[test]
fn accepts_numbers_and_numeric_strings() {
    let params = parse(json!({"int": "42", "opt_int": 7, "int32": "-3", "uint": "5"})).unwrap();
    assert_eq!(params.int, 42);
    assert_eq!(params.opt_int, Some(7));
    assert_eq!(params.int32, -3);
    assert_eq!(params.uint, 5);
}

#[test]
fn optional_integer_treats_null_and_missing_as_none() {
    assert_eq!(
        parse(json!({"int": 1, "opt_int": null})).unwrap().opt_int,
        None
    );
    assert_eq!(parse(json!({"int": 1})).unwrap().opt_int, None);
}

#[test]
fn rejects_invalid_integers() {
    assert!(parse(json!({"int": "abc"})).is_err());
    assert!(parse(json!({"int": 1.5})).is_err());
    assert!(parse(json!({"int": true})).is_err());
    assert!(parse(json!({"int": 1, "int32": 2147483648_i64})).is_err());
    assert!(parse(json!({"int": 1, "uint": -1})).is_err());
}

#[test]
fn accepts_boolean_strings() {
    for (input, expected) in [
        (json!(true), true),
        (json!("true"), true),
        (json!("1"), true),
        (json!(false), false),
        (json!("false"), false),
        (json!("0"), false),
    ] {
        assert_eq!(
            parse(json!({"int": 1, "boolean": input})).unwrap().boolean,
            expected
        );
    }
    assert!(parse(json!({"int": 1, "boolean": "yes"})).is_err());
    assert!(parse(json!({"int": 1, "boolean": 1})).is_err());
}

#[test]
fn accepts_objects_and_json_object_strings() {
    let direct = parse(json!({"int": 1, "object": {"a": 1}})).unwrap();
    let text = parse(json!({"int": 1, "object": "{\"a\":1}"})).unwrap();
    assert_eq!(direct.object, text.object);
    assert!(parse(json!({"int": 1, "object": "[1]"})).is_err());
    assert!(parse(json!({"int": 1, "object": 3})).is_err());
}
