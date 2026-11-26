//! SQLite 状态存储。
//!
//! 单个连接，所有操作经 `spawn_blocking` 执行。结构版本由 `PRAGMA user_version` 记录。

use rusqlite::{
    Connection, OptionalExtension, TransactionBehavior, params, params_from_iter,
    types::Value as SqlValue,
};
use sha1::{Digest, Sha1};

mod gateway;
mod schema;
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::HashMap,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// 群聊被动回复窗口。
pub const GROUP_PASSIVE_WINDOW: i64 = 300;
/// 单聊被动回复窗口。
pub const PRIVATE_PASSIVE_WINDOW: i64 = 3600;
/// 机器人发送消息的本地缓存时长。
pub const SENT_CACHE_TTL: i64 = 3600;

const GROUP_REPLY_LIMIT: i64 = 5;
const PRIVATE_REPLY_LIMIT: i64 = 4;

/// 存储错误。桥接层把它映射为 `ActionError`。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid storage configuration")]
    InvalidConfig,
    #[error("database operation failed: {0}")]
    Database(String),
    #[error("the database belongs to a different app_id")]
    AppMismatch,
    #[error("database schema version {0} is not supported; delete the database")]
    SchemaVersion(i64),
    #[error("deterministic identity id collision")]
    IdentityCollision,
    #[error("unknown target")]
    UnknownTarget,
    #[error("message is not cached")]
    MessageNotCached,
    #[error("reply context is unavailable")]
    ReplyContextUnavailable,
    #[error("reply limit exceeded")]
    ReplyLimitExceeded,
    #[error("quote reference is unavailable")]
    QuoteReferenceUnavailable,
    #[error("message is not owned by the bot")]
    NotOwned,
    #[error("message id space is exhausted")]
    IdExhausted,
    #[error("cached message content is invalid")]
    InvalidContent,
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn create_entity(
    transaction: &rusqlite::Transaction<'_>,
    app_id: &str,
    kind: &str,
    now: i64,
    explicit_id: Option<i64>,
) -> Result<i64, Error> {
    if let Some(id) = explicit_id {
        let exists: bool = transaction
            .query_row("SELECT 1 FROM entity WHERE id=?1", params![id], |_| {
                Ok(true)
            })
            .optional()
            .map_err(database)?
            .unwrap_or(false);
        if exists {
            return Err(Error::IdentityCollision);
        }
        transaction
            .execute(
                "INSERT INTO entity(id, app_id, kind, created_at, updated_at) VALUES(?1, ?2, ?3, ?4, ?4)",
                params![id, app_id, kind, now],
            )
            .map_err(database)?;
        Ok(id)
    } else {
        transaction
            .execute(
                "INSERT INTO entity(app_id, kind, created_at, updated_at) VALUES(?1, ?2, ?3, ?3)",
                params![app_id, kind, now],
            )
            .map_err(database)?;
        Ok(transaction.last_insert_rowid())
    }
}

