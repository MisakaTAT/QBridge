//! 日志脱敏。
//!
//! DEBUG 级别记录请求体、响应体之前必须经过这里。规则：隐藏
//! secret/token/authorization/password/cookie/signature/file_data/file_info
//! 等字段，URL 与 Base64 整体替换，长度和深度截断。

use serde_json::{Map, Value, json};
use std::fmt;

const MAX_NODES: usize = 128;
const MAX_CHARACTERS: usize = 4096;
const MAX_DEPTH: usize = 8;
const MAX_ITEMS: usize = 32;
const MAX_TEXT: usize = 1024;

pub fn value(value: &Value) -> Redacted<'_> {
    Redacted(value)
}

pub struct Redacted<'a>(&'a Value);

impl fmt::Display for Redacted<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut budget = Budget {
            nodes: MAX_NODES,
            characters: MAX_CHARACTERS,
        };
        write!(formatter, "{}", sanitize(self.0, 0, &mut budget))
    }
}

/// 脱敏并截断一段文本。
pub fn text(value: &str) -> String {
    let mut characters = MAX_TEXT;
    sanitized_text(value, &mut characters)
}

fn hidden(key: &str) -> bool {
    let key: String = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    [
        "secret",
        "token",
        "authorization",
        "password",
        "cookie",
        "credential",
        "signature",
    ]
    .iter()
    .any(|part| key.contains(part))
        || matches!(
            key.as_str(),
            "filedata" | "fileinfo" | "sessionid" | "apikey"
        )
}

fn sanitized_text(value: &str, characters: &mut usize) -> String {
    let lower = value.to_ascii_lowercase();
    if [
        "base64://",
        "http://",
        "https://",
        "ws://",
        "wss://",
        "qqbot ",
        "bearer ",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return "[REDACTED]".into();
    }
    let limit = (*characters).min(MAX_TEXT);
    let total = value.chars().count();
    let mut result: String = value.chars().take(limit).collect();
    *characters = characters.saturating_sub(result.chars().count());
    if total > limit {
        result.push_str("[TRUNCATED]");
    }
    result
}

struct Budget {
    nodes: usize,
    characters: usize,
}

impl Budget {
    fn spent(&self) -> bool {
        self.nodes == 0 || self.characters == 0
    }

    fn take(&mut self) {
        self.nodes = self.nodes.saturating_sub(1);
    }
}

fn sanitize(value: &Value, depth: usize, budget: &mut Budget) -> Value {
    if depth >= MAX_DEPTH || budget.spent() {
        return json!("[TRUNCATED]");
    }
    budget.take();
    match value {
        Value::Object(object) => {
            let mut output = Map::new();
            for (index, (key, value)) in object.iter().enumerate() {
                if index >= MAX_ITEMS || budget.spent() {
                    output.insert("...".into(), json!("[TRUNCATED]"));
                    break;
                }
                let name = sanitized_text(key, &mut budget.characters);
                let value = if hidden(key) {
                    budget.take();
                    json!("[REDACTED]")
                } else {
                    sanitize(value, depth + 1, budget)
                };
                output.insert(name, value);
            }
            Value::Object(output)
        }
        Value::Array(array) => {
            let mut output = Vec::new();
            for (index, value) in array.iter().enumerate() {
                if index >= MAX_ITEMS || budget.spent() {
                    output.push(json!("[TRUNCATED]"));
                    break;
                }
                output.push(sanitize(value, depth + 1, budget));
            }
            Value::Array(output)
        }
        Value::String(value) => json!(sanitized_text(value, &mut budget.characters)),
        other => other.clone(),
    }
}
