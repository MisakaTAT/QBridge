//! CQ 码的转义、解析和编码。
//!
//! 规则见 OB11 文档 message/string.md：文本中转义 `&`、`[`、`]`，
//! 参数值中另外转义 `,`。

use serde_json::{Map, Value};

/// 转义普通文本。
pub fn escape_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('[', "&#91;")
        .replace(']', "&#93;")
}

/// 转义 CQ 码参数值。
pub fn escape_param(value: &str) -> String {
    escape_text(value).replace(',', "&#44;")
}

/// 反转义普通文本。
pub fn unescape_text(text: &str) -> String {
    text.replace("&#91;", "[")
        .replace("&#93;", "]")
        .replace("&amp;", "&")
}

/// 反转义 CQ 码参数值。
pub fn unescape_param(value: &str) -> String {
    value
        .replace("&#91;", "[")
        .replace("&#93;", "]")
        .replace("&#44;", ",")
        .replace("&amp;", "&")
}

/// 段类型名只允许字母、数字、`_` 和 `-`。
pub(crate) fn valid_kind(kind: &str) -> bool {
    !kind.is_empty()
        && kind
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// 参数名不能为空，也不能包含 CQ 码的控制字符。
pub(crate) fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && !key
            .chars()
            .any(|c| matches!(c, '[' | ']' | ',' | '=' | '&'))
}

/// 把 CQ 字符串拆成 `(类型, 参数)` 列表。文本段的类型为 `text`。
///
/// 格式不合法的 `[CQ:...]` 按字面文本保留。
pub(crate) fn parse(input: &str) -> Vec<(String, Map<String, Value>)> {
    let mut parts = Vec::new();
    let mut rest = input;
    while let Some(start) = rest.find("[CQ:") {
        let candidate = &rest[start + 4..];
        let Some(end) = candidate.find(']') else {
            break;
        };
        let mut fields = candidate[..end].split(',');
        let kind = fields.next().unwrap_or_default();
        let mut data = Map::new();
        let valid = valid_kind(kind)
            && fields.all(|field| match field.split_once('=') {
                Some((key, value)) if valid_key(key) => {
                    data.insert(key.to_owned(), Value::String(unescape_param(value)));
                    true
                }
                _ => false,
            });
        if valid {
            push_text(&mut parts, &rest[..start]);
            parts.push((kind.to_owned(), data));
        } else {
            push_text(&mut parts, &rest[..start + 4 + end + 1]);
        }
        rest = &candidate[end + 1..];
    }
    push_text(&mut parts, rest);
    parts
}

fn push_text(parts: &mut Vec<(String, Map<String, Value>)>, raw: &str) {
    if raw.is_empty() {
        return;
    }
    let text = unescape_text(raw);
    if let Some((kind, data)) = parts.last_mut()
        && kind == "text"
        && let Some(Value::String(previous)) = data.get_mut("text")
    {
        previous.push_str(&text);
        return;
    }
    let mut data = Map::new();
    data.insert("text".into(), Value::String(text));
    parts.push(("text".into(), data));
}

/// 编码一个非文本段。非字符串的参数值按 JSON 文本编码。
pub(crate) fn encode(kind: &str, data: &Map<String, Value>) -> String {
    let mut output = format!("[CQ:{kind}");
    for (key, value) in data {
        let text = match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        output.push(',');
        output.push_str(key);
        output.push('=');
        output.push_str(&escape_param(&text));
    }
    output.push(']');
    output
}
