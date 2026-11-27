//! OB11 动作的解析。
//!
//! 只有 QBridge 能执行的标准动作才有对应的类型；其余标准动作解析为
//! [`ParseError::Unsupported`]，不校验参数。非标准动作名原样交给上层。

use crate::lenient;
use crate::message::{Message, MessageError};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

/// OneBot v11 公开 API 的全部 38 个动作名。
pub const STANDARD_ACTIONS: [&str; 38] = [
    "send_private_msg",
    "send_group_msg",
    "send_msg",
    "delete_msg",
    "get_msg",
    "get_forward_msg",
    "send_like",
    "set_group_kick",
    "set_group_ban",
    "set_group_anonymous_ban",
    "set_group_whole_ban",
    "set_group_admin",
    "set_group_anonymous",
    "set_group_card",
    "set_group_name",
    "set_group_leave",
    "set_group_special_title",
    "set_friend_add_request",
    "set_group_add_request",
    "get_login_info",
    "get_stranger_info",
    "get_friend_list",
    "get_group_info",
    "get_group_list",
    "get_group_member_info",
    "get_group_member_list",
    "get_group_honor_info",
    "get_cookies",
    "get_csrf_token",
    "get_credentials",
    "get_record",
    "get_image",
    "can_send_image",
    "can_send_record",
    "get_status",
    "get_version_info",
    "set_restart",
    "clean_cache",
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// 标准动作，但 QBridge 不支持。
    #[error("{0} is not supported")]
    Unsupported(String),
    #[error("{0}")]
    BadParam(String),
}

impl From<MessageError> for ParseError {
    fn from(error: MessageError) -> Self {
        Self::BadParam(error.0)
    }
}

/// 执行方式，由动作名后缀决定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Sync,
    /// `_async`：入队后立即返回。
    Async,
    /// `_rate_limited`：入队后按间隔依次执行。
    RateLimited,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub action: Action,
    pub mode: Mode,
}

