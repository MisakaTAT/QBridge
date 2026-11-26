//! Gateway 会话持久化。

use super::{Error, Store, database};
use qbridge_qq::{Session, gateway::SessionStore};
use rusqlite::{OptionalExtension, params};

impl Store {
    pub async fn gateway_session(&self, shard_id: u32) -> Result<Option<Session>, Error> {
        let shard_id = i64::from(shard_id);
        self.call(move |connection| {
            let row: Option<(String, i64, i64, i64)> = connection
                .query_row(
                    "SELECT session_id, seq, intents, shard_count FROM gateway_session WHERE shard_id=?1",
                    params![shard_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()
                .map_err(database)?;
            Ok(row.and_then(|(session_id, seq, intents, shard_count)| {
                let seq = u64::try_from(seq).ok()?;
                let intents = u64::try_from(intents).ok()?;
                let shard_count = u32::try_from(shard_count).ok().filter(|count| *count > 0)?;
                Some(Session {
                    session_id,
                    seq,
                    intents,
                    shard_count,
                })
            }))
        })
        .await
    }

    pub async fn save_gateway_session(
        &self,
        shard_id: u32,
        session: &Session,
    ) -> Result<(), Error> {
        let shard_id = i64::from(shard_id);
        let session_id = session.session_id.clone();
        let seq = i64::try_from(session.seq).unwrap_or(i64::MAX);
        let intents = i64::try_from(session.intents).unwrap_or(i64::MAX);
        let shard_count = i64::from(session.shard_count.max(1));
        self.call(move |connection| {
            connection
                .execute(
                    "INSERT INTO gateway_session(shard_id, session_id, seq, intents, shard_count) \
                     VALUES(?1, ?2, ?3, ?4, ?5) \
                     ON CONFLICT(shard_id) DO UPDATE SET \
                     session_id=excluded.session_id, seq=excluded.seq, intents=excluded.intents, \
                     shard_count=excluded.shard_count",
                    params![shard_id, session_id, seq, intents, shard_count],
                )
                .map_err(database)?;
            Ok(())
        })
        .await
    }

    pub async fn clear_gateway_session(&self, shard_id: u32) -> Result<(), Error> {
        let shard_id = i64::from(shard_id);
        self.call(move |connection| {
            connection
                .execute(
                    "DELETE FROM gateway_session WHERE shard_id=?1",
                    params![shard_id],
                )
                .map_err(database)?;
            Ok(())
        })
        .await
    }
}

#[async_trait::async_trait]
impl SessionStore for Store {
    async fn load(&self, shard_id: u32) -> Result<Option<Session>, qbridge_qq::Error> {
        self.gateway_session(shard_id)
            .await
            .map_err(|_| qbridge_qq::Error::decode("cannot load Gateway session"))
    }

    async fn save(&self, shard_id: u32, session: &Session) -> Result<(), qbridge_qq::Error> {
        self.save_gateway_session(shard_id, session)
            .await
            .map_err(|_| qbridge_qq::Error::decode("cannot save Gateway session"))
    }

    async fn clear(&self, shard_id: u32) -> Result<(), qbridge_qq::Error> {
        self.clear_gateway_session(shard_id)
            .await
            .map_err(|_| qbridge_qq::Error::decode("cannot clear Gateway session"))
    }
}
