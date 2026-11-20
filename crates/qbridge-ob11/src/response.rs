//! OB11 动作响应。

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Async,
    Failed,
}

/// 动作响应。
///
/// `message` 与 `wording` 内容相同：部分 SDK（如 Shiro）读取 `wording`
/// 记录错误，NapCat 等实现也同时返回两者。`reason` 是机器可读的失败原因。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Response {
    pub status: Status,
    pub retcode: i64,
    pub data: Value,
    pub message: String,
    pub wording: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// 请求带了 `echo`（包括 `null`）时原样返回。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub echo: Option<Value>,
}

impl Response {
    pub fn ok(data: Value) -> Self {
        Self {
            status: Status::Ok,
            retcode: 0,
            data,
            message: String::new(),
            wording: String::new(),
            reason: None,
            echo: None,
        }
    }

    /// 已受理，稍后执行（`_async`、`_rate_limited`、`set_restart`）。
    pub fn accepted() -> Self {
        Self {
            status: Status::Async,
            retcode: 1,
            ..Self::ok(Value::Null)
        }
    }

    pub fn failed(retcode: i64, reason: impl Into<String>, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            status: Status::Failed,
            retcode,
            data: Value::Null,
            wording: message.clone(),
            message,
            reason: Some(reason.into()),
            echo: None,
        }
    }

    pub fn with_echo(mut self, echo: Option<Value>) -> Self {
        self.echo = echo;
        self
    }
}