impl Call {
    pub fn parse(name: &str, params: Map<String, Value>) -> Result<Self, ParseError> {
        let (base, mode) = if let Some(base) = name.strip_suffix("_rate_limited") {
            (base, Mode::RateLimited)
        } else if let Some(base) = name.strip_suffix("_async") {
            (base, Mode::Async)
        } else {
            (name, Mode::Sync)
        };
        Ok(Self {
            action: Action::parse(base, params)?,
            mode,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    SendMsg(SendMsg),
    DeleteMsg(MessageRef),
    GetMsg(MessageRef),
    GetImage(GetImage),
    GetGroupInfo(GetGroupInfo),
    GetGroupMemberInfo(GetGroupMemberInfo),
    GetGroupMemberList(GetGroupMemberList),
    GetStrangerInfo(GetStrangerInfo),
    SetGroupBan(SetGroupBan),
    SetGroupKick(SetGroupKick),
    SetGroupAddRequest(SetGroupAddRequest),
    GetLoginInfo,
    GetGroupList,
    GetFriendList,
    GetOnlineClients,
    GetStatus,
    GetVersionInfo,
    CanSendImage,
    CanSendRecord,
    CleanCache,
    SetRestart(SetRestart),
    HandleQuickOperation(QuickOperation),
    /// 非标准动作，由上层按名字解析。
    Extension {
        name: String,
        params: Map<String, Value>,
    },
}

impl Action {
    pub fn parse(name: &str, params: Map<String, Value>) -> Result<Self, ParseError> {
        Ok(match name {
            "send_private_msg" | "send_group_msg" | "send_msg" => {
                Self::SendMsg(SendMsg::parse(name, params)?)
            }
            "delete_msg" => Self::DeleteMsg(from_params(params)?),
            "get_msg" => Self::GetMsg(from_params(params)?),
            "get_image" => Self::GetImage(from_params(params)?),
            "get_group_info" => Self::GetGroupInfo(from_params(params)?),
            "get_group_member_info" => Self::GetGroupMemberInfo(from_params(params)?),
            "get_group_member_list" => Self::GetGroupMemberList(from_params(params)?),
            "get_stranger_info" => Self::GetStrangerInfo(from_params(params)?),
            "set_group_ban" => Self::SetGroupBan(from_params(params)?),
            "set_group_kick" => Self::SetGroupKick(from_params(params)?),
            "set_group_add_request" => Self::SetGroupAddRequest(from_params(params)?),
            "get_login_info" => Self::GetLoginInfo,
            "get_group_list" => Self::GetGroupList,
            "get_friend_list" => Self::GetFriendList,
            "get_online_clients" => Self::GetOnlineClients,
            "get_status" => Self::GetStatus,
            "get_version_info" => Self::GetVersionInfo,
            "can_send_image" => Self::CanSendImage,
            "can_send_record" => Self::CanSendRecord,
            "clean_cache" => Self::CleanCache,
            "set_restart" => Self::SetRestart(from_params(params)?),
            ".handle_quick_operation" => Self::HandleQuickOperation(from_params(params)?),
            _ if STANDARD_ACTIONS.contains(&name) => {
                return Err(ParseError::Unsupported(name.to_owned()));
            }
            _ => Self::Extension {
                name: name.to_owned(),
                params,
            },
        })
    }
}

/// 按类型解析参数，供本模块和上层的扩展动作共用。
pub fn from_params<T: DeserializeOwned>(params: Map<String, Value>) -> Result<T, ParseError> {
    serde_json::from_value(Value::Object(params))
        .map_err(|error| ParseError::BadParam(error.to_string()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Private(i64),
    Group(i64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct SendMsg {
    pub target: Target,
    pub message: Message,
    /// 标准之外的参数，例如扩展参数 `active`。
    pub extra: Map<String, Value>,
}

impl SendMsg {
    fn parse(name: &str, params: Map<String, Value>) -> Result<Self, ParseError> {
        #[derive(Deserialize)]
        struct Raw {
            #[serde(default)]
            message_type: Option<String>,
            #[serde(default, deserialize_with = "lenient::opt_int")]
            user_id: Option<i64>,
            #[serde(default, deserialize_with = "lenient::opt_int")]
            group_id: Option<i64>,
            message: Value,
            #[serde(default, deserialize_with = "lenient::boolean")]
            auto_escape: bool,
            #[serde(flatten)]
            extra: Map<String, Value>,
        }
        let raw: Raw = from_params(params)?;
        let missing = |field: &str| ParseError::BadParam(format!("missing field `{field}`"));
        let private = || {
            raw.user_id
                .map(Target::Private)
                .ok_or_else(|| missing("user_id"))
        };
        let group = || {
            raw.group_id
                .map(Target::Group)
                .ok_or_else(|| missing("group_id"))
        };
        let target = match (name, raw.message_type.as_deref()) {
            ("send_private_msg", _) | ("send_msg", Some("private")) => private()?,
            ("send_group_msg", _) | ("send_msg", Some("group")) => group()?,
            ("send_msg", None) if raw.group_id.is_some() => group()?,
            ("send_msg", None) => private()?,
            _ => {
                return Err(ParseError::BadParam(
                    "message_type must be private or group".into(),
                ));
            }
        };
        Ok(Self {
            target,
            message: Message::from_value(raw.message, raw.auto_escape)?,
            extra: raw.extra,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MessageRef {
    #[serde(deserialize_with = "lenient::int32")]
    pub message_id: i32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GetImage {
    pub file: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GetGroupInfo {
    #[serde(deserialize_with = "lenient::int")]
    pub group_id: i64,
    #[serde(default, deserialize_with = "lenient::boolean")]
    pub no_cache: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GetGroupMemberInfo {
    #[serde(deserialize_with = "lenient::int")]
    pub group_id: i64,
    #[serde(deserialize_with = "lenient::int")]
    pub user_id: i64,
    #[serde(default, deserialize_with = "lenient::boolean")]
    pub no_cache: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GetGroupMemberList {
    #[serde(deserialize_with = "lenient::int")]
    pub group_id: i64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct GetStrangerInfo {
    #[serde(deserialize_with = "lenient::int")]
    pub user_id: i64,
    #[serde(default, deserialize_with = "lenient::boolean")]
    pub no_cache: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SetGroupBan {
    #[serde(deserialize_with = "lenient::int")]
    pub group_id: i64,
    #[serde(deserialize_with = "lenient::int")]
    pub user_id: i64,
    /// 禁言秒数，0 表示解除。
    #[serde(default = "default_ban_duration", deserialize_with = "lenient::uint")]
    pub duration: u64,
}

fn default_ban_duration() -> u64 {
    1800
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SetGroupKick {
    #[serde(deserialize_with = "lenient::int")]
    pub group_id: i64,
    #[serde(deserialize_with = "lenient::int")]
    pub user_id: i64,
    #[serde(default, deserialize_with = "lenient::boolean")]
    pub reject_add_request: bool,
    #[serde(default, deserialize_with = "lenient::boolean")]
    pub add_blacklist: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SetGroupAddRequest {
    pub flag: String,
    pub sub_type: String,
    #[serde(default = "default_true", deserialize_with = "lenient::boolean")]
    pub approve: bool,
    #[serde(default)]
    pub reason: String,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SetRestart {
    /// 延迟毫秒数。
    #[serde(default, deserialize_with = "lenient::uint")]
    pub delay: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct QuickOperation {
    #[serde(deserialize_with = "lenient::object")]
    pub context: Map<String, Value>,
    #[serde(deserialize_with = "lenient::object")]
    pub operation: Map<String, Value>,
}
