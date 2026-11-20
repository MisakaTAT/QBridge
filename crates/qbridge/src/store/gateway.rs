//! Gateway 会话持久化。

use super::{Error, Store, database};
use qbridge_qq::{Session, gateway::SessionStore};
use rusqlite::{OptionalExtension, params};

impl Store {
    pub async fn gateway_session(&self) -> Result<Option<Session>, Error> {
        self.call(move |connection| {
            let row: Option<(String, i64, i64)> = connection
                .query_row(
                    "SELECT session_id, seq, intents FROM gateway_session WHERE single=1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .map_err(database)?;
            Ok(row.and_then(|(session_id, seq, intents)| {
                u64::try_from(seq).ok().and_then(|seq| {
                    u64::try_from(intents).ok().map(|intents| Session {
                        session_id,
                        seq,
                        intents,
                    })
                })
            }))
        })
        .await
    }

    pub async fn save_gateway_session(&self, session: &Session) -> Result<(), Error> {
        let session_id = session.session_id.clone();
        let seq = i64::try_from(session.seq).unwrap_or(i64::MAX);
        let intents = i64::try_from(session.intents).unwrap_or(i64::MAX);
        self.call(move |connection| {
            connection
                .execute(
                    "INSERT INTO gateway_session(single, session_id, seq, intents) VALUES(1, ?1, ?2, ?3) \
                     ON CONFLICT(single) DO UPDATE SET \
                     session_id=excluded.session_id, seq=excluded.seq, intents=excluded.intents",
                    params![session_id, seq, intents],
                )
                .map_err(database)?;
            Ok(())
        })
        .await
    }

    pub async fn clear_gateway_session(&self) -> Result<(), Error> {
        self.call(move |connection| {
            connection
                .execute("DELETE FROM gateway_session WHERE single=1", [])
                .map_err(database)?;
            Ok(())
        })
        .await
    }
}

#[async_trait::async_trait]
impl SessionStore for Store {
    async fn load(&self) -> Result<Option<Session>, qbridge_qq::Error> {
        self.gateway_session()
            .await
            .map_err(|_| qbridge_qq::Error::decode("cannot load Gateway session"))
    }

    async fn save(&self, session: &Session) -> Result<(), qbridge_qq::Error> {
        self.save_gateway_session(session)
            .await
            .map_err(|_| qbridge_qq::Error::decode("cannot save Gateway session"))
    }

    async fn clear(&self) -> Result<(), qbridge_qq::Error> {
        self.clear_gateway_session()
            .await
            .map_err(|_| qbridge_qq::Error::decode("cannot clear Gateway session"))
    }
}
