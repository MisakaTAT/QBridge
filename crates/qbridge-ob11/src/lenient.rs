//! 宽松的参数解析器。
//!
//! OB11 客户端经常把数字和布尔值写成字符串（表单提交时尤其如此），
//! 对象参数也可能以 JSON 字符串的形式出现。这些函数配合
//! `#[serde(deserialize_with = "...")]` 使用，统一接受这些写法。

use serde::{Deserialize, Deserializer, de::Error};
use serde_json::{Map, Value};

/// 整数，接受 JSON 数字或十进制字符串。
pub fn int<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
    parse_int(Value::deserialize(deserializer)?).map_err(D::Error::custom)
}

/// 可选整数，缺省或 `null` 为 `None`。需要同时标注 `#[serde(default)]`。
pub fn opt_int<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<i64>, D::Error> {
    match Value::deserialize(deserializer)? {
        Value::Null => Ok(None),
        value => parse_int(value).map(Some).map_err(D::Error::custom),
    }
}

/// OB11 规定为 int32 的整数，例如 `message_id`。
pub fn int32<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i32, D::Error> {
    let value = int(deserializer)?;
    i32::try_from(value).map_err(|_| D::Error::custom("integer out of int32 range"))
}

/// 非负整数，例如时长和延迟。
pub fn uint<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    let value = int(deserializer)?;
    u64::try_from(value).map_err(|_| D::Error::custom("expected a non-negative integer"))
}

/// 布尔值，接受 JSON 布尔或字符串 `true` / `false` / `1` / `0`。
pub fn boolean<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    match Value::deserialize(deserializer)? {
        Value::Bool(value) => Ok(value),
        Value::String(text) if text == "true" || text == "1" => Ok(true),
        Value::String(text) if text == "false" || text == "0" => Ok(false),
        _ => Err(D::Error::custom("expected a boolean")),
    }
}

/// JSON 对象，接受对象本身或内容为对象的 JSON 字符串。
pub fn object<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Map<String, Value>, D::Error> {
    match Value::deserialize(deserializer)? {
        Value::Object(object) => Ok(object),
        Value::String(text) => match serde_json::from_str(&text) {
            Ok(Value::Object(object)) => Ok(object),
            _ => Err(D::Error::custom("expected an object")),
        },
        _ => Err(D::Error::custom("expected an object")),
    }
}

/// 把数字或数字字符串转成文本，用于 ID 类字段。
pub(crate) fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

pub(crate) fn parse_int(value: Value) -> Result<i64, &'static str> {
    match value {
        Value::Number(number) => number.as_i64().ok_or("expected an integer"),
        Value::String(text) => text.trim().parse().map_err(|_| "expected an integer"),
        _ => Err("expected an integer"),
    }
}
