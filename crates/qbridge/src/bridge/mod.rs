//! OB11 与 QQ 之间的转换与动作执行。

pub mod events;
pub mod extension;
pub mod forward;
pub mod group;
pub(crate) mod notices;
pub mod plan;
pub mod query;
pub mod send;

use crate::{
    error::{ActionError, UnsupportedKind},
    store::Store,
};
use qbridge_ob11::{Action, Call, Event, action::QuickOperation, quick_operation};
use qbridge_qq::{Client, Envelope};
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{Mutex, RwLock};

pub struct BotState {
    online: Arc<AtomicBool>,
    nickname: RwLock<String>,
}

impl BotState {
    pub fn new(online: Arc<AtomicBool>) -> Self {
        Self {
            online,
            nickname: RwLock::new("QQ Official Bot".into()),
        }
    }

    pub fn online(&self) -> bool {
        self.online.load(Ordering::Acquire)
    }

    pub fn online_handle(&self) -> Arc<AtomicBool> {
        self.online.clone()
    }

    pub async fn nickname(&self) -> String {
        self.nickname.read().await.clone()
    }

    pub async fn set_nickname(&self, nickname: impl Into<String>) {
        *self.nickname.write().await = nickname.into();
    }
}

pub struct Bridge {
    pub(crate) qq: Arc<Client>,
    pub(crate) store: Store,
    pub(crate) self_id: i64,
    pub(crate) state: BotState,
    pub(crate) streams: Mutex<HashMap<i64, StreamProgress>>,
}

#[derive(Clone)]
pub(crate) struct StreamProgress {
    pub id: String,
    pub index: u32,
    pub msg_id: Option<String>,
    pub msg_seq: Option<u32>,
    pub text: String,
    pub local_id: i32,
}

impl Bridge {
    pub fn new(qq: Arc<Client>, store: Store, self_id: i64, online: Arc<AtomicBool>) -> Self {
        Self {
            qq,
            store,
            self_id,
            state: BotState::new(online),
            streams: Mutex::new(HashMap::new()),
        }
    }

    pub fn self_id(&self) -> i64 {
        self.self_id
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn state(&self) -> &BotState {
        &self.state
    }

    pub async fn execute(&self, action: Action) -> Result<Value, ActionError> {
        match action {
            Action::HandleQuickOperation(operation) => self.quick_operation(operation).await,
            other => self.execute_action(other).await,
        }
    }

    async fn execute_action(&self, action: Action) -> Result<Value, ActionError> {
        let now = unix_now();
        match action {
            Action::SendMsg(request) => send::send(self, request, now).await,
            Action::DeleteMsg(request) => query::delete_msg(self, request).await,
            Action::GetMsg(request) => query::get_msg(self, request, now).await,
            Action::GetImage(request) => query::get_image(self, &request.file, now).await,
            Action::GetGroupInfo(request) => group::info(self, request).await,
            Action::GetGroupMemberInfo(request) => group::member(self, request).await,
            Action::GetGroupMemberList(request) => group::members(self, request).await,
            Action::SetGroupBan(request) => group::ban(self, request, now).await,
            Action::SetGroupKick(request) => group::kick(self, request).await,
            Action::SetGroupAddRequest(request) => group::add_request(self, request).await,
            Action::GetLoginInfo => query::login_info(self).await,
            Action::GetGroupList => {
                extension::execute(self, "get_group_list", Map::new(), now).await
            }
            Action::GetFriendList => {
                extension::execute(self, "get_friend_list", Map::new(), now).await
            }
            Action::GetOnlineClients => {
                extension::execute(self, "get_online_clients", Map::new(), now).await
            }
            Action::GetStatus => query::status(self),
            Action::GetVersionInfo => query::version(),
            Action::CanSendImage | Action::CanSendRecord => Ok(json!({"yes": true})),
            Action::CleanCache => query::clean_cache(self, now).await,
            Action::Extension { name, params } => {
                extension::execute(self, &name, params, now).await
            }
            Action::SetRestart(_) | Action::HandleQuickOperation(_) => {
                Err(ActionError::Unsupported(UnsupportedKind::Action))
            }
        }
    }

    async fn quick_operation(&self, operation: QuickOperation) -> Result<Value, ActionError> {
        let expanded =
            quick_operation::expand(self.self_id, &operation.context, &operation.operation)?;
        let mut calls = Vec::with_capacity(expanded.len());
        for (name, params) in expanded {
            calls.push(Call::parse(name, params)?);
        }
        for call in calls {
            self.execute_action(call.action).await?;
        }
        Ok(Value::Null)
    }

    pub async fn translate(&self, envelope: Envelope) -> Option<Event> {
        events::translate(self, envelope).await
    }
}

pub(crate) fn unix_now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