pub(super) fn database(error: impl std::fmt::Display) -> Error {
    Error::Database(error.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityKind {
    User,
    Group,
    Member,
}

impl IdentityKind {
    fn entity_kind(self) -> &'static str {
        match self {
            Self::Group => "group",
            Self::User | Self::Member => "user",
        }
    }

    fn for_conversation(conversation: Conversation) -> Self {
        match conversation {
            Conversation::Private(_) => Self::User,
            Conversation::Group(_) => Self::Group,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum IdentityIdMode {
    Sequence,
    #[default]
    Hash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityOptions {
    pub mode: IdentityIdMode,
    pub salt: String,
    pub bits: u8,
}

impl Default for IdentityOptions {
    fn default() -> Self {
        Self {
            mode: IdentityIdMode::Hash,
            salt: "qbridge".into(),
            bits: 52,
        }
    }
}

impl IdentityOptions {
    fn deterministic_id(&self, app_id: &str, target_kind: &str, openid: &str) -> i64 {
        let mut hash = Sha1::new();
        for part in ["qbridge", self.salt.as_str(), app_id, target_kind, openid] {
            hash.update(part.as_bytes());
            hash.update([0]);
        }
        let digest = hash.finalize();
        let mut raw = [0u8; 8];
        raw.copy_from_slice(&digest[..8]);
        let value = u64::from_be_bytes(raw);
        let mask = if self.bits >= 63 {
            i64::MAX as u64
        } else {
            (1u64 << self.bits) - 1
        };
        let id = value & mask;
        if id == 0 { 1 } else { id as i64 }
    }
}

/// OB11 会话。目标 ID 是本地数字 ID。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Conversation {
    Private(i64),
    Group(i64),
}

impl Conversation {
    pub fn target(self) -> i64 {
        match self {
            Self::Private(id) | Self::Group(id) => id,
        }
    }

    fn scope(self) -> &'static str {
        match self {
            Self::Private(_) => "private",
            Self::Group(_) => "group",
        }
    }

    fn from_scope(scope: &str, target: i64) -> Option<Self> {
        match scope {
            "private" => Some(Self::Private(target)),
            "group" => Some(Self::Group(target)),
            _ => None,
        }
    }

    fn reply_limit(self) -> i64 {
        match self {
            Self::Group(_) => GROUP_REPLY_LIMIT,
            Self::Private(_) => PRIVATE_REPLY_LIMIT,
        }
    }

    pub fn passive_window(self) -> i64 {
        match self {
            Self::Group(_) => GROUP_PASSIVE_WINDOW,
            Self::Private(_) => PRIVATE_PASSIVE_WINDOW,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quote {
    /// 原生引用索引；入站消息可能没有。
    pub reference: Option<String>,
    pub inbound: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyContext {
    pub openid: String,
    pub official_id: String,
    pub sequence: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecallTarget {
    pub conversation: Conversation,
    pub official_id: String,
    pub inbound: bool,
    pub openid: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub entity_id: i64,
    pub display_name: String,
    pub card: String,
    pub avatar_url: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityRecord {
    pub entity_id: i64,
    pub target_kind: String,
    pub platform: String,
    pub kind: String,
    pub external_id: String,
    pub first_seen: i64,
    pub last_seen: i64,
}

#[derive(Debug, Default)]
struct IdentityMetrics {
    lookups: AtomicU64,
    cache_hits: AtomicU64,
    cache_misses: AtomicU64,
    batch_requests: AtomicU64,
    profile_hits: AtomicU64,
    profile_misses: AtomicU64,
    binds: AtomicU64,
    unbinds: AtomicU64,
    merges: AtomicU64,
    cleaned_messages: AtomicU64,
    cleaned_profiles: AtomicU64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IdentityStats {
    pub lookups: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub batch_requests: u64,
    pub profile_hits: u64,
    pub profile_misses: u64,
    pub binds: u64,
    pub unbinds: u64,
    pub merges: u64,
    pub cleaned_messages: u64,
    pub cleaned_profiles: u64,
}

#[derive(Clone)]
pub struct Store {
    connection: Arc<Mutex<Connection>>,
    app_id: Arc<str>,
    identity_cache: Arc<Mutex<HashMap<(String, String), i64>>>,
    identity_options: IdentityOptions,
    metrics: Arc<IdentityMetrics>,
}

impl Store {
    /// 打开数据库并检查结构版本与 app_id。默认使用确定性哈希 ID。
    pub fn open(path: &Path, app_id: &str) -> Result<Self, Error> {
        Self::open_with_identity(path, app_id, IdentityOptions::default())
    }

    /// 使用指定 ID 策略打开数据库。
    pub fn open_with_identity(
        path: &Path,
        app_id: &str,
        identity_options: IdentityOptions,
    ) -> Result<Self, Error> {
        if app_id.trim().is_empty()
            || identity_options.salt.is_empty()
            || identity_options.bits == 0
            || identity_options.bits > 63
        {
            return Err(Error::InvalidConfig);
        }
        if path != Path::new(":memory:")
            && let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(database)?;
        }
        let mut connection = Connection::open(path).map_err(database)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(database)?;
        connection
            .execute_batch(
                "PRAGMA locking_mode=EXCLUSIVE; PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;",
            )
            .map_err(database)?;
        schema::prepare(&mut connection)?;
        let stored: Option<String> = connection
            .query_row("SELECT app_id FROM meta WHERE id=1", [], |row| row.get(0))
            .optional()
            .map_err(database)?;
        match stored {
            Some(stored) if stored != app_id => return Err(Error::AppMismatch),
            Some(_) => {}
            None => {
                connection
                    .execute(
                        "INSERT INTO meta(id, app_id) VALUES(1, ?1)",
                        params![app_id],
                    )
                    .map_err(database)?;
            }
        }
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            app_id: Arc::from(app_id),
            identity_cache: Arc::new(Mutex::new(HashMap::new())),
            identity_options,
            metrics: Arc::new(IdentityMetrics::default()),
        })
    }

    async fn call<T, F>(&self, operation: F) -> Result<T, Error>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, Error> + Send + 'static,
    {
        let connection = self.connection.clone();
        tokio::task::spawn_blocking(move || {
            let mut connection = connection
                .lock()
                .map_err(|_| Error::Database("connection lock poisoned".into()))?;
            operation(&mut connection)
        })
        .await
        .map_err(database)?
    }

    /// 取得或分配实体 ID。`User` 和 `Member` 共享同一个 user entity。
    pub async fn identity(&self, kind: IdentityKind, openid: &str) -> Result<i64, Error> {
        let app_id = self.app_id.clone();
        let target_kind = kind.entity_kind().to_owned();
        let scene = match kind {
            IdentityKind::User => "private",
            IdentityKind::Member => "member",
            IdentityKind::Group => "group",
        };
        let openid = openid.to_owned();
        self.metrics.lookups.fetch_add(1, Ordering::Relaxed);
        let cache_key = (app_id.to_string(), format!("{target_kind}:{openid}"));
        if let Some(id) = self
            .identity_cache
            .lock()
            .ok()
            .and_then(|cache| cache.get(&cache_key).copied())
        {
            self.metrics.cache_hits.fetch_add(1, Ordering::Relaxed);
            return Ok(id);
        }
        self.metrics.cache_misses.fetch_add(1, Ordering::Relaxed);
        let key_app = app_id.clone();
        let key_openid = openid.clone();
        let key_kind = target_kind.clone();
        let identity_options = self.identity_options.clone();
        let scene = scene.to_owned();
        let id = self
            .call(move |connection| {
                let now = unix_now();
                let transaction = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(database)?;
                let existing: Option<(i64, i64)> = transaction
                    .query_row(
                        "SELECT e.id, i.id FROM identity i JOIN entity e ON e.id=i.entity_id \
                         WHERE i.app_id=?1 AND i.platform='QQ' AND i.kind='openid' \
                         AND i.external_id=?2 AND i.target_kind=?3",
                        params![key_app.as_ref(), key_openid, key_kind],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()
                    .map_err(database)?;
                if let Some((id, identity_id)) = existing {
                    transaction
                        .execute(
                            "UPDATE identity SET last_seen=?1 WHERE app_id=?2 \
                             AND platform='QQ' AND kind='openid' AND external_id=?3 AND target_kind=?4",
                            params![now, key_app.as_ref(), key_openid, key_kind],
                        )
                        .map_err(database)?;
                    transaction
                        .execute(
                            "INSERT OR REPLACE INTO context_seen(app_id, entity_id, scene_type, scene_id, identity_id, updated_at) \
                             VALUES(?1, ?2, ?3, ?2, ?4, ?5)",
                            params![key_app.as_ref(), id, scene, identity_id, now],
                        )
                        .map_err(database)?;
                    transaction.commit().map_err(database)?;
                    return Ok(id);
                }
                let explicit_id = (identity_options.mode == IdentityIdMode::Hash).then(|| {
                    identity_options.deterministic_id(key_app.as_ref(), &key_kind, &key_openid)
                });
                let id = create_entity(&transaction, key_app.as_ref(), &key_kind, now, explicit_id)?;
                transaction
                    .execute(
                        "INSERT INTO identity(app_id, entity_id, target_kind, platform, kind, external_id, first_seen, last_seen) \
                         VALUES(?1, ?2, ?3, 'QQ', 'openid', ?4, ?5, ?5)",
                        params![key_app.as_ref(), id, key_kind, key_openid, now],
                    )
                    .map_err(database)?;
                let identity_id = transaction.last_insert_rowid();
                transaction
                    .execute(
                        "INSERT OR REPLACE INTO context_seen(app_id, entity_id, scene_type, scene_id, identity_id, updated_at) \
                         VALUES(?1, ?2, ?3, ?2, ?4, ?5)",
                        params![key_app.as_ref(), id, scene, identity_id, now],
                    )
                    .map_err(database)?;
                transaction.commit().map_err(database)?;
                Ok(id)
            })
            .await?;
        if let Ok(mut cache) = self.identity_cache.lock() {
            if cache.len() >= 100_000 {
                cache.clear();
            }
            cache.insert(cache_key, id);
        }
        Ok(id)
    }

    /// 列出某类实体的全部映射，按实体 ID 升序。
    pub async fn identities(&self, kind: IdentityKind) -> Result<Vec<(i64, String)>, Error> {
        let app_id = self.app_id.to_string();
        let target_kind = kind.entity_kind().to_owned();
        let scene = match kind {
            IdentityKind::User => "private",
            IdentityKind::Member => "member",
            IdentityKind::Group => "group",
        }
        .to_owned();
        self.call(move |connection| {
            let mut statement = connection
                .prepare(
                    "SELECT e.id, i.external_id FROM entity e \
                     JOIN identity i ON i.entity_id=e.id \
                     JOIN context_seen c ON c.app_id=e.app_id AND c.entity_id=e.id AND c.scene_type=?3 \
                     WHERE e.app_id=?1 AND e.kind=?2 AND i.target_kind=?2 \
                     GROUP BY e.id ORDER BY e.id",
                )
                .map_err(database)?;
            let rows = statement
                .query_map(params![app_id, target_kind, scene], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .map_err(database)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(database)
        })
        .await
    }

    /// 查找实体 ID 对应的 OpenID。`User` 和 `Member` 都按 user 实体查。
    pub async fn openid(&self, kind: IdentityKind, id: i64) -> Result<Option<String>, Error> {
        let app_id = self.app_id.to_string();
        let target_kind = kind.entity_kind().to_owned();
        self.call(move |connection| {
            connection
                .query_row(
                    "SELECT i.external_id FROM identity i JOIN entity e ON e.id=i.entity_id \
                     WHERE e.id=?1 AND e.app_id=?2 AND i.target_kind=?3 \
                     ORDER BY i.last_seen DESC, i.id ASC LIMIT 1",
                    params![id, app_id, target_kind],
                    |row| row.get(0),
                )
                .optional()
                .map_err(database)
        })
        .await
    }

    /// 批量解析实体 ID，缺失的身份在同一个事务里创建。
    pub async fn identity_batch(
        &self,
        kind: IdentityKind,
        openids: &[String],
    ) -> Result<HashMap<String, i64>, Error> {
        let app_id = self.app_id.to_string();
        let target_kind = kind.entity_kind().to_owned();
        let scene = match kind {
            IdentityKind::User => "private",
            IdentityKind::Member => "member",
            IdentityKind::Group => "group",
        }
        .to_owned();
        self.metrics.batch_requests.fetch_add(1, Ordering::Relaxed);
        self.metrics
            .lookups
            .fetch_add(openids.len() as u64, Ordering::Relaxed);
        let values = openids.to_vec();
        let cache_app = app_id.clone();
        let cache_kind = target_kind.clone();
        let identity_options = self.identity_options.clone();
        let result = self
            .call(move |connection| {
                let now = unix_now();
                let transaction = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(database)?;
                let mut output = HashMap::with_capacity(values.len());
                for openid in values {
                    let existing: Option<(i64, i64)> = transaction
                        .query_row(
                            "SELECT e.id, i.id FROM identity i JOIN entity e ON e.id=i.entity_id \
                             WHERE i.app_id=?1 AND i.platform='QQ' AND i.kind='openid' \
                             AND i.external_id=?2 AND i.target_kind=?3",
                            params![app_id, openid, target_kind],
                            |row| Ok((row.get(0)?, row.get(1)?)),
                        )
                        .optional()
                        .map_err(database)?;
                    let (entity_id, identity_id) = if let Some(found) = existing {
                        transaction
                            .execute(
                                "UPDATE identity SET last_seen=?1 WHERE id=?2",
                                params![now, found.1],
                            )
                            .map_err(database)?;
                        found
                    } else {
                        let explicit_id = (identity_options.mode == IdentityIdMode::Hash).then(|| {
                            identity_options.deterministic_id(&app_id, &target_kind, &openid)
                        });
                        let entity_id = create_entity(
                            &transaction,
                            &app_id,
                            &target_kind,
                            now,
                            explicit_id,
                        )?;
                        transaction
                            .execute(
                                "INSERT INTO identity(app_id, entity_id, target_kind, platform, kind, external_id, first_seen, last_seen) \
                                 VALUES(?1, ?2, ?3, 'QQ', 'openid', ?4, ?5, ?5)",
                                params![app_id, entity_id, target_kind, openid, now],
                            )
                            .map_err(database)?;
                        (entity_id, transaction.last_insert_rowid())
                    };
                    transaction
                        .execute(
                            "INSERT OR REPLACE INTO context_seen(app_id, entity_id, scene_type, scene_id, identity_id, updated_at) \
                             VALUES(?1, ?2, ?3, ?2, ?4, ?5)",
                            params![app_id, entity_id, scene, identity_id, now],
                        )
                        .map_err(database)?;
                    output.insert(openid, entity_id);
                }
                transaction.commit().map_err(database)?;
                Ok(output)
            })
            .await?;
        if let Ok(mut cache) = self.identity_cache.lock() {
            if cache.len() + result.len() >= 100_000 {
                cache.clear();
            }
            for (openid, id) in &result {
                cache.insert((cache_app.clone(), format!("{cache_kind}:{openid}")), *id);
            }
        }
        Ok(result)
    }

    /// 把外部身份绑定到指定实体；已存在则移动到目标实体。
    pub async fn bind_identity(
        &self,
        kind: IdentityKind,
        openid: &str,
        entity_id: i64,
    ) -> Result<(), Error> {
        let app_id = self.app_id.to_string();
        let target_kind = kind.entity_kind().to_owned();
        let scene = match kind {
            IdentityKind::User => "private",
            IdentityKind::Member => "member",
            IdentityKind::Group => "group",
        }
        .to_owned();
        let openid = openid.to_owned();
        self.call(move |connection| {
            let now = unix_now();
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(database)?;
            let exists: bool = transaction
                .query_row(
                    "SELECT 1 FROM entity WHERE id=?1 AND app_id=?2 AND kind=?3",
                    params![entity_id, app_id, target_kind],
                    |_| Ok(true),
                )
                .optional()
                .map_err(database)?
                .unwrap_or(false);
            if !exists {
                return Err(Error::UnknownTarget);
            }
            transaction
                .execute(
                    "INSERT INTO identity(app_id, entity_id, target_kind, platform, kind, external_id, first_seen, last_seen) \
                     VALUES(?1, ?2, ?3, 'QQ', 'openid', ?4, ?5, ?5) \
                     ON CONFLICT(app_id, platform, kind, external_id, target_kind) \
                     DO UPDATE SET entity_id=excluded.entity_id, last_seen=excluded.last_seen",
                    params![app_id, entity_id, target_kind, openid, now],
                )
                .map_err(database)?;
            let identity_id: i64 = transaction
                .query_row(
                    "SELECT id FROM identity WHERE app_id=?1 AND platform='QQ' AND kind='openid' \
                     AND external_id=?2 AND target_kind=?3",
                    params![app_id, openid, target_kind],
                    |row| row.get(0),
                )
                .map_err(database)?;
            transaction
                .execute(
                    "INSERT OR REPLACE INTO context_seen(app_id, entity_id, scene_type, scene_id, identity_id, updated_at) \
                     VALUES(?1, ?2, ?3, ?2, ?4, ?5)",
                    params![app_id, entity_id, scene, identity_id, now],
                )
                .map_err(database)?;
            transaction.commit().map_err(database)
        })
        .await?;
        if let Ok(mut cache) = self.identity_cache.lock() {
            cache.clear();
        }
        self.metrics.binds.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// 解除一个外部身份的映射；实体没有其他身份时一并删除。
    pub async fn unbind_identity(&self, kind: IdentityKind, openid: &str) -> Result<bool, Error> {
        let app_id = self.app_id.to_string();
        let target_kind = kind.entity_kind().to_owned();
        let openid = openid.to_owned();
        let removed = self
            .call(move |connection| {
                let transaction = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(database)?;
                let identity: Option<(i64, i64)> = transaction
                    .query_row(
                        "SELECT id, entity_id FROM identity WHERE app_id=?1 AND platform='QQ' \
                         AND kind='openid' AND external_id=?2 AND target_kind=?3",
                        params![app_id, openid, target_kind],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()
                    .map_err(database)?;
                let Some((identity_id, entity_id)) = identity else {
                    return Ok(false);
                };
                transaction
                    .execute(
                        "DELETE FROM context_seen WHERE identity_id=?1",
                        params![identity_id],
                    )
                    .map_err(database)?;
                transaction
                    .execute("DELETE FROM identity WHERE id=?1", params![identity_id])
                    .map_err(database)?;
                let remaining: i64 = transaction
                    .query_row(
                        "SELECT COUNT(*) FROM identity WHERE entity_id=?1",
                        params![entity_id],
                        |row| row.get(0),
                    )
                    .map_err(database)?;
                if remaining == 0 {
                    transaction
                        .execute("DELETE FROM entity WHERE id=?1", params![entity_id])
                        .map_err(database)?;
                }
                transaction.commit().map_err(database)?;
                Ok(true)
            })
            .await?;
        if removed && let Ok(mut cache) = self.identity_cache.lock() {
            cache.clear();
        }
        Ok(removed)
    }

    /// 导出全部身份映射，用于调试和运维。
    pub async fn export_identities(&self) -> Result<Vec<IdentityRecord>, Error> {
        let app_id = self.app_id.to_string();
        self.call(move |connection| {
            let mut statement = connection
                .prepare(
                    "SELECT entity_id, target_kind, platform, kind, external_id, first_seen, last_seen \
                     FROM identity WHERE app_id=?1 ORDER BY entity_id, id",
                )
                .map_err(database)?;
            let rows = statement
                .query_map(params![app_id], |row| {
                    Ok(IdentityRecord {
                        entity_id: row.get(0)?,
                        target_kind: row.get(1)?,
                        platform: row.get(2)?,
                        kind: row.get(3)?,
                        external_id: row.get(4)?,
                        first_seen: row.get(5)?,
                        last_seen: row.get(6)?,
                    })
                })
                .map_err(database)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(database)
        })
        .await
    }

    pub fn identity_stats(&self) -> IdentityStats {
        IdentityStats {
            lookups: self.metrics.lookups.load(Ordering::Relaxed),
            cache_hits: self.metrics.cache_hits.load(Ordering::Relaxed),
            cache_misses: self.metrics.cache_misses.load(Ordering::Relaxed),
            batch_requests: self.metrics.batch_requests.load(Ordering::Relaxed),
            profile_hits: self.metrics.profile_hits.load(Ordering::Relaxed),
            profile_misses: self.metrics.profile_misses.load(Ordering::Relaxed),
            binds: self.metrics.binds.load(Ordering::Relaxed),
            unbinds: self.metrics.unbinds.load(Ordering::Relaxed),
            merges: self.metrics.merges.load(Ordering::Relaxed),
            cleaned_messages: self.metrics.cleaned_messages.load(Ordering::Relaxed),
            cleaned_profiles: self.metrics.cleaned_profiles.load(Ordering::Relaxed),
        }
    }

    pub async fn profiles_for_entities(
        &self,
        entity_ids: &[i64],
        now: i64,
    ) -> Result<HashMap<i64, Profile>, Error> {
        if entity_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let app_id = self.app_id.to_string();
        let ids = entity_ids.to_vec();
        let placeholders = (0..ids.len())
            .map(|index| format!("?{}", index + 3))
            .collect::<Vec<_>>()
            .join(",");
        let profiles = self
            .call(move |connection| {
                let sql = format!(
                    "SELECT entity_id, display_name, card, avatar_url, updated_at FROM profile_cache \
                     WHERE app_id=?1 AND expires_at>?2 AND entity_id IN ({placeholders})"
                );
                let mut values = vec![SqlValue::Text(app_id), SqlValue::Integer(now)];
                values.extend(ids.into_iter().map(SqlValue::Integer));
                let mut statement = connection.prepare(&sql).map_err(database)?;
                let rows = statement
                    .query_map(params_from_iter(values), |row| {
                        Ok(Profile {
                            entity_id: row.get(0)?,
                            display_name: row.get(1)?,
                            card: row.get(2)?,
                            avatar_url: row.get(3)?,
                            updated_at: row.get(4)?,
                        })
                    })
                    .map_err(database)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(database)
            })
            .await?;
        let found = profiles.len() as u64;
        self.metrics
            .profile_hits
            .fetch_add(found, Ordering::Relaxed);
        self.metrics.profile_misses.fetch_add(
            (entity_ids.len() as u64).saturating_sub(found),
            Ordering::Relaxed,
        );
        Ok(profiles
            .into_iter()
            .map(|profile| (profile.entity_id, profile))
            .collect())
    }

    pub async fn save_profiles(
        &self,
        profiles: &[(i64, String, String, String)],
        ttl: i64,
        now: i64,
    ) -> Result<(), Error> {
        if profiles.is_empty() {
            return Ok(());
        }
        let app_id = self.app_id.to_string();
        let profiles = profiles.to_vec();
        self.call(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(database)?;
            for (entity_id, display_name, card, avatar_url) in profiles {
                transaction
                    .execute(
                        "INSERT INTO profile_cache(app_id, entity_id, display_name, card, avatar_url, updated_at, expires_at) \
                         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                         ON CONFLICT(app_id, entity_id) DO UPDATE SET display_name=excluded.display_name, \
                         card=excluded.card, avatar_url=excluded.avatar_url, updated_at=excluded.updated_at, \
                         expires_at=excluded.expires_at",
                        params![app_id, entity_id, display_name, card, avatar_url, now, now + ttl],
                    )
                    .map_err(database)?;
            }
            transaction.commit().map_err(database)
        })
        .await
    }

    pub async fn save_profile(
        &self,
        entity_id: i64,
        display_name: &str,
        card: &str,
        avatar_url: &str,
        ttl: i64,
        now: i64,
    ) -> Result<(), Error> {
        let app_id = self.app_id.to_string();
        let display_name = display_name.to_owned();
        let card = card.to_owned();
        let avatar_url = avatar_url.to_owned();
        self.call(move |connection| {
            connection
                .execute(
                    "INSERT INTO profile_cache(app_id, entity_id, display_name, card, avatar_url, updated_at, expires_at) \
                     VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                     ON CONFLICT(app_id, entity_id) DO UPDATE SET display_name=excluded.display_name, \
                     card=excluded.card, avatar_url=excluded.avatar_url, updated_at=excluded.updated_at, \
                     expires_at=excluded.expires_at",
                    params![app_id, entity_id, display_name, card, avatar_url, now, now + ttl],
                )
                .map_err(database)?;
            Ok(())
        })
        .await
    }

    pub async fn profile(&self, entity_id: i64, now: i64) -> Result<Option<Profile>, Error> {
        let app_id = self.app_id.to_string();
        let profile = self
            .call(move |connection| {
                connection
                    .query_row(
                        "SELECT entity_id, display_name, card, avatar_url, updated_at FROM profile_cache \
                         WHERE app_id=?1 AND entity_id=?2 AND expires_at>?3",
                        params![app_id, entity_id, now],
                        |row| {
                            Ok(Profile {
                                entity_id: row.get(0)?,
                                display_name: row.get(1)?,
                                card: row.get(2)?,
                                avatar_url: row.get(3)?,
                                updated_at: row.get(4)?,
                            })
                        },
                    )
                    .optional()
                    .map_err(database)
            })
            .await?;
        if profile.is_some() {
            self.metrics.profile_hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.metrics.profile_misses.fetch_add(1, Ordering::Relaxed);
        }
        Ok(profile)
    }

    /// 合并两个实体。
    pub async fn merge_entities(&self, old_id: i64, new_id: i64) -> Result<(), Error> {
        if old_id == new_id {
            return Ok(());
        }
        let app_id = self.app_id.to_string();
        self.call(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(database)?;
            let old_exists: bool = transaction
                .query_row(
                    "SELECT 1 FROM entity WHERE id=?1 AND app_id=?2",
                    params![old_id, app_id],
                    |_| Ok(true),
                )
                .optional()
                .map_err(database)?
                .unwrap_or(false);
            let new_exists: bool = transaction
                .query_row(
                    "SELECT 1 FROM entity WHERE id=?1 AND app_id=?2",
                    params![new_id, app_id],
                    |_| Ok(true),
                )
                .optional()
                .map_err(database)?
                .unwrap_or(false);
            if !old_exists || !new_exists {
                return Err(Error::UnknownTarget);
            }
            transaction
                .execute(
                    "UPDATE identity SET entity_id=?1 WHERE entity_id=?2",
                    params![new_id, old_id],
                )
                .map_err(database)?;
            transaction
                .execute(
                    "UPDATE context_seen SET entity_id=?1 WHERE entity_id=?2",
                    params![new_id, old_id],
                )
                .map_err(database)?;
            transaction
                .execute("DELETE FROM entity WHERE id=?1", params![old_id])
                .map_err(database)?;
            transaction.commit().map_err(database)
        })
        .await?;
        if let Ok(mut cache) = self.identity_cache.lock() {
            cache.clear();
        }
        self.metrics.merges.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// 缓存一条入站消息。重复的官方 ID 返回 `None`。
    pub async fn save_inbound(
        &self,
        conversation: Conversation,
        official_id: &str,
        created: i64,
        expires: i64,
        content: &Value,
        reference: Option<&str>,
    ) -> Result<Option<i32>, Error> {
        let official_id = official_id.to_owned();
        let reference = reference.map(str::to_owned);
        let content = content.clone();
        self.call(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(database)?;
            schema::ensure_id_available(&transaction)?;
            let inserted = transaction
                .execute(
                    "INSERT OR IGNORE INTO message(scope,target,official_id,inbound,created,expires,content,reference) \
                     VALUES(?1,?2,?3,1,?4,?5,'{}',?6)",
                    params![
                        conversation.scope(),
                        conversation.target(),
                        official_id,
                        created,
                        expires,
                        reference
                    ],
                )
                .map_err(database)?;
            if inserted == 0 {
                return Ok(None);
            }
            let id = schema::store_content(&transaction, &content)?;
            transaction.commit().map_err(database)?;
            Ok(Some(id))
        })
        .await
    }

    /// 缓存一条机器人发送的消息，返回本地 `message_id`。
    pub async fn save_outbound(
        &self,
        conversation: Conversation,
        official_id: &str,
        created: i64,
        expires: i64,
        content: &Value,
        reference: Option<&str>,
    ) -> Result<i32, Error> {
        let official_id = official_id.to_owned();
        let reference = reference.map(str::to_owned);
        let content = content.clone();
        self.call(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(database)?;
            schema::ensure_id_available(&transaction)?;
            transaction
                .execute(
                    "INSERT INTO message(scope,target,official_id,inbound,created,expires,content,reference) \
                     VALUES(?1,?2,?3,0,?4,?5,'{}',?6)",
                    params![
                        conversation.scope(),
                        conversation.target(),
                        official_id,
                        created,
                        expires,
                        reference
                    ],
                )
                .map_err(database)?;
            let id = schema::store_content(&transaction, &content)?;
            transaction.commit().map_err(database)?;
            Ok(id)
        })
        .await
    }

    /// 更新机器人发出消息的本地缓存内容、过期时间与可选引用索引。
    pub async fn update_outbound(
        &self,
        id: i32,
        expires: i64,
        content: &Value,
        reference: Option<&str>,
    ) -> Result<(), Error> {
        let reference = reference
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        let mut value = content.clone();
        let Value::Object(object) = &mut value else {
            return Err(Error::InvalidContent);
        };
        object.insert("message_id".into(), Value::from(id));
        let serialized = serde_json::to_string(&value).map_err(database)?;
        self.call(move |connection| {
            let updated = connection
                .execute(
                    "UPDATE message SET content=?1, expires=?2, reference=COALESCE(?3, reference) \
                     WHERE id=?4 AND inbound=0",
                    params![serialized, expires, reference, id],
                )
                .map_err(database)?;
            if updated == 0 {
                return Err(Error::MessageNotCached);
            }
            Ok(())
        })
        .await
    }

    /// 查询缓存的消息事件 JSON。
    pub async fn message(&self, id: i32, now: i64) -> Result<Option<Value>, Error> {
        self.call(move |connection| {
            let content: Option<String> = connection
                .query_row(
                    "SELECT content FROM message WHERE id=?1 AND expires>?2",
                    params![id, now],
                    |row| row.get(0),
                )
                .optional()
                .map_err(database)?;
            content
                .map(|content| serde_json::from_str(&content).map_err(|_| Error::InvalidContent))
                .transpose()
        })
        .await
    }

    /// 查询消息的引用信息。机器人消息缺少原生引用索引时报错。
    pub async fn quote(
        &self,
        conversation: Conversation,
        id: i32,
        now: i64,
    ) -> Result<Quote, Error> {
        self.call(move |connection| {
            let row: Option<(Option<String>, bool)> = connection
                .query_row(
                    "SELECT reference, inbound FROM message \
                     WHERE id=?1 AND scope=?2 AND target=?3 AND expires>?4",
                    params![id, conversation.scope(), conversation.target(), now],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(database)?;
            let (reference, inbound) = row.ok_or(Error::ReplyContextUnavailable)?;
            let reference = reference.filter(|reference| !reference.is_empty());
            if !inbound && reference.is_none() {
                return Err(Error::QuoteReferenceUnavailable);
            }
            Ok(Quote { reference, inbound })
        })
        .await
    }

    /// 原子预留一次被动回复上下文。
    pub async fn reserve_reply(
        &self,
        conversation: Conversation,
        reply: Option<i32>,
        now: i64,
    ) -> Result<ReplyContext, Error> {
        let app_id = self.app_id.to_string();
        self.call(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(database)?;
            let target_kind = IdentityKind::for_conversation(conversation).entity_kind();
            let target = conversation.target();
            let openid: Option<String> = transaction
                .query_row(
                    "SELECT i.external_id FROM identity i JOIN entity e ON e.id=i.entity_id \
                     WHERE e.id=?1 AND e.app_id=?2 AND e.kind=?3 AND i.target_kind=?3 \
                     ORDER BY i.last_seen DESC, i.id ASC LIMIT 1",
                    params![target, app_id, target_kind],
                    |row| row.get(0),
                )
                .optional()
                .map_err(database)?;
            let openid = openid.ok_or(Error::UnknownTarget)?;
            let passive_cutoff = now.saturating_sub(conversation.passive_window());
            let selected: Option<(i32, String, i64)> = match reply {
                Some(id) => transaction
                    .query_row(
                        "SELECT id, official_id, next_seq FROM message \
                         WHERE id=?1 AND scope=?2 AND target=?3 AND inbound=1 \
                         AND expires>?4 AND created>?5 AND created<=?4+30",
                        params![
                            id,
                            conversation.scope(),
                            conversation.target(),
                            now,
                            passive_cutoff
                        ],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()
                    .map_err(database)?,
                None => transaction
                    .query_row(
                        "SELECT id, official_id, next_seq FROM message \
                         WHERE scope=?1 AND target=?2 AND inbound=1 \
                         AND expires>?3 AND created>?4 AND created<=?3+30 \
                         ORDER BY created DESC, id DESC LIMIT 1",
                        params![
                            conversation.scope(),
                            conversation.target(),
                            now,
                            passive_cutoff
                        ],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()
                    .map_err(database)?,
            };
            let (id, official_id, sequence) = selected.ok_or(Error::ReplyContextUnavailable)?;
            if sequence > conversation.reply_limit() {
                return Err(Error::ReplyLimitExceeded);
            }
            transaction
                .execute(
                    "UPDATE message SET next_seq=next_seq+1 WHERE id=?1",
                    params![id],
                )
                .map_err(database)?;
            transaction.commit().map_err(database)?;
            Ok(ReplyContext {
                openid,
                official_id,
                sequence: u32::try_from(sequence).unwrap_or(u32::MAX),
            })
        })
        .await
    }

    pub async fn recall_target(&self, id: i32) -> Result<RecallTarget, Error> {
        let app_id = self.app_id.to_string();
        self.call(move |connection| {
            let row: Option<(String, i64, String, bool, String)> = connection
                .query_row(
                    "SELECT m.scope, m.target, m.official_id, m.inbound, i.external_id \
                     FROM message m \
                     JOIN entity e ON e.id=m.target AND e.app_id=?2 \
                       AND e.kind=CASE m.scope WHEN 'private' THEN 'user' ELSE 'group' END \
                     JOIN identity i ON i.entity_id=e.id AND i.target_kind=e.kind \
                     WHERE m.id=?1 \
                     ORDER BY i.last_seen DESC, i.id ASC LIMIT 1",
                    params![id, app_id],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    },
                )
                .optional()
                .map_err(database)?;
            let (scope, target, official_id, inbound, openid) =
                row.ok_or(Error::MessageNotCached)?;
            let conversation =
                Conversation::from_scope(&scope, target).ok_or(Error::MessageNotCached)?;
            Ok(RecallTarget {
                conversation,
                official_id,
                inbound,
                openid,
            })
        })
        .await
    }

    /// 删除机器人自己发出的缓存消息。
    pub async fn remove_outbound(&self, id: i32) -> Result<(), Error> {
        self.call(move |connection| {
            let inbound: Option<bool> = connection
                .query_row(
                    "SELECT inbound FROM message WHERE id=?1",
                    params![id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(database)?;
            match inbound {
                None => Err(Error::MessageNotCached),
                Some(true) => Err(Error::NotOwned),
                Some(false) => {
                    connection
                        .execute("DELETE FROM message WHERE id=?1", params![id])
                        .map_err(database)?;
                    Ok(())
                }
            }
        })
        .await
    }

    /// 撤回成功后删除消息缓存，入站和机器人消息都适用。
    pub async fn remove_recalled(&self, id: i32) -> Result<(), Error> {
        self.call(move |connection| {
            let removed = connection
                .execute("DELETE FROM message WHERE id=?1", params![id])
                .map_err(database)?;
            if removed == 0 {
                return Err(Error::MessageNotCached);
            }
            Ok(())
        })
        .await
    }

    /// 在入站消息缓存中查找图片 URL。
    pub async fn image_url(&self, file: &str, now: i64) -> Result<Option<String>, Error> {
        let file = file.to_owned();
        self.call(move |connection| {
            let mut statement = connection
                .prepare(
                    "SELECT content FROM message WHERE inbound=1 AND expires>?1 \
                     ORDER BY id DESC LIMIT 1000",
                )
                .map_err(database)?;
            let rows = statement
                .query_map(params![now], |row| row.get::<_, String>(0))
                .map_err(database)?;
            for row in rows {
                let content = row.map_err(database)?;
                let value: Value =
                    serde_json::from_str(&content).map_err(|_| Error::InvalidContent)?;
                if let Some(url) = schema::image_in(&value, &file) {
                    return Ok(Some(url));
                }
            }
            Ok(None)
        })
        .await
    }

    /// 删除过期消息，返回删除条数。
    pub async fn clean_expired(&self, now: i64) -> Result<usize, Error> {
        self.call(move |connection| {
            let messages = connection
                .execute("DELETE FROM message WHERE expires<=?1", params![now])
                .map_err(database)?;
            let profiles = connection
                .execute(
                    "DELETE FROM profile_cache WHERE expires_at<=?1",
                    params![now],
                )
                .map_err(database)?;
            Ok((messages, profiles))
        })
        .await
        .map(|(messages, profiles)| {
            self.metrics
                .cleaned_messages
                .fetch_add(messages as u64, Ordering::Relaxed);
            self.metrics
                .cleaned_profiles
                .fetch_add(profiles as u64, Ordering::Relaxed);
            messages
        })
    }
}
