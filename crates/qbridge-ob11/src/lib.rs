//! OneBot v11 协议层：动作、响应、事件和消息的类型定义，以及 CQ 码编解码。
//!
//! 本 crate 只做数据转换，不做 IO，也不包含任何平台专有内容。
#![forbid(unsafe_code)]

pub mod action;
pub mod event;
pub mod lenient;
pub mod message;
pub mod quick_operation;
pub mod response;

pub use action::{Action, Call, Mode, ParseError, STANDARD_ACTIONS, Target};
pub use event::{Event, EventKind, MessageFormat};
pub use message::{AtTarget, Media, Message, MessageError, Node, Segment};
pub use response::{Response, Status};
