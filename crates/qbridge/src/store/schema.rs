//! 数据库结构与内容写入辅助。

use super::Error;
use rusqlite::{Connection, TransactionBehavior, params};
use serde_json::Value;

const SCHEMA_VERSION: i64 = 20261126;

fn database(error: impl std::fmt::Display) -> Error {
    Error::Database(error.to_string())
}

pub(super) fn prepare(connection: &mut Connection) -> Result<(), Error> {
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(database)?;
    if version == 0 {
        initialize(connection)?;
    } else if version != SCHEMA_VERSION {
        return Err(Error::SchemaVersion(version));
    }
    Ok(())
}

fn initialize(connection: &mut Connection) -> Result<(), Error> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database)?;
    transaction
        .execute_batch(
            "CREATE TABLE meta (
                id INTEGER PRIMARY KEY CHECK(id=1),
                app_id TEXT NOT NULL
             );
             CREATE TABLE entity (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                app_id TEXT NOT NULL,
                kind TEXT NOT NULL CHECK(kind IN ('user','group')),
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
             );
             CREATE INDEX entity_app_kind ON entity(app_id, kind);
             CREATE TABLE identity (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                app_id TEXT NOT NULL,
                entity_id INTEGER NOT NULL REFERENCES entity(id) ON DELETE CASCADE,
                target_kind TEXT NOT NULL CHECK(target_kind IN ('user','group')),
                platform TEXT NOT NULL,
                kind TEXT NOT NULL,
                external_id TEXT NOT NULL,
                first_seen INTEGER NOT NULL,
                last_seen INTEGER NOT NULL,
                UNIQUE(app_id, platform, kind, external_id, target_kind)
             );
             CREATE INDEX identity_entity ON identity(entity_id);
             CREATE INDEX identity_lookup
                ON identity(app_id, platform, kind, external_id, target_kind);
             CREATE TABLE context_seen (
                app_id TEXT NOT NULL,
                entity_id INTEGER NOT NULL,
                scene_type TEXT NOT NULL,
                scene_id INTEGER NOT NULL,
                identity_id INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY(app_id, entity_id, scene_type, scene_id)
             );
             CREATE INDEX context_entity ON context_seen(app_id, entity_id);
             CREATE TABLE profile_cache (
                app_id TEXT NOT NULL,
                entity_id INTEGER NOT NULL,
                display_name TEXT,
                card TEXT,
                avatar_url TEXT,
                updated_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL,
                PRIMARY KEY(app_id, entity_id)
             );
             CREATE INDEX profile_expiry ON profile_cache(expires_at);
             CREATE TABLE message (
                id INTEGER PRIMARY KEY AUTOINCREMENT CHECK(id<=2147483647),
                scope TEXT NOT NULL CHECK(scope IN ('private','group')),
                target INTEGER NOT NULL,
                official_id TEXT NOT NULL,
                inbound INTEGER NOT NULL CHECK(inbound IN (0,1)),
                created INTEGER NOT NULL,
                expires INTEGER NOT NULL,
                content TEXT NOT NULL,
                reference TEXT,
                next_seq INTEGER NOT NULL DEFAULT 1,
                UNIQUE(scope, target, official_id)
             );
             CREATE INDEX message_expiry ON message(expires);
             CREATE INDEX message_reply ON message(scope, target, inbound, created DESC, id DESC);
             CREATE TABLE gateway_session (
                shard_id INTEGER PRIMARY KEY CHECK(shard_id>=0),
                session_id TEXT NOT NULL,
                seq INTEGER NOT NULL,
                intents INTEGER NOT NULL DEFAULT 0,
                shard_count INTEGER NOT NULL DEFAULT 1
             );",
        )
        .map_err(database)?;
    transaction
        .execute_batch(&format!("PRAGMA user_version={SCHEMA_VERSION};"))
        .map_err(database)?;
    transaction.commit().map_err(database)
}

/// 把事件写入占位行，取得本地 ID 后回填 `message_id`。
pub(super) fn ensure_id_available(transaction: &rusqlite::Transaction<'_>) -> Result<(), Error> {
    let used: i64 = transaction
        .query_row(
            "SELECT COALESCE((SELECT seq FROM sqlite_sequence WHERE name='message'), 0)",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if used >= i64::from(i32::MAX) {
        return Err(Error::IdExhausted);
    }
    Ok(())
}

pub(super) fn store_content(
    transaction: &rusqlite::Transaction<'_>,
    content: &Value,
) -> Result<i32, Error> {
    let id = i32::try_from(transaction.last_insert_rowid()).map_err(|_| Error::IdExhausted)?;
    let mut value = content.clone();
    let Value::Object(object) = &mut value else {
        return Err(Error::InvalidContent);
    };
    object.insert("message_id".into(), Value::from(id));
    let serialized = serde_json::to_string(&value).map_err(database)?;
    transaction
        .execute(
            "UPDATE message SET content=?1 WHERE id=?2",
            params![serialized, id],
        )
        .map_err(database)?;
    Ok(id)
}

pub(super) fn image_in(event: &Value, file: &str) -> Option<String> {
    let segments = event.get("message")?.as_array()?;
    for segment in segments {
        if segment.get("type").and_then(Value::as_str) != Some("image") {
            continue;
        }
        let Some(data) = segment.get("data").and_then(Value::as_object) else {
            continue;
        };
        let candidate = data
            .get("file")
            .and_then(Value::as_str)
            .or_else(|| data.get("url").and_then(Value::as_str))?;
        let url = data
            .get("url")
            .and_then(Value::as_str)
            .filter(|url| is_http(url))
            .or_else(|| is_http(candidate).then_some(candidate))?;
        if candidate == file || data.get("url").and_then(Value::as_str) == Some(file) || url == file
        {
            return Some(url.to_owned());
        }
    }
    None
}

fn is_http(value: &str) -> bool {
    value.starts_with("http://") || value.starts_with("https://")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initializes_fresh_database_at_current_schema_version() {
        let mut connection = Connection::open_in_memory().unwrap();
        prepare(&mut connection).unwrap();
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        prepare(&mut connection).unwrap();
    }

    #[test]
    fn rejects_mismatched_schema_versions() {
        for unsupported in [1_i64, 3, 4, SCHEMA_VERSION - 1, SCHEMA_VERSION + 1] {
            let mut connection = Connection::open_in_memory().unwrap();
            connection
                .execute_batch(&format!("PRAGMA user_version={unsupported};"))
                .unwrap();
            assert!(matches!(
                prepare(&mut connection),
                Err(Error::SchemaVersion(version)) if version == unsupported
            ));
        }
    }
}
